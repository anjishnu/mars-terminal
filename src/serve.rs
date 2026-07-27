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
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

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

    print_qr(&url);
    println!();
    println!("  \x1b[38;5;208mRover\x1b[0m — scan to pair  ·  session \x1b[1m{session}\x1b[0m");
    println!("  same wifi · link is single-use · run `mars serve` to accept the connection");
    println!("  {url}");
    Ok(())
}

/// Render a QR as dark-on-white cells (two spaces per module, ANSI bg) so it scans
/// regardless of terminal theme, with a white quiet border.
fn print_qr(text: &str) {
    let qr = match qrcodegen::QrCode::encode_text(text, qrcodegen::QrCodeEcc::Medium) {
        Ok(q) => q,
        Err(_) => {
            println!("  (url too long to QR — open the link below)");
            return;
        }
    };
    let n = qr.size();
    let quiet = 2;
    let white = "\x1b[47m  \x1b[0m";
    let black = "\x1b[40m  \x1b[0m";
    let border_row = |out: &mut String| {
        for _ in 0..(n + quiet * 2) {
            out.push_str(white);
        }
    };
    let mut s = String::new();
    for _ in 0..quiet {
        s.push_str("  ");
        border_row(&mut s);
        s.push('\n');
    }
    for y in 0..n {
        s.push_str("  ");
        for _ in 0..quiet {
            s.push_str(white);
        }
        for x in 0..n {
            s.push_str(if qr.get_module(x, y) { black } else { white });
        }
        for _ in 0..quiet {
            s.push_str(white);
        }
        s.push('\n');
    }
    for _ in 0..quiet {
        s.push_str("  ");
        border_row(&mut s);
        s.push('\n');
    }
    print!("{s}");
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
    let listener = TcpListener::bind(("127.0.0.1", port))?;

    let (_tunnel, base) = start_tunnel(port)?;
    let endpoint = format!("{}/ws", base.replacen("https://", "wss://", 1));
    let token = mint_token();
    let fp = daemon_fingerprint(&session);
    let app_url = format!(
        "https://mars-terminal.lovable.app/rover#h={endpoint}&id={fp}&t={token}&s={session}&v=rover-1"
    );

    print_qr(&app_url);
    println!();
    println!("  \x1b[38;5;208mRover\x1b[0m — scan to pair · session \x1b[1m{session}\x1b[0m");
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

/// Start a Cloudflare quick tunnel to the local bridge; returns the child (kept alive for
/// the process lifetime) and the public https base URL. Requires `cloudflared` on PATH.
fn start_tunnel(local_port: u16) -> Result<(std::process::Child, String)> {
    let mut child = Command::new("cloudflared")
        .args(["tunnel", "--url", &format!("http://localhost:{local_port}")])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| {
            anyhow!("cloudflared not found — install it (`brew install cloudflared`), then re-run `mars serve`")
        })?;

    // cloudflared prints the assigned URL to stderr; scan for it in a drain thread (so a
    // full pipe never blocks cloudflared), and hand the URL back over a channel.
    let stderr = child.stderr.take().ok_or_else(|| anyhow!("no tunnel output"))?;
    let (tx, rx) = mpsc::channel::<String>();
    thread::spawn(move || {
        let reader = BufReader::new(stderr);
        let mut sent = false;
        for line in reader.lines().map_while(|l| l.ok()) {
            if !sent {
                if let Some(idx) = line.find("https://") {
                    let url: String = line[idx..].chars().take_while(|c| !c.is_whitespace()).collect();
                    if url.contains("trycloudflare.com") {
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
            Err(anyhow!("timed out waiting for a tunnel URL from cloudflared"))
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
                    // Raw ANSI (the peek rung) — present only on an attach, not a Subscribe.
                    ServerFrame::Output { b64 } => {
                        if tx
                            .send(format!("{{\"t\":\"output\",\"paneId\":\"main\",\"b64\":\"{b64}\"}}"))
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

    loop {
        // Flush any daemon output waiting in the channel.
        while let Ok(msg) = rx.try_recv() {
            ws.send(Message::Text(msg))?;
        }
        // Read one inbound WS message (times out via the socket read timeout).
        match ws.read() {
            Ok(Message::Text(txt)) => handle_client_msg(&mut writer, &action_tx, &txt),
            Ok(Message::Binary(b)) => handle_client_msg(&mut writer, &action_tx, &String::from_utf8_lossy(&b)),
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

/// The LLM proxy: run the agent on the HOST, with the key that's already in this
/// process's environment (or reached via `mars keyd`). The phone sends only the question;
/// the key never leaves the machine. Mirrors `ask_cli` — blocking, returns the answer.
fn run_agent_ask(question: String) -> String {
    let cfg = crate::agent::AgentConfig::from_env();
    if !cfg.is_configured() {
        return "No LLM key on the host — set MARS_LLM_KEY or run `mars keyd`.".to_string();
    }
    let (tx, rx) = mpsc::channel();
    crate::agent::ask(cfg, question, crate::palette::registry_context(), String::new(), Vec::new(), tx);
    loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(crate::agent::AgentEvent::Answer { text, .. }) => return text,
            Ok(_) => continue, // streaming/progress events — wait for the final answer
            Err(_) => return "The agent didn't respond in time.".to_string(),
        }
    }
}

/// Handle an inbound Rover message. `ask`/`summarize` are proxied to the host LLM (async,
/// result pushed back over `tx`); raw keystrokes/paste go to the pane.
fn handle_client_msg(writer: &mut impl Write, tx: &mpsc::Sender<String>, txt: &str) {
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
                let answer = run_agent_ask(q);
                let _ = tx2.send(format!(
                    "{{\"t\":\"summary\",\"id\":{},\"summary\":{{\"text\":{},\"computedBy\":\"host\"}}}}",
                    json_str(&id),
                    json_str(&answer)
                ));
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
                let _ = session::write_frame(writer, &ClientFrame::Paste(data.to_string()));
            }
        }
        // Structured intents (answer/run/jump/summarize…) land here once the daemon has
        // a JSON action sink; until then they're recorded, not silently dropped.
        Some(other) => crate::session::debug_log(&format!("[rover] intent not yet wired: {other}")),
        None => {}
    }
}

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}
