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
    /// Rename the WORKSPACE (tab) that owns `pane` — the phone's drawer renaming the workspace
    /// it is standing in. Distinct from `Rename`, which renames the whole session and ends the
    /// connection; this one is additive and the new name returns on the next board push.
    RenameWorkspace { pane: usize, to: String },
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
        /// When this daemon started, so a caller can compare it against the mtime of the binary
        /// on disk. `version` cannot answer this — it is CARGO_PKG_VERSION, identical across a
        /// reinstall of the same version over itself, which is the ONLY case that matters here.
        ///
        /// `default` on purpose: a daemon old enough not to send it is, by construction, running
        /// a binary from before this field existed. Zero therefore means "stale" rather than
        /// "unknown", and the absence answers the question it looks like it is failing to answer.
        #[serde(default)]
        started_ts: u64,
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

pub fn socket_dir() -> Result<PathBuf> {
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

/// When this process started, in unix seconds. Recorded once at first call.
pub fn daemon_started_ts() -> u64 {
    use std::sync::OnceLock;
    static STARTED: OnceLock<u64> = OnceLock::new();
    *STARTED.get_or_init(crate::worklog::now_secs)
}

/// Is a daemon that started at `started_ts` running older code than is installed?
///
/// Compares against the mtime of the binary on disk — the same computation the board already
/// sends to the phone, lifted so the CLI can ask it too. A `started_ts` of 0 is a daemon that
/// predates the field, which is itself an answer.
pub fn daemon_is_stale(started_ts: u64) -> bool {
    if started_ts == 0 {
        return true;
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    let Ok(m) = std::fs::metadata(&exe) else { return false };
    let Ok(mtime) = m.modified() else { return false };
    let Ok(d) = mtime.duration_since(std::time::UNIX_EPOCH) else { return false };
    d.as_secs() > started_ts
}

/// Every live session on this host, with whether its daemon is behind the installed binary.
pub fn stale_sessions() -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let Ok(dir) = socket_dir() else { return out };
    let Ok(rd) = std::fs::read_dir(&dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("sock") {
            continue;
        }
        if let Some((name, started)) = probe_started(&p) {
            out.push((name, daemon_is_stale(started)));
        }
    }
    out.sort();
    out
}

/// Ask one daemon its name and start time.
fn probe_started(path: &std::path::Path) -> Option<(String, u64)> {
    let stream = crate::sys::control::connect(path).ok()?;
    stream.set_read_timeout(Some(Duration::from_millis(2500))).ok()?;
    let mut w = stream.try_clone().ok()?;
    write_frame(&mut w, &ClientFrame::Status).ok()?;
    let mut line = String::new();
    std::io::BufReader::new(stream).read_line(&mut line).ok()?;
    match serde_json::from_str::<ServerFrame>(line.trim()).ok()? {
        ServerFrame::Status { name, started_ts, .. } => {
            let name = if name.is_empty() {
                path.file_stem()?.to_str()?.to_string()
            } else {
                name
            };
            Some((name, started_ts))
        }
        _ => None,
    }
}

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

/// Find the socket now serving this SESSION, whatever process is behind it.
/// Returns `(current_name, socket_path)`.
///
/// The distinction from `socket_for_instance` is the whole point, and it is subtle enough to have
/// shipped wrong. An `instance_id` is `pid-nanos`, minted afresh every time a daemon starts — it
/// is immutable, but it is not DURABLE. It names a process, and the process is precisely the thing
/// a reboot replaces. A bridge holding one across a restart finds nothing and refuses every
/// connection from then on, which is a locked-out phone rather than a visible error.
///
/// A session outlives its daemons, and what carries that identity is its DIRECTORY under
/// `~/.mars/sessions/`. The directory name is fixed at creation; a rename rewrites the `name`
/// field inside `meta.json` and leaves the directory alone. So: directory → current name →
/// socket, re-read on every call so a rename is followed rather than cached.
/// The name this session directory currently goes by.
///
/// Split out from the socket lookup so the property that matters is testable without a live
/// daemon: the answer depends on the directory and the CURRENT name, and on nothing about the
/// process. Feeding it a directory whose `instance_id` has changed must change nothing.
pub fn session_name_for_dir(dir_id: &str) -> Option<String> {
    if dir_id.is_empty() {
        return None;
    }
    let sdir = crate::manager::sessions_root()?.join(dir_id);
    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(sdir.join("meta.json")).ok()?).ok()?;
    meta["name"].as_str().filter(|n| !n.is_empty()).map(String::from)
}

pub fn socket_for_session_dir(dir_id: &str) -> Option<(String, PathBuf)> {
    // Re-read per call, so a rename between two connections is followed rather than cached.
    let name = session_name_for_dir(dir_id)?;
    let path = socket_path(&name).ok()?;
    // Probe rather than trust the file's existence: a socket left behind by a dead daemon is
    // exactly what "the session is gone" is supposed to catch, and `identify` already answers it.
    let (live_name, _, _) = identify(&path)?;
    Some((live_name, path))
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
    /// A subscriber renamed the workspace owning this pane (the phone's drawer).
    RenameWorkspace { pane: usize, to: String },
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
    // If a phone is paired to THIS session, make sure it has a bridge. Nothing else owns that:
    // started by hand it dies with whatever terminal launched it, and under a supervisor it needs
    // a plist and a PATH that both drift. The session knows, and the session is here.
    ensure_bridge(&name);

    // A no-file session opens straight into a terminal (multiplexer default) — or, after a
    // reboot, back into the workspaces it had.
    //
    // Restoring HERE rather than from `mars reboot` is what removes the startup race instead of
    // narrowing it. An external process can only paste `cd` over the socket and hope the shell is
    // listening yet; `nudge_manager` already records what that costs — "the command would be typed
    // into a terminal that is not reading yet and silently vanish". Worse, a swallowed `cd` that is
    // followed by a landed `claude --continue` resumes a DIFFERENT project's conversation, because
    // `--continue` picks by directory. A plausible wrong answer, silently.
    //
    // In here neither can happen: a cwd is a spawn argument, so the pane is born in the right
    // place and nothing is typed at all; and `Term::send_bytes` already queues behind the prompt
    // probe, so the agent line waits for a shell that is genuinely ready.
    if !had_file && std::env::var("MARS_OPEN_TERMINAL").is_ok() {
        let panes = read_restore(&name);
        if panes.is_empty() {
            app.open_terminal();
        } else {
            let plan = restore_plan(&panes);
            for (i, (p, start)) in panes.iter().zip(plan.iter()).enumerate() {
                if i > 0 {
                    app.new_tab();
                }
                // The id comes back WITH the workspace. Without this the manager's summary for
                // this workspace, and its conversation gist, would be looked up under an id that
                // now belongs to whichever pane happened to land in the same position.
                app.restore_workspace(std::path::Path::new(&p.cwd), start, &p.wid);
            }
            app.clear_startup_cwd();
            // The manifest just consumed becomes read-only until its promise is delivered —
            // see `restore_hold` for the release conditions. Only panes that will actually START
            // an agent are promised: counting a pane we deliberately left bare would hold the
            // manifest hostage until the deadline for an agent nobody asked to appear.
            let promised = plan.iter().filter(|s| **s != AgentStart::Bare).count();
            if promised > 0 {
                app.restore_promise =
                    Some((promised, crate::worklog::now_secs() + app.tuning.restore_hold_secs));
            }
        }
    }

    let mut client: Option<(crate::sys::control::Stream, u64)> = None;
    let mut term: Option<Terminal<CrosstermBackend<FrameWriter>>> = None;
    let mut latest_client_gen = 0;
    // Read-only mobile subscribers (the Rover phone bridge). Separate from the
    // owning client so a glance never takes over; board/briefing frames are
    // pushed here on a throttled cadence and dead streams are pruned on write.
    let mut subscribers: Vec<crate::sys::control::Stream> = Vec::new();
    // Whether a phone was attached on the previous iteration. `None` until the first pass, which
    // forces one write at boot: a daemon that restarted while a phone was connected would
    // otherwise inherit a `watched: true` from its predecessor and, on the next reconnect,
    // measure the absence from an attach that happened in another process's lifetime.
    let mut presence_watched: Option<bool> = None;
    let mut last_mobile_push: Option<std::time::Instant> = None;
    // The watched pane pushes on its OWN, much tighter clock, and only when the screen actually
    // changed. Coupling it to the board's ~1 Hz status cadence meant a keystroke could take a
    // full second to come back — the renderer was never the latency, the schedule was.
    let mut last_pane_push: Option<std::time::Instant> = None;
    // The manager repo is refreshed on its own clock, independent of whether a phone is
    // listening: the whole point of an ambient layer is that the cards already exist when
    // somebody finally looks.
    let mut last_manager: Option<std::time::Instant> = None;
    // Per-pane read cursor into the terminal's line log, so each snapshot carries what is NEW.
    // In memory deliberately: a daemon restart just means the first snapshot has a tail and no
    // delta, which is the honest answer — we genuinely did not watch that stretch.
    let mut pane_cursors: std::collections::HashMap<usize, u64> = std::collections::HashMap::new();
    // The previous snapshot's tail per pane, so "what is new" can be computed for a pane that
    // repaints instead of scrolling. In memory deliberately, like the cursor above: a daemon
    // restart just means the first snapshot has a tail and no delta, which is the honest answer.
    let mut last_tail: std::collections::HashMap<usize, Vec<String>> = std::collections::HashMap::new();
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
        // The free enrichment answers to a wider question: will anyone READ this board. The
        // manager's snapshot tick below reads every session on this host for as long as the
        // daemon lives, and a phone serves only one session at a time — so keying the board's
        // contents to the phone left every other session unable to say what it was running.
        app.board_has_reader = app.rover_active || app.tuning.manager_snapshot_secs > 0;
        // Record the edge, not the state: the manager needs to know when somebody started
        // looking and how long they had been away, which is a fact about a transition.
        if presence_watched != Some(app.rover_active) {
            if let Some(name) = app.session_name.as_deref() {
                crate::manager::mark_presence(name, app.rover_active, crate::worklog::now_secs());
            }
            presence_watched = Some(app.rover_active);
        }
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
                app.quit_reason.get_or_insert_with(|| "kill frame (mars kill / reboot)".into());
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
            Ok(SrvEvent::RenameWorkspace { pane, to }) => {
                // Additive, like NewTerminal: the phone names a workspace without taking the
                // session. A pane that has since closed simply renames nothing.
                if app.rename_workspace_of_pane(pane, &to) {
                    app.needs_redraw = true;
                }
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
                let now = crate::worklog::now_secs();
                // What each pane has printed: the last 20 lines for where it landed, and
                // everything since we last looked for what changed. Capped, because one runaway
                // logger must not become the whole prompt.
                let mut out = serde_json::Map::new();
                for (pid, pane) in app.panes.iter() {
                    let crate::pane::PaneContent::Terminal(tid) = pane.content else { continue };
                    let Some(term) = app.terms.get(&tid) else { continue };
                    let (_, _, total, _) = term.lines(0, 0);
                    let cursor = pane_cursors.get(&tid).copied().unwrap_or(total);
                    let take = |from: u64, to: u64| -> Vec<String> {
                        let (_, _, _, rows) = term.lines(from, to);
                        rows.iter().map(|r| crate::manager::plain(r))
                            .filter(|l| !l.trim().is_empty()).collect()
                    };
                    let scrolled = take(total.saturating_sub(MANAGER_DELTA_CAP).max(cursor), total);
                    // The tail is the VISIBLE SCREEN, not the log. `capture()` only harvests rows
                    // that scrolled OFF, so a pane whose output fits on screen has an entirely
                    // empty line log — which is why every snapshot carried tail=0, delta=0 and the
                    // agent had nothing to summarise. Screen for where it landed, log for what
                    // streamed past: that is the split the two fields were always meant to be.
                    //
                    // `contents()`, NOT `rows_formatted()`. vt100 encodes a run of blank cells
                    // between two styled ones as a cursor MOVE rather than as spaces, and
                    // stripping escapes then deletes the gap along with the escape — so a
                    // styled TUI reached the agent as "meta agentexiststoansweris". The words
                    // the manager reads were being welded together by the very step meant to
                    // make them readable.
                    let screen = term.screen();
                    let tail: Vec<String> = screen
                        .contents()
                        .lines()
                        .map(|l| l.trim_end().to_string())
                        .filter(|l| !l.trim().is_empty())
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .take(MANAGER_TAIL as usize)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    pane_cursors.insert(tid, total);
                    // What is NEW since the last snapshot.
                    //
                    // The log only holds rows that scrolled OFF the top, so for a full-screen TUI
                    // that repaints in place — an agent pane, vim, htop — it is permanently empty.
                    // Measured on this machine: delta was empty in 147 of 147 pane-records, while
                    // `AGENTS.md` tells the agent an empty delta means "the pane has genuinely not
                    // moved, and that is your licence to skip it". So it skipped everything, wrote
                    // nothing for thirteen hours, and its prose aged out of the briefing entirely.
                    //
                    // Diffing consecutive tails covers exactly the case the log cannot see. Both
                    // are kept: the log is the real record for a pane that scrolls, and the diff
                    // is the only record for one that repaints.
                    let prev = last_tail.get(&tid);
                    let delta: Vec<String> = if !scrolled.is_empty() {
                        scrolled
                    } else {
                        crate::manager::tail_delta(prev.map(|v| v.as_slice()).unwrap_or(&[]), &tail)
                    };
                    last_tail.insert(tid, tail.clone());
                    out.insert(pid.to_string(), serde_json::json!({ "tail": tail, "delta": delta }));
                }
                // The session's shape, refreshed on the same tick as the snapshot. A reboot reads
                // this to come back as itself; writing it here rather than on shutdown means a
                // daemon that is killed outright still leaves one behind.
                let shape: Vec<RestoredPane> = app
                    .tabs
                    .iter()
                    .filter(|t| !t.hidden)
                    .filter_map(|t| {
                        let pid = t.focused_pane;
                        let crate::pane::PaneContent::Terminal(tid) =
                            app.panes.get(&pid)?.content else { return None };
                        let term = app.terms.get(&tid)?;
                        let cwd = term.spawn_cwd.as_ref()?.display().to_string();
                        // Was a coding agent running here? Ask the process table for the pane's
                        // foreground command — `last_command` only knows what MARS typed, so a
                        // `claude` the engineer started by hand is invisible to it.
                        let agent = term.foreground_command().as_deref() == Some("claude");
                        // The conversation id, while the process is still alive to be asked. After
                        // the reboot there is nothing left to ask.
                        let chat = agent.then(|| term.foreground_pid().and_then(claude_session_of)).flatten();
                        Some(RestoredPane { wid: term.wid.clone(), cwd, agent, chat })
                    })
                    .collect();
                let agents_now = shape.iter().filter(|p| p.agent).count();
                if !restore_hold(&mut app.restore_promise, agents_now, now) {
                    if let Some(n) = app.session_name.as_deref() {
                        crate::session::write_restore(n, &shape);
                    }
                }
                let output = serde_json::Value::Object(out);
                let _ = crate::manager::tick_session(
                    &origin,
                    &json,
                    now,
                    keep,
                    app.tuning.manager_detail_min_secs,
                    app.tuning.manager_agent_stale_secs,
                    &output,
                );
                // …and, far more rarely, wake the agent. `agent_tick` owns every gate — the
                // cross-daemon lock, the cadence flag file, the floor, and whether there is any
                // unconsumed work at all — so this call site cannot accidentally spend tokens.
                let agent_secs = app.tuning.manager_agent_secs;
                if agent_secs > 0 {
                    let owner = app
                        .session_instance_id
                        .clone()
                        .unwrap_or_else(|| origin.clone());
                    if let Some(line) = crate::manager::agent_tick(&json, now, agent_secs, &owner) {
                        // Only advance the cadence once the turn is actually in the pane.
                        if app.nudge_manager(&line) {
                            crate::manager::mark_run(&json, now);
                        }
                    }
                }
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
            // The death note, timestamped. "ended cleanly" alone made the August 5 exit a
            // forensic dead end — every quit now says what asked for it and when.
            eprintln!(
                "[mars] {} quitting: {}",
                crate::manager::iso(crate::worklog::now_secs()),
                app.quit_reason.as_deref().unwrap_or("unknown — should_quit set without a reason")
            );
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
                    started_ts: daemon_started_ts(),
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
                    Ok(ClientFrame::RenameWorkspace { pane, to }) => {
                        if tx.send(SrvEvent::RenameWorkspace { pane, to }).is_err() {
                            break;
                        }
                    }
                    // End the session from a subscriber (the phone's end_session card). Kill
                    // was only honoured as a connection's FIRST frame — the `mars kill` path —
                    // so the bridge's forwarded Kill was read here and silently dropped: the
                    // card charged red, the daemon shrugged.
                    Ok(ClientFrame::Kill) => {
                        if tx.send(SrvEvent::Kill).is_err() {
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
        // Claude Code's own session markers. A daemon started from inside a Claude session (a
        // reboot typed into an agent pane, an agent-run install) inherits them, and every claude
        // it then spawns believes it is a CHILD session: transcript saving off, no session file,
        // no id — which is why restore.json captured chat:"" and a reboot resumed fresh
        // conversations instead of the ones it promised. The daemon outlives whatever started
        // it; its environment must too.
        "CLAUDE_CODE_CHILD_SESSION",
        "CLAUDE_CODE_SSE_PORT",
        "CLAUDE_CODE_ENTRYPOINT",
        "CLAUDECODE",
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
/// Lines of scrollback a snapshot carries for context, and the ceiling on how much NEW output one
/// tick may carry. The cap is what keeps a noisy pane from becoming the entire prompt.
const MANAGER_TAIL: u64 = 20;
const MANAGER_DELTA_CAP: u64 = 200;

// ── The bridge belongs to the session ────────────────────────────────────────────────────
//
// Nothing used to own the bridge's existence. It was started by hand, and if that hand happened
// to be a terminal inside the session it bridged, a reboot killed it — the phone lost the machine
// at the moment the reboot succeeded. Handing it to launchd fixed the symptom and introduced a
// plist, an enable flag and a hardcoded PATH: three pieces of configuration that drift, and the
// PATH was wrong within the hour, producing panes where `claude` could not be run.
//
// So the session owns it instead. A daemon knows whether it is the paired session, and it was
// spawned by `mars reboot` from the engineer's own binary — so it already has the right
// environment, and there is nothing to reconstruct or keep in sync. Everything the bridge needs
// is durable and on disk already: which session (this directory), the pairing token, the domain.
//
// The upgrade path then costs nothing. Reboot ends both; the new daemon starts a fresh bridge from
// the binary on disk, and both are current with no supervisor, no exec trick and no second verb.

/// One bridge, one port, one free ngrok tunnel — so exactly one session may own it, and which one
/// is recorded rather than raced for.
pub const BRIDGE_PORT: u16 = 8787;

fn mars_home() -> Option<PathBuf> {
    crate::sys::paths::home_dir().map(|h| h.join(".mars"))
}

/// The session DIRECTORY that a phone is paired to. The directory, not the name and not the
/// instance: a name is renamed and an instance is minted afresh on every start, and this has to
/// survive both.
pub fn remember_paired_session(dir_id: &str) {
    if let Some(d) = mars_home() {
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::write(d.join("serve.session"), dir_id);
    }
}

pub fn paired_session() -> Option<String> {
    let s = std::fs::read_to_string(mars_home()?.join("serve.session")).ok()?;
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Is something already serving the bridge port? Connect rather than look for a process: the
/// question is whether a phone would be answered, and only the socket knows that.
pub fn bridge_listening() -> bool {
    std::net::TcpStream::connect_timeout(
        &([127, 0, 0, 1], BRIDGE_PORT).into(),
        Duration::from_millis(300),
    )
    .is_ok()
}

/// Start a bridge for this session if it is the paired one and nothing is serving yet.
///
/// Detached on purpose. The daemon starts it but must not own it as a child — a bridge that dies
/// with its parent is the failure this whole arrangement exists to remove. Being spawned here only
/// decides WHEN it starts; after that it is independent, and the next daemon to boot will notice
/// if it has gone and start another.
pub fn ensure_bridge(name: &str) {
    let Some(dir) = crate::manager::existing_session_dir_pub(name) else { return };
    let Some(dir_id) = dir.file_name().map(|n| n.to_string_lossy().to_string()) else { return };
    if paired_session().as_deref() != Some(dir_id.as_str()) {
        return; // somebody else's phone, or nothing paired yet
    }
    if bridge_listening() {
        return;
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("pair").arg(name);
    // Its own log, because nobody is watching this one's stdout.
    let log = mars_home()
        .map(|d| d.join("serve-agent.log"))
        .and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
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
        None => { cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()); }
    }
    crate::sys::daemon::detach(&mut cmd);
    let _ = cmd.spawn();
}

/// Ask the running bridge to stop, so the next one starts on the binary that is on disk now.
///
/// The pid file is written by the bridge itself; a bridge that died without cleaning up leaves a
/// stale one, which is why this verifies the port went quiet rather than trusting the signal.
/// Whoever is actually listening on the bridge port, asked of the OS rather than remembered.
///
/// `serve.pid` is a cached copy of a process identity, and it goes stale. Found in the wild holding
/// 95203 while the live bridge was 94100: `mars reboot` signalled a pid that no longer meant
/// anything, reported success, and left a four-hour-old bridge serving the previous binary — which
/// is the precise failure the reboot exists to prevent, wearing the appearance of having worked.
///
/// The port is the bridge's real identity: exactly one process can hold it, and that process IS the
/// bridge by definition. The pid file stays as a fallback for a host with no `lsof`.
fn bridge_pids() -> Vec<i32> {
    std::process::Command::new("lsof")
        .args(["-ti", &format!("tcp:{BRIDGE_PORT}"), "-sTCP:LISTEN"])
        .output()
        .ok()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.trim().parse::<i32>().ok())
                .collect()
        })
        .unwrap_or_default()
}

pub fn stop_bridge() -> bool {
    let Some(d) = mars_home() else { return false };
    let pid_file = d.join("serve.pid");
    let recorded = std::fs::read_to_string(&pid_file)
        .ok()
        .and_then(|t| t.trim().parse::<i32>().ok());
    // The listener first, the remembered pid only if the OS told us nothing. Signalling both is
    // deliberate: a bridge mid-restart may hold the port under a pid the file has not caught up to.
    let mut pids = bridge_pids();
    if let Some(p) = recorded {
        if !pids.contains(&p) {
            pids.push(p);
        }
    }
    if pids.is_empty() {
        return false;
    }
    for pid in pids {
    #[cfg(unix)]
    unsafe {
        // The process GROUP, not just the bridge. The bridge spawns ngrok as a child, and ngrok's
        // free tier allows exactly one agent session — so an orphaned one holds that slot and
        // every replacement bridge times out waiting for a tunnel it can never be given. Signalling
        // the pid alone did precisely that: the bridge died, ngrok did not, and the session's
        // fresh bridge could not start.
        //
        // `ensure_bridge` spawns it detached, which makes it a group leader, so the negative pid
        // reaches ngrok too. Fall back to the bare pid if the group is gone.
        if libc::kill(-pid, libc::SIGTERM) != 0 {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    }
    for _ in 0..30 {
        if !bridge_listening() {
            let _ = std::fs::remove_file(&pid_file);
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// What a session needs in order to come back as itself.
///
/// A daemon owns its PTYs, so restarting it kills every process in every pane — nothing can save
/// those. What CAN be saved is the shape: which workspaces existed, where each was rooted, and
/// which were running a coding agent whose conversation is resumable.
///
/// Stored rather than derived, for the same reason the archive is: the live state it describes
/// stops existing at exactly the moment somebody wants it back. Written on the manager's tick, so
/// a manifest is always on disk whether the daemon exits cleanly or is killed outright — waiting
/// for a graceful shutdown to write it would mean a wedged daemon, the one you most want to
/// reboot, is the one that comes back empty.
pub fn restore_path(name: &str) -> Option<std::path::PathBuf> {
    crate::manager::existing_session_dir_pub(name).map(|d| d.join("restore.json"))
}

/// Which Claude Code conversation is running under this pid, if any.
///
/// Claude Code writes `~/.claude/sessions/<pid>.json` for every live session, holding its
/// `sessionId`. A pane's foreground process group leader IS that pid, so this turns "an agent is
/// running here" into "and it is exactly this conversation".
///
/// That distinction is the difference between `--continue` and `--resume`. `--continue` reopens
/// the most recent conversation IN A DIRECTORY, which is only the right one by luck: this session
/// is keyed to `Mars-Mission` while its panes restore into `Mars-Mission/mars-terminal`, so a
/// reboot would have resumed something else entirely and looked like it worked.
/// The daemon is the only process that can answer this — it holds the pane's pid.
pub fn claude_session_of_pub(pid: i32) -> Option<String> {
    claude_session_of(pid)
}

fn claude_session_of(pid: i32) -> Option<String> {
    let home = crate::sys::paths::home_dir()?;

    // Two sources, because neither is complete on its own. Measured on this machine: of five live
    // `claude` processes, only two had a `sessions/<pid>.json`, while the daemon roster carried
    // ids for pids that file was missing. Checking one and trusting it would silently fall back to
    // `--continue` for the rest, which is the wrong-conversation risk this exists to remove.
    let direct = home.join(".claude").join("sessions").join(format!("{pid}.json"));
    if let Some(id) = std::fs::read_to_string(&direct).ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .and_then(|v| v["sessionId"].as_str().map(String::from))
        .filter(|s| !s.is_empty())
    {
        return Some(id);
    }

    // The roster records the same thing per worker, and reaches processes the file above does not.
    // Only the id is taken from it — its `cwd` disagrees with the session file for the very
    // conversation this was written in, so it is not a field to rely on.
    let roster: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(home.join(".claude").join("daemon").join("roster.json")).ok()?,
    ).ok()?;
    let want = pid.to_string();
    fn find(v: &serde_json::Value, want: &str) -> Option<String> {
        match v {
            serde_json::Value::Object(m) => {
                let matches = m.get("pid").map(|p| match p {
                    serde_json::Value::String(s) => s == want,
                    serde_json::Value::Number(n) => n.to_string() == want,
                    _ => false,
                }).unwrap_or(false);
                if matches {
                    if let Some(id) = m.get("sessionId").and_then(|x| x.as_str()) {
                        if !id.is_empty() {
                            return Some(id.to_string());
                        }
                    }
                }
                m.values().find_map(|x| find(x, want))
            }
            serde_json::Value::Array(a) => a.iter().find_map(|x| find(x, want)),
            _ => None,
        }
    }
    find(&roster, &want)
}

/// Whether the restore manifest is still read-only: a reboot promised `want` agent panes and
/// they are not all observed running yet. Releases — permanently, by clearing the promise —
/// the moment reality catches up OR the deadline passes: a restore that genuinely failed must
/// not shadow the live truth forever, only long enough that a crash mid-restore cannot replace
/// the manifest being restored with a degraded copy of the attempt.
pub fn restore_hold(promise: &mut Option<(usize, u64)>, agents_now: usize, now: u64) -> bool {
    if let Some((want, deadline)) = *promise {
        if agents_now >= want || now >= deadline {
            *promise = None;
        }
    }
    promise.is_some()
}

/// One workspace as the manifest remembers it.
///
/// A struct rather than a tuple because it grew a fourth field whose whole point is that it is NOT
/// interchangeable with the others: `wid` is the durable identity, and an anonymous `String` in
/// position 0 next to another `String` is how the wrong one gets passed.
#[derive(Clone, Debug, PartialEq)]
pub struct RestoredPane {
    /// Durable workspace id. Empty for a manifest written before ids existed.
    pub wid: String,
    pub cwd: String,
    pub agent: bool,
    pub chat: Option<String>,
}

/// Snapshot the session's shape: one entry per workspace, in tab order.
pub fn write_restore(name: &str, panes: &[RestoredPane]) {
    let Some(p) = restore_path(name) else { return };
    let body = serde_json::json!({
        "at_ts": crate::worklog::now_secs(),
        "panes": panes.iter().map(|p| serde_json::json!({
            "wid": p.wid, "cwd": p.cwd, "agent": p.agent, "chat": p.chat,
        })).collect::<Vec<_>>(),
    });
    if let Ok(t) = serde_json::to_string_pretty(&body) {
        let _ = std::fs::write(p, t);
    }
}

/// Is this a conversation id, or is it a payload wearing one?
///
/// The restored id is interpolated into `claude --resume {id}\r` and TYPED INTO A LIVE SHELL, so
/// a `chat` field containing a carriage return is arbitrary shell on the next reboot. That is not
/// hypothetical: the manager agent runs `acceptEdits` over `~/.mars/sessions` with nobody at the
/// keyboard, and it reads pane output that any program on the host can write — so "text on a
/// screen" reaches this string without a human ever reviewing it, and `mars reboot` then types
/// it. The human confirms the reboot, never the payload.
///
/// Claude Code ids are UUIDs. Anything else is refused rather than sanitized: a real id has no
/// reason to hold a space, a quote, or a control character, so there is nothing to preserve by
/// escaping and everything to lose by getting the escaping subtly wrong.
pub fn valid_chat_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// What this session is called NOW, given a name that may be its birth name.
///
/// `MARS_SESSION` is set once, when the daemon starts, and holds the session's directory id — so
/// it keeps saying `0` long after the session was renamed to `mars-dev`. Anything that echoes it
/// reports a name that appears in no listing and resolves to no socket, which reads as "my session
/// is called 0" and makes a rename to its own current name look like it did nothing.
///
/// The directory is the durable identity and the name is the label: correct for storage, wrong for
/// display. Translate at the seam where a human will read it.
pub fn live_session_name(v: &str) -> String {
    if socket_path(v).map(|p| p.exists()).unwrap_or(false) {
        return v.to_string();
    }
    session_name_for_dir(v).unwrap_or_else(|| v.to_string())
}

/// How one restored pane should bring its agent back.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum AgentStart {
    /// The conversation is known by id — the only outcome that restores the RIGHT one.
    Resume(String),
    /// No id, but nothing else in this directory is claiming `--continue`, so the most recent
    /// conversation here is a fair guess.
    Continue,
    /// A shell, and no agent line at all.
    Bare,
}

/// Decide, for the whole manifest at once, how each agent pane comes back.
///
/// `--continue` resumes the most recent conversation IN A DIRECTORY, which is fine for one pane
/// and actively wrong for several: three panes in the same repo all resumed the SAME conversation,
/// so a reboot produced three copies of one thread and lost the other two. That is worse than not
/// restoring them — a bare shell is obviously empty, whereas a confidently wrong conversation
/// looks like it worked and is discovered much later.
///
/// So `--continue` is rationed: at most one per directory, and only where no id was captured.
/// Panes with a real id are unaffected and never consume the ration.
pub fn restore_plan(panes: &[RestoredPane]) -> Vec<AgentStart> {
    let mut spent: Vec<&str> = Vec::new();
    let mut claimed: Vec<&str> = Vec::new();
    // Directories where some pane resumes a KNOWN conversation. No guess may be spent there.
    //
    // `--continue` picks the most recent conversation in a directory — and the one another pane is
    // about to resume by id becomes exactly that, the moment it is resumed. So the guess lands on
    // the conversation already open in the pane beside it. Observed on a real reboot: two panes
    // came back holding one conversation, which is the duplicate the ration was meant to stop,
    // arriving through the other door.
    let resumed_dirs: Vec<&str> = panes
        .iter()
        .filter(|p| p.agent && p.chat.as_deref().is_some_and(valid_chat_id))
        .map(|p| p.cwd.as_str())
        .collect();
    panes
        .iter()
        .map(|p| {
            let (cwd, chat) = (&p.cwd, &p.chat);
            if !p.agent {
                return AgentStart::Bare;
            }
            // An id can be recorded against two panes — the live manifest on this machine has
            // exactly that, one conversation claimed by two of four agent panes. Resuming both
            // reopens one thread twice, which is the same wrong outcome the ration below exists
            // to prevent, reached through a different door. First claimant keeps it; the rest are
            // treated as having no id at all.
            if let Some(id) = chat.as_deref().filter(|s| valid_chat_id(s)) {
                if !claimed.iter().any(|c| *c == id) {
                    claimed.push(id);
                    return AgentStart::Resume(id.to_string());
                }
            }
            if spent.iter().any(|d| *d == cwd.as_str())
                || resumed_dirs.iter().any(|d| *d == cwd.as_str())
            {
                AgentStart::Bare
            } else {
                spent.push(cwd.as_str());
                AgentStart::Continue
            }
        })
        .collect()
}

pub fn read_restore(name: &str) -> Vec<RestoredPane> {
    let Some(p) = restore_path(name) else { return Vec::new() };
    let Ok(txt) = std::fs::read_to_string(p) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { return Vec::new() };
    let mut seen: Vec<String> = Vec::new();
    v["panes"].as_array().map(|a| {
        a.iter().filter_map(|p| {
            let cwd = p["cwd"].as_str()?.to_string();
            (!cwd.is_empty()).then(|| RestoredPane {
                // A manifest written before ids existed has none. Empty means "mint a fresh one",
                // which loses that workspace's history exactly once and never again — the same
                // trade `restore.json` itself already makes for pre-feature sessions.
                wid: {
                    // Repair a manifest that carries the same id twice. An earlier build minted
                    // `<secs>-<dir>`, which collided whenever a reboot restored two workspaces
                    // into one directory in one second — and a duplicate id is worse than none,
                    // because both workspaces then answer to the same files. The first claimant
                    // keeps it; the rest come back blank and mint fresh, losing their history
                    // once rather than sharing somebody else's for good.
                    let w = p["wid"].as_str().unwrap_or_default().to_string();
                    if w.is_empty() || seen.contains(&w) {
                        if !w.is_empty() {
                            debug_log(&format!("[restore] duplicate workspace id {w:?} — minting a fresh one"));
                        }
                        String::new()
                    } else {
                        seen.push(w.clone());
                        w
                    }
                },
                cwd,
                agent: p["agent"].as_bool().unwrap_or(false),
                // A rejected id degrades to `--continue`, which restores by directory: the
                // documented fallback for a pane whose id was never captured, and the right
                // landing place for one whose id cannot be trusted.
                chat: p["chat"].as_str().filter(|s| {
                    let ok = valid_chat_id(s);
                    if !ok {
                        debug_log(&format!("[restore] refused a chat id that is not one: {s:?}"));
                    }
                    ok
                }).map(String::from),
            })
        }).collect()
    }).unwrap_or_default()
}

/// `mars reboot [name]` — bring a session back on the binary that is on disk NOW.
///
/// The whole point is that the caller is not the thing being restarted. A daemon cannot restart
/// itself (it is the process going away) and the bridge must not try (it is the phone's only way
/// back, and a bridge inside the blast radius turns a failed reboot into a lost machine). So this
/// runs as its own short-lived process, and both of those keep serving.
pub fn reboot_main(name_arg: Option<String>) -> Result<()> {
    let name = match name_arg {
        Some(n) => n,
        None => attached_session()
            .ok_or_else(|| anyhow!("no attached session — name one: mars reboot <name>"))?,
    };
    // Reboot RESTARTS a running session; it must never quietly create one. Skipping the kill for
    // a name with no live socket and spawning anyway looks like success and is not: asked to
    // reboot `0` — a session since renamed — it left the real daemon untouched and started a
    // second session beside it. The board then shows two, neither of them what was asked for.
    if crate::sys::control::probe(&socket_path(&name)?) != crate::sys::control::Probe::Live {
        let live: Vec<String> = list_sessions()
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, alive, _)| *alive)
            .map(|(n, _, _)| n)
            .collect();
        anyhow::bail!(
            "no live session '{name}' — reboot restarts a running session, it does not create \
             one.\nlive: {}",
            if live.is_empty() { "(none)".into() } else { live.join(", ") }
        );
    }
    // The new daemon restores its own workspaces from this manifest at boot — see the
    // MARS_OPEN_TERMINAL branch in `server_main`. Read here only to say what is about to happen,
    // and to be honest when there is nothing to restore.
    let panes = read_restore(&name);
    if panes.is_empty() {
        println!(
            "rebooting '{name}' — no saved layout, so it comes back as a bare shell.\n\
             (A session started before this feature has no manifest; the next reboot will have one.)"
        );
    } else {
        // Say which agents come back as THEMSELVES and which do not. The failure this reports is
        // silent by nature: a pane that resumes the wrong conversation looks identical to one
        // that resumed the right one, and is noticed only when somebody reads it and finds
        // somebody else's work.
        let plan = restore_plan(&panes);
        let exact = plan.iter().filter(|s| matches!(s, AgentStart::Resume(_))).count();
        let guessed = plan.iter().filter(|s| **s == AgentStart::Continue).count();
        let dropped = panes.iter().zip(plan.iter())
            .filter(|(p, s)| p.agent && **s == AgentStart::Bare)
            .count();
        println!("rebooting '{name}' — {} workspace(s) to restore", panes.len());
        if exact > 0 {
            println!("  {exact} agent(s) resume their exact conversation");
        }
        if guessed > 0 {
            println!("  {guessed} agent(s) had no conversation id — resuming the most recent one \
                      in their directory");
        }
        if dropped > 0 {
            println!(
                "  {dropped} agent(s) come back as a bare shell: no id, and another pane in the \
                 same directory is already resuming there.\n  Restoring them too would reopen \
                 that same conversation, not theirs."
            );
        }
    }

    // The bridge goes too — but ONLY when this session is the paired one. It is a separate
    // process running its own copy of the code, so leaving it up would upgrade the worker and
    // silently keep serving the phone from yesterday's build; the new daemon starts a fresh one
    // on boot, so for the paired session this is a replacement rather than a loss. For any OTHER
    // session it was pure loss: `ensure_bridge` on the rebooted daemon would see "somebody
    // else's phone" and start nothing, so rebooting an unpaired session cut the phone off and
    // printed a promise nobody was going to keep.
    let this_dir = crate::manager::existing_session_dir_pub(&name)
        .and_then(|d| d.file_name().map(|n| n.to_string_lossy().to_string()));
    if this_dir.as_deref() == paired_session().as_deref() && this_dir.is_some() {
        if stop_bridge() {
            println!("  bridge stopped — the new session will start one");
        }
    }
    // Graceful: the daemon flushes its state and removes its own socket. kill_main already waits
    // for the socket to disappear, which is the only reliable "it is really gone".
    kill_main(&name)?;
    spawn_daemon(&name, None)?;
    println!("'{name}' is back on {}", env!("CARGO_PKG_VERSION"));
    Ok(())
}

/// Start a session's daemon and wait for its socket, without attaching a client.
///
/// Factored out of `session_main` so a reboot can bring a session back up from a process that is
/// NOT going to attach to it — the phone has no terminal to hand over to.
///
/// `std::env::current_exe()` is what makes a reboot pick up a new build: the caller is whichever
/// `mars` binary is on disk now, so the daemon it spawns is that one and not the one that has been
/// running since yesterday.
pub fn spawn_daemon(name: &str, file: Option<String>) -> Result<()> {
    let path = socket_path(name)?;
    let _ = std::fs::remove_file(&path);
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    isolate_session_daemon_env(&mut cmd);
    cmd.arg("--server").arg(name);
    if let Some(f) = &file {
        cmd.arg(f);
    }
    let log = state_dir()
        .map(|d| d.join(format!("{name}.log")))
        .and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
    cmd.env("RUST_BACKTRACE", "1");
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
            cmd.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        }
    }
    crate::sys::daemon::detach(&mut cmd);
    cmd.spawn()?;
    for _ in 0..60 {
        std::thread::sleep(Duration::from_millis(50));
        if crate::sys::control::probe(&path) == crate::sys::control::Probe::Live {
            return Ok(());
        }
    }
    Err(anyhow!("session daemon for '{name}' did not start"))
}

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
            spawn_daemon(name, file.clone())?;
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
