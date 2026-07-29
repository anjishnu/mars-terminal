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
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use crate::session::{self, ClientFrame, ServerFrame};
use tungstenite::{accept, Error as WsError, Message};

const DEFAULT_PORT: u16 = 8787;

/// Resolve the session to bridge: explicit arg → `$MARS_SESSION` → the first live one.
fn resolve_session(arg: Option<String>) -> Result<String> {
    if let Some(name) = arg {
        return Ok(name);
    }
    if let Ok(name) = std::env::var("MARS_SESSION") {
        if !name.is_empty() {
            return Ok(name);
        }
    }
    let sessions = session::list_sessions()?;
    sessions
        .into_iter()
        .map(|(name, _, _)| name)
        .next()
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
fn mint_token() -> String {
    let mut buf = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
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
    }
}
/// Read the persisted token, minting + storing one on first use.
fn ensure_token() -> String {
    if let Some(t) = read_token() { return t; }
    let t = mint_token();
    write_token(&t);
    t
}

/// A stable-ish daemon identity fingerprint for the prototype (LAN-scoped, TOFU).
/// The real bridge signs with a persistent daemon keypair; credentials pin to THIS.
fn daemon_fingerprint(session: &str) -> String {
    let host = std::env::var("HOSTNAME").unwrap_or_else(|_| "lan".into());
    format!("{host}-{session}")
}

// ── mars qr ──────────────────────────────────────────────────────────────────

pub fn qr_main(session_arg: Option<String>) -> Result<()> {
    let session = resolve_session(session_arg)?;
    let ip = lan_ip();
    let port = DEFAULT_PORT;
    let token = mint_token();
    let fp = daemon_fingerprint(&session);

    // The phone loads the app and reads endpoint+identity+token from the FRAGMENT
    // (never a query — it stays out of server logs). LAN prototype: ws:// same host.
    let endpoint = format!("ws://{ip}:{port}/ws");
    let url = format!(
        "http://{ip}:{port}/rover#h={endpoint}&id={fp}&t={token}&s={session}&v=rover-1"
    );

    print_wordmark();
    println!();
    print_qr(&url);
    println!();
    println!("  \x1b[38;5;208mRover\x1b[0m — scan to pair  ·  session \x1b[1m{session}\x1b[0m");
    println!("  same wifi · link is single-use · run `mars serve` to accept the connection");
    println!("  {url}");
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

pub fn serve_main(session_arg: Option<String>) -> Result<()> {
    let session = resolve_session(session_arg)?;
    let socket = session::socket_path(&session)?;
    if !socket.exists() {
        return Err(anyhow!("session '{session}' has no socket — is it running? (`mars ls`)"));
    }
    let port = DEFAULT_PORT;
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
    let endpoint = format!("{}/ws", base.replacen("https://", "wss://", 1));
    let token = ensure_token();
    let fp = daemon_fingerprint(&session);
    let app_url = format!(
        "https://mars-terminal.lovable.app/rover#h={endpoint}&id={fp}&t={token}&s={session}&v=rover-1"
    );

    print_wordmark();
    println!();
    print_qr(&app_url);
    println!();
    println!("  \x1b[38;5;208mRover\x1b[0m — scan to pair · session \x1b[1m{session}\x1b[0m");
    if std::env::var("MARS_NGROK_DOMAIN").ok().filter(|d| !d.trim().is_empty()).is_some() {
        println!("  \x1b[38;5;35mstable endpoint\x1b[0m — this URL survives restarts; no re-scan needed");
    } else {
        println!("  \x1b[38;5;244mephemeral URL\x1b[0m — set MARS_NGROK_DOMAIN=<your ngrok static domain> to pin it across restarts");
    }
    println!("  bridge live · Ctrl-C to stop");
    println!("  {app_url}");

    for stream in listener.incoming() {
        let stream = match stream {
            Ok(s) => s,
            Err(_) => continue,
        };
        let sock = socket.clone();
        thread::spawn(move || {
            if let Err(e) = handle_conn(stream, &sock) {
                crate::session::debug_log(&format!("[rover] conn ended: {e}"));
            }
        });
    }
    Ok(())
}

/// Read the live public tunnel URL from ngrok's local inspection API (127.0.0.1:4040), so we can
/// reprint the pairing QR for an already-running bridge without standing up a second server.
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
        "a bridge is already running on :{DEFAULT_PORT}, but its tunnel URL couldn't be read from \
         ngrok (http://127.0.0.1:4040). See the bridge's log (`~/.mars/serve-agent.log`)."
    ))?;
    let endpoint = format!("{}/ws", base.replacen("https://", "wss://", 1));
    let token = ensure_token();
    let fp = daemon_fingerprint(session);
    let app_url = format!(
        "https://mars-terminal.lovable.app/rover#h={endpoint}&id={fp}&t={token}&s={session}&v=rover-1"
    );
    print_wordmark();
    println!();
    print_qr(&app_url);
    println!();
    if reset {
        println!("  \x1b[38;5;208mRover\x1b[0m — QR reset · session \x1b[1m{session}\x1b[0m");
        println!("  connected phones drop within ~1s · scan this to reconnect");
    } else {
        println!("  \x1b[38;5;208mRover\x1b[0m — bridge already live · session \x1b[1m{session}\x1b[0m");
        println!("  scan to pair another phone · `mars serve --reset` rotates the QR + disconnects");
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
    write_token(&mint_token());
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
fn start_tunnel(local_port: u16) -> Result<(std::process::Child, String)> {
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
    thread::spawn(move || {
        let reader = BufReader::new(stdout);
        let mut sent = false;
        for line in reader.lines().map_while(|l| l.ok()) {
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
        Ok(url) => Ok((child, url)),
        Err(_) => {
            let _ = child.kill();
            Err(anyhow!("timed out waiting for an ngrok tunnel URL — is your authtoken set? (`ngrok config add-authtoken …`)"))
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

/// Minimal static serving. In production the built Rover bundle is embedded (rust-embed)
/// or served from Lovable; here we serve `$MARS_WEB_DIR/index.html` if set, else a notice.
fn serve_static(mut stream: TcpStream) -> Result<()> {
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
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream.write_all(resp.as_bytes())?;
    stream.flush()?;
    Ok(())
}

/// Bridge one WebSocket to the daemon socket: daemon `Output` → WS `{t:"output"}`, and
/// inbound intents/keystrokes → the pane. Single-threaded WS loop; a dedicated thread
/// pumps the daemon's output through a channel so the loop never blocks on it.
fn bridge_ws(stream: TcpStream, socket: &std::path::Path) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(40))).ok();
    let mut ws = accept(stream).map_err(|e| anyhow!("ws handshake failed: {e}"))?;

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

    // Pairing enforcement: the phone must present the current token (its `{t:"auth"}` frame)
    // before we stream anything, and a rotated token (`mars serve --reset`) is picked up within a
    // second and drops the socket. No token file at all → open (backward-compatible).
    let valid = read_token();
    let mut authed = valid.is_none();
    let auth_deadline = Instant::now() + Duration::from_secs(5);
    let mut last_token_check = Instant::now();

    loop {
        // Reset: if the persisted token was rotated out from under us, drop this phone.
        if valid.is_some() && last_token_check.elapsed() >= Duration::from_secs(1) {
            last_token_check = Instant::now();
            if read_token() != valid {
                break;
            }
        }
        if authed {
            // Flush any daemon output waiting in the channel.
            while let Ok(msg) = rx.try_recv() {
                ws.send(Message::Text(msg))?;
            }
        } else if Instant::now() > auth_deadline {
            break; // never presented a valid token → refuse
        }
        // Read one inbound WS message (times out via the socket read timeout).
        match ws.read() {
            Ok(Message::Text(txt)) => {
                if authed {
                    handle_client_msg(&mut writer, &action_tx, socket, &txt);
                } else {
                    match auth_result(&txt, valid.as_deref()) {
                        AuthCheck::Ok => authed = true,
                        AuthCheck::Bad => break, // wrong token → refuse now
                        AuthCheck::Other => {}   // hello/subscribe before auth → keep waiting
                    }
                }
            }
            Ok(Message::Binary(b)) => {
                if authed {
                    handle_client_msg(&mut writer, &action_tx, socket, &String::from_utf8_lossy(&b));
                }
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
        (Some(a), Some(b)) if a == b => AuthCheck::Ok,
        _ => AuthCheck::Bad,
    }
}

/// The LLM proxy: run the agent on the HOST, with the key that's already in this
/// process's environment (or reached via `mars keyd`). The phone sends only the question;
/// the key never leaves the machine. Mirrors `ask_cli` — blocking. Returns
/// `(answer, provenance, usable)`: `usable == false` means the host has no working key
/// (none set, or its limits/credits are expired) and the phone should use its fallback.
fn run_agent_ask(question: String) -> (String, String, bool) {
    let cfg = crate::agent::AgentConfig::from_env();
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
fn handle_client_msg(writer: &mut impl Write, tx: &mpsc::Sender<String>, socket: &std::path::Path, txt: &str) {
    let v: serde_json::Value = match serde_json::from_str(txt) {
        Ok(v) => v,
        Err(_) => return,
    };
    match v.get("t").and_then(|t| t.as_str()) {
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
        // Open a new terminal tab in the session (additive; surfaces on the next board push).
        Some("new_terminal") => {
            let _ = session::write_frame(writer, &ClientFrame::NewTerminal);
        }
        // Rename the session from the phone — reflected on the host. Carried on a FRESH
        // short-lived connection: the daemon closes the stream that sends Rename, so using
        // our subscribe writer would drop the live board. The daemon does the fs-rename and
        // the next board snapshot carries the new name back.
        Some("rename") => {
            if let Some(name) = v.get("name").and_then(|x| x.as_str()) {
                let name = name.trim();
                if !name.is_empty() {
                    if let Ok(mut conn) = crate::sys::control::connect(socket) {
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

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

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

/// The path seam. v1: expand `~`, then canonicalize (resolving `..`/symlinks against the
/// real fs). A not-yet-existing write target canonicalizes its parent and rejoins the name.
fn resolve_path(raw: &str) -> std::io::Result<PathBuf> {
    let raw = raw.trim();
    let p = if raw.is_empty() || raw == "~" {
        fs_home()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        fs_home().join(rest)
    } else {
        PathBuf::from(raw)
    };
    match p.canonicalize() {
        Ok(c) => Ok(c),
        Err(e) => match (p.parent(), p.file_name()) {
            (Some(parent), Some(name)) => Ok(parent.canonicalize()?.join(name)),
            _ => Err(e),
        },
    }
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
