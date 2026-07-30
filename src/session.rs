/// Session daemon: tmux/zellij-style detach/reattach.
///
/// Architecture (recorded in key_design.md §H2): thin client, server renders.
/// The server runs the entire `App` headless and streams ratatui's ANSI bytes
/// over the platform control channel as `Output` frames; the client owns the real TTY,
/// forwards serialized input events, and writes frames verbatim to stdout.
/// One client per session; a new attach takes over. Disconnect leaves the
/// session (buffers, panes, shells, agent threads) running.

use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::time::Duration;

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use crossterm::event::{Event, KeyEvent, MouseEvent};
use ratatui::{
    backend::CrosstermBackend,
    layout::Rect,
    Terminal, TerminalOptions, Viewport,
};
use serde::{Deserialize, Serialize};

use crate::{
    app::{App, InputEvent},
    ui,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");
pub(crate) const SESSION_PROTOCOL_VERSION: &str =
    concat!(env!("CARGO_PKG_VERSION"), "/session-2");
pub const RUNTIME_DIR_ENV: &str = "MARS_RUNTIME_DIR";

// ── Protocol ─────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub enum ClientFrame {
    Hello {
        cols: u16,
        rows: u16,
        version: String,
        #[serde(default)]
        broker_sock: Option<String>,
        #[serde(default)]
        broker_capability: Option<String>,
    },
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    Resize { cols: u16, rows: u16 },
    /// One-shot query: reply with `Status`, then close (used by `mars ls`).
    Status,
    /// Terminate the session daemon (used by `mars kill <name>`).
    Kill,
    /// Rename the session (used by `mars rename <old> <new>`).
    Rename { to: String },
    /// Open a file as a new tab in the running session (used by a nested
    /// `mars <file>` run from a terminal pane inside this session).
    Open { path: String },
    /// Return the daemon's current broker route to a Mars subprocess running
    /// inside one of its persistent terminal panes.
    BrokerRoute,
    /// Read-only board/briefing subscription (the Rover phone bridge). Unlike
    /// `Hello` this NEVER becomes the owning client — the desktop attach and its
    /// `latest_client_gen` are untouched — it only asks the daemon to start
    /// pushing structured `Board`/`Briefing` frames on the tick cadence. A phone
    /// glancing must not kick the person at the keyboard.
    Subscribe,
    /// Pane-targeted raw input from a Rover subscriber (answering a `[y/N]`) — written
    /// straight to that pane's terminal, WITHOUT taking over the session.
    PaneInput { pane: usize, data: String },
    /// Start (`Some`) or stop (`None`) streaming a pane's screen to this subscriber —
    /// the phone watching a terminal (e.g. Claude Code) live. `cols`/`rows` are the
    /// viewer's size: while the session is detached, the watched pane is reflowed to fit.
    WatchPane {
        pane: Option<usize>,
        #[serde(default)]
        cols: Option<u16>,
        #[serde(default)]
        rows: Option<u16>,
        /// Raw-byte streaming (the xterm.js renderer): instead of the ~1 Hz `PaneScreen`
        /// snapshot, seed with the current screen and then stream raw PTY output deltas as
        /// `PaneOutput` frames. Defaults false → the DOM/ANSI snapshot path is unchanged.
        #[serde(default)]
        raw: bool,
    },
    /// The opening line the phone greeted the captain with. The daemon folds it into the next
    /// briefing prompt so the narrative doesn't repeat or contradict it.
    RoverGreeting { text: String },
    /// A raw watcher paging upward: send the last `lines` of that pane's scrollback
    /// (ending with the live screen) so the phone can rebuild its buffer with history.
    PaneHistory { pane: usize, lines: usize },
    /// A window of a pane's transcript, half-open `[from, to)` in line ids. Supersedes
    /// `PaneHistory`: a numbered line means the same thing on both sides of the wire, where
    /// "the last N rows of scrollback" meant something different after every reflow.
    PaneLines { pane: usize, from: u64, to: u64 },
    /// A Rover subscriber asks the daemon to open a NEW terminal tab in this session —
    /// additive (never takes over the desktop client). It appears as a new workspace on the
    /// board and is watchable like any other pane.
    NewTerminal,
}

#[derive(Serialize, Deserialize)]
pub enum ServerFrame {
    /// One rendered frame's ANSI bytes (base64).
    Output { b64: String },
    /// Raw PTY output of a specific watched pane (base64), for a Rover subscriber running the
    /// xterm.js renderer — the seed frame and the subsequent byte deltas. Carries the pane id
    /// so the phone routes it to the right terminal (unlike `Output`, which is the attach TTY).
    PaneOutput { pane: usize, b64: String },
    /// Reply to `PaneHistory`: scrollback + live screen, with how many history rows are
    /// included and how deep the pane's history actually goes (for a "X of Y" readout).
    PaneHistory { pane: usize, b64: String, lines: usize, total: usize },
    /// Reply to `PaneLines`: one base64 blob per line starting at `from`, plus the retained
    /// floor (`first`) and the id the next line will get (`total`) — enough for an honest
    /// scrollbar and an unambiguous "this is the top".
    PaneLines { pane: usize, from: u64, first: u64, total: u64, rows: Vec<String> },
    /// Connection is over (detach, quit, takeover, refusal) — show `message`.
    Exit { message: String },
    /// Reply to `ClientFrame::Status`.
    /// Reply to `ClientFrame::Status`. `instance_id` is minted ONCE per daemon process and never
    /// re-derived from the name, so it is the stable handle for a session across renames — which
    /// the socket path is not, since the socket is named after the session.
    Status {
        attached: bool,
        version: String,
        #[serde(default)]
        instance_id: String,
        #[serde(default)]
        name: String,
    },
    /// Reply to `ClientFrame::BrokerRoute`.
    BrokerRoute {
        session_instance_id: String,
        broker_sock: Option<String>,
        broker_capability: Option<String>,
    },
    /// Pre-serialized workspace-board JSON (the mobile seam's WorkspaceRow[]
    /// plus session + ts). Pushed ONLY to `Subscribe`d clients, so app types
    /// stay out of the protocol and normal attach clients never see it.
    Board { json: String },
    /// Pre-serialized reattach-briefing JSON (the seam's `Briefing`), pushed to
    /// subscribers only while a shift report exists.
    Briefing { json: String },
    /// Pre-serialized `{pane, text}` — the live screen of a watched pane.
    PaneScreen { json: String },
}

pub fn write_frame<T: Serialize>(w: &mut impl Write, frame: &T) -> io::Result<()> {
    let mut line = serde_json::to_string(frame).map_err(io::Error::other)?;
    line.push('\n');
    w.write_all(line.as_bytes())?;
    w.flush()
}

fn send_exit(stream: &crate::sys::control::Stream, message: &str) -> io::Result<()> {
    let mut w = stream.try_clone()?;
    write_frame(&mut w, &ServerFrame::Exit { message: message.to_string() })
}

// ── TTY hygiene ──────────────────────────────────────────────────────────────

/// Repair a TTY left in raw mode by a killed client (SIGKILL can't restore
/// termios, and the next process inherits the mess — `\n` without `\r`,
/// staircase output, no echo). Idempotent; a no-op when stdout isn't a TTY.
/// Also run before entering the TUI, so crossterm saves a *sane* state to
/// restore on exit instead of faithfully re-breaking the terminal.
pub fn sanitize_tty() {
    crate::sys::tty::sanitize();
}

/// On panic, put the terminal back together before the message prints —
/// otherwise the report is unreadable and the shell is left broken.
pub fn install_panic_restore() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        use crossterm::{event, execute, terminal};
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            terminal::LeaveAlternateScreen,
            event::DisableMouseCapture,
            event::DisableBracketedPaste,
            crossterm::cursor::Show
        );
        sanitize_tty();
        default(info);
    }));
}

// ── Socket paths ─────────────────────────────────────────────────────────────

fn socket_dir() -> Result<PathBuf> {
    let base = std::env::var_os(RUNTIME_DIR_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(format!("mars-{}", crate::sys::proc::uid_tag()));
    std::fs::create_dir_all(&dir)?;
    crate::sys::fsperm::restrict_dir(&dir)?;
    Ok(dir)
}

pub fn validate_session_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(anyhow!("session name cannot be empty"));
    }
    if name != name.trim() {
        return Err(anyhow!("session name cannot start or end with whitespace"));
    }
    if matches!(name, "." | "..") {
        return Err(anyhow!("session name cannot be a path component"));
    }
    if name.ends_with('.') {
        return Err(anyhow!("session name cannot end with '.'"));
    }
    if name.chars().any(|c| {
        c <= '\u{1f}' || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
    }) {
        return Err(anyhow!(
            "session name contains a path separator or reserved character"
        ));
    }

    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    let numbered_device = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|n| matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"));
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || numbered_device
    {
        return Err(anyhow!("session name is reserved by Windows"));
    }
    Ok(())
}

pub fn socket_path(name: &str) -> Result<PathBuf> {
    validate_session_name(name)?;
    Ok(socket_dir()?.join(format!("{name}.sock")))
}


/// Ask a live socket who it is: `(name, instance_id, attached)`.
///
/// This is the only way to bind to a session that survives a rename. A socket is named after the
/// session, so `mars rename` moves it; the instance id inside does not move.
pub fn identify(path: &std::path::Path) -> Option<(String, String, bool)> {
    // More patient than `query_attached`'s 500ms: this is called by a bridge re-resolving its
    // target, sometimes moments after a rename, against a daemon that is also serving an attached
    // client and a phone. Giving up on one slow reply would look exactly like "the session is
    // gone" and drop a live connection.
    let stream = crate::sys::control::connect(path).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(2500))).ok()?;
    let mut w = stream.try_clone().ok()?;
    write_frame(&mut w, &ClientFrame::Status).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(stream).read_line(&mut line).ok()?;
    match serde_json::from_str::<ServerFrame>(line.trim()).ok()? {
        ServerFrame::Status { attached, instance_id, name, .. } => {
            let name = if name.is_empty() {
                path.file_stem()?.to_str()?.to_string()
            } else {
                name
            };
            Some((name, instance_id, attached))
        }
        _ => None,
    }
}

/// Find the live session with this instance id, wherever it has been renamed to.
/// Returns `(current_name, socket_path)`.
pub fn socket_for_instance(instance_id: &str) -> Option<(String, PathBuf)> {
    if instance_id.is_empty() {
        return None;
    }
    let dir = socket_dir().ok()?;
    // Two passes: a rename moves the socket file while the daemon keeps serving the same inode,
    // and the first probe can land inside that window.
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(150));
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().and_then(|x| x.to_str()) != Some("sock") {
                continue;
            }
            if let Some((name, id, _)) = identify(&p) {
                if id == instance_id {
                    return Some((name, p));
                }
            }
        }
    }
    None
}

/// The session a bridge should serve when told nothing: the one a client is ATTACHED to.
///
/// Defaulting to "the first session listed" is how a bridge ended up serving a name whose daemon
/// had no socket, then quietly forwarding an empty board forever — a failure that looks exactly
/// like a broken phone app.
pub fn attached_session() -> Option<String> {
    let live: Vec<(String, bool, bool)> = list_sessions().ok()?;
    live.iter()
        .find(|(_, alive, attached)| *alive && *attached)
        .or_else(|| live.iter().find(|(_, alive, _)| *alive))
        .map(|(n, _, _)| n.clone())
}

/// Ask a live session whether a client is currently attached.
fn query_attached(path: &std::path::Path) -> Option<bool> {
    let stream = crate::sys::control::connect(path).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(500))).ok()?;
    let mut w = stream.try_clone().ok()?;
    write_frame(&mut w, &ClientFrame::Status).ok()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    match serde_json::from_str::<ServerFrame>(line.trim()).ok()? {
        ServerFrame::Status { attached, .. } => Some(attached),
        _ => None,
    }
}

#[cfg(feature = "ssh")]
fn query_broker_route_at(
    path: &std::path::Path,
) -> Result<(Option<String>, Option<String>, String)> {
    let stream = crate::sys::control::connect(&path)
        .map_err(|_| anyhow!("parent session control endpoint is unavailable"))?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    let mut w = stream.try_clone()?;
    write_frame(&mut w, &ClientFrame::BrokerRoute)?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    match serde_json::from_str::<ServerFrame>(line.trim())? {
        ServerFrame::BrokerRoute {
            session_instance_id,
            broker_sock,
            broker_capability,
        } => Ok((broker_sock, broker_capability, session_instance_id)),
        ServerFrame::Exit { message } => Err(anyhow!(message)),
        _ => Err(anyhow!("parent session returned an invalid broker route")),
    }
}

#[cfg(feature = "ssh")]
pub(crate) fn query_broker_route(
    name: &str,
    expected_instance_id: Option<&str>,
) -> Result<(Option<String>, Option<String>, String)> {
    let preferred = socket_path(name)?;
    if let Ok(route) = query_broker_route_at(&preferred) {
        if expected_instance_id.is_none_or(|expected| route.2 == expected) {
            return Ok(route);
        }
    }
    let Some(expected) = expected_instance_id else {
        return Err(anyhow!("no live parent session '{name}'"));
    };
    for entry in std::fs::read_dir(socket_dir()?)?.flatten() {
        let path = entry.path();
        if path == preferred || path.extension().and_then(|value| value.to_str()) != Some("sock") {
            continue;
        }
        if let Ok(route) = query_broker_route_at(&path) {
            if route.2 == expected {
                return Ok(route);
            }
        }
    }
    Err(anyhow!("no live parent session instance '{expected}'"))
}

/// Lowest free numeric session name (tmux-style: 0, 1, 2, …).
pub fn next_auto_name() -> Result<String> {
    let taken: std::collections::HashSet<String> =
        list_sessions()?.into_iter().map(|(n, _, _)| n).collect();
    Ok((0..)
        .map(|n| n.to_string())
        .find(|n| !taken.contains(n))
        .unwrap_or_else(|| "0".to_string()))
}

/// (name, alive, attached) for every session socket; stale sockets are removed.
pub fn list_sessions() -> Result<Vec<(String, bool, bool)>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(socket_dir()?)? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sock") {
            continue;
        }
        let name = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
        match query_attached(&path) {
            Some(attached) => out.push((name, true, attached)),
            None => match crate::sys::control::probe(&path) {
                crate::sys::control::Probe::Dead => {
                    let _ = std::fs::remove_file(&path);
                    out.push((name, false, false));
                }
                crate::sys::control::Probe::Live
                | crate::sys::control::Probe::Indeterminate => {
                    out.push((name, true, false));
                }
            },
        }
    }
    out.sort();
    Ok(out)
}

// ── Server ───────────────────────────────────────────────────────────────────

/// Render sink: buffers ratatui's ANSI writes, ships one Output frame per
/// flush (i.e. per drawn frame). IO errors mark the client dead instead of
/// erroring the draw — the reader thread reports the disconnect.
struct FrameWriter {
    stream: crate::sys::control::Stream,
    buf: Vec<u8>,
    dead: bool,
}

impl FrameWriter {
    fn new(stream: crate::sys::control::Stream) -> Self {
        // Don't let one wedged client stall the whole session forever.
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        FrameWriter { stream, buf: Vec::new(), dead: false }
    }
}

impl Write for FrameWriter {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.buf.is_empty() || self.dead {
            self.buf.clear();
            return Ok(());
        }
        let frame = ServerFrame::Output { b64: B64.encode(&self.buf) };
        self.buf.clear();
        if write_frame(&mut self.stream, &frame).is_err() {
            self.dead = true;
        }
        Ok(())
    }
}

enum SrvEvent {
    Attach {
        stream: crate::sys::control::Stream,
        cols: u16,
        rows: u16,
        gen: u64,
        broker_sock: Option<String>,
        broker_capability: Option<String>,
    },
    Input {
        event: InputEvent,
        gen: u64,
    },
    ClientGone(u64),
    /// `mars kill <name>` — force-quit (autosaves first, skips the dirty guard).
    Kill,
    /// `mars rename <old> <new>`.
    Rename(String),
    /// A nested `mars <file>` — open it as a new tab here.
    OpenFile(String),
    /// A read-only mobile subscriber joined: start pushing board/briefing frames
    /// to this stream. Does NOT touch client ownership (non-takeover glance).
    Subscribe { stream: crate::sys::control::Stream },
    /// A subscriber wrote raw input to a pane (the phone answering a prompt).
    PaneInput { pane: usize, data: String },
    /// A subscriber wants to watch (or stop watching) a pane's screen, at their size.
    /// `raw` selects the xterm.js byte-stream path over the ANSI-snapshot path.
    WatchPane { pane: Option<usize>, cols: Option<u16>, rows: Option<u16>, raw: bool },
    /// A subscriber asked to open a new terminal tab (the phone's "New terminal").
    NewTerminal,
    /// A subscriber asked for a pane's scrollback (paging up in the xterm.js renderer).
    PaneHistory { pane: usize, lines: usize },
    /// A subscriber asked for a window of a pane's transcript, by line id.
    PaneLines { pane: usize, from: u64, to: u64 },
    /// The phone reported the greeting it opened with (briefing continuity).
    RoverGreeting(String),
}

/// Push the current structured board (and briefing, when one exists) to every
/// mobile subscriber, dropping any whose socket has closed. Inert with no
/// subscribers — the JSON is never built unless a phone is listening.
fn push_mobile(app: &mut App, subs: &mut Vec<crate::sys::control::Stream>) {
    if subs.is_empty() {
        return;
    }
    // Sample host health so the phone's ambient stats stay live even while the session
    // is detached (the render loop that normally samples isn't running headless). GPU is
    // included — valuable during training; self-trims to nothing when nvidia-smi is absent.
    app.health.maybe_sample(std::path::Path::new("."), true);
    let board = app.mobile_board_json();
    let briefing = app.mobile_briefing_json();
    subs.retain_mut(|s| {
        if write_frame(s, &ServerFrame::Board { json: board.clone() }).is_err() {
            return false;
        }
        if let Some(b) = &briefing {
            if write_frame(s, &ServerFrame::Briefing { json: b.clone() }).is_err() {
                return false;
            }
        }
        true
    });
}

struct BrokerRouteReset;

impl Drop for BrokerRouteReset {
    fn drop(&mut self) {
        crate::broker::reset_session_broker();
    }
}

fn make_terminal(
    stream: crate::sys::control::Stream,
    cols: u16,
    rows: u16,
) -> Result<Terminal<CrosstermBackend<FrameWriter>>> {
    let backend = CrosstermBackend::new(FrameWriter::new(stream));
    // Fixed viewport: the daemon has no TTY to query for a size.
    let term = Terminal::with_options(
        backend,
        TerminalOptions { viewport: Viewport::Fixed(Rect::new(0, 0, cols, rows)) },
    )?;
    Ok(term)
}

/// The daemon: owns the App, keeps running with or without a client.
/// `name`/`path` are mutable: live rename moves the socket file (the bound
/// listener follows the inode, so clients keep connecting — verified).
pub fn server_main(name: &str, file: Option<String>) -> Result<()> {
    crate::broker::reset_session_broker();
    let _broker_route_reset = BrokerRouteReset;
    let mut name = name.to_string();
    let mut path = socket_path(&name)?;
    // Clean a stale socket (previous daemon died without unlinking).
    if path.exists() {
        match crate::sys::control::probe(&path) {
            crate::sys::control::Probe::Dead => {
                let _ = std::fs::remove_file(&path);
            }
            crate::sys::control::Probe::Indeterminate => {
                anyhow::bail!(
                    "session '{name}' has an incompatible or busy control endpoint; \
                     stop its old daemon or run `mars killall`"
                );
            }
            crate::sys::control::Probe::Live => {}
        }
    }
    let listener = crate::sys::control::bind(&path)
        .map_err(|e| anyhow!("cannot create session '{name}': {e} (already running?)"))?;
    let session_instance_id = format!(
        "{:x}-{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    );
    let shared_instance_id: Arc<str> = Arc::from(session_instance_id.as_str());

    let (tx, rx) = mpsc::channel::<SrvEvent>();
    let gen_counter = Arc::new(AtomicU64::new(0));
    // Shared with connection threads so `mars ls` can report attached state.
    let attached = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let tx = tx.clone();
        let gen_counter = gen_counter.clone();
        let attached = attached.clone();
        let session_instance_id = shared_instance_id.clone();
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { continue };
                let tx = tx.clone();
                let gc = gen_counter.clone();
                let at = attached.clone();
                let session_instance_id = session_instance_id.clone();
                std::thread::spawn(move || {
                    client_connection(stream, tx, gc, at, session_instance_id)
                });
            }
        });
    }

    let had_file = file.is_some();
    let mut app = App::new(file)?;
    app.session_name = Some(name.to_string());
    app.session_instance_id = Some(session_instance_id);
    // A no-file session opens straight into a terminal (multiplexer default).
    if !had_file && std::env::var("MARS_OPEN_TERMINAL").is_ok() {
        app.open_terminal();
    }

    let mut client: Option<(crate::sys::control::Stream, u64)> = None;
    let mut term: Option<Terminal<CrosstermBackend<FrameWriter>>> = None;
    let mut latest_client_gen = 0;
    // Read-only mobile subscribers (the Rover phone bridge). Separate from the
    // owning client so a glance never takes over; board/briefing frames are
    // pushed here on a throttled cadence and dead streams are pruned on write.
    let mut subscribers: Vec<crate::sys::control::Stream> = Vec::new();
    let mut last_mobile_push: Option<std::time::Instant> = None;
    // The watched pane pushes on its OWN, much tighter clock, and only when the screen actually
    // changed. Coupling it to the board's ~1 Hz status cadence meant a keystroke could take a
    // full second to come back — the renderer was never the latency, the schedule was.
    let mut last_pane_push: Option<std::time::Instant> = None;
    // The manager repo is refreshed on its own clock, independent of whether a phone is
    // listening: the whole point of an ambient layer is that the cards already exist when
    // somebody finally looks.
    let mut last_manager: Option<std::time::Instant> = None;
    let mut last_pane_json: Option<String> = None;
    let mut watched_pane: Option<usize> = None;
    // Whether the watched pane is streamed as raw PTY bytes (the xterm.js renderer) rather
    // than ~1 Hz ANSI snapshots. Carries the pane's raw tap, so it's cleared when the last
    // phone drops (below) to stop the reader thread from buffering output nobody reads.
    let mut watch_raw = false;
    // Which pane the raw subscriber has already been seeded with (None = needs a seed).
    let mut seeded_pane: Option<usize> = None;

    loop {
        // Only spend LLM tokens on Rover's map/reduce while a phone is glancing in.
        // (Dead streams are pruned on the next push; a one-tick lag is harmless.)
        app.rover_active = !subscribers.is_empty();
        // The last phone dropped while raw-watching → stop capturing its pane's output.
        if subscribers.is_empty() {
            seeded_pane = None;
        }
        if subscribers.is_empty() && watch_raw {
            if let Some(p) = watched_pane {
                app.disable_pane_raw_tap(p);
            }
            watch_raw = false;
            watched_pane = None;
            last_pane_json = None; // next watch must seed, never be suppressed as 'unchanged'
            app.mobile_reflow = None;
        }
        app.tick();
        // Draw only when visible state moved — the frames go to the client over
        // the socket (and thus over SSH), so an idle no-op draw is a wasted packet
        // that contends with the user's own keystrokes.
        if std::mem::take(&mut app.needs_redraw) {
            if let Some(t) = term.as_mut() {
                if let Err(e) = t.draw(|f| ui::render(f, &mut app)) {
                    debug_log(&format!("srv: draw error: {e}"));
                }
                // A copy queues an OSC 52 escape: append it raw after the
                // frame so it reaches the client's real terminal (and through
                // ssh, the clipboard of the machine the user is sitting at).
                if let Some(osc) = app.take_osc() {
                    let w = t.backend_mut(); // CrosstermBackend forwards Write to the FrameWriter
                    let _ = w.write_all(osc.as_bytes());
                    let _ = w.flush();
                }
            }
        }

        match rx.recv_timeout(Duration::from_millis(app.tuning.poll_interval_ms)) {
            Ok(SrvEvent::Attach {
                stream,
                cols,
                rows,
                gen,
                broker_sock,
                broker_capability,
            }) => {
                if gen <= latest_client_gen {
                    let _ = send_exit(&stream, "detached: a newer client already attached");
                    continue;
                }
                if let Err(e) =
                    crate::broker::set_session_broker(broker_sock, broker_capability)
                {
                    let _ = send_exit(&stream, &format!("invalid broker handoff: {e}"));
                    continue;
                }
                latest_client_gen = gen;
                if let Some((old, _)) = client.take() {
                    let _ = send_exit(&old, "detached: another client attached");
                }
                client = Some((stream.try_clone()?, gen));
                term = Some(make_terminal(stream, cols, rows)?);
                attached.store(true, Ordering::SeqCst);
                app.needs_redraw = true; // fresh client → full repaint
                app.on_attach(); // W7: "where was I?" briefing from the detach diff
                if let Some(t) = term.as_mut() {
                    if let Err(e) = t.clear() {
                        debug_log(&format!("srv: clear error: {e}"));
                    }
                }
            }
            Ok(SrvEvent::Input { event, gen }) => {
                if client.as_ref().is_some_and(|(_, current)| *current == gen) {
                    match event {
                        InputEvent::Resize(cols, rows) => {
                            if let Some((s, _)) = client.as_ref() {
                                term = Some(make_terminal(s.try_clone()?, cols, rows)?);
                                if let Some(t) = term.as_mut() {
                                    let _ = t.clear();
                                }
                                app.needs_redraw = true;
                            }
                        }
                        ev => {
                            let visible = ev.forces_redraw();
                            // Real desk interaction → mars is active again; drop Rover's
                            // takeover so the next render sizes panes back to the layout.
                            if visible {
                                app.mobile_reflow = None;
                            }
                            let _ = app.apply_input(ev);
                            if visible {
                                app.needs_redraw = true;
                            }
                        }
                    }
                }
            }
            Ok(SrvEvent::ClientGone(gen)) => {
                if client.as_ref().map(|(_, g)| *g == gen).unwrap_or(false) {
                    client = None;
                    term = None; // keep running headless
                    attached.store(false, Ordering::SeqCst);
                    app.on_detach(); // W7: snapshot for the reattach briefing
                    app.autosave(); // the window may have been closed for good
                }
            }
            Ok(SrvEvent::Kill) => {
                app.autosave();
                app.should_quit = true; // forced: `mars kill` skips the dirty guard
            }
            Ok(SrvEvent::OpenFile(path)) => {
                app.open_file_in_new_tab(&path);
                app.needs_redraw = true;
            }
            Ok(SrvEvent::Rename(to)) => {
                app.rename_session_to = Some(to);
            }
            Ok(SrvEvent::Subscribe { stream }) => {
                subscribers.push(stream);
                // Greet the new subscriber immediately with a full snapshot.
                push_mobile(&mut app, &mut subscribers);
                last_mobile_push = Some(std::time::Instant::now());
            }
            Ok(SrvEvent::PaneInput { pane, data }) => {
                // The phone answered a prompt — write it to that pane's terminal.
                app.write_to_pane(pane, &data);
            }
            Ok(SrvEvent::WatchPane { pane, cols, rows, raw }) => {
                let raw = raw && pane.is_some();
                // Switching panes or leaving raw mode → stop capturing the old pane's output.
                if watch_raw {
                    if let Some(prev) = watched_pane {
                        if Some(prev) != pane || !raw {
                            app.disable_pane_raw_tap(prev);
                        }
                    }
                }
                watched_pane = pane;
                last_pane_json = None; // a new pane (or a reflow) must seed, not be deduped away
                watch_raw = raw;
                if watch_raw {
                    if let Some(p) = pane {
                        app.enable_pane_raw_tap(p);
                    }
                }
                // Rover takes over the pane's size while watching: reflow it to the phone's
                // width even with a desktop attached (the render honours `mobile_reflow`).
                // Mars reclaims it the instant the desk user interacts (SrvEvent::Input).
                match (pane, cols, rows) {
                    (Some(p), Some(c), Some(r)) => {
                        app.mobile_reflow = Some((p, r, c));
                        app.resize_pane_to(p, r, c);
                        app.needs_redraw = true;
                    }
                    _ => app.mobile_reflow = None,
                }
                // Raw mode: SEED xterm.js with the current screen so it starts in sync; byte
                // deltas then stream on each tick below. Seed ONLY when the watched pane
                // changes — a phone re-watches on every grid nudge (keyboard, command bar,
                // rotation, font load) and reseeding each time wipes the client's accumulated
                // scrollback, which is why history kept vanishing mid-session.
                let seed_now = watch_raw && seeded_pane != pane;
                if seed_now {
                    seeded_pane = pane;
                }
                if !watch_raw {
                    seeded_pane = None;
                }
                if seed_now {
                    if let Some(p) = pane {
                        if let Some(seed) = app.pane_raw_seed(p) {
                            let b64 = B64.encode(&seed);
                            subscribers.retain_mut(|s| {
                                write_frame(s, &ServerFrame::PaneOutput { pane: p, b64: b64.clone() }).is_ok()
                            });
                        }
                    }
                }
            }
            Ok(SrvEvent::RoverGreeting(text)) => {
                app.rover_greeting = text;
            }
            Ok(SrvEvent::PaneHistory { pane, lines }) => {
                if let Some((bytes, rows, total)) = app.pane_history(pane, lines) {
                    let b64 = B64.encode(&bytes);
                    subscribers.retain_mut(|s| {
                        write_frame(s, &ServerFrame::PaneHistory { pane, b64: b64.clone(), lines: rows, total }).is_ok()
                    });
                }
            }
            Ok(SrvEvent::PaneLines { pane, from, to }) => {
                if let Some((from, first, total, rows)) = app.pane_lines(pane, from, to) {
                    let rows: Vec<String> = rows.iter().map(|r| B64.encode(r)).collect();
                    subscribers.retain_mut(|s| {
                        write_frame(
                            s,
                            &ServerFrame::PaneLines { pane, from, first, total, rows: rows.clone() },
                        )
                        .is_ok()
                    });
                }
            }
            Ok(SrvEvent::NewTerminal) => {
                // Phone tapped "New terminal": open a terminal in a new tab (additive — the
                // desktop client's ownership is untouched). It surfaces on the next board push.
                app.new_tab();
                app.open_terminal();
                app.needs_redraw = true;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }

        // Refresh ~/.mars/manager. Deliberately fed from `mobile_board_json()` — the exact
        // bytes the phone sees — so the manager can never describe a world Rover does not
        // show. Failures are ignored: a full disk must not take the session down.
        let mgr_secs = app.tuning.manager_snapshot_secs;
        if mgr_secs > 0 {
            let mgr_due = last_manager
                .map(|t| t.elapsed() >= Duration::from_secs(mgr_secs))
                .unwrap_or(true);
            if mgr_due {
                last_manager = Some(std::time::Instant::now());
                let json = app.mobile_board_json();
                let keep = app.tuning.manager_snapshot_keep as usize;
                let origin = std::env::var("MARS_ORIGIN")
                    .ok()
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "local".to_string());
                // No repo path passed: `tick_session` derives it, so this call site cannot send
                // writes somewhere the runtime isolation does not cover.
                let _ = crate::manager::tick_session(
                    &origin,
                    &json,
                    crate::worklog::now_secs(),
                    keep,
                    app.tuning.manager_detail_min_secs,
                );
            }
        }

        // Push the board/briefing to any phone glancing in, on a throttled
        // cadence (the tick loop runs every poll_interval_ms — far too chatty
        // for a LAN push). Inert when nobody's subscribed.
        if !subscribers.is_empty() {
            // Raw byte streaming (the xterm.js renderer): flush the watched pane's output
            // deltas every loop iteration for liveness — NOT gated by the 1 Hz snapshot
            // cadence. xterm.js owns the grid client-side, so no server snapshot is sent.
            if watch_raw {
                if let Some(p) = watched_pane {
                    match app.take_pane_raw_delta(p) {
                        Some(bytes) if !bytes.is_empty() => {
                            let b64 = B64.encode(&bytes);
                            subscribers.retain_mut(|s| {
                                write_frame(s, &ServerFrame::PaneOutput { pane: p, b64: b64.clone() }).is_ok()
                            });
                        }
                        _ => {}
                    }
                }
            }
            let due = last_mobile_push
                .map(|t| {
                    t.elapsed()
                        >= Duration::from_millis(app.tuning.mobile_push_interval_ms.max(1))
                })
                .unwrap_or(true);
            if due {
                push_mobile(&mut app, &mut subscribers);
                last_mobile_push = Some(std::time::Instant::now());
            }
            // The watched pane's live screen, on its own clock. Only the DOM/ANSI renderer needs
            // this snapshot; the raw path streams bytes above instead. An unchanged screen is
            // never resent, so an idle pane costs one string compare per tick rather than a
            // frame on the wire and a full re-render on the phone.
            let pane_due = last_pane_push
                .map(|t| {
                    t.elapsed()
                        >= Duration::from_millis(app.tuning.mobile_pane_interval_ms.max(1))
                })
                .unwrap_or(true);
            if pane_due && !watch_raw {
                if let Some(p) = watched_pane {
                    if let Some(json) = app.pane_screen_json(p) {
                        if last_pane_json.as_deref() != Some(json.as_str()) {
                            last_pane_json = Some(json.clone());
                            subscribers.retain_mut(|s| {
                                write_frame(s, &ServerFrame::PaneScreen { json: json.clone() }).is_ok()
                            });
                        }
                    }
                }
                last_pane_push = Some(std::time::Instant::now());
            }
        }

        // Live rename (from the editor's RenameSession action or `mars rename`).
        if let Some(to) = app.rename_session_to.take() {
            match socket_path(&to) {
                Ok(new_path) if new_path != path => {
                    if new_path.exists() {
                        app.status_msg = Some(format!("session '{to}' already exists"));
                    } else if std::fs::rename(&path, &new_path).is_ok() {
                        path = new_path;
                        name = to.clone();
                        app.session_name = Some(to);
                        app.status_msg = Some(format!("session renamed to '{name}'"));
                    } else {
                        app.status_msg = Some("session rename failed".into());
                    }
                }
                Ok(_) => {}
                Err(e) => app.status_msg = Some(format!("bad session name: {e}")),
            }
        }

        if app.detach_requested {
            app.detach_requested = false;
            // Snapshot for the reattach shift report BEFORE dropping the client —
            // the intended "quit = detach" path (C-x C-c) must arm the save-state
            // restore exactly like an accidental disconnect (ClientGone) does.
            app.on_detach();
            if let Some((s, _)) = client.take() {
                let _ = send_exit(&s, &format!("detached — reattach with: mars --resume {name}"));
            }
            term = None;
            attached.store(false, Ordering::SeqCst);
            app.autosave();
        }
        if app.should_quit {
            if let Some((s, _)) = client.take() {
                let _ = send_exit(&s, "session ended");
            }
            break;
        }
    }

    app.save_state_now();
    let _ = std::fs::remove_file(&path);
    Ok(())
}

pub fn debug_log(msg: &str) {
    if let Ok(path) = std::env::var("MARS_DEBUG_LOG").or_else(|_| std::env::var("ARES_DEBUG_LOG")) {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0);
            let _ = writeln!(f, "[{ms}] {msg}");
        }
    }
}

/// Per-connection thread: handshake, then pump client frames into the server.
fn client_connection(
    stream: crate::sys::control::Stream,
    tx: mpsc::Sender<SrvEvent>,
    gc: Arc<AtomicU64>,
    attached: Arc<std::sync::atomic::AtomicBool>,
    session_instance_id: Arc<str>,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let Ok(read_half) = stream.try_clone() else { return };
    let mut reader = BufReader::new(read_half);

    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) => return, // liveness ping or dead peer — not a real client
        Err(e) => { debug_log(&format!("hello: read err {e}")); return; }
        Ok(_) => {}
    }
    let first = serde_json::from_str::<ClientFrame>(line.trim());
    match &first {
        // One-shot management frames: answer and hang up.
        Ok(ClientFrame::Status) => {
            if let Ok(mut w) = stream.try_clone() {
                let _ = write_frame(&mut w, &ServerFrame::Status {
                    attached: attached.load(Ordering::SeqCst),
                    version: VERSION.to_string(),
                    instance_id: session_instance_id.to_string(),
                    // Left empty deliberately: the socket FILENAME is the current name (a rename
                    // moves the socket), so `identify()` reads it from the path and this cannot
                    // go stale behind a rename.
                    name: String::new(),
                });
            }
            return;
        }
        Ok(ClientFrame::Kill) => {
            let _ = tx.send(SrvEvent::Kill);
            let _ = send_exit(&stream, "killed");
            return;
        }
        Ok(ClientFrame::Rename { to }) => {
            let _ = tx.send(SrvEvent::Rename(to.clone()));
            let _ = send_exit(&stream, &format!("rename to '{to}' requested"));
            return;
        }
        Ok(ClientFrame::Open { path }) => {
            let _ = tx.send(SrvEvent::OpenFile(path.clone()));
            let _ = send_exit(&stream, &format!("opening '{path}'"));
            return;
        }
        Ok(ClientFrame::Subscribe) => {
            // Non-takeover read channel: register for board/briefing pushes
            // WITHOUT sending an Attach, so the owning desktop client keeps the
            // session. Modeled on the Status side-channel above.
            let _ = stream.set_read_timeout(None);
            let _ = reader.get_ref().set_read_timeout(None);
            let Ok(push_stream) = stream.try_clone() else { return };
            let _ = push_stream.set_write_timeout(Some(Duration::from_secs(2)));
            if tx.send(SrvEvent::Subscribe { stream: push_stream }).is_err() {
                return;
            }
            // Keep this connection alive so the server's push clone stays open;
            // drain and ignore any inbound frames. Action handling (answer /
            // summarize / jump / restart / run) is the serve.rs bridge's job —
            // TODO(serve.rs): translate ClientAction frames to daemon ops here
            // or in the bridge. Return on EOF; the server prunes the dead push
            // stream on its next write.
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                // What a non-takeover subscriber may send: pane-targeted input (answering
                // a prompt) and watch/unwatch a pane's screen. Everything else is ignored.
                match serde_json::from_str::<ClientFrame>(line.trim()) {
                    Ok(ClientFrame::PaneInput { pane, data }) => {
                        if tx.send(SrvEvent::PaneInput { pane, data }).is_err() {
                            break;
                        }
                    }
                    Ok(ClientFrame::WatchPane { pane, cols, rows, raw }) => {
                        if tx.send(SrvEvent::WatchPane { pane, cols, rows, raw }).is_err() {
                            break;
                        }
                    }
                    Ok(ClientFrame::NewTerminal) => {
                        if tx.send(SrvEvent::NewTerminal).is_err() {
                            break;
                        }
                    }
                    Ok(ClientFrame::PaneHistory { pane, lines }) => {
                        if tx.send(SrvEvent::PaneHistory { pane, lines }).is_err() {
                            break;
                        }
                    }
                    Ok(ClientFrame::PaneLines { pane, from, to }) => {
                        if tx.send(SrvEvent::PaneLines { pane, from, to }).is_err() {
                            break;
                        }
                    }
                    Ok(ClientFrame::RoverGreeting { text }) => {
                        if tx.send(SrvEvent::RoverGreeting(text)).is_err() {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            return;
        }
        Ok(ClientFrame::BrokerRoute) => {
            let mut w = stream;
            match crate::broker::current_session_broker_route() {
                Ok((broker_sock, broker_capability)) => {
                    let _ = write_frame(
                        &mut w,
                        &ServerFrame::BrokerRoute {
                            session_instance_id: session_instance_id.to_string(),
                            broker_sock,
                            broker_capability,
                        },
                    );
                }
                Err(e) => {
                    let _ = write_frame(
                        &mut w,
                        &ServerFrame::Exit {
                            message: e.to_string(),
                        },
                    );
                }
            }
            return;
        }
        _ => {}
    }
    let Ok(ClientFrame::Hello {
        cols,
        rows,
        version,
        broker_sock,
        broker_capability,
    }) = first else {
        debug_log(&format!("hello parse failed: {:?}", first.err()));
        return;
    };
    if version != SESSION_PROTOCOL_VERSION {
        let _ = send_exit(
            &stream,
            &format!(
                "version mismatch: server session protocol {SESSION_PROTOCOL_VERSION}, \
                 client {version} — restart the session or upgrade Mars"
            ),
        );
        return;
    }
    let _ = stream.set_read_timeout(None);
    // A Windows TcpStream clone retains the Hello deadline on its own handle.
    let _ = reader.get_ref().set_read_timeout(None);

    let gen = gc.fetch_add(1, Ordering::SeqCst) + 1;
    let Ok(attach_stream) = stream.try_clone() else { return };
    if tx.send(SrvEvent::Attach {
        stream: attach_stream,
        cols,
        rows,
        gen,
        broker_sock,
        broker_capability,
    }).is_err() {
        return;
    }

    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break, // client disconnected — normal detach/close
            Err(e) => { debug_log(&format!("conn: read err {e}")); break; }
            Ok(_) => {
                let parsed = serde_json::from_str::<ClientFrame>(line.trim());
                let ev = match &parsed {
                    Ok(ClientFrame::Key(k)) => Some(InputEvent::Key(*k)),
                    Ok(ClientFrame::Mouse(m)) => Some(InputEvent::Mouse(*m)),
                    Ok(ClientFrame::Paste(s)) => Some(InputEvent::Paste(s.clone())),
                    Ok(ClientFrame::Resize { cols, rows }) => Some(InputEvent::Resize(*cols, *rows)),
                    _ => {
                        debug_log(&format!("conn: parse failed on {:?}: {:?}", line.trim(), parsed.err()));
                        None
                    }
                };
                if let Some(ev) = ev {
                    if tx.send(SrvEvent::Input { event: ev, gen }).is_err() {
                        break; // server loop gone
                    }
                }
            }
        }
    }
    let _ = tx.send(SrvEvent::ClientGone(gen));
}

// ── Client ───────────────────────────────────────────────────────────────────

pub(crate) fn client_exit_is_error(message: &str) -> bool {
    message.starts_with("version mismatch:") || message.starts_with("invalid broker handoff:")
}

/// Attach the real TTY to a running session.
pub fn client_main(name: &str) -> Result<()> {
    use crossterm::{
        event::{
            DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
            KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
        },
        execute,
        terminal::{
            disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement,
            EnterAlternateScreen, LeaveAlternateScreen,
        },
    };

    let path = socket_path(name)?;
    let stream = crate::sys::control::connect(&path)
        .map_err(|_| anyhow!("no live session '{name}' — see: mars ls"))?;
    let mut writer = stream.try_clone()?;
    let (cols, rows) = crossterm::terminal::size()?;
    let broker_sock = crate::broker::detect_broker_sock();
    let broker_capability = broker_sock
        .as_deref()
        .and_then(crate::broker::broker_capability_for);
    write_frame(
        &mut writer,
        &ClientFrame::Hello {
            cols,
            rows,
            version: SESSION_PROTOCOL_VERSION.to_string(),
            broker_sock,
            broker_capability,
        },
    )?;

    install_panic_restore();
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }

    // Server-frame pump: Output → stdout verbatim; Exit → done.
    let (done_tx, done_rx) = mpsc::channel::<(String, bool)>();
    {
        let read_half = stream.try_clone()?;
        std::thread::spawn(move || {
            let mut reader = BufReader::new(read_half);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => {
                        let _ = done_tx.send(("connection lost".into(), true));
                        break;
                    }
                    Ok(_) => match serde_json::from_str::<ServerFrame>(line.trim()) {
                        Ok(ServerFrame::Output { b64 }) => {
                            if let Ok(bytes) = B64.decode(b64) {
                                let mut so = io::stdout().lock();
                                let _ = so.write_all(&bytes);
                                let _ = so.flush();
                            }
                        }
                        Ok(ServerFrame::Exit { message }) => {
                            let failed = client_exit_is_error(&message);
                            let _ = done_tx.send((message, failed));
                            break;
                        }
                        Ok(ServerFrame::Status { .. }) => {} // not expected mid-attach
                        Ok(ServerFrame::BrokerRoute { .. }) => {} // not expected mid-attach
                        Ok(ServerFrame::Board { .. }) => {} // subscriber-only, never on an attach
                        Ok(ServerFrame::Briefing { .. }) => {} // subscriber-only, never on an attach
                        Ok(ServerFrame::PaneScreen { .. }) => {} // subscriber-only, never on an attach
                        Ok(ServerFrame::PaneOutput { .. }) => {} // subscriber-only, never on an attach
                        Ok(ServerFrame::PaneHistory { .. }) => {} // subscriber-only, never on an attach
                        Ok(ServerFrame::PaneLines { .. }) => {} // subscriber-only, never on an attach
                        Err(_) => {}
                    },
                }
            }
        });
    }

    // Input pump: TTY events → frames.
    let (exit_msg, exit_error);
    loop {
        if let Ok((msg, failed)) = done_rx.try_recv() {
            exit_msg = msg;
            exit_error = failed;
            break;
        }
        if crossterm::event::poll(Duration::from_millis(50))? {
            let frame = match crossterm::event::read()? {
                Event::Key(k) => Some(ClientFrame::Key(k)),
                Event::Mouse(m) => Some(ClientFrame::Mouse(m)),
                Event::Paste(s) => Some(ClientFrame::Paste(s)),
                Event::Resize(c, r) => Some(ClientFrame::Resize { cols: c, rows: r }),
                _ => None,
            };
            if let Some(f) = frame {
                if write_frame(&mut writer, &f).is_err() {
                    exit_msg = "connection lost".into();
                    exit_error = true;
                    break;
                }
            }
        }
    }

    disable_raw_mode()?;
    if enhanced {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        io::stdout(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
        crossterm::cursor::Show
    );
    if exit_error {
        return Err(anyhow!(exit_msg));
    }
    println!("[mars] {exit_msg}");
    Ok(())
}

// ── CLI entries ──────────────────────────────────────────────────────────────

pub(crate) fn isolate_session_daemon_env(command: &mut std::process::Command) {
    for name in [
        "MARS_SESSION",
        "MARS_SESSION_ID",
        "MARS_AUTH_SOCK",
        "MARS_BROKER_CAPABILITY",
    ] {
        command.env_remove(name);
    }
}

/// `~/.local/state/mars` (or $XDG_STATE_HOME/mars) — daemon logs live here.
fn state_dir() -> Option<PathBuf> {
    let base = std::env::var("XDG_STATE_HOME").map(PathBuf::from).ok().or_else(|| {
        crate::sys::paths::home_dir().map(|h| h.join(".local").join("state"))
    })?;
    let dir = base.join("mars");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// `mars --session <name>`: attach if alive, else spawn the daemon and attach.
pub fn session_main(name: &str, file: Option<String>) -> Result<()> {
    crate::broker::ensure_broker(); // auto-start the key broker so every session reaches the LLM
    let path = socket_path(name)?;
    match crate::sys::control::probe(&path) {
        crate::sys::control::Probe::Indeterminate => {
            anyhow::bail!(
                "session '{name}' has an incompatible or busy control endpoint; \
                 stop its old daemon or run `mars killall`"
            );
        }
        crate::sys::control::Probe::Dead => {
            let _ = std::fs::remove_file(&path);
            let exe = std::env::current_exe()?;
            let mut cmd = std::process::Command::new(exe);
            isolate_session_daemon_env(&mut cmd);
            cmd.arg("--server").arg(name);
            if let Some(f) = &file {
                cmd.arg(f);
            }
            // Daemon output goes to a log file — a crashed session must leave a
            // postmortem, not vanish into /dev/null.
            let log = state_dir()
                .map(|d| d.join(format!("{name}.log")))
                .and_then(|p| {
                    std::fs::OpenOptions::new().create(true).append(true).open(p).ok()
                });
            cmd.env("RUST_BACKTRACE", "1");
            // A no-file session opens straight into a terminal pane.
            if file.is_none() {
                cmd.env("MARS_OPEN_TERMINAL", "1");
            }
            cmd.stdin(std::process::Stdio::null());
            match log {
                Some(f) => {
                    let f2 = f.try_clone().ok();
                    cmd.stdout(f);
                    match f2 {
                        Some(f2) => { cmd.stderr(f2); }
                        None => { cmd.stderr(std::process::Stdio::null()); }
                    }
                }
                None => {
                    cmd.stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null());
                }
            }
            // Fully detach from this TTY so the daemon survives the window.
            crate::sys::daemon::detach(&mut cmd);
            cmd.spawn()?;
            // Wait for the daemon's socket to come up.
            let mut ok = false;
            for _ in 0..60 {
                std::thread::sleep(Duration::from_millis(50));
                if crate::sys::control::probe(&path) == crate::sys::control::Probe::Live {
                    ok = true;
                    break;
                }
            }
            if !ok {
                return Err(anyhow!("session daemon for '{name}' did not start"));
            }
        }
        crate::sys::control::Probe::Live => {}
    }
    client_main(name)
}

/// `mars attach [name]` / `--resume`: reattach (most recent if unnamed).
pub fn resume_main(name: Option<String>) -> Result<()> {
    if let Some(n) = name {
        return client_main(&n);
    }
    let alive: Vec<String> = list_sessions()?
        .into_iter()
        .filter(|(_, a, _)| *a)
        .map(|(n, _, _)| n)
        .collect();
    match alive.len() {
        0 => Err(anyhow!("no running sessions — start one with: mars new <name>")),
        1 => client_main(&alive[0]),
        _ => {
            // Most recently touched socket wins.
            let mut best: Option<(std::time::SystemTime, String)> = None;
            for n in &alive {
                let mtime = std::fs::metadata(socket_path(n)?)?.modified()?;
                if best.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                    best = Some((mtime, n.clone()));
                }
            }
            client_main(&best.unwrap().1)
        }
    }
}

/// One row of `mars ls`: local daemon sessions and remote fleet hosts behind a
/// single shape, so rendering, ordinals, and the follow-up resolver are one
/// code path and the freshest known status flows through the same field for
/// both — a live probe for locals, the broker status push for remotes.
pub struct SessionEntry {
    /// Session name (local) or host name (remote).
    pub name: String,
    pub remote: bool,
    pub status: String,
    /// LLM-derived gloss of what the session is FOR (inferred mission, else the
    /// last work-journal verdict) — kept apart from `status` so liveness stays
    /// scannable and the prose gets its own column at the end of the table.
    pub summary: String,
    /// When the status was observed: `None` = right now (live local probe);
    /// `Some(ts)` = the last time the remote self-reported.
    pub as_of: Option<u64>,
    /// The command that gets you there (`mars attach x` / `mars ssh h`).
    pub connect: String,
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Lifecycle noise that says nothing about the work: an interactive shell being
/// closed, a bare exit. These flooded the summary with "user quit" before the
/// auto-watch noise gate; filter them here too so the journal's legacy lines
/// (and any manual-watch lifecycle verdicts) never become the headline.
fn is_lifecycle_noise(verdict: &str) -> bool {
    let l = verdict.to_lowercase();
    [
        "user exited", "user quit", "shell exited", "shell closed", "user left",
        "terminal session closed", "exit command", "idle at prompt",
        "exited voluntarily", "exited terminal",
    ]
    .iter()
    .any(|m| l.contains(m))
}

/// Keep a verdict to its headline: model verdicts can ramble across clauses
/// ("done: shipped X; also touched Y; and auto-…"), which reads as noise in a
/// narrow column. Take the first clause and a sane width. Paths keep their dots
/// (we never cut on '.').
fn trim_verdict(v: &str) -> String {
    let head = v.split([';', '\n']).next().unwrap_or(v).trim();
    clip(head, 72)
}

/// What the session is FOR / what it needs — the useful glance, not a vague or
/// STALE distillation. Priority, all from cheap on-disk signals: (1) a recent
/// failure/block that needs you, (2) the goals captured at the last detach —
/// the concrete intent, (3) a recent inferred mission, (4) the freshest real
/// event — all age-gated, so a days-old line never masquerades as current.
/// (5) When a fresh summary is being generated right now, say "…summarizing…"
/// rather than surface something stale. (6) A deterministic floor so a live
/// session is never blank. Lifecycle noise and rambling verdicts never win.
pub fn session_summary(name: &str) -> String {
    // Anything older than this isn't "what's happening now"; it ages out of the
    // headline tiers and the floor (dir · cmd · ago) carries the honest staleness.
    const FRESH_SECS: u64 = 3 * 86_400;
    // Show "…summarizing…" for at most this long after a detach fires the capture
    // call — if the model never lands, the placeholder gives way to the floor.
    const SUMMARIZING_TTL: u64 = 300;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let fresh = |ts: u64| now.saturating_sub(ts) < FRESH_SECS;
    let recent = crate::worklog::recent(name, 12);
    let meaningful = recent.iter().rev().find(|e| !is_lifecycle_noise(&e.verdict));
    // 1. A RECENT failure/block that needs you leads — the reason you'd scan the list.
    if let Some(e) = meaningful {
        let low = e.verdict.to_lowercase();
        if fresh(e.ts) && (e.failed || low.starts_with("blocked") || low.contains("failed")) {
            return format!("{} · {}", trim_verdict(&e.verdict), crate::worklog::ago(e.ts));
        }
    }
    // 2. The goals captured at the last detach — the clearest "what is this session
    //    for" — while still fresh. All of them, one per line (the renderer wraps
    //    each wide), so the ls table lays them out as a block, not a "+N more" tease.
    let goals = crate::worklog::load_goals(name);
    if !goals.is_empty() && crate::worklog::goals_as_of(name).map(fresh).unwrap_or(false) {
        return goals.iter().map(|g| format!("→ {}", clip(g, 72))).collect::<Vec<_>>().join("\n");
    }
    // 3. A recent inferred mission — age-gated so a days-old vague line doesn't
    //    masquerade as current state (the "basically useless" complaint).
    if let Some((mission, as_of)) = crate::worklog::load_mission(name) {
        if fresh(as_of) {
            return clip(&mission, 160);
        }
    }
    // 4. The freshest real event (a completed run, etc.) — while fresh.
    if let Some(e) = meaningful {
        if fresh(e.ts) {
            return format!("{} · {}", trim_verdict(&e.verdict), crate::worklog::ago(e.ts));
        }
    }
    // 5. A fresh summary is being generated right now (the detach fired the LLM
    //    call) — say so, rather than surface something stale, until it lands.
    if let Some(ts) = crate::worklog::summarizing_since(name) {
        if now.saturating_sub(ts) < SUMMARIZING_TTL {
            return "…summarizing…".to_string();
        }
    }
    // 6. Floor — a live session is NEVER blank, even with no model summary and
    //    only lifecycle noise in the journal. The freshest line of any kind still
    //    says where and when: the working directory and how long ago. This is the
    //    deterministic guarantee — it does not depend on any LLM call landing.
    if let Some(e) = recent.last() {
        let dir = std::path::Path::new(&e.cwd)
            .file_name()
            .and_then(|s| s.to_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("session");
        let what = e.command.as_deref().map(|c| clip(c, 48)).unwrap_or_else(|| "active".into());
        return format!("{dir} · {what} · {}", crate::worklog::ago(e.ts));
    }
    "active — nothing logged yet".to_string()
}

/// Greedy word-wrap to `width` columns; words longer than a line are
/// hard-split rather than overflowing. Empty input → no lines.
pub fn wrap_text(s: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut len = 0;
    for word in s.split_whitespace() {
        let chars: Vec<char> = word.chars().collect();
        for piece in chars.chunks(width) {
            if len > 0 && len + 1 + piece.len() > width {
                lines.push(std::mem::take(&mut cur));
                len = 0;
            }
            if len > 0 {
                cur.push(' ');
                len += 1;
            }
            cur.extend(piece);
            len += piece.len();
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// Everything `mars ls` knows about, locals first. The single access path for
/// both kinds — callers never touch `list_sessions`/`fleet_load` shapes.
pub fn all_sessions() -> Result<Vec<SessionEntry>> {
    let mut out = Vec::new();
    for (name, alive, attached) in list_sessions()? {
        let status = match (alive, attached) {
            (true, true) => "attached",
            (true, false) => "detached",
            (false, _) => "dead (cleaned up)",
        }
        .to_string();
        let summary = if alive { session_summary(&name) } else { String::new() };
        out.push(SessionEntry {
            connect: format!("mars attach {name}"),
            name,
            remote: false,
            status,
            summary,
            as_of: None,
        });
    }
    for e in crate::fleet::fleet_load() {
        let mut status = e.last_status.clone().unwrap_or_else(|| "seen".to_string());
        if let Some(s) = &e.session {
            status = format!("{status} · session {s}");
        }
        out.push(SessionEntry {
            connect: format!("mars ssh {}", e.host),
            name: e.host,
            remote: true,
            status,
            summary: String::new(),
            as_of: Some(e.as_of),
        });
    }
    Ok(out)
}

/// `mars ls` — one numbered table over local and remote alike; the follow-up
/// prompt resolves an ordinal/name to `attach` or `ssh` through the same list.
pub fn list_main(prompt: bool) -> Result<()> {
    let entries = all_sessions()?;
    if entries.is_empty() {
        println!("no sessions — start one with: mars new <name>, or reach a box with: mars ssh <host>");
        return Ok(());
    }
    println!(
        "  #  {:<18} {:<6} {:<18} {:<8} {}",
        "SESSION", "WHERE", "STATUS", "AS OF", "SUMMARY"
    );
    // Keep the columns tight so the summary gets real width. A summary that fits
    // sits inline; a longer one goes on its own full-width indented lines rather
    // than wrapping into a thin ragged column jammed against the screen edge.
    let cols = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(100).max(48);
    for (i, e) in entries.iter().enumerate() {
        let seen = match e.as_of {
            None => "now".to_string(),
            Some(t) => crate::worklog::ago(t),
        };
        let prefix = format!(
            "  {:<2} {:<18} {:<6} {:<18} {:<8} ",
            i + 1,
            clip(&e.name, 18),
            if e.remote { "remote" } else { "local" },
            clip(&e.status, 18),
            seen
        );
        let indent = prefix.chars().count();
        let first_width = cols.saturating_sub(indent);
        let one_line = !e.summary.contains('\n');
        if e.summary.is_empty() {
            println!("{}", prefix.trim_end());
        } else if one_line && e.summary.chars().count() <= first_width {
            println!("{prefix}{}", e.summary);
        } else {
            // Multi-line (a goal list) or too long for the row — give each line
            // the full width on its own indented line(s).
            println!("{}", prefix.trim_end());
            for seg in e.summary.split('\n') {
                for l in wrap_text(seg, cols.saturating_sub(6)) {
                    println!("      {l}");
                }
            }
        }
    }

    // Interactive follow-up: an ordinal or (prefix of a) name attaches a local
    // session or sshes to a remote host — same resolver over the same list.
    // Skipped by --no-prompt or when stdin isn't a TTY (scripts).
    let is_tty = crate::sys::tty::is_stdin_tty();
    if prompt && is_tty {
        use std::io::Write;
        print!("\n→ open (number/name, Enter to skip): ");
        io::stdout().flush().ok();
        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_ok() {
            let names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
            if let Some(name) = crate::fleet::resolve_target(&names, &line) {
                let e = entries.iter().find(|e| e.name == name).unwrap();
                return if e.remote {
                    crate::broker::ssh_main(e.name.clone(), Vec::new())
                } else {
                    client_main(&e.name)
                };
            }
        }
    }
    Ok(())
}

/// Open a file as a new tab in a running session (nested `mars <file>`).
/// Relative paths resolve against the caller's cwd (the shell's), so the file
/// opens correctly even though the daemon has a different working directory.
pub fn open_in_session(name: &str, path: &str) -> Result<()> {
    let sock = socket_path(name)?;
    let stream = crate::sys::control::connect(&sock)
        .map_err(|_| anyhow!("session '{name}' is not running"))?;
    let p = std::path::Path::new(path);
    let abs = if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()?.join(p)
    };
    let mut w = stream.try_clone()?;
    write_frame(&mut w, &ClientFrame::Open { path: abs.to_string_lossy().to_string() })?;
    Ok(())
}

/// `mars rename <old> <new>`: rename a running session from outside.
pub fn rename_main(old: &str, new: &str) -> Result<()> {
    let new_path = socket_path(new)?; // validates the name
    if new_path.exists() {
        return Err(anyhow!("session '{new}' already exists"));
    }
    let old_path = socket_path(old)?;
    let stream = crate::sys::control::connect(&old_path)
        .map_err(|_| anyhow!("no live session '{old}' — see: mars ls"))?;
    let mut w = stream.try_clone()?;
    write_frame(&mut w, &ClientFrame::Rename { to: new.to_string() })?;
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(50));
        if new_path.exists() && !old_path.exists() {
            println!("session '{old}' renamed to '{new}'");
            return Ok(());
        }
    }
    Err(anyhow!("rename did not complete — see: mars ls"))
}

/// `mars killall`: the reset button. End EVERY live session daemon (each
/// autosaves first), and with `force` (the CLI path) also put down anything
/// that didn't answer its socket, shut down lingering ssh ControlMasters and
/// the key broker, and sweep the stale sockets they leave behind. Agentic
/// memory (cmd_memory, worklog, mission, denylist, fleet) is untouched, and
/// no new session is started. `force: false` is for the selfcheck, whose
/// runtime-dir isolation a process-wide kill sweep would not respect.
pub fn killall_main(force: bool) -> Result<()> {
    let mut ended = 0;
    for (name, alive, _) in list_sessions()? {
        if alive {
            let _ = kill_main(&name); // graceful: autosave, then exit
            ended += 1;
        }
    }
    if !force {
        if ended == 0 {
            println!("no live sessions to kill");
        }
        return Ok(());
    }
    // Anything still standing didn't answer its socket — put it down hard.
    // Windows intentionally treats this reset command as permission to stop
    // every other mars.exe; Unix retains its targeted daemon sweep.
    crate::sys::proc::kill_all_mars();
    // The capability-marked reverse forward uniquely identifies a Windows
    // handoff. Its ssh.exe child can outlive a force-killed Mars parent.
    crate::sys::proc::kill_matching("ssh -R /tmp/mars-auth-cap-");
    // Shut down Unix ControlMasters cleanly, then sweep their socket files —
    // a leftover master ambushes the next `mars ssh` with a broken pipe.
    #[cfg(feature = "ssh")]
    if let Some(dir) = crate::broker::broker_socket_path().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                if !e.file_name().to_string_lossy().starts_with("cm-") {
                    continue;
                }
                let _ = crate::ssh::ssh_command()
                    .arg("-O").arg("exit")
                    .arg("-o").arg(format!("ControlPath={}", e.path().display()))
                    .arg("killall-sweep")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status();
                let _ = std::fs::remove_file(e.path());
            }
        }
        let _ = std::fs::remove_file(dir.join("auth.sock")); // keyd is down
    }
    // Dead forwarded sockets in /tmp (this box may itself be someone's remote).
    let _ = crate::broker::find_live_auth_sock(std::path::Path::new("/tmp")); // probe = sweep dead ones
    // Leftover session sockets of force-killed daemons.
    if let Ok(entries) = std::fs::read_dir(socket_dir()?) {
        for e in entries.flatten() {
            if e.path().extension().and_then(|x| x.to_str()) == Some("sock") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    // The deliberate off-switch for the Rover bridge. Order matters: drop the enable flag
    // FIRST so the launchd agent won't relaunch, THEN stop the running bridge — the reverse
    // races supervision back up. Dropping the flag alone only stops *relaunching*; nothing
    // signals the live process, which is why a bridge could outlive repeated killalls.
    if let Some(home) = crate::sys::paths::home_dir() {
        let _ = std::fs::remove_file(home.join(".mars").join("serve.enabled"));
    }
    crate::sys::proc::kill_matching("mars serve");
    println!(
        "killall: {ended} session(s) ended gracefully; force-swept Mars processes, \
         the Rover bridge, ssh masters, and stale sockets. Memory files untouched."
    );
    Ok(())
}

/// `mars kill <name>`: terminate a session daemon (autosaves, then exits).
pub fn kill_main(name: &str) -> Result<()> {
    let path = socket_path(name)?;
    let stream = crate::sys::control::connect(&path)
        .map_err(|_| anyhow!("no live session '{name}' — see: mars ls"))?;
    let mut w = stream.try_clone()?;
    write_frame(&mut w, &ClientFrame::Kill)?;
    // Wait briefly for the socket to disappear (clean shutdown).
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(50));
        if !path.exists() {
            println!("session '{name}' ended");
            return Ok(());
        }
    }
    println!("kill sent to '{name}' (still shutting down)");
    Ok(())
}
