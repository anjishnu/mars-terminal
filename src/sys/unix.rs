//! Unix adapter for the platform abstraction layer.
//!
//! Every `std::os::unix` / `libc` call the application makes lives here.
//! Each function wraps *exactly* the behavior the pre-abstraction code had, so the
//! Unix build is byte-for-byte identical after the migration.

/// Where the app's files live.
pub mod paths {
    use std::path::PathBuf;

    /// The env var `home_dir` reads — for tests that redirect the home dir.
    pub const HOME_ENV: &str = "HOME";

    /// The user's home directory (`$HOME`). All of `~/.mars`, `~/.config/mars`,
    /// and `~/.local/state/mars` are derived from this by the call sites.
    pub fn home_dir() -> Option<PathBuf> {
        std::env::var_os(HOME_ENV).map(PathBuf::from)
    }
}

/// A named, same-machine, bidirectional byte channel — the session control plane.
///
/// The wire protocol (JSON-line `ClientFrame`/`ServerFrame`) is written once
/// against `Stream: Read + Write` and never learns what's underneath. On Unix the
/// channel is a Unix-domain socket; the Windows adapter uses authenticated
/// loopback TCP with the same method surface (`read`, `write`, `flush`,
/// `try_clone`, `set_read_timeout` on `Stream`; `bind` +
/// `incoming()`/`accept()` on `Listener`).
pub mod control {
    use std::io;
    use std::path::Path;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum Probe {
        Live,
        Dead,
        Indeterminate,
    }

    pub type Stream = std::os::unix::net::UnixStream;
    pub struct Listener(std::os::unix::net::UnixListener);

    impl Listener {
        pub fn accept(&self) -> io::Result<Stream> {
            self.0.accept().map(|(stream, _)| stream)
        }

        pub fn incoming(&self) -> impl Iterator<Item = io::Result<Stream>> + '_ {
            std::iter::from_fn(move || Some(self.accept()))
        }
    }

    /// Connect to the channel at `addr` (a filesystem socket path on Unix).
    pub fn connect(addr: impl AsRef<Path>) -> io::Result<Stream> {
        Stream::connect(addr.as_ref())
    }

    /// Bind a listener at `addr`.
    pub fn bind(addr: impl AsRef<Path>) -> io::Result<Listener> {
        std::os::unix::net::UnixListener::bind(addr.as_ref()).map(Listener)
    }

    /// Classify a rendezvous without deleting it on permission or timeout errors.
    pub fn probe(addr: impl AsRef<Path>) -> Probe {
        match Stream::connect(addr.as_ref()) {
            Ok(_) => Probe::Live,
            Err(e)
                if matches!(
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
    /// Restore the controlling terminal to a sane cooked mode (`OPOST|ONLCR`,
    /// `ICANON|ECHO|ECHOE|ISIG`, `ICRNL`) after a force-killed program left it
    /// raw — the "staircase output" repair. Idempotent; a no-op when stdout isn't
    /// a TTY, and run before entering the TUI so crossterm saves a *sane* state to
    /// restore on exit.
    pub fn sanitize() {
        unsafe {
            if libc::isatty(libc::STDOUT_FILENO) != 1 {
                return;
            }
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDOUT_FILENO, &mut t) == 0 {
                t.c_oflag |= libc::OPOST | libc::ONLCR;
                t.c_lflag |= libc::ICANON | libc::ECHO | libc::ECHOE | libc::ISIG;
                t.c_iflag |= libc::ICRNL;
                let _ = libc::tcsetattr(libc::STDOUT_FILENO, libc::TCSANOW, &t);
            }
        }
    }

    /// Is stdin a real terminal?
    pub fn is_stdin_tty() -> bool {
        unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
    }
}

/// Spawn a process detached from this terminal (the session daemon).
pub mod daemon {
    use std::process::Command;

    /// Configure `cmd` so the spawned child becomes a session leader, fully
    /// detached from this controlling terminal, and survives the window closing.
    pub fn detach(cmd: &mut Command) {
        use std::os::unix::process::CommandExt;
        // SAFETY: setsid() is async-signal-safe and the closure does nothing else.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
}

/// Process identity and lifecycle.
pub mod proc {
    /// A per-user tag used to namespace runtime sockets — the numeric uid on Unix.
    pub fn uid_tag() -> String {
        unsafe { libc::getuid() }.to_string()
    }

    /// This machine's hostname, for broker fleet status.
    #[cfg(feature = "ssh")]
    pub fn hostname() -> Option<String> {
        let mut buf = [0u8; 256];
        if unsafe { libc::gethostname(buf.as_mut_ptr().cast::<libc::c_char>(), buf.len()) } != 0 {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        let name = String::from_utf8_lossy(&buf[..end]).trim().to_string();
        (!name.is_empty()).then_some(name)
    }

    /// Kill every process whose command line contains `needle`. Best-effort.
    pub fn kill_matching(needle: &str) {
        let _ = std::process::Command::new("pkill")
            .arg("-f")
            .arg(needle)
            .status();
    }

    /// Force-stop Mars daemons that survived their graceful control request.
    pub fn kill_all_mars() {
        for needle in ["mars --server", "mars keyd"] {
            kill_matching(needle);
        }
    }
}

/// Make a path private to its owning user.
pub mod fsperm {
    use std::io;
    use std::path::Path;

    /// Restrict a directory to its owner (`0700`).
    pub fn restrict_dir(path: &Path) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }

    /// Restrict a file or Unix-domain socket to its owner (`0600`).
    #[cfg(feature = "ssh")]
    pub fn restrict_file(path: &Path) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }
}

/// Which shell a new terminal pane runs.
pub mod shell {
    /// `$SHELL`, falling back to `/bin/bash` — exactly the pre-abstraction
    /// behavior of `terminal.rs`.
    pub fn default_shell() -> String {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".to_string())
    }
}

/// Host health probes for the SPACES status line (load, disk, memory). Only this
/// PAL adapter may name `libc`; callers reach these through `sys::health`.
pub mod health {
    use std::path::Path;

    /// 1-minute load average, if the OS reports one.
    pub fn load_avg1() -> Option<f64> {
        let mut avg = [0f64; 3];
        let n = unsafe { libc::getloadavg(avg.as_mut_ptr(), 3) };
        if n >= 1 { Some(avg[0]) } else { None }
    }

    /// Percent of the filesystem holding `path` that is FREE to a normal user (0..=100).
    pub fn disk_free_pct(path: &Path) -> Option<u8> {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 { return None; }
        let total = s.f_blocks as u128;
        if total == 0 { return None; }
        let free = s.f_bavail as u128;
        Some(((free * 100 / total) as u8).min(100))
    }

    /// Percent of host RAM currently in use (0..=100).
    pub fn mem_used_pct() -> Option<u8> { imp::mem_used_pct() }

    #[cfg(target_os = "macos")]
    mod imp {
        // `mach_host_self` is marked deprecated in `libc` (it points at the `mach2`
        // crate), but it's the stable Mach entry point and adding a dependency just for
        // the rename isn't worth it — the call is correct.
        #[allow(deprecated)]
        pub fn mem_used_pct() -> Option<u8> {
            let total = memsize()? as u128;
            if total == 0 { return None; }
            let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
            if page <= 0 { return None; }
            let page = page as u64;
            let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
            let mut count = (std::mem::size_of::<libc::vm_statistics64>()
                / std::mem::size_of::<libc::integer_t>()) as libc::mach_msg_type_number_t;
            let kr = unsafe {
                libc::host_statistics64(
                    libc::mach_host_self(),
                    libc::HOST_VM_INFO64,
                    &mut stats as *mut _ as *mut libc::integer_t,
                    &mut count,
                )
            };
            if kr != libc::KERN_SUCCESS { return None; }
            // Activity-Monitor-style "used": active + wired + compressed pages.
            let used_pages = stats.active_count as u64
                + stats.wire_count as u64
                + stats.compressor_page_count as u64;
            let used = used_pages.saturating_mul(page) as u128;
            Some(((used * 100 / total) as u8).min(100))
        }

        fn memsize() -> Option<u64> {
            let mut val: u64 = 0;
            let mut len = std::mem::size_of::<u64>();
            let name = b"hw.memsize\0";
            let r = unsafe {
                libc::sysctlbyname(
                    name.as_ptr() as *const libc::c_char,
                    &mut val as *mut _ as *mut libc::c_void,
                    &mut len,
                    std::ptr::null_mut(),
                    0,
                )
            };
            if r == 0 && val > 0 { Some(val) } else { None }
        }
    }

    #[cfg(target_os = "linux")]
    mod imp {
        // From /proc/meminfo. `MemAvailable` is the kernel's own "usable without
        // swapping" figure — the right basis for a used% matching `free`. NOT verified
        // on Linux from the macOS dev box.
        pub fn mem_used_pct() -> Option<u8> {
            let txt = std::fs::read_to_string("/proc/meminfo").ok()?;
            let (mut total, mut avail) = (0u64, 0u64);
            for line in txt.lines() {
                if let Some(v) = line.strip_prefix("MemTotal:") {
                    total = v.split_whitespace().next()?.parse().ok()?;
                } else if let Some(v) = line.strip_prefix("MemAvailable:") {
                    avail = v.split_whitespace().next()?.parse().ok()?;
                }
            }
            if total == 0 { return None; }
            let used = total.saturating_sub(avail) as u128;
            Some(((used * 100 / total as u128) as u8).min(100))
        }
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    mod imp {
        pub fn mem_used_pct() -> Option<u8> { None }
    }
}
