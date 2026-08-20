//! The Rover bridge — `mars serve` (WebSocket ⇄ session-socket pump) and `mars qr`
//! (in-terminal pairing QR). Gated behind the `web` feature so the base binary pays
//! nothing.
//!
//! Design (see `design_ideas/mobile-pwa-mvp.md`, `rover-brand.md`):
//!  - The bridge is a **second client of the existing session socket** — it reuses the
//!    same `ClientFrame`/`ServerFrame` protocol the desktop client speaks; the daemon
//!    is unchanged. Synchronous, thread-per-connection, matching the codebase.
//!  - **Scope of this scaffold:** it stands up the transport and bridges the *raw
//!    terminal* — daemon `Output` (ANSI) → WS `{t:"output"}` frames, and inbound
//!    keystrokes → the pane. That powers Rover's *peek* rung end-to-end. The
//!    **structured meaning channel** (JSON board/verdict/summary events the phone
//!    renders natively) is the next daemon-side step: the daemon must emit
//!    `bar_workspace_rows()`/`ShiftReport` as JSON. Until then the phone shows the peek
//!    rung against a live daemon and the semantic surfaces against the mock backend.
//!  - Transport is LAN http for the prototype (this file); a tunnel/Tailscale endpoint
//!    is the same bridge reached differently (the `Transport` seam is the deploy story).

use anyhow::{anyhow, Result};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::session::{self, ClientFrame, ServerFrame};
use tungstenite::{accept, Error as WsError, Message};

// Not a const: `MARS_BRIDGE_PORT` lets a second bridge run beside a live one.
fn default_port() -> u16 { crate::session::bridge_port() }

/// Resolve the session to bridge: explicit arg → `$MARS_SESSION` → the first live one.
fn resolve_session(arg: Option<String>) -> Result<String> {
    if let Some(name) = arg {
        return Ok(name);
    }
    if let Ok(name) = std::env::var("MARS_SESSION") {
        if !name.is_empty() {
            // Through the directory: the variable holds the BIRTH name, which stops being the
            // session's name at the first rename. See `live_session_name`.
            return Ok(session::live_session_name(&name));
        }
    }
    // The ATTACHED session, not merely the first listed. Taking the first is how a bridge ended
    // up bound to a name whose daemon had no socket, then forwarded an empty board indefinitely —
    // which looks exactly like a broken phone app rather than a misdirected bridge.
    session::attached_session()
        .ok_or_else(|| anyhow!("no sessions found — start one with `mars` first"))
}

/// Best-effort LAN IP: the source address the OS would use to reach the internet.
/// No packet is actually sent (UDP connect just fixes the route).
fn lan_ip() -> String {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| {
            s.connect("8.8.8.8:80")?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "127.0.0.1".to_string())
}

/// 128-bit hex token from the OS CSPRNG (single-use pairing token, short-lived by policy).
/// 128 bits from the OS, or nothing.
///
/// This used to swallow both failures — the open and the read — leaving `buf` as sixteen zero
/// bytes and minting the token `00000000000000000000000000000000`, silently, while pairing
/// reported success. An attacker who can reach the tunnel tries that constant first.
///
/// There is no safe degraded mode for a secret, so the only honest failure is to refuse. Note the
/// asymmetry with everything else in this file: elsewhere a missing piece degrades to a smaller
/// feature, because the worst case is a duller product. Here the worst case is an open door.
fn mint_token() -> Result<String> {
    crate::session::mint_hex(16)
}

/// The current pairing token, persisted at `~/.mars/serve.token` so it survives a bridge restart
/// (a launchd relaunch keeps the same token → paired phones reconnect with no re-scan). It's what
/// the bridge validates each phone's `{t:"auth"}` frame against; `mars serve --reset` rotates it,
/// which refuses stale tokens and drops every connected phone.
fn token_file() -> Option<std::path::PathBuf> {
    crate::sys::paths::home_dir().map(|h| h.join(".mars").join("serve.token"))
}
fn read_token() -> Option<String> {
    token_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
fn write_token(tok: &str) {
    if let Some(p) = token_file() {
        if let Some(dir) = p.parent() { let _ = std::fs::create_dir_all(dir); }
        let _ = std::fs::write(&p, tok);
        // 0600, and RETROACTIVELY: files written before this line existed inherited the umask
        // (world-readable), and SECURITY.md promises user-only. Applied on every write and on
        // every ensure, so one bridge start heals an old file.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
        }
    }
}
/// Read the persisted token, minting + storing one on first use. Propagates a minting failure
/// rather than serving: a bridge that cannot produce a credential must not accept connections.
fn ensure_token() -> Result<String> {
    if let Some(t) = read_token() {
        // Re-assert 0600 on the existing file — see write_token.
        write_token(&t);
        return Ok(t);
    }
    let t = mint_token()?;
    write_token(&t);
    Ok(t)
}

/// The pairing fingerprint, re-exported from `session` where it now lives.
///
/// It moved because it is not a bridge concept: it identifies THIS MACHINE, and `mars attach` has
/// to answer "is this link about the machine I am sitting at" with no bridge compiled in at all.
/// A `web`-gated identity would have made the local attach shortcut impossible in exactly the
/// build most likely to want it.
fn daemon_fingerprint(session: &str) -> String {
    crate::session::daemon_fingerprint(session)
}

// ── mars qr ──────────────────────────────────────────────────────────────────

/// The LAN pairing link: the bundle served by THIS host, with the credentials in the fragment.
///
/// One builder, because it is now printed as a QR, opened in a browser and named in the pair
/// output. Three copies of a URL whose fragment carries a single-use token is three chances for
/// one of them to drift into a link that looks right and pairs nothing.
///
/// The fragment, never a query — it stays out of server logs and out of the Referer header.
/// THE pairing link — the one `mars pair`, `mars pair --link` and `mars pair --open` all hand out.
///
/// Hosted app plus the tunnel over `wss`, which is the only shape that works from everywhere: this
/// machine, a phone on any network, another computer. Pasted into the web view it connects.
///
/// One builder because there were four, and a URL whose fragment carries a single-use token is a
/// bad thing to have four copies of. Two of them had already drifted: `--open` was handing out a
/// loopback address the hosted app cannot dial, and `mars qr` a LAN address the bridge does not
/// even listen on.
pub fn pair_link(session: &str, tunnel_base: &str, route: &str) -> Result<String> {
    let endpoint = format!("{}/ws", tunnel_base.replacen("https://", "wss://", 1));
    let token = ensure_token()?;
    let fp = daemon_fingerprint(session);
    Ok(format!(
        "https://mars-terminal.lovable.app/{route}#h={endpoint}&id={fp}&t={token}&s={session}&v=rover-1"
    ))
}

/// The three routes in, named by where the reader is standing rather than by mechanism.
///
/// Printed wherever the link is, so the terminal says the same thing every time. It used to open
/// with a QR — which answers "how" before anyone has decided "which", and serves the one case a
/// person sitting at this machine is least likely to be in.
pub fn print_routes() {
    println!("  \x1b[1mOn this machine\x1b[0m      mars pair --open        opens a browser, already paired");
    println!("  \x1b[1mOn your phone\x1b[0m        scan the code below     camera at the QR");
    println!("  \x1b[1mAnother computer\x1b[0m     paste the link below    any network");
    println!();
}

pub fn lan_pair_url(session: &str) -> Result<String> {
    pair_url_for(session, &lan_ip())
}

/// The same link, aimed at a chosen host.
///
/// **The desktop case must use loopback, and this is not a preference.** The bridge binds
/// `127.0.0.1` only — verified with `lsof`: `TCP 127.0.0.1:8787 (LISTEN)`. The LAN address answers
/// nothing, so `mars pair --open` was opening a browser onto a refused connection while reporting
/// "Paired on open". The QR keeps the LAN address because a phone cannot use loopback; that path
/// needs the bridge bound wider, which is a separate question from this one.
pub fn pair_url_for(session: &str, host: &str) -> Result<String> {
    let ip = host.to_string();
    let port = default_port();
    let token = mint_token()?;
    let fp = daemon_fingerprint(session);
    let endpoint = format!("ws://{ip}:{port}/ws");
    Ok(format!(
        "http://{ip}:{port}/rover#h={endpoint}&id={fp}&t={token}&s={session}&v=rover-1"
    ))
}

/// `mars pair --desk` — the local link for the full-screen web terminal.
///
/// Aimed at a dev server rather than the hosted app, and at THIS bridge over plain `ws://`. Both
/// are deliberate: `/desk` is not deployed yet, and a page served over http may open a ws socket
/// to the same host, which is the combination that works today without a tunnel.
///
/// It exists because the alternative was telling somebody to write a session into localStorage by
/// hand. A URL is a thing you can paste; a JSON blob typed into a console is not.
pub fn desk_main(session_arg: Option<String>, web: Option<String>) -> Result<()> {
    let session = resolve_session(session_arg)?;
    let web = web.unwrap_or_else(|| "http://localhost:8080".into());
    let port = default_port();
    let token = ensure_token()?;
    let fp = daemon_fingerprint(&session);
    let endpoint = format!("ws://127.0.0.1:{port}/ws");

    if !bridge_listening() {
        println!("  No bridge on :{port}. Start one first:");
        println!("    mars pair --supervise           (or: MARS_BRIDGE_PORT={port} mars pair {session})");
        println!();
    }
    println!("  \x1b[38;5;208mmars\x1b[0m — the web terminal  ·  session \x1b[1m{session}\x1b[0m");
    println!();
    println!("  {web}/desk#h={endpoint}&id={fp}&t={token}&s={session}&v=rover-1");
    println!();
    println!("  Paste that into a browser ON THIS MACHINE. The session must be running on a binary");
    println!("  that knows `Mirror` — if the screen stays black, `mars reboot {session}` is why.");

    // AND THE ONE THAT CAN LEAVE THE MACHINE.
    //
    // The line above is loopback, which is right for the browser on this desk and useless
    // everywhere else — a phone cannot reach `127.0.0.1`, and the hosted app is https and cannot
    // dial `ws://` at all. `--desk` was the fifth builder of this URL and the only one that never
    // learned the tunnel, so "open the web terminal on my phone" had no answer that worked.
    match running_tunnel_url() {
        Some(base) => {
            if let Err(why) = tunnel_answers(&base) {
                eprintln!("{}", tunnel_warning(&why));
            }
            println!();
            println!("  From anywhere else — phone, another laptop — over the tunnel:");
            println!();
            // QUOTED, because the shell eats it otherwise. `&` backgrounds and `#` starts a
            // comment, so an unquoted paste is silently truncated at the first ampersand and the
            // credentials never arrive. Remembering to quote is not the reader's problem to solve
            // at the moment they are copying something they cannot read.
            println!("  mars attach '{}'", pair_link(&session, &base, "desk")?);
        }
        None => {
            println!();
            println!("  No tunnel is running, so there is no link that works off this machine.");
            println!("  `mars pair {session}` brings one up; `mars pair --link` prints the phone's.");
        }
    }
    Ok(())
}

/// `mars pair --open` — the desktop case, with no pairing step at all.
///
/// **The most common way somebody first meets Rover is sitting at the machine running it**, and
/// until now that path went through a phone camera: print a QR, pick up a handset, scan a screen
/// you are already looking at. The link that QR encodes already works in a browser, already serves
/// the bundle from this host, and already carries the credentials in its fragment — so the session
/// is paired before first paint and there is nothing to type, scan or paste.
///
/// LAN only, and that is not a limitation to paper over: the hosted site is `https`, and a page on
/// `https` cannot dial `ws://192.168.x.x`. Serving the bundle from the host is what makes the
/// origin match. `mars pair` (tunnel) is the answer for a phone that is not on this wifi.
pub fn open_main(session_arg: Option<String>) -> Result<()> {
    let session = resolve_session(session_arg)?;
    // THE SAME LINK `mars pair` PRINTS. Not a loopback variant, not a LAN one: one URL that
    // connects wherever it is pasted, which is the whole contract. It needs the tunnel, so the
    // bridge has to be up first.
    if !bridge_listening() {
        println!("  the bridge was not running — starting it");
        supervise_main(Some(session.clone()))?;
        if !wait_for_bridge(std::time::Duration::from_secs(20)) {
            println!();
            println!("  It did not come up within 20s. `mars pair --check` says what is missing.");
            return Ok(());
        }
    }
    let base = running_tunnel_url().ok_or_else(|| anyhow!(
        "the bridge is up but its tunnel URL could not be read from ngrok \
         (http://127.0.0.1:4040) — see ~/.mars/serve-agent.log"
    ))?;
    let url = pair_link(&session, &base, "rover")?;

    // Printed BEFORE the open, and printed whether or not it succeeds. A browser that does not
    // come up must still leave the person holding the link, rather than a command that appeared
    // to do nothing.
    println!("  \x1b[38;5;208mRover\x1b[0m — opening  ·  session \x1b[1m{session}\x1b[0m");
    println!("  {url}");


    match open_in_browser(&url) {
        Ok(()) => {
            println!();
            println!("  Paired on open — no QR, no paste. This link is single-use.");
            println!("  On your phone instead:  mars pair");
        }
        Err(e) => {
            println!();
            println!("  Could not open a browser here ({e}).");
            println!("  Paste the link above into one on this machine — it is already paired.");
        }
    }
    Ok(())
}

/// Is anything answering on the bridge's port?
///
/// A TCP connect, not an HTTP request: the question is whether the port is being served at all,
/// and a bridge that is up but mid-start would answer the socket before it answers a route.
fn bridge_listening() -> bool {
    use std::net::{SocketAddr, TcpStream};
    let addr: SocketAddr = ([127, 0, 0, 1], default_port()).into();
    TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(400)).is_ok()
}

/// Poll until the bridge answers, or give up. Polled rather than slept, so a fast start opens
/// immediately and a slow one is still waited for.
fn wait_for_bridge(limit: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + limit;
    while std::time::Instant::now() < deadline {
        if bridge_listening() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    false
}

/// Hand a URL to the desktop's own browser./// Hand a URL to the desktop's own browser. Not a shell — the URL carries a single-use token, and
/// a token spliced into a shell line is a token in somebody's history.
fn open_in_browser(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    let mut cmd = std::process::Command::new("open");
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut cmd = std::process::Command::new("xdg-open");
    #[cfg(windows)]
    let mut cmd = {
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    let status = cmd
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("exit {}", status.code().unwrap_or(-1))
    }
}

pub fn qr_main(session_arg: Option<String>) -> Result<()> {
    let session = resolve_session(session_arg)?;
    let url = lan_pair_url(&session)?;

    print_wordmark();
    println!();
    // LED BY WHERE YOU ARE, NOT BY THE MECHANISM.
    //
    // This screen used to open with a QR, which answers "how" before anyone has decided "which".
    // The QR only serves one of the three cases — a phone on this wifi — and it is the case a
    // person sitting at this machine is least likely to be in. Three routes, each named by the
    // situation it belongs to, and the mechanism second.
    println!("  \x1b[38;5;208mRover\x1b[0m — your sessions, on a screen  ·  session \x1b[1m{session}\x1b[0m");
    println!();
    println!("  \x1b[1mOn this machine\x1b[0m      mars pair --open        opens a browser, already paired");
    println!("  \x1b[1mOn your phone\x1b[0m        scan the code below     same wifi as this machine");
    println!("  \x1b[1mAnother computer\x1b[0m     paste the link below    any network, via the tunnel");
    println!();
    print_qr(&url);
    println!();
    println!("  {url}");
    println!("  single-use  ·  `mars serve` accepts the connection");
    // THE FOURTH DOOR, named on the screen that hands out the link. `mars attach` reads the same
    // fragment a browser does, so somebody who already has this URL in their scrollback has no
    // reason to reach for a browser. The quoting warning rides along because an unquoted paste
    // fails silently — the shell truncates at the first `&` and the link arrives credential-less.
    println!("  in a terminal, the same link attaches:  \x1b[1mmars attach '<link>'\x1b[0m  (quoted — the shell eats `#` and `&`)");
    Ok(())
}

/// Render a compact QR with half-block characters — one glyph encodes TWO vertical
/// modules (▀ ▄ █ space), so it's ~half the height of a full-cell QR while still
/// scanning on any terminal theme (black ink on a white quiet zone).
fn print_qr(text: &str) {
    let qr = match qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium) {
        Ok(q) => q,
        Err(_) => {
            println!("  (url too long to QR — open the link below)");
            return;
        }
    };
    let n = qr.size();
    let q = 2i32; // quiet zone; get_module() returns false (white) out of bounds
    let dark = |x: i32, y: i32| qr.get_module(x, y);
    let mut s = String::new();
    let mut y = -q;
    while y < n + q {
        s.push_str("  \x1b[30;47m"); // indent + black-on-white
        for x in -q..n + q {
            let top = dark(x, y);
            let bot = dark(x, y + 1);
            s.push(match (top, bot) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        s.push_str("\x1b[0m\n");
        y += 2;
    }
    print!("{s}");
}

/// The ROVER wordmark with a small left-justified `mars` above it.
fn print_wordmark() {
    println!("  \x1b[38;5;244mmars\x1b[0m");
    for line in [
        "██████╗  ██████╗ ██╗   ██╗███████╗██████╗",
        "██╔══██╗██╔═══██╗██║   ██║██╔════╝██╔══██╗",
        "██████╔╝██║   ██║██║   ██║█████╗  ██████╔╝",
        "██╔══██╗██║   ██║╚██╗ ██╔╝██╔══╝  ██╔══██╗",
        "██║  ██║╚██████╔╝ ╚████╔╝ ███████╗██║  ██║",
        "╚═╝  ╚═╝ ╚═════╝   ╚═══╝  ╚══════╝╚═╝  ╚═╝",
    ] {
        println!("  \x1b[38;5;208m{line}\x1b[0m");
    }
}

// ── mars serve ───────────────────────────────────────────────────────────────

/// Where the bridge records which daemon instance it is serving.
///
/// A session name is a label the engineer changes; an instance id is not. Persisting it is what
/// lets a bridge started as `mars serve <name>` come back up after that name has moved — the case
/// a launchd-supervised bridge hits on its very next restart.
fn instance_note() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".mars/serve.instance"))
}

fn remember_instance(id: &str) {
    if let Some(p) = instance_note() {
        if let Some(d) = p.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(p, id);
    }
}

fn remembered_instance() -> Option<String> {
    let s = std::fs::read_to_string(instance_note()?).ok()?;
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// `mars pair --supervise` — hand the bridge to launchd so it stops being a child of the session
/// it serves.
///
/// This is not merely crash recovery, which is how it was first scoped. A bridge started by hand
/// from a terminal is a child of THAT terminal — and if that terminal is a pane in the session
/// being bridged, rebooting the session kills its own bridge and the tunnel with it. Observed
/// exactly once and immediately: the phone lost the machine at the moment the reboot succeeded.
///
/// Under launchd the bridge's parent is launchd, so it is genuinely outside the blast radius
/// rather than merely believed to be.
pub fn supervise_main(session_arg: Option<String>) -> Result<()> {
    let session = resolve_session(session_arg)?;
    let home = crate::sys::paths::home_dir().ok_or_else(|| anyhow!("no home directory"))?;
    let exe = std::env::current_exe()?;
    let label = "com.mars.rover-bridge";
    let plist = home.join("Library/LaunchAgents").join(format!("{label}.plist"));
    // The flag KeepAlive watches. Absent, launchd never starts the job at all — which is why
    // supervision has been silently off on this machine for as long as the plist has existed.
    let flag = home.join(".mars/serve.enabled");
    let log = home.join(".mars/serve-agent.log");

    if let Some(d) = plist.parent() {
        std::fs::create_dir_all(d)?;
    }
    if let Some(d) = flag.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&flag, "1\n")?;

    // PATH is CAPTURED, not guessed.
    //
    // launchd hands its jobs a bare PATH, and everything downstream inherits it: the bridge, the
    // `mars reboot` it spawns, the daemon that spawns, and every shell in every pane. A hardcoded
    // list is a guess about where the engineer's tools live, and it was wrong immediately — the
    // first supervised reboot produced a session where `claude` could not be run at all, because
    // it lives in ~/.local/bin and the list did not say so.
    //
    // This command is being run from the engineer's own shell, so the correct PATH is right here
    // in the environment. Take it, and union the essentials in case it is unusually bare — a
    // supervised job that cannot find `ngrok` fails in a way nobody would connect to this.
    let mut dirs: Vec<String> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    for must in [
        "/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin",
        &home.join(".cargo/bin").display().to_string(),
        &home.join(".local/bin").display().to_string(),
    ] {
        if !dirs.iter().any(|d| d == must) {
            dirs.push(must.to_string());
        }
    }
    let path = dirs.join(":");

    // `pair`, and the CURRENT session name. The plist shipped before this hardcoded `serve 0`,
    // and session 0 had been renamed days earlier — so the one time launchd did start the bridge
    // it bound to a name that no longer existed.
    std::fs::write(&plist, format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>pair</string>
        <string>{session}</string>
    </array>
    <!-- launchd agents get a bare PATH; ngrok is in Homebrew and mars in cargo's bin. -->
    <key>EnvironmentVariables</key>
    <dict><key>PATH</key><string>{path}</string></dict>
    <!-- Alive only while the flag exists, so `mars killall` deleting it is a real off switch. -->
    <key>KeepAlive</key><dict><key>PathState</key><dict><key>{flag}</key><true/></dict></dict>
    <key>RunAtLoad</key><true/>
    <key>ThrottleInterval</key><integer>5</integer>
    <key>ProcessType</key><string>Background</string>
    <key>StandardOutPath</key><string>{log}</string>
    <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        exe = exe.display(),
        path = path,
        flag = flag.display(),
        log = log.display(),
    ))?;

    let domain = format!("gui/{}", unsafe { libc::getuid() });
    // Boot it out first so a changed plist is actually re-read; failure here is normal when it
    // was not loaded, so it is ignored rather than reported as a problem.
    let _ = Command::new("launchctl").args(["bootout", &format!("{domain}/{label}")]).output();
    let out = Command::new("launchctl")
        .args(["bootstrap", &domain, &plist.display().to_string()])
        .output()?;
    if !out.status.success() {
        return Err(anyhow!(
            "launchctl bootstrap failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    // Report what launchd actually did rather than assuming the bootstrap took — the previous
    // plist was "installed" and not running for days without anything saying so.
    let state = Command::new("launchctl")
        .args(["print", &format!("{domain}/{label}")])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    let running = state.lines().any(|l| l.trim().starts_with("state = running"));
    println!("  supervised   {}", plist.display());
    println!("  session      {session}");
    println!("  state        {}", if running { "running" } else { "loaded (starting)" });
    println!("  logs         {}", log.display());
    println!("\nthe bridge is launchd's child now, so a session reboot cannot take it down.");
    Ok(())
}

/// Is Rover's agent up, and what did it cost to get there?
///
/// A cold turn is ~4.3s of process start, auth and first token before a word can be said. Paying
/// that AFTER somebody has asked is the whole of "why is this thing slow"; paying it when the
/// phone connects spends it while they are still reading the briefing.
///
/// The mark is gated on this, so the control appears when it can actually be used rather than
/// sitting there looking broken for the first four seconds.
/// 0 warming · 1 ready · 2 unavailable.
///
/// Three states rather than a boolean, because "not ready yet" and "this will never work" are
/// opposite facts that a boolean renders identically — and the phone has to draw them differently:
/// one is worth waiting through, the other is worth explaining.
static ROVER_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
static ROVER_RAMP_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static ROVER_DETAIL: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());

pub fn rover_status() -> (&'static str, u64, String) {
    let st = match ROVER_STATE.load(std::sync::atomic::Ordering::Relaxed) {
        1 => "ready",
        2 => "unavailable",
        _ => "warming",
    };
    (
        st,
        ROVER_RAMP_MS.load(std::sync::atomic::Ordering::Relaxed),
        ROVER_DETAIL.lock().map(|d| d.clone()).unwrap_or_default(),
    )
}

/// Warm the agent in the background. Idempotent: a second phone connecting must not start a
/// second warm-up, which would spend a whole turn to learn something already known.
fn warm_rover(session: String) {
    // PER SESSION, not per process.
    //
    // This was one machine-wide flag, and its comment justified it correctly for the case it was
    // written for: a second PHONE joining the same session must not warm twice. But the thing
    // being warmed is per session — `chat.json` lives in the session's own directory — so on a
    // host running several sessions the first one to connect warmed and created its thread, and
    // every session after it hit the flag, never warmed, and had no thread at all.
    //
    // The user-visible shape of that: the second session shows "Rover ready" (the state is
    // process-global too), and then pays the entire cold cost inline on the first question, with
    // nothing covering it. Which is the "it needs a few seconds at the start" complaint — not once
    // per app launch, but once per session, unhidden, after the UI has claimed to be ready.
    static WARMING: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
        std::sync::Mutex::new(None);
    {
        let Ok(mut set) = WARMING.lock() else { return };
        let set = set.get_or_insert_with(std::collections::HashSet::new);
        if !set.insert(session.clone()) {
            return; // this session is already warming or warm
        }
    }
    let warming_key = session.clone();
    thread::spawn(move || {
        let (ms, err) = crate::manager::rover_warm(&session, crate::worklog::now_secs());
        ROVER_RAMP_MS.store(ms, std::sync::atomic::Ordering::Relaxed);
        match err {
            None => {
                ROVER_STATE.store(1, std::sync::atomic::Ordering::Relaxed);
                crate::session::debug_log(&format!("[rover] agent warm in {ms}ms"));
            }
            Some(why) => {
                // Keep it short — this is drawn on a phone, under a heading that already says
                // which subsystem it is about.
                let why = why.lines().next().unwrap_or("agent did not start").chars().take(120).collect::<String>();
                if let Ok(mut d) = ROVER_DETAIL.lock() {
                    *d = why.clone();
                }
                ROVER_STATE.store(2, std::sync::atomic::Ordering::Relaxed);
                crate::session::debug_log(&format!("[rover] warm-up failed after {ms}ms: {why}"));
                // Let the next connection try again. A failure here is often transient — a laptop
                // that woke with no network yet — and latching it until the bridge restarts turns
                // a bad minute into a dead feature for the rest of the session.
                if let Ok(mut set) = WARMING.lock() {
                    if let Some(set) = set.as_mut() {
                        set.remove(&warming_key);
                    }
                }
            }
        }
    });
}

pub fn serve_main(session_arg: Option<String>) -> Result<()> {
    let session = resolve_session(session_arg)?;
    let mut socket = session::socket_path(&session)?;
    let mut session = session;
    if !socket.exists() {
        // The name we were given no longer resolves — almost always because the session was
        // renamed. An instance id is minted once per daemon and survives renames, so follow the
        // one this bridge served last. Without this, a supervised `mars serve <name>` dies on
        // every restart after a rename and the phone is simply cut off — exactly what the
        // per-connection re-resolve below already exists to prevent, missing at startup.
        match remembered_instance().and_then(|id| session::socket_for_instance(&id)) {
            Some((name, p)) => {
                eprintln!("session '{session}' is gone — following the rename to '{name}'");
                session = name;
                socket = p;
            }
            None => {
                return Err(anyhow!("session '{session}' has no socket — is it running? (`mars ls`)"));
            }
        }
    }
    // Bind to the session's DIRECTORY, not its name and not the daemon's instance id.
    //
    // Not the name: `mars rename` moves the socket, so a path captured here goes stale and the
    // bridge keeps accepting phones while forwarding nothing.
    //
    // Not the instance id either, which is what this used to do. An instance id is `pid-nanos`,
    // minted afresh on every daemon start — immutable, but not DURABLE. It names a process, and
    // the process is exactly what `mars reboot` replaces. A bridge holding one across a restart
    // found nothing and refused every connection from then on: a locked-out phone, delivered by
    // the feature meant to save a trip to the keyboard.
    //
    // A session outlives its daemons and its own name; the directory under ~/.mars/sessions is
    // what carries that. Resolve through it per connection and both a rename and a reboot are
    // followed with no restart here at all.
    let instance_id = session::identify(&socket).map(|(_, id, _)| id).unwrap_or_default();
    let session_dir = crate::manager::existing_session_dir_pub(&session)
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_default();
    if session_dir.is_empty() && instance_id.is_empty() {
        return Err(anyhow!(
            "session '{session}' has neither a manager directory nor an instance id — is it an \
             older `mars`? Restart the session so the bridge can follow it."
        ));
    }
    // Record who this bridge serves and where to find it. The session directory is what a daemon
    // checks on boot to decide whether it should be starting one; the pid is what `mars reboot`
    // signals so the replacement runs the binary on disk rather than this one.
    if !session_dir.is_empty() {
        crate::session::remember_paired_session(&session_dir);
    }
    if let Some(h) = crate::sys::paths::home_dir() {
        let _ = std::fs::create_dir_all(h.join(".mars"));
        let _ = std::fs::write(h.join(".mars/serve.pid"), std::process::id().to_string());
    }
    // Remember it: this is what a later start reads when the name it was given has moved.
    remember_instance(&instance_id);
    let port = default_port();
    // Bind localhost; cloudflared fronts it with a public https/wss URL, so the phone
    // (loading the app from Lovable over https) can reach this bridge over wss without a
    // LAN/cert dance.
    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(l) => l,
        // A bridge is already running (commonly the launchd agent owns :8787). Don't stand up a
        // second one — just reprint the pairing QR for the live tunnel, so `mars serve` doubles
        // as "show me the QR" (to add another phone).
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => return reprint_running(&session, false),
        Err(e) => return Err(e.into()),
    };

    let (_tunnel, base) = start_tunnel(port)?;
    let app_url = pair_link(&session, &base, "rover")?;
    let endpoint = format!("{}/ws", base.replacen("https://", "wss://", 1));

    // Prove the path before showing a QR. A QR that cannot work is worse than an error: it moves
    // the failure onto a device with no diagnostics.
    //
    // Retried, because ngrok reports its URL before the edge is routing to it — checking once, at
    // the earliest possible moment, tests the tunnel at exactly the instant it is least likely to
    // answer.
    let https = endpoint.replacen("wss://", "https://", 1);
    let mut checked = verify_public(&https);
    for attempt in 1..=4 {
        if !checked.iter().any(|c| c.failed()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(750 * attempt));
        checked = verify_public(&https);
    }
    // A NOTE, never a gate.
    //
    // This probe leaves the host, crosses the internet and comes back to the same host. That is a
    // hairpin, and it fails for reasons that have nothing to do with whether a phone can connect:
    // a VPN, split-horizon DNS, a captive network. On the machine this was written on it fails
    // every single time — four `utun` interfaces — while phones connect immediately.
    //
    // So it cannot tell "the tunnel is broken" from "the tunnel is not visible from here", and
    // something that cannot tell those apart has no business refusing to print a QR. It once did
    // exactly that, and under a supervisor it exited rather than serve: a working tunnel produced
    // a bridge restarting every five seconds, each restart killing the ngrok it had just started.
    //
    // The phone is the only instrument that can actually answer this. Show it the QR and let it.
    if checked.iter().any(|c| c.failed()) {
        print_checks("bridge", &checked);
        println!(
            "  \x1b[38;5;244mcould not reach the tunnel FROM THIS MACHINE — normal behind a VPN or \n\
             \x20 split-horizon DNS, and not evidence that your phone cannot. Scan and see.\x1b[0m"
        );
    } else {
        print_checks("bridge", &checked);
    }
    println!();

    print_wordmark();
    println!();
    print_qr(&app_url);
    println!();
    println!("  \x1b[38;5;208mRover\x1b[0m — scan to pair · session \x1b[1m{session}\x1b[0m");
    // Report what was OBSERVED, not what was configured.
    //
    // This used to read the raw MARS_NGROK_DOMAIN env var and call anything else "ephemeral". Two
    // ways to be wrong, and it managed both: it ignored `ngrok_domain()`, which also reads config,
    // and — the real error — it inferred stability from whether Mars had been TOLD a domain. ngrok
    // now gives free accounts a permanent one automatically, so a URL can be perfectly stable
    // while Mars knows nothing about it, and the engineer is told to fix something that is not
    // broken.
    //
    // Mars cannot know another product's account policy. It can know whether this is the same URL
    // it saw last time, which is the thing the reader actually cares about.
    let url_note = crate::sys::paths::home_dir().map(|h| h.join(".mars/serve.url"));
    let previous = url_note.as_ref().and_then(|p| std::fs::read_to_string(p).ok());
    let same_as_before = previous.as_deref().map(str::trim) == Some(endpoint.as_str());
    if let Some(p) = &url_note {
        let _ = std::fs::write(p, &endpoint);
    }
    if same_as_before {
        println!("  \x1b[38;5;35mstable endpoint\x1b[0m — same URL as last start; paired phones reconnect with no re-scan");
    } else if ngrok_domain().is_some() {
        println!("  \x1b[38;5;35mpinned domain\x1b[0m — this URL is reserved and survives restarts");
    } else if previous.is_some() {
        println!("  \x1b[38;5;208mthe URL changed since last start\x1b[0m — paired phones need to re-scan");
    } else {
        println!("  \x1b[38;5;244mfirst pairing\x1b[0m — the next start will report whether this URL held");
    }
    println!("  bridge live · Ctrl-C to stop");
    println!("  {app_url}");

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        // Re-resolve per connection: follow a rename, and REFUSE loudly when the instance is
        // gone rather than serving an empty board. Silent success is the failure mode that makes
        // a misdirected bridge indistinguishable from a broken client.
        let fallback = socket.clone();
        let id = instance_id.clone();
        let dir_id = session_dir.clone();
        thread::spawn(move || {
            // Directory first — it survives both a rename and a reboot. The instance id is kept
            // only as a fallback for a session with no manager directory yet.
            let found = crate::session::socket_for_session_dir(&dir_id)
                .or_else(|| crate::session::socket_for_instance(&id));
            let sock = match found {
                Some((name, p)) => {
                    if p != fallback {
                        crate::session::debug_log(&format!(
                            "[rover] session followed a rename → '{name}' ({})",
                            p.display()
                        ));
                    }
                    p
                }
                None => {
                    crate::session::debug_log(
                        "[rover] session instance is gone — refusing the connection",
                    );
                    return;
                }
            };
            if let Err(e) = handle_conn(stream, &sock) {
                crate::session::debug_log(&format!("[rover] conn ended: {e}"));
            }
        });
    }
    Ok(())
}

/// Read the live public tunnel URL from ngrok's local inspection API (127.0.0.1:4040), so we can
/// reprint the pairing QR for an already-running bridge without standing up a second server.
/// The header `serve_static` stamps on its reply, and the only proof a tunnel reaches THIS bridge.
const BRIDGE_HEADER: &str = "X-Mars-Bridge";

/// Does the public URL actually reach this bridge?
///
/// ngrok's local API is not evidence. It reports the agent's own belief, and the failure that cost
/// an evening was an agent that believed it had a tunnel while the edge session was gone: the phone
/// got `ERR_SSL_PROTOCOL_ERROR` while `/api/tunnels` went on listing a healthy https URL, so every
/// local check said "fine" and the one thing that mattered was broken. This asks from OUTSIDE, over
/// the public name, and requires our own header in the answer.
///
/// A failure here is weaker evidence than a success. A reply has been to ngrok's edge and back, so
/// it proves the path; a silence could equally be this laptop's own network. So every caller REPORTS
/// this and none of them refuse to work because of it.
///
/// Never call it from the serving thread: the request comes back to this process, and a bridge
/// waiting on its own answer cannot give one.

/// Run the notification gates over one board and, if something has earned an interrupt, render the
/// frame for it.
///
/// `sent` is this connection's cooldown memory — keyed on `<session>:<pane>` rather than on
/// content, because the failure it exists to prevent wrote different words every single time.
fn notify_for_board(
    board_json: &str,
    sent: &mut std::collections::HashMap<String, u64>,
) -> Option<String> {
    let session = serde_json::from_str::<serde_json::Value>(board_json)
        .ok()?["session"]
        .as_str()
        .unwrap_or("")
        .to_string();
    notify_frame_from_board(
        board_json,
        crate::manager::presence_watched(&session),
        sent,
        crate::worklog::now_secs(),
        &crate::tuning::Tuning::default(),
    )
}

/// The reader, separated from the I/O so a selfcheck can drive it.
///
/// **This split is the point.** The six gates were written, tested, and passed for weeks while
/// nothing called them — and a reader that misspells one field name reproduces exactly that: every
/// gate green, every board silently ineligible. The test that matters is not "do the rules work",
/// it is "does a real board reach the rules".
pub fn notify_frame_from_board(
    board_json: &str,
    watched: bool,
    sent: &mut std::collections::HashMap<String, u64>,
    ts: u64,
    t: &crate::tuning::Tuning,
) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(board_json).ok()?;
    let session = v["session"].as_str().unwrap_or("").to_string();
    let facts: Vec<crate::manager::PaneFacts> = v["rows"]
        .as_array()?
        .iter()
        .map(|r| crate::manager::PaneFacts {
            session: session.clone(),
            pane: r["name"].as_str().unwrap_or("a workspace").to_string(),
            verdict: r["verdict"].as_str().unwrap_or("").to_string(),
            cmd: r["cmd"].as_str().map(str::to_string),
            // Silence, not run time — see `quietSecs` in `mobile_board_json`. Falls back to
            // `ageSecs` only for a board old enough not to carry it, where the two agree for the
            // short-lived commands that case covers.
            stall_secs: r["quietSecs"].as_u64().or_else(|| r["ageSecs"].as_u64()).unwrap_or(0),
            prompt: r["blocked"]["prompt"].as_str().map(str::to_string),
        })
        .collect();
    let n = crate::manager::push_candidate(&facts, watched, sent, ts, t)?;
    sent.insert(n.key.clone(), ts);
    let mut out = serde_json::json!({
        "t": "notify", "key": n.key, "title": n.title, "body": n.body,
    });
    if let Some(s) = n.stakes {
        out["stakes"] = serde_json::json!(s);
    }
    // The pane the tap should land on. Without it the notification opens the app and leaves you to
    // find the thing yourself, which is most of the way back to not having been told.
    if let Some(p) = v["rows"].as_array().and_then(|rows| {
        rows.iter().find(|r| r["name"].as_str() == Some(n.pane.as_str()))
            .and_then(|r| r["paneId"].as_str())
    }) {
        out["paneId"] = serde_json::json!(p);
    }
    Some(out.to_string())
}

/// A failed tunnel probe, and what to actually do about it.
///
/// The remedy used to be one fixed paragraph, on the reasoning that it is the same problem
/// wherever it is noticed. It is not. "Restart the tunnel" is right for a dead agent and actively
/// wrong for a filtered one — restarting mints a fresh random `.ngrok-free.dev` name that gets
/// blocked exactly like the last one, so the advice produces a loop that reads as ngrok "going
/// stale" when nothing has gone stale at all. Cost us an evening. The fault carries its own
/// remedy now, because a diagnosis whose fix does not follow from it is not a diagnosis.
struct TunnelFault {
    why: String,
    remedy: Vec<String>,
}

fn restart_remedy() -> Vec<String> {
    vec![
        "   A phone will not reach this machine until the tunnel is replaced:".into(),
        "   stop the bridge and its ngrok agent, then run `mars pair` again.".into(),
        "   (Away from the machine, this is what the phone reports as \"the host did not answer\".)".into(),
    ]
}

/// The tunnel's hostname, for probes that need to address it directly.
fn host_of(base: &str) -> Option<String> {
    let rest = base.split("://").nth(1).unwrap_or(base);
    let host = rest.split('/').next()?.split('@').next_back()?;
    (!host.is_empty()).then(|| host.to_string())
}

/// Is something between here and ngrok's edge answering FOR this host?
///
/// The signature is a middlebox doing SNI inspection: it lets the TCP connection through, reads
/// the hostname out of the TLS ClientHello, and replies in plaintext instead of completing the
/// handshake — which surfaces as a corrupt-record error rather than anything that says "blocked".
/// Tunnel domains are a standard category for these filters, so the case is common and looks
/// nothing like its cause.
///
/// Proof rather than inference: ask the same host over plain HTTP and read where it sends us. The
/// edge's own http→https redirect points back at itself; a filter points at its warning page, and
/// that hostname is the name of the thing blocking you. Redirects are NOT followed — the redirect
/// is the evidence, and following it throws the evidence away and reports on the warning page.
fn intercepted_by(host: &str) -> Option<String> {
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout(Duration::from_secs(5))
        .build();
    let resp = match agent.get(&format!("http://{host}/")).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(_, r)) => r,
        Err(_) => return None,
    };
    let loc = resp.header("location")?;
    let target = loc.split("://").nth(1)?.split('/').next()?;
    let target = target.split(':').next()?.trim_end_matches('.');
    if target.is_empty() || target.eq_ignore_ascii_case(host) {
        return None; // plain http→https on the same host is the edge behaving correctly
    }
    // Anything still inside ngrok is ngrok's business — an interstitial, a moved edge — not a
    // third party standing in front of it.
    const NGROK: [&str; 5] = ["ngrok.com", "ngrok.io", "ngrok.app", "ngrok-free.dev", "ngrok-free.app"];
    if NGROK.iter().any(|d| target.ends_with(d)) {
        return None;
    }
    Some(target.to_string())
}

/// One JSON row per brief — the single definition of what the board sends.
///
/// It was written out twice, once for `brief.list` and once to re-send the board after an
/// override. Two copies of a wire shape is two places a field gets added to and one place it gets
/// forgotten, and the field most likely to be forgotten is the newest — which here is the whole
/// decision set.
fn brief_rows() -> Vec<serde_json::Value> {
    crate::briefs::list()
        .into_iter()
        .map(|b| {
            let decisions = crate::briefs::dir()
                .map(|d| crate::briefs::decisions_of(&d.join(&b.id)))
                .unwrap_or_default();
            serde_json::json!({
                "id": b.id, "title": b.title, "state": b.state.label(),
                "priority": b.priority, "branch": b.branch,
                "addresses": b.addresses, "createdTs": b.created_ts,
                // The forks are what approval reads, and the verify list is what pressing assign
                // authorises to run. Both belong on the card rather than one tap in: a command
                // nobody saw is a command nobody approved.
                //
                // `decisions` carries the same design with its ALTERNATIVES intact — you cannot
                // override an option you cannot see. `forks` stays alongside so a client older
                // than this field degrades to the line it already rendered rather than to nothing.
                "forks": b.forks, "verify": b.verify,
                "decisions": decisions.iter().map(|d| serde_json::json!({
                    "id": d.id, "layer": d.layer, "question": d.question,
                    "dependsOn": d.depends_on, "stale": d.stale, "overridden": d.overridden,
                    "options": d.options.iter().map(|o| serde_json::json!({
                        "key": o.key, "text": o.text, "chosen": o.chosen, "why": o.why,
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "repo": b.repo.as_ref().map(|r| r.display().to_string()),
                "report": b.report.as_ref().map(|r| serde_json::json!({
                    "outcome": r.outcome, "pr": r.pr, "met": r.met, "total": r.total,
                })),
            })
        })
        .collect()
}

fn tunnel_answers(base: &str) -> Result<(), TunnelFault> {
    let ours = |r: &ureq::Response| r.header(BRIDGE_HEADER).is_some();
    match ureq::get(base).timeout(Duration::from_secs(6)).call() {
        Ok(r) if ours(&r) => Ok(()),
        // A status code is still an answer, and our own bridge may legitimately return one.
        Err(ureq::Error::Status(_, r)) if ours(&r) => Ok(()),
        // Something is there but did not identify itself. Two very different causes, and naming
        // both is what keeps this from being ignored: a bridge older than this header looks
        // exactly like ngrok's edge answering for a bridge that has gone.
        Ok(_) | Err(ureq::Error::Status(_, _)) => Err(TunnelFault {
            why: concat!(
                "that URL answers, but not as this bridge — if you have just upgraded, the running ",
                "bridge predates this check and only needs restarting; otherwise ngrok's edge is up ",
                "and the agent is no longer attached to it",
            )
            .into(),
            remedy: restart_remedy(),
        }),
        Err(e) => {
            let msg = e.to_string();
            // A TLS failure is not a silent host. The connection was accepted and then the
            // handshake did not complete, which means something answered — so the "it may be
            // asleep" reading is already excluded before we ask who.
            let tls = ["tls", "handshake", "certificate", "corrupt", "InvalidContentType", "version number"]
                .iter()
                .any(|k| msg.to_ascii_lowercase().contains(&k.to_ascii_lowercase()));
            let host = host_of(base);
            if tls {
                if let Some(by) = host.as_deref().and_then(intercepted_by) {
                    return Err(TunnelFault {
                        why: format!(
                            "this tunnel is being blocked on your network — `{by}` answers for it \
                             instead of ngrok"
                        ),
                        remedy: vec![
                            "   The tunnel itself is fine: the agent is connected and the session is up.".into(),
                            "   Something on the path (router or ISP) inspects TLS, sees an ngrok".into(),
                            "   hostname and replies in plaintext, so the handshake never completes.".into(),
                            "".into(),
                            "   RESTARTING WILL NOT HELP — a new tunnel gets a new ngrok name and the".into(),
                            "   same block. What does work:".into(),
                            "     · this machine     mars attach '<link>'   no tunnel involved".into(),
                            "     · same wifi        `mars qr` — a LAN link, which never leaves the network".into(),
                            "     · a phone          turn wifi off; cellular is a different network".into(),
                            "     · everywhere       allow ngrok on the router, or use a custom domain".into(),
                        ],
                    });
                }
                return Err(TunnelFault {
                    why: format!("the tunnel host accepted the connection but TLS did not complete ({msg})"),
                    remedy: vec![
                        "   Something answered and then failed to speak TLS, so the tunnel is".into(),
                        "   reachable and the fault is on the path — a filter, a proxy, or a captive".into(),
                        "   portal. Restarting the tunnel does not address any of those.".into(),
                        "   `mars attach '<link>'` works on this machine regardless.".into(),
                    ],
                });
            }
            Err(TunnelFault { why: format!("the tunnel did not answer ({msg})"), remedy: restart_remedy() })
        }
    }
}

/// What to print when a probe fails.
fn tunnel_warning(f: &TunnelFault) -> String {
    let mut out = vec![String::new(), format!("⚠  {}.", f.why)];
    out.extend(f.remedy.iter().cloned());
    out.push(String::new());
    out.join("\n")
}

fn running_tunnel_url() -> Option<String> {
    let mut stream = TcpStream::connect(("127.0.0.1", 4040)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(3))).ok();
    stream
        .write_all(b"GET /api/tunnels HTTP/1.1\r\nHost: 127.0.0.1:4040\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;
    let key = "\"public_url\":\"";
    let start = body.find(key)? + key.len();
    let rest = &body[start..];
    let end = rest.find('"')?;
    let url = rest[..end].trim_end_matches('/').to_string();
    url.starts_with("https://").then_some(url)
}

/// Print the pairing QR for a bridge that's already live: read its tunnel URL from ngrok and the
/// persisted token, and render the QR. `reset` tailors the message (after a token rotation).
fn reprint_running(session: &str, reset: bool) -> Result<()> {
    let base = running_tunnel_url().ok_or_else(|| anyhow!(
        "a bridge is already running on :{}, but its tunnel URL couldn't be read from \
         ngrok (http://127.0.0.1:4040). See the bridge's log (`~/.mars/serve-agent.log`).",
        default_port()
    ))?;
    // Before the QR, not after: a code printed under a warning still gets scanned.
    if let Err(why) = tunnel_answers(&base) {
        println!("{}", tunnel_warning(&why));
    }
    let app_url = pair_link(session, &base, "rover")?;
    print_wordmark();
    println!();
    print_qr(&app_url);
    println!();
    if reset {
        println!("  \x1b[38;5;208mRover\x1b[0m — QR reset · session \x1b[1m{session}\x1b[0m");
        println!("  connected phones drop within ~1s · scan this to reconnect");
    } else {
        println!("  \x1b[38;5;208mRover\x1b[0m — your sessions, on a screen  ·  session \x1b[1m{session}\x1b[0m");
        println!();
        print_routes();
    }
    println!("  {app_url}");
    Ok(())
}

/// `mars serve --reset`: rotate the pairing token and disconnect every connected phone. The live
/// bridge re-reads the token each second and drops sockets whose token no longer matches;
/// reconnecting phones present the stale token and are refused, so a fresh scan is required. The
/// tunnel and its URL are untouched.
pub fn reset_main(session_arg: Option<String>) -> Result<()> {
    let session = resolve_session(session_arg)?;
    write_token(&mint_token()?);
    if running_tunnel_url().is_some() {
        reprint_running(&session, true)
    } else {
        println!("Pairing token rotated. No bridge is running — start one with `mars serve`.");
        Ok(())
    }
}

/// Start an ngrok tunnel to the local bridge; returns the child (kept alive for the process
/// lifetime) and the public https base URL. We use ngrok, not cloudflared quick tunnels: the
/// latter currently 502 the WebSocket UPGRADE (HTTP proxies fine, the socket fails), and the
/// whole Rover bridge is one long-lived WebSocket. Requires `ngrok` on PATH with an authtoken.
///
/// If `MARS_NGROK_DOMAIN` is set (your free ngrok static domain, e.g.
/// `curly-owl-1234.ngrok-free.app`), we pin the tunnel to it with `--url`, so the public URL —
/// and therefore the paired phone's stored endpoint and the QR — stay identical across every
/// `mars serve` restart. Without it, ngrok mints a fresh random URL each run, and the phone has
/// to re-scan every time the daemon bounces.
fn start_tunnel(local_port: u16) -> Result<(Option<std::process::Child>, String)> {
    // ADOPT before spawning. A free ngrok account allows ONE agent session, and a bridge that
    // died (or exec-replaced itself) can leave its ngrok child running with the tunnel intact —
    // in which case a fresh spawn cannot get a slot and times out, which serially killed every
    // bridge restart one night. If a live agent already fronts a tunnel, use it.
    if let Some(url) = running_tunnel_url() {
        crate::session::debug_log(&format!("[rover] adopting the running ngrok tunnel {url}"));
        return Ok((None, url));
    }
    let domain = std::env::var("MARS_NGROK_DOMAIN").ok().filter(|d| !d.trim().is_empty());
    let mut args: Vec<String> = vec!["http".into(), local_port.to_string()];
    if let Some(d) = &domain {
        // Accept a bare host or a full https URL; ngrok's --url wants the scheme.
        let url = if d.contains("://") { d.clone() } else { format!("https://{}", d.trim_end_matches('/')) };
        args.push("--url".into());
        args.push(url);
    }
    args.extend(["--log".into(), "stdout".into(), "--log-format".into(), "logfmt".into()]);
    let mut child = Command::new("ngrok")
        .args(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| {
            anyhow!("ngrok not found — install it (`brew install ngrok`) and add your authtoken (`ngrok config add-authtoken …`), then re-run `mars serve`")
        })?;

    // ngrok logs `... url=https://<name>.ngrok-free.dev ...` to stdout when the tunnel comes up;
    // scan for it in a drain thread (so a full pipe never blocks ngrok) and hand it back.
    let stdout = child.stdout.take().ok_or_else(|| anyhow!("no tunnel output"))?;
    let (tx, rx) = mpsc::channel::<String>();
    // A pinned custom domain need not contain "ngrok" (paid custom domains don't), so accept any
    // URL that matches the configured host as well.
    let expect_host = domain.as_ref().map(|d| d.split("://").last().unwrap_or(d).trim_end_matches('/').to_string());
    // KEEP ngrok's own account of itself. This thread already reads every line and threw all but
    // the URL away — so the one component that knows why a tunnel died wrote its explanation into
    // a pipe nobody read. A tunnel went down today, its local API kept reporting a healthy
    // tunnel, and the outage was misdiagnosed twice as a phone problem because there was no record
    // anywhere. Appended, capped, and never fatal: a log that breaks the tunnel is worse than none.
    let log = crate::sys::paths::home_dir().map(|h| h.join(".mars").join("tunnel.log"));
    if let Some(p) = &log {
        // Start each run clean rather than growing without bound — the interesting window is
        // always "since this tunnel came up".
        if let Some(d) = p.parent() { let _ = std::fs::create_dir_all(d); }
        let _ = std::fs::write(p, format!("--- tunnel started {} ---\n", crate::worklog::now_secs()));
    }
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut sent = false;
        let mut sink = log.and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
        for line in reader.lines().map_while(|l| l.ok()) {
            if let Some(f) = sink.as_mut() {
                let _ = writeln!(f, "{line}");
            }
            if !sent {
                if let Some(idx) = line.find("url=https://") {
                    let url: String = line[idx + 4..].chars().take_while(|c| !c.is_whitespace()).collect();
                    let ok = url.contains("ngrok") || expect_host.as_ref().is_some_and(|h| url.contains(h.as_str()));
                    if ok {
                        let _ = tx.send(url.trim_end_matches('/').to_string());
                        sent = true;
                    }
                }
            }
        }
    });

    match rx.recv_timeout(Duration::from_secs(25)) {
        Ok(url) => Ok((Some(child), url)),
        Err(_) => {
            let _ = child.kill();
            Err(anyhow!(
                "timed out waiting for an ngrok tunnel URL — is your authtoken set? \
                 (`ngrok config add-authtoken …`) An orphaned `ngrok` from a dead bridge can also \
                 hold the account's one agent slot while answering on no API port — `pgrep ngrok` \
                 and kill it, then retry."
            ))
        }
    }
}

/// Classify each connection by peeking (no bytes consumed): a WebSocket upgrade goes to
/// the bridge; anything else gets the static app shell.
fn handle_conn(stream: TcpStream, socket: &std::path::Path) -> Result<()> {
    let mut peek = [0u8; 1024];
    let n = stream.peek(&mut peek).unwrap_or(0);
    let head = String::from_utf8_lossy(&peek[..n]).to_ascii_lowercase();
    if head.contains("sec-websocket-key") {
        bridge_ws(stream, socket)
    } else {
        serve_static(stream)
    }
}

/// Serve the built Rover bundle from this host.
///
/// **This used to return `index.html` for every path**, which loads a page whose every script and
/// stylesheet also arrives as that same HTML — so the app never started and the only visible
/// result was the bridge's own placeholder. A single-file "static server" is not a smaller static
/// server; it cannot serve an app at all.
///
/// Serving it here is the point of the LAN route: the hosted app is https and cannot dial
/// `ws://192.168.x.x`, so the copy that CAN talk to this bridge is the one this bridge hands out.
///
/// Unknown paths fall back to `index.html` because the router is client-side — `/desk` and
/// `/connect` are routes in the bundle, not files on disk.
fn serve_static(mut stream: TcpStream) -> Result<()> {
    let path = read_request_path(&stream).unwrap_or_else(|| "/".into());
    if let Some(dir) = std::env::var("MARS_WEB_DIR").ok().map(std::path::PathBuf::from) {
        // Take only the last segment chain and refuse `..`: this serves a directory to whoever
        // reaches the port, and a path that can climb out of it serves the whole disk.
        let rel = path.trim_start_matches('/').split('?').next().unwrap_or("");
        let safe = !rel.split('/').any(|seg| seg == ".." || seg == ".");
        let file = dir.join(rel);
        if safe && !rel.is_empty() && file.is_file() {
            if let Ok(bytes) = std::fs::read(&file) {
                let ctype = content_type(rel);
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\n{BRIDGE_HEADER}: {}\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n",
                    env!("CARGO_PKG_VERSION"), bytes.len(),
                );
                stream.write_all(head.as_bytes())?;
                stream.write_all(&bytes)?;
                stream.flush()?;
                return Ok(());
            }
        }
    }
    let body = std::env::var("MARS_WEB_DIR")
        .ok()
        .and_then(|dir| std::fs::read_to_string(std::path::Path::new(&dir).join("index.html")).ok())
        .unwrap_or_else(|| {
            "<!doctype html><meta charset=utf-8><title>Rover bridge</title>\
             <body style=\"font-family:monospace;background:#0a0a0a;color:#f5f2f0;padding:2rem\">\
             <h1 style=\"color:#ea5a3a\">Rover bridge</h1>\
             <p>The WebSocket bridge is live. Point the Rover PWA here, or set \
             <code>MARS_WEB_DIR</code> to serve the built app.</p></body>"
                .to_string()
        });
    // A header only this bridge sends. It is what makes a tunnel probe conclusive: ngrok's own
    // "tunnel not found" page is a perfectly good HTTP response, and without something of ours in
    // the answer, an edge that has lost its agent reads exactly like a working link.
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n{BRIDGE_HEADER}: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        env!("CARGO_PKG_VERSION"),
        body.len(),
        body
    );
    stream.write_all(resp.as_bytes())?;
    stream.flush()?;
    Ok(())
}

/// The path off the request line, without consuming the stream a websocket upgrade still needs.
fn read_request_path(stream: &TcpStream) -> Option<String> {
    let mut peek = [0u8; 2048];
    let n = stream.peek(&mut peek).ok()?;
    let head = String::from_utf8_lossy(&peek[..n]);
    let line = head.lines().next()?;
    let mut parts = line.split_whitespace();
    let _method = parts.next()?;
    Some(parts.next()?.to_string())
}

/// Enough of a MIME table to start an app. A `.js` served as `text/html` is refused by the module
/// loader, which is the same silent failure as serving no file at all.
fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("") {
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "html" => "text/html; charset=utf-8",
        "json" | "webmanifest" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "map" => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// Bridge one WebSocket to the daemon socket: daemon `Output` → WS `{t:"output"}`, and
/// inbound intents/keystrokes → the pane. Single-threaded WS loop; a dedicated thread
/// pumps the daemon's output through a channel so the loop never blocks on it.
fn bridge_ws(stream: TcpStream, socket: &std::path::Path) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(40))).ok();
    let mut ws = accept(stream).map_err(|e| anyhow!("ws handshake failed: {e}"))?;

    // ── Phase 1: authenticate, and learn WHICH session this phone wants ─────────────────────
    //
    // One machine has one bridge (one port, one tunnel) but may run many sessions — and a QR for
    // a second session used to route to whichever session the bridge was bound to, so 'replyguy'
    // paired fine and then showed daemon-restart's board. The phone's hello now carries `s`, and
    // the daemon is dialed AFTER auth, per connection, for the session actually asked for. An
    // older phone sends no `s` and gets the bridge's own session, exactly as before.
    let valid = read_token();
    let mut wanted: Option<String> = None;
    let auth_deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if Instant::now() > auth_deadline {
            return Ok(()); // never presented a valid token → refuse
        }
        match ws.read() {
            Ok(Message::Text(txt)) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                    if v.get("t").and_then(|t| t.as_str()) == Some("hello") {
                        wanted = v.get("s").and_then(|s| s.as_str()).map(String::from).filter(|s| !s.is_empty());
                    }
                }
                match auth_result(&txt, valid.as_deref()) {
                    AuthCheck::Ok => break,
                    AuthCheck::Bad => {
                        // A beat before refusing: the token is 128 bits so brute force is a
                        // fantasy, but a guessing loop gets nothing for free either.
                        thread::sleep(Duration::from_millis(250));
                        return Ok(());
                    }
                    AuthCheck::Other => {} // subscribe before auth → keep waiting
                }
            }
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => {}
            Err(WsError::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return Ok(()),
        }
    }

    // ── Phase 2: resolve the daemon for the REQUESTED session, then bridge ──────────────────
    let socket_buf: std::path::PathBuf = match &wanted {
        Some(name) => {
            // Name-direct first; then the DIRECTORY resolver — a session renamed at the desk
            // keeps its directory's birth name, which is exactly the name the phone paired
            // under, so a stale `s` still finds the live session instead of a refusal.
            let direct = session::socket_path(name).ok().filter(|p| session::identify(p).is_some());
            match direct.or_else(|| session::socket_for_session_dir(name).map(|(_, p)| p)) {
                Some(p) => p,
                None => {
                    // Refuse LOUDLY: an empty board wearing the right name is the failure mode
                    // that makes a misrouted bridge indistinguishable from a broken phone.
                    let _ = ws.send(Message::Text(format!(
                        "{{\"t\":\"bye\",\"message\":{}}}",
                        json_str(&format!("session '{name}' is not running on this machine"))
                    )));
                    return Ok(());
                }
            }
        }
        None => socket.to_path_buf(),
    };
    let socket: &std::path::Path = &socket_buf;

    // Dial the daemon as a READ-ONLY subscriber (non-takeover) — a phone glancing in
    // must not kick the person at the keyboard. The daemon pushes structured Board /
    // Briefing frames; it does not become the owning client.
    let daemon = crate::sys::control::connect(socket)?;
    let mut writer = daemon.try_clone()?;
    session::write_frame(&mut writer, &ClientFrame::Subscribe)?;

    // Daemon-reader thread → channel of already-JSON output frames for the WS.
    let (tx, rx) = mpsc::channel::<String>();
    // A clone for async results (the LLM proxy) pushed from the inbound handler.
    let action_tx = tx.clone();
    let reader_stream = daemon.try_clone()?;
    thread::spawn(move || {
        let mut lines = BufReader::new(reader_stream);
        let mut line = String::new();
        // Cooldown memory, one map per connection. Deliberately not persisted: a reconnect is a
        // new session and the phone's own 60s dedup covers the replay. Persisting it would mean a
        // notification you never received still burning its hour.
        let mut notify_sent: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
        loop {
            line.clear();
            match lines.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if let Ok(frame) = serde_json::from_str::<ServerFrame>(line.trim()) {
                match frame {
                    // Structured board JSON is `{"session":…,"rows":[…],"ts":…}`; wrap it
                    // into the client's `{t:"snapshot",…}` event by splicing after the `{`.
                    ServerFrame::Board { json } => {
                        // EVERY BOARD IS A CHANCE TO INTERRUPT SOMEBODY, so the gates run here —
                        // the bridge sees every board, and it is the only process that both knows
                        // the rules and has a socket to the phone.
                        //
                        // The phone is a renderer. It never re-judges: a second copy of the rules
                        // on the client is two copies that will disagree, and the client is the
                        // one that cannot see a stall age or a foreground command.
                        if let Some(n) = notify_for_board(&json, &mut notify_sent) {
                            let _ = tx.send(n);
                        }
                        let wrapped = format!("{{\"t\":\"snapshot\",{}", &json[1..]);
                        if tx.send(wrapped).is_err() {
                            break;
                        }
                    }
                    ServerFrame::Briefing { json } => {
                        let wrapped = format!("{{\"t\":\"briefing\",\"briefing\":{json},\"ts\":0}}");
                        if tx.send(wrapped).is_err() {
                            break;
                        }
                    }
                    // Live pane screen (the phone watching a terminal). `{pane,text}` →
                    // `{t:"screen",pane,text}`.
                    ServerFrame::PaneScreen { json } => {
                        let wrapped = format!("{{\"t\":\"screen\",{}", &json[1..]);
                        if tx.send(wrapped).is_err() {
                            break;
                        }
                    }
                    // Raw ANSI (the peek rung) — present only on an attach, not a Subscribe.
                    ServerFrame::Output { b64 } => {
                        if tx
                            .send(format!("{{\"t\":\"output\",\"paneId\":\"main\",\"b64\":\"{b64}\"}}"))
                            .is_err()
                        {
                            break;
                        }
                    }
                    // Raw PTY bytes of a specifically watched pane (the xterm.js renderer):
                    // seed + deltas, tagged with the pane id so the phone routes them.
                    ServerFrame::PaneHistory { pane, b64, lines, total } => {
                        if tx
                            .send(format!(
                                "{{\"t\":\"history\",\"paneId\":\"{pane}\",\"b64\":\"{b64}\",\"lines\":{lines},\"total\":{total}}}"
                            ))
                            .is_err()
                        {
                            break;
                        }
                    }
                    // A window of a pane's transcript, addressed by line id.
                    ServerFrame::PaneLines { pane, from, first, total, rows } => {
                        let rows: Vec<String> =
                            rows.iter().map(|r| format!("\"{r}\"")).collect();
                        if tx
                            .send(format!(
                                "{{\"t\":\"lines\",\"paneId\":\"{pane}\",\"from\":{from},\"first\":{first},\"total\":{total},\"rows\":[{}]}}",
                                rows.join(",")
                            ))
                            .is_err()
                        {
                            break;
                        }
                    }
                    ServerFrame::PaneOutput { pane, b64 } => {
                        if tx
                            .send(format!("{{\"t\":\"output\",\"paneId\":\"{pane}\",\"b64\":\"{b64}\"}}"))
                            .is_err()
                        {
                            break;
                        }
                    }
                    ServerFrame::Exit { message } => {
                        let _ = tx.send(format!("{{\"t\":\"bye\",\"message\":{}}}", json_str(&message)));
                        break;
                    }
                    _ => {}
                }
            }
        }
    });

    // Auth already happened — phase 1 gated this connection before the daemon was even dialed.
    // What remains of enforcement here is ROTATION: a token reset (`mars serve --reset`) is
    // picked up within a second and drops the socket.
    let valid = read_token();
    let mut last_token_check = Instant::now();

    // A phone is genuinely here, aimed at this resolved session. Spend the ~4.3s agent ramp NOW,
    // while they are still reading the briefing, rather than after they have asked something and
    // are watching a halo breathe.
    if let Some(n) = socket.file_stem().map(|s| s.to_string_lossy().to_string()) {
        warm_rover(n);
    }

    loop {
        // Reset: if the persisted token was rotated out from under us, drop this phone.
        if valid.is_some() && last_token_check.elapsed() >= Duration::from_secs(1) {
            last_token_check = Instant::now();
            if read_token() != valid {
                break;
            }
        }
        // Flush any daemon output waiting in the channel.
        while let Ok(msg) = rx.try_recv() {
            ws.send(Message::Text(msg))?;
        }
        // Read one inbound WS message (times out via the socket read timeout).
        match ws.read() {
            Ok(Message::Text(txt)) => {
                handle_client_msg(&mut writer, &action_tx, socket, &txt);
            }
            Ok(Message::Binary(b)) => {
                handle_client_msg(&mut writer, &action_tx, socket, &String::from_utf8_lossy(&b));
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(WsError::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
            {
                thread::sleep(Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
    Ok(())
}

/// Model calls cost real quota and this socket faces a public tunnel URL — an authenticated
/// caller still gets a ceiling. One fixed window shared by every model-spending intent
/// (chat/ask/summarize): generous for two thumbs, ruinous for a loop. Refusals are said out
/// loud on the same channel the answer would have used, never silently dropped.
pub fn llm_spend_allowed() -> bool {
    // 20, not 12: Mission Control legitimately prefetches one summarize per workspace on board
    // load, so an 8-workspace board plus a few chat turns brushed the old ceiling in its first
    // minute — the limit exists to stop loops, not to tax opening the app.
    const MAX_PER_MIN: u32 = 20;
    static WINDOW: std::sync::Mutex<(u64, u32)> = std::sync::Mutex::new((0, 0));
    let now = crate::worklog::now_secs() / 60;
    let Ok(mut w) = WINDOW.lock() else { return false };
    if now != w.0 {
        *w = (now, 0);
    }
    if w.1 >= MAX_PER_MIN {
        return false;
    }
    w.1 += 1;
    true
}

enum AuthCheck { Ok, Bad, Other }
/// Classify a pre-auth inbound frame: the phone's `{t:"auth",token}` matching the valid token, an
/// auth carrying the WRONG token, or anything else (a hello/subscribe sent before the auth frame).
fn auth_result(txt: &str, valid: Option<&str>) -> AuthCheck {
    let v: serde_json::Value = match serde_json::from_str(txt) {
        Ok(v) => v,
        Err(_) => return AuthCheck::Other,
    };
    if v.get("t").and_then(|t| t.as_str()) != Some("auth") {
        return AuthCheck::Other;
    }
    match (valid, v.get("token").and_then(|t| t.as_str())) {
        (Some(a), Some(b)) if ct_eq(a.as_bytes(), b.as_bytes()) => AuthCheck::Ok,
        _ => AuthCheck::Bad,
    }
}

/// Constant-time equality for the pairing token. `==` on strings returns at the first differing
/// byte, and this check faces a PUBLIC tunnel URL — a remote caller who can time rejections can
/// grow a matching prefix byte by byte. Compare every byte regardless, fold the differences, and
/// let the length difference poison the result the same way.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= (x ^ y) as usize;
    }
    diff == 0
}

/// The LLM proxy: run the agent on the HOST, with the key that's already in this
/// process's environment (or reached via `mars keyd`). The phone sends only the question;
/// the key never leaves the machine. Mirrors `ask_cli` — blocking. Returns
/// `(answer, provenance, usable)`: `usable == false` means the host has no working key
/// (none set, or its limits/credits are expired) and the phone should use its fallback.
fn run_agent_ask(question: String) -> (String, String, bool) {
    run_agent_ask_with(question, false)
}

/// `deep` widens the budget for Rover's chat.
///
/// `AgentConfig::from_env` was built for the per-workspace MAP call — a one-line summary, so
/// `max_tokens: 512` and the cheapest model are exactly right there. Reusing it for a chat is why
/// answers came back snappy and useless: the model is asked to reason about a workspace and then
/// cut off mid-thought, with the fastest model available.
///
/// A chat is a different job. It is asked perhaps twice an hour, not once per pane per tick, so it
/// can afford room to think — and an answer worth reading is the entire point of the surface.
fn run_agent_ask_with(question: String, deep: bool) -> (String, String, bool) {
    let mut cfg = crate::agent::AgentConfig::from_env();
    if deep {
        cfg.max_tokens = 2048;
        // Only when nothing was pinned: an explicit LLM_MODEL is the engineer's choice and must
        // not be quietly overridden by a heuristic about what a chat deserves.
        // The MODEL is deliberately left alone.
        //
        // Upgrading it looked obvious and was wrong: on this machine `claude-sonnet-4-5` answers
        // "your credit balance is too low" while `claude-haiku-4-5` works, so silently reaching
        // for the better model would have turned a shallow chat into a broken one. A model the
        // engineer has not pinned is not ours to choose on their behalf — set MARS_LLM_MODEL to
        // change it, and know that it has to be a model your key can actually reach.
    }
    if !cfg.is_configured() {
        return ("No LLM key on the host.".to_string(), "none".to_string(), false);
    }
    let provenance = agent_provenance(&cfg);
    let (tx, rx) = mpsc::channel();
    crate::agent::ask(cfg, question, crate::palette::registry_context(), String::new(), Vec::new(), tx);
    loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(crate::agent::AgentEvent::Answer { text, .. }) => {
                let usable = !looks_like_key_failure(&text);
                return (text, provenance, usable);
            }
            Ok(_) => continue, // streaming/progress events — wait for the final answer
            Err(_) => return ("The agent didn't respond in time.".to_string(), provenance, false),
        }
    }
}

/// Heuristic: does this answer look like an auth / quota / rate-limit failure (so the phone
/// should fall back)? Provider errors come back as the Answer text; scoped to short,
/// error-shaped strings so a real answer that merely mentions "rate limit" isn't misread.
fn looks_like_key_failure(text: &str) -> bool {
    if text.len() > 240 {
        return false;
    }
    let t = text.to_ascii_lowercase();
    [
        "auth failed", "unauthorized", "invalid api key", "rate limit", "rate-limit", "quota",
        "insufficient", "credit", "exceeded", "billing", " 401", " 429", "payment required",
    ]
    .iter()
    .any(|p| t.contains(p))
}

/// A short "who answered" label for the phone: provider + the model's short name
/// (`groq · llama-3.3-70b-versatile`). The broker path hides the model it picks.
fn agent_provenance(cfg: &crate::agent::AgentConfig) -> String {
    if cfg.provider == "broker" {
        return "broker".to_string();
    }
    let model = cfg.model.rsplit('/').next().unwrap_or(cfg.model.as_str());
    if model.is_empty() { cfg.provider.to_string() } else { format!("{} · {model}", cfg.provider) }
}

/// Handle an inbound Rover message. `ask`/`summarize` are proxied to the host LLM (async,
/// result pushed back over `tx`); raw keystrokes/paste go to the pane.
/// The mirror's own write half, so a keystroke reaches the connection its frames come from.
///
/// One per bridge process: a browser drives one session at a time, and a second desk tab replaces
/// the first rather than typing into it by accident.
static MIRROR_IN: std::sync::Mutex<Option<crate::sys::control::Stream>> = std::sync::Mutex::new(None);
/// Which mirror connection is the live one. Bumped on every `mirror` request.
///
/// A REPLACED MIRROR IS NOT A DEAD SESSION. Re-mirroring drops the previous stream, whose reader
/// thread then falls out of its loop and — before this — announced `mirror.gone`, "the session
/// ended", to a browser that had merely resized. The page dutifully showed the death notice for
/// the connection it had itself just replaced, cleared it when the new frames arrived, and did the
/// whole thing again on the next resize. What looked like a flapping session was the page being
/// told, correctly, about the end of something it no longer cared about.
static MIRROR_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn handle_client_msg(writer: &mut impl Write, tx: &mpsc::Sender<String>, socket: &std::path::Path, txt: &str) {
    let v: serde_json::Value = match serde_json::from_str(txt) {
        Ok(v) => v,
        Err(_) => return,
    };
    // Judgements about what the manager said. Recorded before dispatch, and for `dismiss`/`snooze`
    // that is the WHOLE handling — they change nothing on the host by design. A dismissal is the
    // reader's opinion of a memo, not a request to delete it, and the agent owns that file.
    //
    // `seen` carries no side effect at all. It exists because an action log alone cannot tell a
    // memo that was ignored from one that was never on screen: both are silence. Only the pairing
    // of impressions with actions makes the log mean anything.
    if let Some(kind @ ("dismiss" | "snooze" | "seen" | "answer" | "ask" | "jump" | "summarize")) =
        v.get("t").and_then(|t| t.as_str())
    {
        crate::manager::record_client_event(kind, &v, crate::worklog::now_secs());
    }
    match v.get("t").and_then(|t| t.as_str()) {
        // Opinions, not commands — already recorded above, and there is nothing else to do.
        Some("dismiss") | Some("snooze") | Some("seen") | Some("jump") => {}
        // Reboot the session onto whatever `mars` is on disk now.
        //
        // Spawned DETACHED, as a separate short-lived process, and deliberately not done here.
        // The bridge is the phone's only route back to this machine: if it took part in the
        // restart it would be inside the blast radius, and a reboot that failed halfway would
        // leave no way to see that it had, let alone retry. It stays up, watches the daemon go
        // and come back, and re-resolves on the next connection like it already does for a
        // rename.
        // Upgrade the BRIDGE itself onto the binary on disk, by replacing its own process image.
        //
        // The tempting alternative is a supervisor: give this a launchd job so "restart" is just
        // "exit". Rejected, because it answers "who restarts the doorman" by adding a thing that
        // needs its own correctness — and the plist on this machine is the proof that decays. It
        // hardcodes `serve 0` for a session renamed days ago, and its KeepAlive is gated on a
        // flag file that does not exist, so supervision has been silently off the whole time.
        //
        // exec() has nothing above it to drift: same pid, same argv, new code, nothing external
        // involved. The listener closes across the exec and the new image rebinds within
        // milliseconds; ngrok holds the public tunnel throughout, so the phone sees the same brief
        // blip it already handles for a worker reboot.
        Some("reboot") if v.get("target").and_then(|x| x.as_str()) == Some("bridge") => {
            crate::manager::record_client_event("reboot-bridge", &v, crate::worklog::now_secs());
            match replace_self() {
                // Unreachable on success — exec does not return.
                Ok(()) => {}
                Err(e) => {
                    let _ = tx.send(format!(
                        "{{\"t\":\"toast\",\"text\":{}}}",
                        json_str(&format!("bridge upgrade refused: {e}"))
                    ));
                }
            }
        }
        Some("reboot") => {
            let session = v.get("session").and_then(|x| x.as_str()).unwrap_or_default().to_string();
            crate::manager::record_client_event("reboot", &v, crate::worklog::now_secs());
            let _ = tx.send(format!(
                "{{\"t\":\"rebooting\",\"session\":{}}}", json_str(&session)
            ));
            if let Ok(exe) = std::env::current_exe() {
                let mut cmd = std::process::Command::new(exe);
                cmd.arg("reboot");
                if !session.is_empty() {
                    cmd.arg(&session);
                }
                cmd.stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                crate::sys::daemon::detach(&mut cmd);
                let _ = cmd.spawn();
            }
        }
        // Rover's chat. Routed through the same host-side proxy as `ask` for now, so the
        // surface is usable before the Claude Code agent behind it exists — see
        // design_ideas/rover_agent.md. Recorded either way: the questions people actually ask
        // ARE the requirements document for that agent, and they only exist if logged.
        Some("chat") => {
            let q = v.get("q").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if q.trim().is_empty() {
                return;
            }
            if !llm_spend_allowed() {
                let _ = tx.send(
                    "{\"t\":\"summary\",\"id\":\"chat\",\"summary\":{\"text\":\"Easy, captain — that's the model-call ceiling for this minute. Ask again shortly.\",\"computedBy\":\"rate-limit\"}}".to_string(),
                );
                return;
            }
            crate::manager::record_client_event("chat", &v, crate::worklog::now_secs());
            // Put the TARGET in front of the model. Without this the chat is an oracle that
            // cannot see the machine it is being asked about — "why did it fail?" is unanswerable
            // when nothing says what "it" is. The phone knows, because one thing is outlined on
            // screen, so it says so and this reads that thing's own note and recent output.
            let ctx = match (
                v.get("targetKind").and_then(|x| x.as_str()),
                v.get("targetId").and_then(|x| x.as_str()),
            ) {
                (Some(kind), Some(id)) => {
                    let c = crate::manager::target_context(kind, id);
                    if c.trim().is_empty() { String::new() } else {
                        format!("The captain is asking about this workspace. Everything here is \
                                 true of it right now:\n\n{c}")
                    }
                }
                _ => String::new(),
            };
            let sess = v.get("session").and_then(|x| x.as_str()).unwrap_or_default().to_string();
            let tx2 = tx.clone();
            let _ = tx2.send("{\"t\":\"summary\",\"id\":\"chat\",\"summary\":{\"text\":\"…\",\"streaming\":true}}".to_string());
            thread::spawn(move || {
                // A real Claude Code session on the subscription, not the summary-shaped LLM proxy:
                // it reads the repo, shares the manager's memory, and `--resume` keeps the
                // thread rather than starting fresh on every message.
                // Forward each delta as it lands. A turn costs ~4.3s before its first token and
                // up to 13s when it reads files; held back until complete that is a surface that
                // looks broken, and released as it arrives the same answer starts almost at once.
                let tx3 = tx2.clone();
                let tx4 = tx2.clone();
                let (answer, usable) = crate::manager::rover_chat_stream(
                    &sess, &q, &ctx, crate::worklog::now_secs(),
                    |so_far| {
                        // Cut the stream at the fence. The proposal block is machinery, and
                        // watching raw JSON type itself out below an answer reads as the thing
                        // having broken — the buttons arrive with the final frame instead.
                        let shown = match so_far.find("```do") {
                            Some(i) => so_far[..i].trim_end(),
                            None => so_far,
                        };
                        let _ = tx3.send(format!(
                            "{{\"t\":\"summary\",\"id\":\"chat\",\"summary\":{{\"text\":{},\"streaming\":true}}}}",
                            json_str(shown)
                        ));
                    },
                    |stage| {
                        // A separate id so a stage never overwrites the answer being typed.
                        let _ = tx4.send(format!(
                            "{{\"t\":\"summary\",\"id\":\"chat-stage\",\"summary\":{{\"text\":{},\"streaming\":true}}}}",
                            json_str(stage)
                        ));
                    },
                );
                let provenance = "claude code".to_string();
                let (prose, mut proposals) = crate::manager::split_proposals(&answer);
                // Ground `open` offers before they reach a thumb: resolve the path with the same
                // rules the eventual fs.read will use, and drop the offer if that fails. A card
                // for a file that will not open teaches the captain the button is decorative —
                // and the model DOES hallucinate paths, so the check has to live here, not in
                // the prompt.
                // Ground the offer AND canonicalise it. The phone reads the file back through
                // `fs.read`, which has no idea which session the path came from — so what it is
                // handed must already be absolute. Judging a path here and then shipping the
                // relative form would put a working button on a path that cannot be opened.
                let base = crate::manager::session_cwd(&sess);
                proposals = proposals
                    .into_iter()
                    .filter_map(|mut p| {
                        if p.get("verb").and_then(|x| x.as_str()) != Some("open") {
                            return Some(p);
                        }
                        let raw = p.get("path").and_then(|x| x.as_str()).unwrap_or("").to_string();
                        match resolve_path_in(base.as_deref(), &raw) {
                            Ok(rp) if rp.is_file() => {
                                p["path"] = serde_json::json!(rp.to_string_lossy());
                                Some(p)
                            }
                            _ => {
                                crate::session::debug_log(&format!(
                                    "[rover] dropped open offer for a path that will not read: {raw:?}"
                                ));
                                None
                            }
                        }
                    })
                    .collect();
                let summary = if usable {
                    format!(
                        "{{\"text\":{},\"computedBy\":{},\"proposals\":{}}}",
                        json_str(&prose),
                        json_str(&provenance),
                        serde_json::Value::Array(proposals)
                    )
                } else {
                    "{\"text\":\"\",\"fallback\":true}".to_string()
                };
                let _ = tx2.send(format!("{{\"t\":\"summary\",\"id\":\"chat\",\"summary\":{}}}", summary));
            });
        }
        // A proposed next step was taken, or waved away. Neither changes anything on the host —
        // the point is the RECORD. An action nobody took and an action nobody was shown look
        // identical without it, and they are opposite facts about whether the suggestion was any
        // good.
        Some("act") | Some("act_dismiss") => {
            let kind = v.get("t").and_then(|x| x.as_str()).unwrap_or("act");
            crate::manager::record_client_event(kind, &v, crate::worklog::now_secs());
            // Taking an action still routes through the pane it belongs to; dismissing does not.
            if kind == "act" {
                if let (Some(pane), Some(keys)) = (
                    v.get("paneId").and_then(|x| x.as_str()).and_then(|s| s.parse::<usize>().ok()),
                    v.get("keys").and_then(|x| x.as_str()),
                ) {
                    let _ = session::write_frame(writer, &ClientFrame::PaneInput {
                        pane, data: keys.to_string(),
                    });
                }
            }
        }
        // LLM proxy — the phone asks, the host answers with its own key.
        Some(kind @ ("ask" | "summarize")) => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("session").to_string();
            let q = if kind == "summarize" {
                format!("In one or two sentences, what is workspace '{id}' doing and what's the next step?")
            } else {
                v.get("q").and_then(|x| x.as_str()).unwrap_or("").to_string()
            };
            if q.trim().is_empty() {
                return;
            }
            if !llm_spend_allowed() {
                let _ = tx.send(format!(
                    "{{\"t\":\"summary\",\"id\":{},\"summary\":{{\"text\":\"rate limit — try again in a minute\",\"computedBy\":\"rate-limit\"}}}}",
                    json_str(&id)
                ));
                return;
            }
            let tx2 = tx.clone();
            // immediate "thinking" ping so the phone shows it's working
            let _ = tx2.send(format!(
                "{{\"t\":\"summary\",\"id\":{},\"summary\":{{\"text\":\"…\",\"streaming\":true}}}}",
                json_str(&id)
            ));
            thread::spawn(move || {
                let (answer, provenance, usable) = run_agent_ask(q);
                let summary = if usable {
                    format!("{{\"text\":{},\"computedBy\":{}}}", json_str(&answer), json_str(&provenance))
                } else {
                    // No usable key on the host — tell the phone to use its server fallback.
                    "{\"text\":\"\",\"fallback\":true}".to_string()
                };
                let _ = tx2.send(format!("{{\"t\":\"summary\",\"id\":{},\"summary\":{}}}", json_str(&id), summary));
            });
        }
        // Pane-targeted write-back — the phone answering a prompt (`y\n` / `n\n`).
        Some("answer") => {
            let pane = v.get("paneId").and_then(|x| x.as_str()).and_then(|s| s.parse::<usize>().ok());
            let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("");
            if let Some(pane) = pane {
                let _ = session::write_frame(writer, &ClientFrame::PaneInput { pane, data: text.to_string() });
            }
        }
        Some("key") | Some("paste") => {
            if let Some(data) = v.get("data").or_else(|| v.get("text")).and_then(|d| d.as_str()) {
                // Pane-targeted when the phone names a pane (typing into a specific
                // terminal, e.g. sending a command to Claude Code); else the focused pane.
                match v.get("paneId").and_then(|x| x.as_str()).and_then(|s| s.parse::<usize>().ok()) {
                    Some(pane) => {
                        let _ = session::write_frame(writer, &ClientFrame::PaneInput { pane, data: data.to_string() });
                    }
                    None => {
                        let _ = session::write_frame(writer, &ClientFrame::Paste(data.to_string()));
                    }
                }
            }
        }
        // Watch/stop-watching a pane's live screen (the phone reading a terminal).
        Some("watch") => {
            let pane = v.get("paneId").and_then(|x| x.as_str()).and_then(|s| s.parse::<usize>().ok());
            let cols = v.get("cols").and_then(|x| x.as_u64()).map(|n| n as u16);
            let rows = v.get("rows").and_then(|x| x.as_u64()).map(|n| n as u16);
            // The xterm.js renderer asks for raw byte streaming; the DOM renderer omits it.
            let raw = v.get("raw").and_then(|x| x.as_bool()).unwrap_or(false);
            let _ = session::write_frame(writer, &ClientFrame::WatchPane { pane, cols, rows, raw });
        }
        Some("unwatch") => {
            let _ = session::write_frame(writer, &ClientFrame::WatchPane { pane: None, cols: None, rows: None, raw: false });
        }
        // The phone tells us how it greeted the captain, so the briefing can continue from it.
        Some("greeting") => {
            if let Some(text) = v.get("text").and_then(|x| x.as_str()) {
                let _ = session::write_frame(writer, &ClientFrame::RoverGreeting { text: text.to_string() });
            }
        }
        // Page upward in a watched pane: ask for `lines` of scrollback (xterm.js renderer).
        Some("history") => {
            let pane = v.get("paneId").and_then(|x| x.as_str()).and_then(|s| s.parse::<usize>().ok());
            let lines = v.get("lines").and_then(|x| x.as_u64()).unwrap_or(200) as usize;
            if let Some(pane) = pane {
                let _ = session::write_frame(writer, &ClientFrame::PaneHistory { pane, lines });
            }
        }
        // A window of a watched pane's transcript, half-open [from, to) in line ids. A request
        // with an empty range is how a client asks "how much is there?" before it asks for any.
        Some("lines") => {
            let pane = v.get("paneId").and_then(|x| x.as_str()).and_then(|s| s.parse::<usize>().ok());
            let from = v.get("from").and_then(|x| x.as_u64()).unwrap_or(0);
            let to = v.get("to").and_then(|x| x.as_u64()).unwrap_or(0);
            if let Some(pane) = pane {
                let _ = session::write_frame(writer, &ClientFrame::PaneLines { pane, from, to });
            }
        }
        // Open a new terminal tab in the session (additive; surfaces on the next board push).
        Some("new_terminal") => {
            let _ = session::write_frame(writer, &ClientFrame::NewTerminal);
        }
        // The same, with a coding agent started in it. The HOST decides when the shell is ready to
        // be written to — a bridge that sent `new_terminal` and then typed `claude` would be
        // guessing at that from the wrong side of the socket, and a launch line that lands before
        // the shell reads is swallowed in silence.
        Some("new_agent") => {
            let _ = session::write_frame(writer, &ClientFrame::NewAgent);
        }
        // Rename ONE workspace, addressed by the pane the phone is standing in. Unlike
        // `rename` (the session) this rides the subscribe writer: the daemon closes the stream
        // that sends `Rename`, but a workspace rename leaves the connection open, so the live
        // board keeps flowing and the new name arrives on the next push.
        Some("rename_workspace") => {
            let pane = v.get("paneId").and_then(|x| x.as_str()).and_then(|s| s.parse::<usize>().ok());
            let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if let Some(pane) = pane {
                if !name.is_empty() {
                    let _ = session::write_frame(writer, &ClientFrame::RenameWorkspace { pane, to: name });
                }
            }
        }
        // Every live session on this host: the phone's fleet page renders the truth instead of
        // its own bookkeeping. Enumerate the socket dir and identify each — dead sockets are
        // filtered by the probe itself.
        Some("sessions.list") => {
            let mut out: Vec<serde_json::Value> = Vec::new();
            if let Ok(dir) = session::socket_dir() {
                if let Ok(rd) = std::fs::read_dir(&dir) {
                    for e in rd.flatten() {
                        let p = e.path();
                        if p.extension().and_then(|x| x.to_str()) != Some("sock") {
                            continue;
                        }
                        if let Some((name, _, attached)) = session::identify(&p) {
                            out.push(serde_json::json!({ "name": name, "attached": attached }));
                        }
                    }
                }
            }
            let _ = tx.send(serde_json::json!({ "t": "sessions.list", "sessions": out }).to_string());
        }
        // A whole new session: spawn its daemon here (the bridge is the same binary), and the
        // multiplexed routing serves it the moment its socket answers. The phone adds its own
        // fleet row — nothing else to do host-side.
        Some("new_session") => {
            if let Some(name) = v.get("name").and_then(|x| x.as_str()).map(str::trim).filter(|s| !s.is_empty()) {
                let msg = match session::spawn_daemon(name, None) {
                    Ok(()) => format!("session '{name}' is up"),
                    Err(e) => format!("could not start '{name}': {e}"),
                };
                let _ = tx.send(format!("{{\"t\":\"toast\",\"text\":{}}}", json_str(&msg)));
            }
        }
        // END the connection's own session — the phone's card carries the deliberate gesture,
        // and the aim is fixed by construction: only the session this socket serves can be
        // ended. The daemon flushes state on Kill (autosave before should_quit), same as
        // `mars kill`.
        Some("end_session") => {
            crate::manager::record_client_event("end_session", &v, crate::worklog::now_secs());
            let _ = session::write_frame(writer, &ClientFrame::Kill);
        }
        // Save a memo the agent OFFERED. Written by the bridge on the captain's press — the
        // agent itself still cannot touch the manager's files. Title-slugged alongside the
        // manager's own memos, so the feed, the archive and assignment all pick it up.
        Some("memo.note") => {
            let body = v.get("body").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            let title = v.get("name").and_then(|x| x.as_str()).map(str::trim).filter(|s| !s.is_empty()).unwrap_or("note").to_string();
            let session_name = socket.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            if !body.is_empty() && !session_name.is_empty() {
                let msg = match crate::manager::write_captain_note(&session_name, &title, &body) {
                    Ok(()) => "note saved to memos".to_string(),
                    Err(e) => format!("could not save the note: {e}"),
                };
                crate::manager::record_client_event("memo.note", &v, crate::worklog::now_secs());
                let _ = tx.send(format!("{{\"t\":\"toast\",\"text\":{}}}", json_str(&msg)));
            }
        }
        // Rename the session from the phone — reflected on the host. Carried on a FRESH
        // short-lived connection: the daemon closes the stream that sends Rename, so using
        // our subscribe writer would drop the live board. The daemon does the fs-rename and
        // the next board snapshot carries the new name back.
        Some("rename") => {
            if let Some(name) = v.get("name").and_then(|x| x.as_str()) {
                let name = name.trim();
                if !name.is_empty() {
                    // Re-resolve by instance rather than reusing the path captured when this
                    // websocket opened: after one rename that path is gone, so a SECOND rename
                    // from the phone would silently do nothing.
                    let live = remembered_instance()
                        .and_then(|id| session::socket_for_instance(&id))
                        .map(|(_, p)| p)
                        .unwrap_or_else(|| socket.to_path_buf());
                    if let Ok(mut conn) = crate::sys::control::connect(&live) {
                        let _ = session::write_frame(&mut conn, &ClientFrame::Rename { to: name.to_string() });
                    }
                }
            }
        }
        // Filesystem explorer — the phone browses/reads/edits host files directly. Reads
        // are self-contained here (this process already touches the fs for tokens/static
        // files); every path flows through `resolve_path`, the single seam where
        // allowed-roots containment will be re-woven later (none today, by decision).
        Some("fs.list") => {
            let path = v.get("path").and_then(|x| x.as_str()).unwrap_or("");
            let _ = tx.send(fs_list_json(path));
        }
        // The manager view, computed from the tree on request. There is no index file to read,
        // so there is nothing to be stale. Three verbs rather than one because they change at
        // completely different rates — the board with every command you run, memos and health
        // only after an agent run — and the phone should not refetch a briefing to learn a
        // run tally.
        // Conversations the captain could bind to a pane, when Mars could not work it out.
        //
        // Discovery by pid fails often — a live `claude` pane with no `sessions/<pid>.json` and no
        // roster entry is the normal case, not the edge — and the honest answer to "we cannot tell
        // which conversation this is" is to ask, not to hide the feature. Titles come from Claude
        // Code's own `aiTitle`, so this is a list of names rather than a list of uuids.
        Some("agent.candidates") => {
            let cwd = v.get("cwd").and_then(|x| x.as_str()).filter(|c| !c.is_empty());
            let limit = v.get("limit").and_then(|x| x.as_u64()).unwrap_or(12).min(40) as usize;
            let _ = tx.send(
                serde_json::json!({
                    "t": "agent.candidates",
                    "paneId": v.get("paneId").and_then(|x| x.as_str()).unwrap_or_default(),
                    "candidates": crate::timeline::candidates(cwd, limit),
                })
                .to_string(),
            );
        }
        // The agent conversation as rows, for the phone's timeline lens.
        //
        // The CHAT ID COMES FROM THE CLIENT, and that is safe for exactly one reason: it flows
        // through `valid_chat_id`, which admits only `[A-Za-z0-9_-]{1,64}`. No separator, no dot,
        // so nothing here can be steered out of `~/.claude/projects` — the same seam that disarmed
        // the restore-manifest injection. Without that check this would be an arbitrary-file read
        // wearing a conversation id.
        //
        // The bridge relays board frames rather than holding them, so it cannot map a pane to its
        // conversation on its own; the phone already has that mapping from the board it is
        // rendering, and sending it back is cheaper than teaching the bridge to cache.
        Some("agent.timeline") => {
            let pane = v.get("paneId").and_then(|x| x.as_str()).unwrap_or_default().to_string();
            let chat = v.get("chat").and_then(|x| x.as_str()).unwrap_or_default().to_string();
            let limit = v.get("limit").and_then(|x| x.as_u64()).unwrap_or(60).min(300) as usize;
            let reply = if !crate::session::valid_chat_id(&chat) {
                // Not an error to report loudly: a shell pane simply has no conversation, and the
                // client says so rather than rendering an empty list that looks like a hung fetch.
                serde_json::json!({
                    "t": "agent.timeline", "paneId": pane, "rows": [],
                    "reason": "this workspace is not running a coding agent",
                })
            } else {
                match crate::timeline::rows_for(&chat, limit) {
                    Some(rows) => serde_json::json!({
                        "t": "agent.timeline", "paneId": pane, "chat": chat,
                        "rows": crate::timeline::rows_json(&rows),
                    }),
                    None => serde_json::json!({
                        "t": "agent.timeline", "paneId": pane, "chat": chat, "rows": [],
                        "reason": "no transcript found for this conversation yet",
                    }),
                }
            };
            let _ = tx.send(reply.to_string());
        }
        Some("manager.board") | Some("manager.memos") | Some("manager.health") => {
            let want = v.get("t").and_then(|t| t.as_str()).unwrap_or_default().to_string();
            let session_name = socket.file_stem().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
            let _ = tx.send(manager_view_json(&want, &session_name));
        }
        // The archive, read-only: what the manager said, kept, by day. The phone browses it —
        // old briefings, workspace notes and memos that have since been rewritten or pruned.
        Some("manager.archive") => {
            let day = v.get("day").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let _ = tx.send(manager_archive_json(&day));
        }
        // The queue. A directory read, computed on demand — there is no index to go stale, and
        // "what is in flight" is answered by which files exist rather than by a stored status.
        Some("brief.list") => {
            let rows = brief_rows();
            let _ = tx.send(serde_json::json!({"t": "brief.board", "briefs": rows}).to_string());
        }
        // Hand a brief to a worker. The bridge forwards and does NOT judge — the pane's tool scope
        // is read from the argv of the process actually running, which only the daemon can see.
        Some("brief.assign") => {
            if let (Some(pane), Some(brief)) = (
                v.get("paneId").and_then(|x| x.as_str()).and_then(|s| s.parse::<usize>().ok()),
                v.get("brief").and_then(|x| x.as_str()),
            ) {
                crate::manager::record_client_event("act", &v, crate::worklog::now_secs());
                let _ = session::write_frame(writer, &ClientFrame::AssignBrief {
                    pane, brief: brief.to_string(),
                });
            }
        }
        // Start an agent in a pane WITH the worker tool scope. One composer, host-side: the phone
        // never builds a shell line for a host whose version it does not know, and the deny-list
        // has exactly one definition.
        Some("brief.worker") => {
            if let Some(pane) = v.get("paneId").and_then(|x| x.as_str()).and_then(|s| s.parse::<usize>().ok()) {
                let _ = session::write_frame(writer, &ClientFrame::PaneInput {
                    pane, data: crate::briefs::worker_start_command(),
                });
            }
        }
        // Acceptance, observed rather than reported. Runs the brief's own commands here, in the
        // directory the brief recorded, with no shell anywhere in the path.
        Some("brief.verify") => {
            let id = v.get("brief").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let tx2 = tx.clone();
            // Off the reader thread: a build is minutes, and the bridge must keep serving the
            // board while it runs — a phone that goes dead during a verify looks like a crash.
            std::thread::spawn(move || {
                let rows = crate::briefs::dir()
                    .filter(|_| crate::briefs::safe_id(&id))
                    .and_then(|root| crate::briefs::read(&root.join(&id)))
                    .map(|b| crate::briefs::verify(&b, std::time::Duration::from_secs(600)))
                    .unwrap_or_default();
                let out: Vec<serde_json::Value> = rows.iter()
                    .map(|r| serde_json::json!({"cmd": r.cmd, "exit": r.exit, "note": r.note}))
                    .collect();
                let _ = tx2.send(serde_json::json!({
                    "t": "brief.verified", "brief": id, "rows": out,
                }).to_string());
            });
        }
        // THE WEB TERMINAL. A second daemon connection, opened as a Mirror, whose rendered
        // frames are pumped to this browser.
        //
        // A separate connection on purpose: the first one is a `Subscribe` and a socket speaks one
        // role for its lifetime. And a mirror must be able to come and go — a closed browser tab
        // should cost its own connection and nothing else, least of all the board this phone is
        // watching over the other one.
        //
        // Nothing is transcoded. `FrameWriter` already emits `ServerFrame::Output { b64 }`, which
        // is the same frame a real client receives, so the bytes reaching xterm.js are the bytes
        // MARS drew.
        Some("mirror") => {
            let cols = v.get("cols").and_then(|x| x.as_u64()).unwrap_or(120).clamp(20, 400) as u16;
            let rows = v.get("rows").and_then(|x| x.as_u64()).unwrap_or(32).clamp(8, 200) as u16;
            // ONE MIRROR PER BROWSER, REPLACED RATHER THAN STACKED.
            //
            // A resize re-sends `mirror` at the new size, and the daemon's handshake is once per
            // connection — so without this, every window drag would leave another live render
            // target behind, each drawing a full frame at a size nobody is looking at.
            //
            // Dropping the old stream closes it; the daemon prunes the dead target on its next
            // draw, which is the same path a closed tab takes.
            let gen = MIRROR_GEN.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
            drop(MIRROR_IN.lock().unwrap().take());
            let Ok(sock) = crate::sys::control::connect(socket) else {
                let _ = tx.send(serde_json::json!({
                    "t": "mirror.gone", "why": "could not reach the session",
                }).to_string());
                return;
            };
            let mut w = match sock.try_clone() { Ok(w) => w, Err(_) => return };
            if session::write_frame(&mut w, &ClientFrame::Mirror { cols, rows }).is_err() {
                return;
            }
            // Kept so `mirror.key` can write to the same connection the frames come from — a
            // keystroke on a different socket would reach a daemon that has no idea which mirror
            // it belongs to.
            if let Ok(w2) = sock.try_clone() {
                *MIRROR_IN.lock().unwrap() = Some(w2);
            }
            let out = tx.clone();
            std::thread::spawn(move || {
                let mut lines = BufReader::new(sock);
                let mut line = String::new();
                loop {
                    line.clear();
                    match lines.read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    match serde_json::from_str(line.trim()) {
                        // Same envelope the pane path uses, under its own name so a desk shell and
                        // a pane view can be open at once without reading each other's bytes.
                        Ok(ServerFrame::Output { b64 }) => {
                            if out.send(serde_json::json!({"t": "mirror.data", "b64": b64}).to_string()).is_err() {
                                break;
                            }
                        }
                        // The size the session is ACTUALLY drawn at, which is not necessarily the
                        // size this browser asked for — the person at the desk decides that, and a
                        // tab opening must not move it. Forwarded so the browser can fit the whole
                        // grid into its window instead of showing the top-left corner of it and
                        // looking, correctly but uselessly, like a rendering bug.
                        Ok(ServerFrame::GridSize { cols, rows, desk }) => {
                            if out.send(serde_json::json!({"t": "mirror.size", "cols": cols, "rows": rows, "desk": desk}).to_string()).is_err() {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                // Only the CURRENT mirror gets to report a death. A superseded one is ending
                // because the browser asked for a different size, which is the opposite of news.
                if MIRROR_GEN.load(std::sync::atomic::Ordering::SeqCst) == gen {
                    let _ = out.send(serde_json::json!({"t": "mirror.gone", "why": "the session ended"}).to_string());
                }
            });
        }
        // Typing into the web terminal. Straight through: the daemon decodes the bytes, because
        // the mapping from a terminal's bytes to MARS's chords is MARS's business and a second
        // copy of it in the phone bundle is a second copy that drifts.
        Some("mirror.key") => {
            let data = v.get("data").and_then(|x| x.as_str()).unwrap_or("");
            if !data.is_empty() {
                if let Some(w) = MIRROR_IN.lock().unwrap().as_mut() {
                    let _ = session::write_frame(w, &ClientFrame::MirrorKeys { data: data.to_string() });
                }
            }
        }
        // Same, for the planner scope — the one role allowed to write a brief.
        Some("brief.planner") => {
            if let Some(pane) = v.get("paneId").and_then(|x| x.as_str()).and_then(|s| s.parse::<usize>().ok()) {
                let _ = session::write_frame(writer, &ClientFrame::PaneInput {
                    pane, data: crate::briefs::planner_start_command(),
                });
            }
        }
        // Kick off ideation: mint an empty brief and set the planner in this pane filling it in.
        // The title is the whole payload because it is the only thing the host cannot derive —
        // everything else about the brief (its id, its path, its sections) is minted daemon-side.
        // REFINEMENT IS ONE GESTURE: pick a different option. No free-text editing of a binding
        // document, no second composer, no re-drafting — an override is definitionally a decision
        // already made, so it lands in the section that is already binding and already parsed.
        Some("brief.decide") => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            let did = v.get("decision").and_then(|x| x.as_str()).unwrap_or("");
            let key = v.get("option").and_then(|x| x.as_str()).unwrap_or("");
            let dir = crate::briefs::dir().map(|d| d.join(id));
            // `safe_id` because this id names a directory and arrives over the wire.
            let out = match dir {
                Some(d) if crate::briefs::safe_id(id) && !did.is_empty() && !key.is_empty() => {
                    match crate::briefs::override_decision(&d, did, key) {
                        Ok(stale) => serde_json::json!({
                            "t": "brief.decided", "id": id, "decision": did,
                            "option": key.to_uppercase(), "stale": stale,
                        }),
                        Err(e) => serde_json::json!({
                            "t": "brief.decided", "id": id, "decision": did, "error": e.to_string(),
                        }),
                    }
                }
                _ => serde_json::json!({
                    "t": "brief.decided", "id": id, "error": "bad brief id, decision or option",
                }),
            };
            crate::manager::record_client_event("act", &v, crate::worklog::now_secs());
            let _ = tx.send(out.to_string());
            // The board is the source of truth for what the card renders, so re-read it rather
            // than patching the row client-side — two copies of a decision is two things to keep
            // true.
            let rows = brief_rows();
            let _ = tx.send(serde_json::json!({"t": "brief.board", "briefs": rows}).to_string());
        }
        // THE DOCUMENT ITSELF. "Read it" expanded to show the brief's ID, which is the one fact
        // about a brief nobody needs — the design, the acceptance criteria and the out-of-scope
        // list all stayed in a file on a machine you are not sitting at. Approving something you
        // cannot read is the failure this whole surface exists to prevent.
        Some("brief.read") => {
            let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("");
            let out = match crate::briefs::dir().filter(|_| crate::briefs::safe_id(id)) {
                Some(d) => {
                    let read = |f: &str| std::fs::read_to_string(d.join(id).join(f)).ok();
                    match read("brief.md") {
                        Some(md) => serde_json::json!({
                            "t": "brief.doc", "id": id, "md": md,
                            // Sent alongside rather than fetched separately: they are read
                            // together or not at all, and a second round trip is a second way for
                            // half the story to arrive.
                            "report": read("completed.md"),
                            "review": read("review.md"),
                        }),
                        None => serde_json::json!({"t": "brief.doc", "id": id, "error": "no brief.md"}),
                    }
                }
                None => serde_json::json!({"t": "brief.doc", "id": id, "error": "bad brief id"}),
            };
            let _ = tx.send(out.to_string());
        }
        Some("brief.draft") => {
            if let (Some(pane), Some(title)) = (
                v.get("paneId").and_then(|x| x.as_str()).and_then(|s| s.parse::<usize>().ok()),
                v.get("title").and_then(|x| x.as_str()).map(str::trim).filter(|s| !s.is_empty()),
            ) {
                crate::manager::record_client_event("act", &v, crate::worklog::now_secs());
                // WHAT THE ARGUMENT SETTLED, carried with the title. Optional: a draft pressed
                // from a board rather than out of a conversation still sends none, and the planner
                // derives everything as it always did.
                let decisions: Vec<session::PriorDecision> = v
                    .get("decisions")
                    .and_then(|d| serde_json::from_value(d.clone()).ok())
                    .unwrap_or_default();
                let _ = session::write_frame(writer, &ClientFrame::DraftBrief {
                    pane,
                    title: title.chars().take(160).collect(),
                    decisions,
                });
            }
        }
        Some("fs.read") => {
            let path = v.get("path").and_then(|x| x.as_str()).unwrap_or("");
            let _ = tx.send(fs_read_json(path));
        }
        Some("fs.write") => {
            let path = v.get("path").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let content = v.get("content").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let base = v.get("baseMtime").and_then(|x| x.as_u64());
            let _ = tx.send(fs_write_json(&path, &content, base));
        }
        // Structured intents (run/jump…) land here once the daemon has a JSON action
        // sink; until then they're recorded, not silently dropped.
        Some(other) => crate::session::debug_log(&format!("[rover] intent not yet wired: {other}")),
        None => {}
    }
}

/// Replace this process with the `mars` on disk now. Returns only on refusal — a successful
/// `exec` never comes back.
///
/// The version probe is the entire safety story and must come first. Exec'ing a binary that
/// cannot start is the one failure here with no way back: the bridge is the phone's only route to
/// this machine, and there is nothing above it to notice and retry. So the last act before the
/// point of no return is to run the candidate and require it to work.
/// Does this binary actually run? The gate that stands between a live bridge and a dead one.
///
/// Separated from the exec so it can be tested: the exec cannot be, because a successful one
/// replaces the test process. This half carries all the judgement; the other half is one line.
pub fn candidate_ok(exe: &Path) -> bool {
    Command::new(exe)
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(unix)]
fn replace_self() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    if !candidate_ok(&exe) {
        return Err(anyhow!("{} --version failed — keeping the bridge that works", exe.display()));
    }
    // Same argv, so whatever session and flags this bridge was started with are preserved.
    let args: Vec<String> = std::env::args().skip(1).collect();
    crate::session::debug_log("[rover] replacing bridge image with the binary on disk");
    Err(anyhow!("exec failed: {}", Command::new(&exe).args(&args).exec()))
}

#[cfg(not(unix))]
fn replace_self() -> Result<()> {
    Err(anyhow!("in-place upgrade needs a unix exec; restart the bridge by hand"))
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

const MANAGER_STALE_SECS: u64 = 2700;

// ── Filesystem explorer (fs.list / fs.read / fs.write) ───────────────────────────────
// v1 has NO path containment — reads/writes hit arbitrary absolute paths, exactly as the
// desktop editor already does. `resolve_path` is the single seam where allowed-roots /
// symlink / traversal guards will be added later; callers won't change when it tightens.

/// A read is capped to this many lines so a huge file can't blow the WS frame / phone; the
/// payload carries `truncated:true` past it. A transport safety bound, not an editor knob.
const FS_READ_MAX_LINES: usize = 20_000;

fn fs_home() -> PathBuf {
    crate::sys::paths::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

/// Directories whose contents are credentials, never documents. Denied even inside the root,
/// because the point of the bridge is reading your work, and nothing here is your work.
const FS_DENY: [&str; 6] = [".ssh", ".aws", ".gnupg", ".config/gh", ".kube", ".docker"];

/// Is `p` inside `root`, and outside every denied directory?
fn contained(p: &Path, root: &Path) -> bool {
    if !p.starts_with(root) {
        return false;
    }
    let Ok(rel) = p.strip_prefix(root) else { return false };
    // Compare whole path COMPONENTS, never a string prefix: `.sshfoo` starts with `.ssh` as text
    // and is a perfectly ordinary directory.
    let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into()).collect();
    if parts.iter().any(|c| c == ".env" || c.starts_with(".env.")) {
        return false;
    }
    !FS_DENY.iter().any(|deny| {
        let d: Vec<&str> = deny.split('/').collect();
        parts.windows(d.len()).any(|w| w.iter().zip(&d).all(|(a, b)| a == b))
    })
}

/// The path seam: expand `~`, canonicalize (resolving `..`/symlinks against the real fs), then
/// **verify containment**. A not-yet-existing write target canonicalizes its parent and rejoins
/// the name.
///
/// Order is the whole security property. Canonicalize FIRST, check SECOND — a check on the raw
/// string is defeated by `../` and by a symlink inside the root pointing out of it, both of which
/// canonicalization resolves before we ever look. Reversing these two lines silently reopens the
/// hole while every test still passes.
///
/// Without this, one authenticated frame read `~/.ssh/id_rsa`, and one wrote `~/.zshrc` — which is
/// remote code execution on the next shell. The bridge is reachable from a public tunnel, so a
/// leaked pairing token meant total host compromise rather than "somebody can see my terminals".
pub fn resolve_path_for_test(raw: &str) -> std::io::Result<PathBuf> {
    resolve_path(raw)
}

/// Selfcheck seams for the session-relative resolution.
pub fn resolve_path_in_for_test(base: Option<&std::path::Path>, raw: &str) -> std::io::Result<PathBuf> {
    resolve_path_in(base, raw)
}
pub fn fs_home_for_test() -> PathBuf {
    fs_home()
}

fn resolve_path(raw: &str) -> std::io::Result<PathBuf> {
    resolve_path_in(None, raw)
}

/// Resolve a path the way the SESSION would, not the way the bridge would.
///
/// A relative path only means something against a directory, and the two processes have different
/// ones: the chat agent runs in the session's own directory, while this bridge runs wherever it
/// was started — measured on this machine, `~/Code` against `~/Mars-Mission/mars-terminal`. So a
/// perfectly good `docs/design.md` from the agent resolved to nothing here, and the offer to open
/// it was dropped silently by the grounding check. Every session whose directory differs from the
/// bridge's was affected, which is nearly all of them.
///
/// Containment is unchanged: whatever the base, the result must still canonicalize inside $HOME
/// and outside the deny-list. A base cannot widen what is reachable, only disambiguate it.
fn resolve_path_in(base: Option<&std::path::Path>, raw: &str) -> std::io::Result<PathBuf> {
    let raw = raw.trim();
    let p = if raw.is_empty() || raw == "~" {
        fs_home()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        fs_home().join(rest)
    } else {
        let p = PathBuf::from(raw);
        match base {
            Some(b) if p.is_relative() => b.join(p),
            _ => p,
        }
    };
    let resolved = match p.canonicalize() {
        Ok(c) => c,
        Err(e) => match (p.parent(), p.file_name()) {
            (Some(parent), Some(name)) => parent.canonicalize()?.join(name),
            _ => return Err(e),
        },
    };
    // The root is canonicalized too: on macOS `$HOME` is often reached through `/System/Volumes`,
    // so comparing a canonical path against a raw root rejects every legitimate file.
    let root = fs_home().canonicalize().unwrap_or_else(|_| fs_home());
    if !contained(&resolved, &root) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "outside the shared root",
        ));
    }
    Ok(resolved)
}

fn mtime_secs(md: &std::fs::Metadata) -> u64 {
    md.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn io_code(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::NotFound => "notfound",
        _ => "denied",
    }
}

fn fs_error_json(path: &str, code: &str, message: &str) -> String {
    serde_json::json!({ "t": "fs.error", "path": path, "code": code, "message": message }).to_string()
}

fn fs_list_json(raw: &str) -> String {
    let dir = match resolve_path(raw) {
        Ok(p) => p,
        Err(e) => return fs_error_json(raw, io_code(&e), &e.to_string()),
    };
    let rd = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(e) => return fs_error_json(&dir.display().to_string(), io_code(&e), &e.to_string()),
    };
    // (name, is_dir, size, mtime); dotfiles included — the whole point.
    let mut rows: Vec<(String, bool, u64, u64)> = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let md = e.metadata().ok();
        let is_dir = md.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = md.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = md.as_ref().map(mtime_secs).unwrap_or(0);
        rows.push((name, is_dir, size, mtime));
    }
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.to_lowercase().cmp(&b.0.to_lowercase())));
    let entries: Vec<_> = rows
        .into_iter()
        .map(|(name, dir, size, mtime)| serde_json::json!({ "name": name, "dir": dir, "size": size, "mtime": mtime }))
        .collect();
    serde_json::json!({
        "t": "fs.dir",
        "path": dir.display().to_string(),
        "parent": dir.parent().map(|p| p.display().to_string()),
        "entries": entries,
    })
    .to_string()
}


/// Slice the computed view for one verb. Computing the whole thing and slicing is deliberate:
/// it is a couple of kilobytes over a couple of dozen small files, so three separate walks would
/// buy nothing but three code paths to keep in step.
/// Which session's daemon currently holds the manager lock — the "who is doing the thinking"
/// debug fact. The lock records an instance id; resolving it to a live session name is the same
/// identity walk the bridge does, and `null` fields mean the lock is stale or free.
fn agent_home_json() -> serde_json::Value {
    let lock = crate::sys::paths::home_dir()
        .and_then(|h| std::fs::read_to_string(h.join(".mars/manager/agent.lock")).ok())
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    let Some(lock) = lock else { return serde_json::Value::Null };
    let owner = lock["owner"].as_str().unwrap_or_default().to_string();
    let session = session::socket_for_instance(&owner).map(|(name, _)| name);
    serde_json::json!({ "owner": owner, "session": session, "at_ts": lock["ts"] })
}

fn manager_view_json(want: &str, session: &str) -> String {
    let Some(repo) = crate::manager::repo_dir() else {
        return serde_json::json!({ "t": want, "error": "no manager repo" }).to_string();
    };
    let ts = crate::worklog::now_secs();
    let mut v = crate::manager::view(&repo, ts, MANAGER_STALE_SECS);
    // CONTEXT ISOLATION. The manager is one agent over many sessions, and its view aggregates
    // them all — but this connection serves exactly ONE session, and showing it another's memos
    // or briefing is how boards started bleeding into each other once the bridge multiplexed.
    // Scope by the durable DIRECTORY id, not the name: renames left stale dirs whose names
    // collide, and a name match with a fallback is how the wrong briefing got picked.
    let dir_id = crate::manager::existing_session_dir_pub(session)
        .and_then(|d| d.file_name().map(|x| x.to_string_lossy().to_string()));
    if let Some(dir_id) = &dir_id {
        if let Some(memos) = v["memos"].as_array_mut() {
            memos.retain(|c| c["dir"].as_str() == Some(dir_id.as_str()));
        }
        if let Some(sessions) = v["sessions"].as_array_mut() {
            sessions.retain(|e| e["dir"].as_str() == Some(dir_id.as_str()));
        }
    }
    let body = match want {
        "manager.memos" => serde_json::json!({ "memos": v["memos"] }),
        "manager.health" => serde_json::json!({
            "agentEnabled": v["agentEnabled"], "agentRuns": v["agentRuns"],
            // Rover's own readiness. The mark is gated on this so the control appears when it can
            // be used — a button that does nothing for four seconds teaches people it is broken.
            // `ready` is kept alongside `state` so a phone running an older bundle — which knows
            // only the boolean — keeps working instead of losing the mark on a host upgrade.
            "rover": { "state": rover_status().0, "ready": rover_status().0 == "ready", "rampMs": rover_status().1, "detail": rover_status().2 },
            // WHERE the manager runs: the lock's owner is an instance id; resolve it to the
            // session currently holding it, so the sidebar can say which daemon does the work.
            "agentHome": agent_home_json(),
            "agentStaleSecs": v["agentStaleSecs"],
        }),
        _ => serde_json::json!({ "sessions": v["sessions"] }),
    };
    let mut out = body;
    out["t"] = serde_json::Value::String(want.to_string());
    out["generated_ts"] = serde_json::json!(ts);
    out.to_string()
}

/// How many archive entries one answer carries. A day is tens of lines; the cap only matters if
/// something floods the file, and then it is exactly what stops the flood reaching the phone.
const ARCHIVE_MAX_ENTRIES: usize = 200;

/// One archive day, newest entries first, plus the list of days that exist. The requested day is
/// matched against the ENUMERATED list, never joined into a path — a string from a phone must
/// not aim, same rule as everywhere else on this boundary. Unknown or empty → the newest day.
fn manager_archive_json(day: &str) -> String {
    let ts = crate::worklog::now_secs();
    let Some(repo) = crate::manager::repo_dir() else {
        return serde_json::json!({ "t": "manager.archive", "error": "no manager repo", "generated_ts": ts }).to_string();
    };
    let dir = repo.join("archive");
    let mut days: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                return None;
            }
            p.file_stem().map(|s| s.to_string_lossy().to_string())
        })
        .filter(|s| s.len() == 10 && s.chars().all(|c| c.is_ascii_digit() || c == '-'))
        .collect();
    days.sort();
    days.reverse();
    let pick = if days.iter().any(|d| d == day) {
        day.to_string()
    } else {
        days.first().cloned().unwrap_or_default()
    };
    let mut entries: Vec<serde_json::Value> = Vec::new();
    if !pick.is_empty() {
        for line in std::fs::read_to_string(dir.join(format!("{pick}.jsonl")))
            .unwrap_or_default()
            .lines()
            .rev()
            .take(ARCHIVE_MAX_ENTRIES)
        {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                entries.push(v);
            }
        }
    }
    serde_json::json!({
        "t": "manager.archive", "days": days, "day": pick, "entries": entries, "generated_ts": ts,
    })
    .to_string()
}

fn fs_read_json(raw: &str) -> String {
    let path = match resolve_path(raw) {
        Ok(p) => p,
        Err(e) => return fs_error_json(raw, io_code(&e), &e.to_string()),
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return fs_error_json(&path.display().to_string(), io_code(&e), &e.to_string()),
    };
    let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let ext = path.extension().map(|e| e.to_string_lossy().to_string()).unwrap_or_default();
    let mtime = std::fs::metadata(&path).map(|m| mtime_secs(&m)).unwrap_or(0);
    let truncated = text.lines().count() > FS_READ_MAX_LINES;
    let body = if truncated {
        text.lines().take(FS_READ_MAX_LINES).collect::<Vec<_>>().join("\n")
    } else {
        text
    };
    // The bridge is a separate process with no live App/palette, so highlight against the
    // default palette. Feature off → `highlight` returns None → plain lines.
    let palette = crate::themes::resolve(None);
    let lines = match crate::syntax::highlight(&body, &ext, &palette) {
        Some(hl) => fs_lines_highlighted(&hl),
        None => fs_lines_plain(&body),
    };
    serde_json::json!({
        "t": "fs.file",
        "path": path.display().to_string(),
        "name": name,
        "lang": ext,
        "mtime": mtime,
        "truncated": truncated,
        "lines": lines,
    })
    .to_string()
}

fn fs_lines_highlighted(hl: &[Vec<(ratatui::style::Style, String)>]) -> serde_json::Value {
    let lines: Vec<_> = hl
        .iter()
        .enumerate()
        .map(|(i, runs)| {
            let spans: Vec<_> = runs
                .iter()
                .map(|(style, text)| match style.fg {
                    Some(c) => {
                        let [r, g, b] = crate::themes::rgb_of(c);
                        serde_json::json!({ "t": text, "c": format!("#{r:02x}{g:02x}{b:02x}") })
                    }
                    None => serde_json::json!({ "t": text }),
                })
                .collect();
            serde_json::json!({ "n": i + 1, "spans": spans })
        })
        .collect();
    serde_json::Value::Array(lines)
}

fn fs_lines_plain(text: &str) -> serde_json::Value {
    let lines: Vec<_> = text
        .lines()
        .enumerate()
        .map(|(i, l)| serde_json::json!({ "n": i + 1, "spans": [{ "t": l }] }))
        .collect();
    serde_json::Value::Array(lines)
}

fn fs_write_json(raw: &str, content: &str, base_mtime: Option<u64>) -> String {
    // Reads are capped; writes were not — a hostile paired device could fill the disk through
    // this seam one frame at a time. 2MB is generous for anything a phone legitimately edits.
    const FS_WRITE_MAX_BYTES: usize = 2 * 1024 * 1024;
    if content.len() > FS_WRITE_MAX_BYTES {
        return fs_error_json(raw, "too-large", "write exceeds the 2MB cap for phone edits");
    }
    let path = match resolve_path(raw) {
        Ok(p) => p,
        Err(e) => return fs_error_json(raw, io_code(&e), &e.to_string()),
    };
    // Optimistic-concurrency guard: refuse if the file moved under us since the phone read it.
    if let (Some(base), Ok(md)) = (base_mtime, std::fs::metadata(&path)) {
        if mtime_secs(&md) != base {
            return fs_error_json(&path.display().to_string(), "conflict", "file changed on disk since it was opened");
        }
    }
    if let Err(e) = std::fs::write(&path, content) {
        return fs_error_json(&path.display().to_string(), io_code(&e), &e.to_string());
    }
    let mtime = std::fs::metadata(&path).map(|m| mtime_secs(&m)).unwrap_or(0);
    serde_json::json!({ "t": "fs.saved", "path": path.display().to_string(), "mtime": mtime }).to_string()
}

// ── Preflight ────────────────────────────────────────────────────────────────────────────
//
// Everything that has to be true before a QR is worth printing, checked cheaply and up front.
// The alternative — start work and interpret the wreckage — is what produced a 25-second timeout
// ending in "is your authtoken set?", which is a guess about a condition we can simply read.

/// One preflight line. `Skip` is a real outcome, not a soft failure: pairing without a static
/// domain works fine, it just costs a re-scan later, and saying so is different from an error.
#[derive(Clone, Debug, PartialEq)]
pub enum CheckState {
    Ok(String),
    Skip(String),
    Fail(String),
}

#[derive(Clone, Debug)]
pub struct Check {
    pub name: &'static str,
    pub state: CheckState,
    /// What the developer should do. Printed only when it would help.
    pub fix: Option<String>,
}

impl Check {
    fn ok(name: &'static str, detail: impl Into<String>) -> Self {
        Check { name, state: CheckState::Ok(detail.into()), fix: None }
    }
    fn skip(name: &'static str, detail: impl Into<String>) -> Self {
        Check { name, state: CheckState::Skip(detail.into()), fix: None }
    }
    fn fail(name: &'static str, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Check { name, state: CheckState::Fail(detail.into()), fix: Some(fix.into()) }
    }
    pub fn failed(&self) -> bool {
        matches!(self.state, CheckState::Fail(_))
    }
}

/// The ngrok static domain: config first, environment as an override.
///
/// It lived only in `MARS_NGROK_DOMAIN`, which is lost in a new shell and invisible to the launchd
/// agent — so the "this URL survives restarts" promise quietly broke under exactly the supervision
/// meant to keep it alive.
pub fn ngrok_domain() -> Option<String> {
    if let Some(d) = std::env::var("MARS_NGROK_DOMAIN").ok().filter(|d| !d.trim().is_empty()) {
        return Some(d.trim().to_string());
    }
    let path = crate::sys::paths::home_dir()?.join(".mars").join("config.json");
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    v["ngrok_domain"].as_str().map(str::trim).filter(|d| !d.is_empty()).map(String::from)
}

/// Persist the domain where a new shell and the launchd agent will both find it.
pub fn set_ngrok_domain(domain: &str) -> Result<()> {
    let path = crate::sys::paths::home_dir()
        .ok_or_else(|| anyhow!("no home directory"))?
        .join(".mars")
        .join("config.json");
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut v: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    v["ngrok_domain"] = serde_json::json!(domain.trim());
    std::fs::write(&path, serde_json::to_string_pretty(&v)?)?;
    Ok(())
}

fn tool_version(bin: &str, arg: &str) -> Option<String> {
    let out = std::process::Command::new(bin).arg(arg).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout);
    Some(s.split_whitespace().find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap_or_else(|| s.trim())
        .to_string())
}

/// Can the phone reach this machine.
pub fn preflight_bridge(session: Option<&str>) -> Vec<Check> {
    let mut out = Vec::new();

    out.push(match session {
        Some(s) => Check::ok("session", s.to_string()),
        None => Check::fail("session", "none running", "start one with `mars`"),
    });

    match tool_version("ngrok", "version") {
        Some(v) => out.push(Check::ok("ngrok", v)),
        None => out.push(Check::fail("ngrok", "not installed", "brew install ngrok")),
    }

    // Read the condition instead of inferring it from a tunnel that never appears.
    let token_ok = std::process::Command::new("ngrok")
        .args(["config", "check"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    out.push(if token_ok {
        Check::ok("authtoken", "configured")
    } else {
        Check::fail("authtoken", "not configured", "ngrok config add-authtoken <token>")
    });

    out.push(match ngrok_domain() {
        Some(d) => Check::ok("stable URL", d),
        // Skip carries no fix, and this is the one skip a reader wants to act on — a changing URL
        // means re-scanning after every restart, which is the most common "Rover stopped working".
        None => Check::skip(
            "stable URL",
            "not set — the QR changes on every restart. Reserve a free domain at \
             dashboard.ngrok.com/domains, then: mars pair --domain <name>.ngrok-free.dev",
        ),
    });
    out
}

/// Can the manager actually run. Independent of the bridge: a dead agent must never stop someone
/// pairing their phone, it just means the briefing will not move.
pub fn preflight_manager(probe: bool) -> Vec<Check> {
    let mut out = Vec::new();

    let have = tool_version("claude", "--version");
    match &have {
        Some(v) => out.push(Check::ok("claude", v.clone())),
        None => out.push(Check::fail(
            "claude",
            "not installed",
            "install Claude Code, then re-run `mars pair`",
        )),
    }

    if have.is_some() && probe {
        out.push(claude_can_run());
    }

    let enabled = crate::manager::repo_dir().is_some_and(|r| crate::manager::agent_enabled(&r));
    out.push(if enabled {
        Check::ok("agent", "on")
    } else {
        Check::skip("agent", "off")
    });
    out
}

/// One tiny turn, with the same environment scrub the manager's `run.sh` uses.
///
/// Worth its single small call: ANTHROPIC_API_KEY silently takes precedence over a claude.ai
/// login, so a stale or empty key makes the manager fail with "credit balance is too low" while
/// the developer's subscription is perfectly fine. Scrubbing it is what run.sh already does —
/// this check confirms the result rather than leaving them to discover it from a briefing that
/// never changes.
fn claude_can_run() -> Check {
    let out = std::process::Command::new("env")
        .args(["-u", "ANTHROPIC_API_KEY", "-u", "ANTHROPIC_AUTH_TOKEN", "claude", "-p", "ok"])
        .output();
    match out {
        Ok(o) if o.status.success() => {
            let keyed = std::env::var("ANTHROPIC_API_KEY").is_ok();
            Check::ok(
                "can run",
                if keyed { "on your subscription (API key ignored)" } else { "on your subscription" },
            )
        }
        Ok(o) => {
            let err = String::from_utf8_lossy(&o.stderr);
            let line = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("no output").trim();
            Check::fail("can run", line.chars().take(90).collect::<String>(), "run `claude` once to sign in")
        }
        Err(e) => Check::fail("can run", e.to_string(), "run `claude` once to sign in"),
    }
}

/// Render a group. Failures carry their fix; a skip states its cost and moves on.
pub fn print_checks(group: &str, checks: &[Check]) {
    println!("  \x1b[38;5;244m{group}\x1b[0m");
    for c in checks {
        let (mark, colour, detail) = match &c.state {
            CheckState::Ok(d) => ("✓", 35, d),
            CheckState::Skip(d) => ("·", 244, d),
            CheckState::Fail(d) => ("✗", 208, d),
        };
        println!("  \x1b[38;5;{colour}m{mark}\x1b[0m {:<13} {detail}", c.name);
        if let (CheckState::Fail(_), Some(fix)) = (&c.state, &c.fix) {
            println!("      \x1b[38;5;244m{fix}\x1b[0m");
        }
    }
}


/// Exercise the public path before showing a QR.
///
/// The bridge used to announce itself ready when the local listener bound, which proves nothing
/// about the tunnel. A QR that cannot work is worse than an error, because it moves the failure
/// onto a device with no diagnostics.
pub fn verify_public(base: &str) -> Vec<Check> {
    let mut out = Vec::new();

    let reachable = std::process::Command::new("curl")
        .args(["-sS", "-o", "/dev/null", "-m", "12", "-w", "%{http_code}", base])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|c| c.starts_with('2') || c.starts_with('3'));
    out.push(match reachable {
        Some(code) => Check::ok("reachable", format!("public URL answered {code}")),
        None => Check::fail("reachable", "no answer over the tunnel", "check ngrok is still up"),
    });

    // The whole bridge is one long-lived WebSocket, so an HTTP 200 is not enough evidence.
    let ws = base.replacen("https://", "wss://", 1);
    let upgraded = std::process::Command::new("curl")
        .args([
            "-sS", "-o", "/dev/null", "-m", "12", "-w", "%{http_code}",
            "-H", "Connection: Upgrade", "-H", "Upgrade: websocket",
            "-H", "Sec-WebSocket-Version: 13",
            "-H", "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==",
            base,
        ])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    out.push(match upgraded.as_deref() {
        Some("101") => Check::ok("websocket", "upgrade accepted"),
        Some(other) => Check::fail(
            "websocket",
            format!("tunnel answered {other}, not 101"),
            format!("the tunnel is not passing websockets through to {ws}"),
        ),
        None => Check::fail("websocket", "no answer", "check ngrok is still up"),
    });
    out
}

/// `mars pair --check` — the preflight alone, so it can be run without starting anything.
pub fn check_main(session_arg: Option<String>) -> Result<()> {
    let session = resolve_session(session_arg).ok();
    println!();
    println!("  \x1b[38;5;208mRover\x1b[0m · preflight");
    println!();
    let bridge = preflight_bridge(session.as_deref());
    print_checks("bridge", &bridge);
    println!();
    // Probe for real here: `--check` is the one place a developer explicitly asked us to find out,
    // so spending one tiny turn to answer "can the manager actually run" is what they came for.
    print_checks("manager", &preflight_manager(true));
    println!();
    if bridge.iter().any(|c| c.failed()) {
        anyhow::bail!("bridge preflight failed — fix the ✗ lines above, then `mars pair`");
    }
    // A bridge may already BE up, and this is where somebody asks "is this working". Answering
    // "ready to start one" is wrong when one is running on a tunnel that no longer carries
    // anything — precisely the case where every local signal reads fine.
    if let Some(base) = running_tunnel_url() {
        match tunnel_answers(&base) {
            Ok(()) => println!("  a bridge is already live at {base}, and it answers from outside"),
            Err(why) => {
                println!("  a bridge is already live at {base}");
                println!("{}", tunnel_warning(&why));
            }
        }
        return Ok(());
    }
    println!("  ready — run `mars pair` to bring the bridge up");
    Ok(())
}

/// `mars pair --link` — just the pairing URL, for when a camera will not focus.
pub fn link_main(session_arg: Option<String>) -> Result<()> {
    let session = resolve_session(session_arg)?;
    let base = running_tunnel_url()
        .ok_or_else(|| anyhow!("no bridge is running — start one with `mars pair`"))?;
    if let Err(why) = tunnel_answers(&base) {
        eprintln!("{}", tunnel_warning(&why));
    }
    println!("{}", pair_link(&session, &base, "rover")?);
    Ok(())
}
