/// Terminal panes — a real shell running inside a pane via a PTY.
/// Output is parsed by `vt100` into a screen grid that the UI renders.

use std::io::{Read, Write};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::{Duration, Instant};

use anyhow::Result;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

pub type TermId = usize;

/// Emitted when terminal `id`'s screen changes or its child process exits.
pub enum TermEvent {
    Output(TermId),
    Exited(TermId),
}

static NEXT_STARTUP_PROBE: AtomicU64 = AtomicU64::new(1);

struct StartupInput {
    bytes: Vec<u8>,
    marker: String,
    probe_interval: Duration,
    last_probe: Option<Instant>,
    // When set, only a SEEN marker releases the queue — the prompt-glyph shortcut is skipped.
    // The glyph heuristic fires on prompts that are drawn but not yet reading (zsh instant
    // prompt), and input written then vanishes silently. For interactive typing that latency
    // trade is right; for a restored `claude --resume` it is how a conversation fails to come
    // back while looking like it was typed.
    require_marker: bool,
}

/// How much raw PTY output each pane retains for phone scrollback. ~512 KB is thousands of
/// lines of terminal output while staying trivial next to a session's other state.
const RAW_HISTORY_BYTES: usize = 512 * 1024;

/// Feed the parser in slices this small so a single burst can never scroll more lines than
/// the parser retains. One byte can be one line (`\n`), so a chunk of N bytes produces at
/// most N lines — keep it well under `terminal_scrollback_lines` and nothing is ever lost
/// between the moment it scrolls off and the moment we read it.
const CAPTURE_CHUNK: usize = 512;

/// A pane's transcript: every line that has scrolled off the top of its screen, laid out by
/// the emulator at the pane's real geometry and kept as formatted ANSI.
///
/// The unit is a numbered LINE, not "a screenful at scrollback offset N". Screen-relative
/// addressing is what made phone history duplicate and stall: an over-ask clamped into an
/// overlapping page, and the live screen kept arriving twice. A line id is assigned once,
/// never reused, and stays valid across reconnects, reflows and resizes.
struct LineLog {
    rows: std::collections::VecDeque<Vec<u8>>,
    bytes: usize,
    /// Id of `rows[0]`; `first + rows.len()` is the id the next line will get.
    first: u64,
    cap: usize,
}

impl LineLog {
    fn push(&mut self, row: Vec<u8>) {
        self.bytes += row.len();
        self.rows.push_back(row);
        while self.bytes > self.cap && self.rows.len() > 1 {
            if let Some(old) = self.rows.pop_front() {
                self.bytes -= old.len();
                self.first += 1;
            }
        }
    }
}

/// Feed `bytes` to the pane's parser, appending whatever scrolls off the top to its transcript.
///
/// vt100 exposes the scrollback OFFSET but not its DEPTH, and the depth saturates at the
/// configured limit, so "how much is stored now" can't be differenced to find what just
/// scrolled. The offset can: while it is non-zero, vt100 bumps it once per scrolled line to
/// keep a scrolled-back view pinned to the same content. Park it at 1 before processing and it
/// comes back as `1 + lines scrolled off`.
///
/// This reads the LIVE parser rather than replaying a byte ring into a private one. The ring
/// held screen UPDATES, not a transcript, so it had to be re-simulated at exactly the pane's
/// geometry on every request — expensive, and wrong the moment the geometry was off by a row.
/// Here the pane's own emulator does the layout once, as it happens, at the only geometry that
/// was ever correct. Programs that repaint inside a scroll region never scroll the grid, so they
/// never enter the transcript — which is what stopped commands appearing three times.
fn capture(p: &mut vt100::Parser, bytes: &[u8], log: &Mutex<LineLog>) {
    let saved = p.screen().scrollback(); // the desktop user's own scroll position
    let mut total_scrolled = 0usize;
    for chunk in bytes.chunks(CAPTURE_CHUNK) {
        p.set_scrollback(1);
        let armed = p.screen().scrollback(); // 0 until any history exists at all
        p.process(chunk);
        let scrolled = if armed > 0 {
            p.screen().scrollback().saturating_sub(armed)
        } else {
            // Nothing was stored before this chunk, so the whole depth is new.
            p.set_scrollback(usize::MAX);
            p.screen().scrollback()
        };
        if scrolled == 0 {
            continue;
        }
        total_scrolled += scrolled;
        // At offset `back` the top `back` visible rows ARE the newest `back` history lines, so
        // walking `back` down by a screenful at a time yields them oldest-first with no overlap.
        let (rows, cols) = p.screen().size();
        let mut back = scrolled;
        let mut harvested: Vec<Vec<u8>> = Vec::with_capacity(scrolled);
        while back > 0 {
            p.set_scrollback(back);
            let take = (rows as usize).min(back);
            harvested.extend(p.screen().rows_formatted(0, cols).take(take));
            back -= take;
        }
        if let Ok(mut l) = log.lock() {
            for row in harvested {
                l.push(row);
            }
        }
    }
    // Put the view back. A reader scrolled up stays on the same content — which is what vt100
    // would have done by itself had we not parked the offset to count with it.
    p.set_scrollback(if saved > 0 { saved + total_scrolled } else { 0 });
}

/// Mint a durable workspace id: `<unix-secs>-<token>-<directory>`.
///
/// Not a random UUID: this is read by people in filenames and logs, so the leading timestamp
/// sorts by age and the trailing directory says what it belongs to, where a bare hex string
/// says nothing.
///
/// THE TOKEN IS NOT DECORATION. The first version was `<secs>-<directory>` on the theory that a
/// timestamp is unique enough — and a reboot restored two workspaces into the same directory
/// within the same second and gave them the SAME id, which is the collision this whole mechanism
/// exists to prevent, reintroduced by the mechanism itself. Workspaces are born in batches, so a
/// clock is not an identity. The token is this process plus a per-process sequence, which makes
/// ids unique within a daemon by construction and across daemons by a margin that would need the
/// same second, the same low pid bits and the same sequence number to fail.
pub fn new_wid(cwd: Option<&std::path::Path>) -> String {
    let tail = cwd
        .and_then(|c| c.file_name())
        .map(|n| n.to_string_lossy().to_lowercase())
        .map(|n| {
            n.chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect::<String>()
                .trim_matches('-')
                .to_string()
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "workspace".into());
    let tail: String = tail.chars().take(24).collect();
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}-{:x}{:x}-{}",
        crate::worklog::now_secs(),
        std::process::id() & 0xffff,
        seq & 0xff,
        tail.trim_matches('-'),
    )
}

pub struct Term {
    /// This workspace's DURABLE identity: `<unix-secs>-<directory>`, minted once when the shell is
    /// spawned and carried across a reboot in `restore.json`.
    ///
    /// Distinct from `PaneId`, which is a process-scoped counter and therefore a runtime HANDLE:
    /// unique while this daemon lives, reassigned from zero by the next one. Anything that must
    /// still mean the same workspace after a restart — the manager's per-workspace summary, a
    /// conversation gist, a decision the captain recorded about this workspace — keys on this, and
    /// anything that addresses a pane in the running daemon keys on the handle. Conflating the two
    /// is what let a reboot attach one workspace's summary to another.
    pub wid: String,
    /// The shell has exited; the pane shows a notice until the user closes it.
    pub exited: bool,
    /// Where the shell was spawned (the work journal's cwd). The shell may
    /// `cd` later — a PTY can't see that without shell integration, so this
    /// is honest spawn-time truth, not a live value.
    pub spawn_cwd: Option<std::path::PathBuf>,
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    /// The shell we spawned. Compared against the PTY's foreground process group to
    /// answer "is a command executing right now" — see `foreground_busy`.
    shell_pid: Option<u32>,
    kill_tx: mpsc::Sender<()>,
    startup_input: Option<StartupInput>,
    exit_code: Arc<Mutex<Option<i32>>>,
    notify_exit: Arc<AtomicBool>,
    rows: u16,
    cols: u16,
    /// How far back the view is scrolled (0 = live). Mirrors the vt100 state.
    view_offset: usize,
    scrollback_limit: usize,
    /// Rover xterm.js raw streaming: when a phone watches this pane in raw mode, the reader
    /// thread appends the newly-read PTY bytes here (gated by `raw_tap_on`) for the session
    /// loop to drain and forward as `{t:"output"}` deltas. Off (and free) otherwise.
    raw_tap: Arc<Mutex<Vec<u8>>>,
    raw_tap_on: Arc<AtomicBool>,
    /// A rolling window of raw PTY bytes, appended ALWAYS. vt100's scrollback cannot be the
    /// source of history for the phone: reflowing the pane to the phone's grid resizes the
    /// parser, which discards its scrollback — so watching destroyed the very history we were
    /// trying to read. Bytes survive resizes because they are just bytes.
    raw_history: Arc<Mutex<std::collections::VecDeque<u8>>>,
    /// This pane's transcript — every line that has scrolled off, numbered. Written by the
    /// reader thread as it happens; read by `lines()` for a phone paging upward.
    line_log: Arc<Mutex<LineLog>>,
}

/// Bound on the raw-tap buffer so a firehose against an absent/slow drain can't grow
/// without limit; on overflow the buffer is dropped (xterm.js resyncs on the next reseed).
const RAW_TAP_CAP: usize = 1 << 20;

/// Spawn the platform shell on a PTY sized `rows` x `cols` with `scrollback` lines of
/// history, streaming output into a `vt100::Parser`. One background thread
/// pumps the PTY; another waits for child-process exit.
pub fn spawn(
    id: TermId,
    rows: u16,
    cols: u16,
    scrollback: usize,
    line_log_bytes: usize,
    cwd: Option<std::path::PathBuf>,
    session: Option<&str>,
    session_instance_id: Option<&str>,
    startup_probe_interval: Duration,
    tx: mpsc::Sender<TermEvent>,
) -> Result<Term> {
    let rows = rows.max(1);
    let cols = cols.max(1);

    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;

    let shell = crate::sys::shell::default_shell();
    let mut cmd = CommandBuilder::new(shell);
    let spawn_cwd = cwd.filter(|d| d.is_dir());
    if let Some(dir) = &spawn_cwd {
        cmd.cwd(dir);
    }
    // Mark the shell as living inside this Mars session, so a nested `mars <file>`
    // opens a tab in the running instance instead of launching a new one.
    if let Some(name) = session {
        cmd.env("MARS_SESSION", name);
    }
    if let Some(id) = session_instance_id {
        cmd.env("MARS_SESSION_ID", id);
    }
    let mut child = pair.slave.spawn_command(cmd)?;
    let shell_pid = child.process_id();
    // Drop our slave copy; child-process exit is tracked separately because
    // ConPTY can keep the master output pipe open after the child is gone.
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;

    let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, scrollback)));
    let reader_parser = parser.clone();
    let raw_tap = Arc::new(Mutex::new(Vec::<u8>::new()));
    let raw_tap_on = Arc::new(AtomicBool::new(false));
    let raw_history = Arc::new(Mutex::new(std::collections::VecDeque::<u8>::new()));
    let line_log = Arc::new(Mutex::new(LineLog {
        rows: std::collections::VecDeque::new(),
        bytes: 0,
        first: 0,
        cap: line_log_bytes,
    }));
    let reader_raw_tap = raw_tap.clone();
    let reader_raw_tap_on = raw_tap_on.clone();
    let reader_raw_history = raw_history.clone();
    let reader_line_log = line_log.clone();
    let output_tx = tx.clone();
    // Captured for the OSC-133 ledger: exact command records are keyed by session
    // (skipped in standalone mode, which has no session log). The surface label is
    // the term id — the pane's tab label is resolved at render time.
    let ledger_session = session.map(|s| s.to_string());
    let ledger_surface = id.to_string();
    let (reader_done_tx, reader_done_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        // OSC-133 command-boundary scanner — additive: a shell that emits no
        // markers yields no events, so this is a no-op for un-integrated shells.
        let mut osc = crate::osc133::Scanner::new();
        let mut cmd_started: Option<Instant> = None;
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut p) = reader_parser.lock() {
                        capture(&mut p, &buf[..n], &reader_line_log);
                    }
                    // Raw streaming to a Rover xterm.js subscriber: forward the same bytes we
                    // just fed the parser (only while a phone is watching this pane in raw mode).
                    // Always retain a rolling window, whether or not a phone is watching —
                    // history has to exist BEFORE someone asks for it.
                    if let Ok(mut h) = reader_raw_history.lock() {
                        h.extend(buf[..n].iter().copied());
                        let over = h.len().saturating_sub(RAW_HISTORY_BYTES);
                        if over > 0 { h.drain(..over); }
                    }
                    if reader_raw_tap_on.load(Ordering::Relaxed) {
                        if let Ok(mut tap) = reader_raw_tap.lock() {
                            if tap.len() + n > RAW_TAP_CAP {
                                tap.clear();
                            }
                            tap.extend_from_slice(&buf[..n]);
                        }
                    }
                    if let Some(sess) = &ledger_session {
                        for ev in osc.feed(&buf[..n]) {
                            match ev {
                                crate::osc133::CmdEvent::Start => cmd_started = Some(Instant::now()),
                                crate::osc133::CmdEvent::End { command, cwd, exit } => {
                                    let dur = cmd_started.take().map(|t| t.elapsed().as_secs());
                                    if let Some(entry) = crate::osc133::to_ledger_entry(
                                        sess, &ledger_surface, command, cwd, exit, dur,
                                    ) {
                                        crate::worklog::record(&entry);
                                    }
                                }
                            }
                        }
                    }
                    if output_tx.send(TermEvent::Output(id)).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = reader_done_tx.send(());
    });

    let exit_code = Arc::new(Mutex::new(None));
    let wait_exit_code = exit_code.clone();
    let notify_exit = Arc::new(AtomicBool::new(true));
    let wait_notify_exit = notify_exit.clone();
    let (kill_tx, kill_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status.exit_code() as i32),
                Err(_) => break None,
                Ok(None) => {}
            }
            match kill_rx.recv_timeout(Duration::from_millis(20)) {
                Ok(()) => {
                    let _ = child.kill();
                    break child.wait().ok().map(|status| status.exit_code() as i32);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = child.kill();
                    break child.wait().ok().map(|status| status.exit_code() as i32);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        };
        if let Ok(mut slot) = wait_exit_code.lock() {
            *slot = code;
        }
        if wait_notify_exit.swap(false, Ordering::AcqRel) {
            let _ = reader_done_rx.recv_timeout(Duration::from_millis(100));
            let _ = tx.send(TermEvent::Exited(id));
        }
    });

    let marker = format!(
        "__MARS_READY_{:x}__",
        NEXT_STARTUP_PROBE.fetch_add(1, Ordering::Relaxed)
    );
    Ok(Term {
        // Timestamp first so ids sort by age and collide only within the same second in the same
        // directory; the directory is there so a human reading `~/.mars` can tell which workspace
        // a file belongs to without cross-referencing anything.
        wid: new_wid(spawn_cwd.as_deref()),
        exited: false,
        spawn_cwd,
        parser,
        writer,
        master: pair.master,
        shell_pid,
        kill_tx,
        startup_input: Some(StartupInput {
            bytes: Vec::new(),
            marker,
            probe_interval: startup_probe_interval,
            last_probe: None,
            require_marker: false,
        }),
        exit_code,
        notify_exit,
        rows,
        cols,
        view_offset: 0,
        scrollback_limit: scrollback,
        raw_tap,
        raw_tap_on,
        raw_history,
        line_log,
    })
}

/// Removing a Term (closed pane/tab, app exit) must not orphan the shell process.
/// Descendant containment is a separate platform lifecycle responsibility.
impl Drop for Term {
    fn drop(&mut self) {
        self.notify_exit.store(false, Ordering::Release);
        let _ = self.kill_tx.send(());
    }
}

impl Term {
    fn prompt_visible(&self) -> bool {
        let Ok(parser) = self.parser.lock() else { return false };
        let screen = parser.screen();
        if screen.hide_cursor() {
            return false;
        }
        let (row, _) = screen.cursor_position();
        let (_, cols) = screen.size();
        let line: String = (0..cols)
            .filter_map(|col| screen.cell(row, col))
            .map(|cell| cell.contents())
            .collect();
        matches!(
            line.trim_end().chars().last(),
            Some('$' | '#' | '%' | '>' | '❯' | '➜' | 'λ' | '»' | '›')
        )
    }

    pub fn flush_startup_input(&mut self) {
        if self.startup_input.is_none() {
            return;
        }
        let marker_only = self.startup_input.as_ref().is_some_and(|s| s.require_marker);
        if !marker_only && self.prompt_visible() {
            let bytes = self.startup_input.take().map(|startup| startup.bytes);
            if let Some(bytes) = bytes {
                self.write_input(&bytes);
            }
            return;
        }
        let marker_seen = self.startup_input.as_ref().is_some_and(|startup| {
            let Ok(parser) = self.parser.lock() else { return false };
            parser
                .screen()
                .contents()
                .lines()
                .any(|line| line.trim() == startup.marker)
        });
        if marker_seen {
            let bytes = self.startup_input.take().map(|startup| startup.bytes);
            if let Some(bytes) = bytes {
                self.write_input(&bytes);
            }
            return;
        }
        let probe = self.startup_input.as_mut().and_then(|startup| {
            let last_probe = startup.last_probe?;
            if last_probe.elapsed() < startup.probe_interval {
                return None;
            }
            startup.last_probe = Some(Instant::now());
            Some(format!("echo {}\r", startup.marker))
        });
        if let Some(probe) = probe {
            self.write_input(probe.as_bytes());
        }
    }

    pub fn send_bytes(&mut self, bytes: &[u8]) {
        if let Some(startup) = self.startup_input.as_mut() {
            startup.bytes.extend_from_slice(bytes);
            startup.last_probe.get_or_insert_with(Instant::now);
            self.flush_startup_input();
            return;
        }
        self.write_input(bytes);
    }

    /// Queue bytes that must not be lost: released only once the marker probe has ROUND-TRIPPED,
    /// never on the prompt-glyph shortcut. For a restored agent line, a dropped byte is a
    /// conversation that fails to come back while looking typed.
    pub fn send_bytes_marker_gated(&mut self, bytes: &[u8]) {
        if let Some(startup) = self.startup_input.as_mut() {
            startup.require_marker = true;
        }
        self.send_bytes(bytes);
    }

    fn write_input(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// The shell's exit code, if it has exited and the OS reported one —
    /// available once the process watcher has reported exit.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code.lock().ok().and_then(|code| *code)
    }

    /// Is a foreground command executing in this pane right now?
    ///
    /// The kernel already knows: a PTY has one foreground process group, and the shell
    /// puts a job there while it runs and takes it back at the prompt. So the answer is
    /// `tcgetpgrp(master) != shell_pid`, with no cooperation from the shell at all.
    ///
    /// This is deliberately NOT the OSC-133 route. Mars scans those markers already, but
    /// it does not install shell integration — so on a plain zsh no markers ever arrive
    /// and a feature built on them would silently do nothing on most machines. The
    /// process group is there whether or not anyone configured anything.
    ///
    /// The bare command name out of whatever `ps` reported.
    ///
    /// Two surprises, each of which silently defeats an exact match. macOS `ps -o comm=` includes
    /// ARGUMENTS, so Claude Code's helpers report as `claude bg-pty-host`; and anything off the
    /// default PATH reports a full path. Take the first token, then its basename.
    ///
    /// Getting this wrong is invisible rather than loud: the agent flag simply never gets set, and
    /// a reboot quietly comes back without resuming the conversation it was supposed to restore.
    pub fn command_name(ps_output: &str) -> String {
        let first = ps_output.trim().split_whitespace().next().unwrap_or_default();
        first.rsplit('/').next().unwrap_or(first).to_string()
    }

    /// The pid of the foreground process group, when a command is running.
    ///
    /// Kept because a pid is a JOIN KEY, not just a number: Claude Code records each live session
    /// at `~/.claude/sessions/<pid>.json`, so this is what turns "a coding agent is running here"
    /// into "and here is exactly which conversation it is".
    pub fn foreground_pid(&self) -> Option<i32> {
        #[cfg(unix)]
        {
            let leader = self.master.process_group_leader()?;
            (Some(leader as u32) != self.shell_pid).then_some(leader)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// The NAME of the foreground command, when one is running (`claude`, `cargo`, …).
    ///
    /// Same source as `foreground_busy` — the PTY's foreground process group — resolved to a
    /// command name. This is how Mars can tell an agent pane from a build: `last_command` only
    /// records what Mars itself typed, so anything the engineer started by hand is invisible to
    /// it, and OSC-133 needs shell integration that Mars does not install.
    pub fn foreground_command(&self) -> Option<String> {
        #[cfg(unix)]
        {
            let leader = self.master.process_group_leader()?;
            if Some(leader as u32) == self.shell_pid {
                return None; // at a prompt: nothing is running
            }
            let out = std::process::Command::new("ps")
                .args(["-o", "comm=", "-p", &leader.to_string()])
                .output()
                .ok()?;
            Some(Self::command_name(&String::from_utf8_lossy(&out.stdout))).filter(|s| !s.is_empty())
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    /// `None` means "cannot know" (Windows, or a PTY that does not report a leader), and
    /// every caller must read it as *no claim* rather than as `false`.
    pub fn foreground_busy(&self) -> Option<bool> {
        #[cfg(unix)]
        {
            let leader = self.master.process_group_leader()?;
            let shell = self.shell_pid? as i32;
            Some(leader != shell)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        let rows = rows.max(1);
        let cols = cols.max(1);
        if rows == self.rows && cols == self.cols {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(rows, cols);
        }
    }

    /// Clone of the latest screen for rendering.
    pub fn screen(&self) -> vt100::Screen {
        self.parser.lock().unwrap().screen().clone()
    }

    /// Rover xterm.js raw streaming: start/stop capturing this pane's raw PTY output, and
    /// drain what's accumulated since the last call. Off by default — no capture cost when
    /// no phone is watching the pane in raw mode.
    pub fn enable_raw_tap(&self) {
        if let Ok(mut tap) = self.raw_tap.lock() {
            tap.clear();
        }
        self.raw_tap_on.store(true, Ordering::Relaxed);
    }
    pub fn disable_raw_tap(&self) {
        self.raw_tap_on.store(false, Ordering::Relaxed);
        if let Ok(mut tap) = self.raw_tap.lock() {
            tap.clear();
        }
    }
    pub fn take_raw_delta(&self) -> Vec<u8> {
        self.raw_tap
            .lock()
            .map(|mut t| std::mem::take(&mut *t))
            .unwrap_or_default()
    }

    /// Scroll the view within the scrollback: positive = further back in
    /// history, negative = toward live. Clamped to [0, real history depth].
    pub fn scroll_view(&mut self, delta: i64) {
        let requested = (self.view_offset as i64 + delta)
            .clamp(0, self.scrollback_limit as i64) as usize;
        if let Ok(mut p) = self.parser.lock() {
            p.set_scrollback(requested);
            // vt100 clamps to the ACTUAL available history; mirror that real
            // value so the "↑N" indicator is honest and a wheel-down doesn't
            // have to burn through a phantom offset past the top of history.
            self.view_offset = p.screen().scrollback();
        } else {
            self.view_offset = requested;
        }
    }

    /// The last `lines` lines of scrollback + live screen, oldest-first (W5).
    /// Pages back through history under the lock and restores the live view.
    pub fn history_tail(&self, lines: usize) -> String {
        let Ok(mut p) = self.parser.lock() else { return String::new() };
        let rows = p.screen().size().0 as usize;
        let saved = self.view_offset;
        let mut pages: Vec<String> = Vec::new();
        let (mut off, mut got) = (0usize, 0usize);
        loop {
            p.set_scrollback(off);
            pages.push(p.screen().contents());
            got += rows;
            if got >= lines || off >= self.scrollback_limit {
                break;
            }
            off += rows;
        }
        p.set_scrollback(saved); // restore the live view before releasing the lock
        pages.reverse(); // oldest screenful first
        let joined = pages.join("\n");
        let all: Vec<&str> = joined.lines().collect();
        let start = all.len().saturating_sub(lines);
        all[start..].join("\n")
    }

    /// The last `lines` of scrollback as ANSI rows (colour preserved), oldest-first, plus how
    /// much history actually exists. This is what a phone renderer replays to page in earlier
    /// output: `history_tail` returns plain text, which would make paged-in history render
    /// colourless next to the live screen. Excludes the live screen — the caller already has it.
    /// Returns `(bytes, rows_included, history_depth)`. `bytes` ends with the LIVE screen, so a
    /// client can rebuild its whole buffer from one payload (xterm.js has no way to prepend to
    /// its scrollback, so paging in older output means rewriting, not appending).
    /// Raw PTY bytes for a phone paging upward: the tail of the retained window, ending with
    /// whatever is on screen now. Returns `(bytes, rows_included, rows_available)`. Replaying
    /// bytes keeps the original colour and, unlike vt100's scrollback, survives the reflow the
    /// phone triggers when it watches.
    /// A readable transcript for a phone paging upward. Returns `(bytes, rows, total)`.
    ///
    /// The retained bytes are screen UPDATES, not a transcript — a repainting program emits
    /// cursor jumps and clears, so replaying them raw makes the terminal overwrite itself into
    /// nonsense. Replay them into a PRIVATE parser instead (one nothing else resizes, which is
    /// what made vt100's own scrollback useless here), then emit its laid-out rows. Colour is
    /// preserved because we emit formatted rows, not plain text.
    /// A readable transcript for a phone paging upward. Returns `(bytes, rows, total)`.
    ///
    /// Replay the retained bytes into a PRIVATE parser sized EXACTLY like the real pane, then
    /// page back through its scrollback. The geometry is the whole point. Those bytes are screen
    /// updates carrying absolute cursor moves, scroll regions and clears that a program computed
    /// for a `self.rows` x `self.cols` grid; replaying them into a grid of any other size sends
    /// each repaint somewhere the program never meant, so repaints stop landing on top of each
    /// other and pile up as near-identical copies instead. Both earlier attempts replayed into a
    /// grid `lines` tall and both duplicated, for exactly that reason — reading the final screen
    /// rather than the scrollback changed which copies survived, not whether they were made.
    ///
    /// At the true size the replay re-simulates the pane: repaints overwrite in place and only
    /// genuinely scrolled-off lines enter scrollback, so what comes back is what the desktop
    /// terminal's own scrollback holds. Private, so it never resizes the real PTY — and fed from
    /// the byte ring rather than the live parser, whose scrollback the phone's reflow discards.
    pub fn history_ansi(&self, lines: usize) -> (Vec<u8>, usize, usize) {
        let Ok(h) = self.raw_history.lock() else { return (Vec::new(), 0, 0) };
        let raw: Vec<u8> = h.iter().copied().collect();
        drop(h);
        if raw.is_empty() {
            return (Vec::new(), 0, 0);
        }
        let cols = self.cols.max(20);
        let rows = self.rows.max(4);
        let step = rows as usize;
        let want = lines.clamp(24, 4000);
        let mut p = vt100::Parser::new(rows, cols, want + step);
        p.process(&raw);

        // Page back a screenful at a time, newest first. vt100 CLAMPS `set_scrollback` to the
        // history that actually exists, so an over-ask silently returns a page overlapping the
        // one before it — trim that overlap off the bottom or the shortfall reappears as
        // duplication, which is the same bug wearing a different hat.
        let mut pages: Vec<(Vec<Vec<u8>>, Vec<String>)> = Vec::new();
        let mut off = 0usize;
        let depth;
        loop {
            p.set_scrollback(off);
            let actual = p.screen().scrollback();
            let mut fmt: Vec<Vec<u8>> = p.screen().rows_formatted(0, cols).collect();
            let mut txt: Vec<String> = p.screen().rows(0, cols).collect();
            let overlap = off.saturating_sub(actual);
            if overlap > 0 {
                fmt.truncate(fmt.len().saturating_sub(overlap));
                txt.truncate(txt.len().saturating_sub(overlap));
            }
            if !fmt.is_empty() {
                pages.push((fmt, txt));
            }
            // `want + step`: one of these pages is the live screen, which is dropped below.
            // Stopping at `want` returned a screenful FEWER rows than the caller asked for, every
            // single time — which a client reasonably reads as "that's all there is".
            if overlap > 0 || pages.len() * step >= want + step {
                depth = if overlap > 0 { actual } else { off + step };
                break;
            }
            off += step;
        }
        pages.reverse(); // oldest screenful first

        let mut fmt: Vec<Vec<u8>> = Vec::new();
        let mut txt: Vec<String> = Vec::new();
        for (f, t) in pages {
            fmt.extend(f);
            txt.extend(t);
        }
        // Drop the LIVE screen. The page at offset 0 is what's on screen right now, and the client
        // already renders that itself — including it here is why the last screenful appeared twice.
        // History is strictly what has scrolled OFF; the seam between the two is exactly here.
        let live = step.min(fmt.len());
        fmt.truncate(fmt.len() - live);
        txt.truncate(txt.len() - live);
        // Blankness must be judged on the PLAIN text: a formatted row for an empty line still
        // carries SGR bytes, so `is_empty()` is never true and the emitted rows were all black
        // space. Trim only the outer blank runs — interior blank lines are real output.
        let first = txt.iter().position(|r| !r.trim().is_empty()).unwrap_or(txt.len());
        let last = txt.iter().rposition(|r| !r.trim().is_empty()).map(|i| i + 1).unwrap_or(first);
        let keep = &fmt[first.min(fmt.len())..last.min(fmt.len())];
        let mut out = Vec::new();
        for row in keep {
            out.extend_from_slice(row);
            out.extend_from_slice(b"\x1b[0m\r\n");
        }
        (out, keep.len(), depth.max(keep.len()))
    }

    /// A window of this pane's transcript, half-open `[from, to)`, clamped to what is retained.
    /// Returns `(from, first, total, rows)`: `from` is the id of `rows[0]` after clamping,
    /// `first` the oldest id still held, `total` the id the next line will get. Those two are
    /// what lets a client draw an honest scrollbar and know when it has genuinely hit the top —
    /// as opposed to inferring it from a short reply, which is how paging used to latch shut.
    ///
    /// An empty window is a legitimate request: `lines(0, 0)` answers "how much is there?"
    /// without transferring anything.
    pub fn lines(&self, from: u64, to: u64) -> (u64, u64, u64, Vec<Vec<u8>>) {
        let Ok(l) = self.line_log.lock() else { return (0, 0, 0, Vec::new()) };
        let total = l.first + l.rows.len() as u64;
        let lo = from.clamp(l.first, total);
        let hi = to.clamp(lo, total);
        let rows = ((lo - l.first) as usize..(hi - l.first) as usize)
            .filter_map(|i| l.rows.get(i).cloned())
            .collect();
        (lo, l.first, total, rows)
    }

    /// Snap back to the live screen (any keystroke does this).
    pub fn scroll_to_live(&mut self) {
        if self.view_offset != 0 {
            self.scroll_view(-(self.view_offset as i64));
        }
    }

    pub fn view_offset(&self) -> usize {
        self.view_offset
    }
}
