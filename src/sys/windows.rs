//! Windows adapter for the platform abstraction layer.
//!
//! Compiled and selfchecked on Windows 11 (aarch64 + x86_64 MSVC). The contract
//! each module satisfies is the Unix adapter (`sys/unix.rs`) — same signatures,
//! so the rest of the tree compiles unchanged.

/// Where the app's files live.
pub mod paths {
    use std::path::PathBuf;

    /// The env var `home_dir` reads — for tests that redirect the home dir.
    pub const HOME_ENV: &str = "USERPROFILE";

    /// The user's home directory. Windows has no `$HOME`; use `%USERPROFILE%`
    /// (falling back to `%HOMEDRIVE%%HOMEPATH%`). The call sites then append
    /// `.mars` etc., so a Windows user gets `C:\Users\me\.mars\...` — acceptable
    /// for the MVP. A later refinement can move state under `%LOCALAPPDATA%`
    /// (see the `paths` port note in WINDOWS_PORT.md).
    pub fn home_dir() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os(HOME_ENV) {
            return Some(PathBuf::from(p));
        }
        match (std::env::var_os("HOMEDRIVE"), std::env::var_os("HOMEPATH")) {
            (Some(d), Some(p)) => {
                let mut s = std::ffi::OsString::from(d);
                s.push(p);
                Some(PathBuf::from(s))
            }
            _ => None,
        }
    }
}

/// A named, same-machine, bidirectional byte channel — the session control plane.
///
/// On Windows the channel is a **loopback TCP socket** with a filesystem
/// rendezvous: `bind` listens on `127.0.0.1:0` and writes `"<port> <token>"`
/// into the file at `addr`. Named pipes were the first draft, but
/// `interprocess`'s pipe streams cannot set read/write timeouts — and the
/// daemon poll loops depend on them — while `TcpStream` carries the exact
/// `UnixStream` method surface (`try_clone`, `set_read_timeout`,
/// `set_write_timeout`).
///
/// The rendezvous file keeps every Unix-socket-path semantic the callers rely
/// on: a live channel is a file that exists, a stale one is swept with
/// `remove_file`, `rename` moves a live session, and `bind` over an existing
/// file fails like `AddrInUse`. A random token drives a nonce/HMAC handshake
/// before either side exposes the stream. The file is ACL-protected under the
/// user profile, and mutual proof also prevents a process that rebinds a stale
/// port from impersonating the recorded listener.
pub mod control {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    use std::io::{self, Read, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::path::Path;
    use std::time::{Duration, Instant};

    type HmacSha256 = Hmac<Sha256>;

    #[derive(Debug)]
    struct ServerAuthenticationFailed;

    impl std::fmt::Display for ServerAuthenticationFailed {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("control-channel server authentication failed")
        }
    }

    impl std::error::Error for ServerAuthenticationFailed {}

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Probe {
        Live,
        Dead,
        Indeterminate,
    }

    /// How long `accept` waits for a connector to present the token. Generous
    /// for a same-machine round-trip; bounds how long a broken or hostile
    /// connection can stall the daemon's accept loop.
    const HANDSHAKE_TIMEOUT: Duration = Duration::from_millis(500);
    const CONTROL_PROTOCOL: &str = "2";
    const NONCE_HEX_LEN: usize = 32;
    const PROOF_HEX_LEN: usize = 64;

    /// A connected channel end — `Read + Write + Send`, `try_clone`,
    /// `set_read_timeout`, `set_write_timeout`, mirroring `UnixStream`.
    pub struct Stream(TcpStream);

    impl Stream {
        pub fn try_clone(&self) -> io::Result<Stream> {
            self.0.try_clone().map(Stream)
        }
        pub fn set_read_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
            self.0.set_read_timeout(dur)
        }
        pub fn set_write_timeout(&self, dur: Option<Duration>) -> io::Result<()> {
            self.0.set_write_timeout(dur)
        }
    }

    impl Read for Stream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }
    impl Read for &Stream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            (&self.0).read(buf)
        }
    }
    impl Write for Stream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            self.0.flush()
        }
    }
    impl Write for &Stream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            (&self.0).write(buf)
        }
        fn flush(&mut self) -> io::Result<()> {
            (&self.0).flush()
        }
    }

    /// A bound channel — yields token-verified `Stream`s via `incoming`/`accept`.
    pub struct Listener {
        inner: TcpListener,
        token: String,
    }

    impl Listener {
        /// Accept the next *authenticated* connection. A connector that fails
        /// the token handshake is dropped and never surfaced — a hostile local
        /// process must not be able to reach the frame protocol, nor kill the
        /// daemon's accept loop with a handshake error.
        pub fn accept(&self) -> io::Result<Stream> {
            loop {
                let (stream, _) = self.inner.accept()?;
                if let Some(s) = self.handshake(stream) {
                    return Ok(s);
                }
            }
        }

        pub fn incoming(&self) -> impl Iterator<Item = io::Result<Stream>> + '_ {
            std::iter::from_fn(move || Some(self.accept()))
        }

        fn handshake(&self, mut stream: TcpStream) -> Option<Stream> {
            let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
            stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT)).ok()?;
            let nonce = read_handshake_line(&mut stream, NONCE_HEX_LEN, deadline).ok()?;
            if !is_lower_hex(&nonce, NONCE_HEX_LEN) {
                return None;
            }
            let server_proof = proof(&self.token, b"server", &nonce).ok()?;
            stream.write_all(server_proof.as_bytes()).ok()?;
            stream.write_all(b"\n").ok()?;
            stream.flush().ok()?;

            let client_proof = read_handshake_line(&mut stream, PROOF_HEX_LEN, deadline).ok()?;
            let expected = proof(&self.token, b"client", &nonce).ok()?;
            if !constant_time_eq(expected.as_bytes(), &client_proof) {
                return None;
            }
            stream.set_read_timeout(None).ok()?;
            stream.set_write_timeout(None).ok()?;
            Some(Stream(stream))
        }
    }

    fn read_handshake_line(
        stream: &mut TcpStream,
        max: usize,
        deadline: Instant,
    ) -> io::Result<Vec<u8>> {
        let mut got = Vec::with_capacity(max);
        let mut byte = [0u8; 1];
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "control-channel authentication timed out",
                ));
            }
            stream.set_read_timeout(Some(remaining))?;
            match stream.read(&mut byte) {
                Ok(1) if byte[0] == b'\n' => return Ok(got),
                Ok(1) if byte[0] != b'\r' && got.len() < max => got.push(byte[0]),
                Ok(1) => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid control-channel handshake",
                    ));
                }
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "control channel closed during authentication",
                    ));
                }
                Ok(_) => unreachable!(),
                Err(e) => return Err(e),
            }
        }
    }

    fn is_lower_hex(value: &[u8], len: usize) -> bool {
        value.len() == len
            && value
                .iter()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    }

    fn constant_time_eq(expected: &[u8], got: &[u8]) -> bool {
        expected.len() == got.len()
            && expected
                .iter()
                .zip(got)
                .fold(0u8, |diff, (left, right)| diff | (left ^ right))
                == 0
    }

    fn hex(bytes: &[u8]) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }

    fn proof(token: &str, role: &[u8], nonce: &[u8]) -> io::Result<String> {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(token.as_bytes())
            .map_err(|_| io::Error::other("invalid control-channel token"))?;
        mac.update(role);
        mac.update(b"\0");
        mac.update(nonce);
        Ok(hex(&mac.finalize().into_bytes()))
    }

    /// `"2 <port> <token>"` from the rendezvous file at `addr`. A missing file
    /// surfaces the same `NotFound` a missing Unix socket would.
    fn read_addr(addr: &Path) -> io::Result<(u16, String)> {
        let s = std::fs::read_to_string(addr)?;
        let mut it = s.split_whitespace();
        match (
            it.next(),
            it.next().and_then(|p| p.parse::<u16>().ok()),
            it.next(),
            it.next(),
        ) {
            (Some(CONTROL_PROTOCOL), Some(port), Some(token), None)
                if is_lower_hex(token.as_bytes(), NONCE_HEX_LEN) =>
            {
                Ok((port, token.to_string()))
            }
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "outdated or malformed control-channel rendezvous file: {}; restart Mars",
                    addr.display()
                ),
            )),
        }
    }

    /// 128 bits of operating-system randomness for the per-listener secret.
    fn fresh_token() -> io::Result<String> {
        let mut bytes = [0u8; 16];
        getrandom::getrandom(&mut bytes)
            .map_err(|e| io::Error::other(format!("channel token generation failed: {e}")))?;
        Ok(hex(&bytes))
    }

    pub fn connect(addr: impl AsRef<Path>) -> io::Result<Stream> {
        let (port, token) = read_addr(addr.as_ref())?;
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port))?;
        let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
        stream.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        let nonce = fresh_token()?;
        stream.write_all(nonce.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        let server_proof = read_handshake_line(&mut stream, PROOF_HEX_LEN, deadline)?;
        let expected = proof(&token, b"server", nonce.as_bytes())?;
        if !constant_time_eq(expected.as_bytes(), &server_proof) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                ServerAuthenticationFailed,
            ));
        }
        let client_proof = proof(&token, b"client", nonce.as_bytes())?;
        stream.write_all(client_proof.as_bytes())?;
        stream.write_all(b"\n")?;
        stream.flush()?;
        stream.set_read_timeout(None)?;
        stream.set_write_timeout(None)?;
        Ok(Stream(stream))
    }

    pub fn bind(addr: impl AsRef<Path>) -> io::Result<Listener> {
        let inner = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = inner.local_addr()?.port();
        let token = fresh_token()?;
        // create_new: binding over an existing file must fail like AddrInUse
        // does on Unix — a second daemon must never hijack a live session's name.
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(addr.as_ref())?;
        f.write_all(format!("{CONTROL_PROTOCOL} {port} {token}\n").as_bytes())?;
        Ok(Listener { inner, token })
    }

    /// Authenticate the recorded listener without treating a timeout or legacy
    /// descriptor as proof that its owner is dead.
    pub fn probe(addr: impl AsRef<Path>) -> Probe {
        match connect(addr) {
            Ok(_) => Probe::Live,
            Err(e)
                if e
                    .get_ref()
                    .and_then(|inner| inner.downcast_ref::<ServerAuthenticationFailed>())
                    .is_some()
                    || matches!(
                        e.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                    ) =>
            {
                Probe::Dead
            }
            Err(_) => Probe::Indeterminate,
        }
    }
}

/// Terminal hygiene.
pub mod tty {
    /// No-op on Windows: crossterm's raw-mode teardown already restores the
    /// console on exit, and there is no termios cooked-mode to repair.
    pub fn sanitize() {}

    /// Is stdin a real terminal? `std::io::IsTerminal` is cross-platform and
    /// backed by `GetConsoleMode` on Windows.
    pub fn is_stdin_tty() -> bool {
        use std::io::IsTerminal;
        std::io::stdin().is_terminal()
    }
}

/// Spawn a process detached from this console (the session daemon).
pub mod daemon {
    use std::process::Command;

    // CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW: the daemon runs with a
    // hidden console of its own — detached enough to survive this window
    // closing, but still console-backed so ConPTY panes can spawn from it.
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    /// Detach the child from this console so it survives the window closing.
    pub fn detach(cmd: &mut Command) {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
}

/// Process identity and lifecycle.
pub mod proc {
    /// No `lsof` here, and the remedy reads fine without a pid. See the Unix implementation.
    pub fn listener_pid(_port: u16) -> Option<u32> {
        None
    }

    /// A per-user tag to namespace runtime channels. Windows has no uid; use
    /// the username (sanitized to path-safe chars).
    pub fn uid_tag() -> String {
        std::env::var("USERNAME")
            .unwrap_or_else(|_| "user".into())
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    }

    /// This machine's hostname, for broker fleet status.
    #[cfg(feature = "ssh")]
    pub fn hostname() -> Option<String> {
        std::env::var("COMPUTERNAME")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// Kill every process whose executable and arguments match `needle`
    /// (`killall`). Shells out to PowerShell's CIM sweep — the same best-effort
    /// contract as Unix `pkill -f`.
    pub fn kill_matching(needle: &str) {
        let wildcard_literal = |s: &str| {
            s.replace('\'', "''")
                .replace('`', "``")
                .replace('[', "`[")
                .replace(']', "`]")
                .replace('*', "`*")
                .replace('?', "`?")
        };
        let predicate = if let Some((program, args)) = needle.split_once(' ') {
            let program = if program.to_ascii_lowercase().ends_with(".exe") {
                program.to_string()
            } else {
                format!("{program}.exe")
            };
            let program = program.replace('\'', "''");
            let args = wildcard_literal(args.trim());
            format!("$_.Name -ieq '{program}' -and $_.CommandLine -like '*{args}*'")
        } else {
            let needle = wildcard_literal(needle);
            format!("$_.CommandLine -like '*{needle}*'")
        };
        let caller_pid = std::process::id();
        let script = format!(
            "Get-CimInstance Win32_Process | \
             Where-Object {{ {predicate} -and $_.ProcessId -ne {caller_pid} }} | \
             ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}"
        );
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }

    pub(crate) fn kill_all_mars_script(caller_pid: u32) -> String {
        format!(
            "Get-CimInstance Win32_Process -Filter \"Name = 'mars.exe'\" | \
             Where-Object {{ $_.ProcessId -ne {caller_pid} }} | \
             ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force -ErrorAction SilentlyContinue }}"
        )
    }

    /// Force-stop every other `mars.exe`. `killall` has already asked reachable
    /// sessions to autosave, so this is the intentionally broad recovery path.
    pub fn kill_all_mars() {
        let script = kill_all_mars_script(std::process::id());
        let _ = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &script])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// Make a path private to its owning user.
pub mod fsperm {
    use std::io;
    use std::path::Path;

    /// No-op for the MVP: a directory created under the user profile inherits
    /// an ACL that already restricts it to the owner (+ SYSTEM/Administrators).
    /// HARDENING: set an explicit owner-only DACL for parity with Unix `0700`
    /// once state can live outside the profile.
    pub fn restrict_dir(_path: &Path) -> io::Result<()> {
        Ok(())
    }

    /// Rendezvous files currently inherit the same user-profile ACL as the
    /// containing directory. Explicit creation-time DACLs are the next hardening
    /// milestone documented in WINDOWS_PORT.md.
    #[cfg(feature = "ssh")]
    pub fn restrict_file(_path: &Path) -> io::Result<()> {
        Ok(())
    }
}

/// Which shell a new terminal pane runs.
pub mod shell {
    /// Windows has no `$SHELL` (and inherited ones often hold unspawnable
    /// Unix paths, e.g. under Git Bash) — prefer PowerShell 7, then Windows
    /// PowerShell, then `%ComSpec%` (cmd).
    pub fn default_shell() -> String {
        for cand in ["pwsh.exe", "powershell.exe"] {
            if on_path(cand) {
                return cand.to_string();
            }
        }
        std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string())
    }

    fn on_path(exe: &str) -> bool {
        std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|d| d.join(exe).is_file()))
            .unwrap_or(false)
    }
}

/// Host health probes — Windows stubs (return None until wired to
/// GlobalMemoryStatusEx / GetDiskFreeSpaceExW). The SPACES line simply omits any
/// segment it can't source, so `None` degrades cleanly.
pub mod health {
    use std::path::Path;
    pub fn load_avg1() -> Option<f64> { None }
    pub fn disk_free_pct(_path: &Path) -> Option<u8> { None }
    pub fn mem_used_pct() -> Option<u8> { None }
}
