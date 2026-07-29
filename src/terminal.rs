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
}

/// How much raw PTY output each pane retains for phone scrollback. ~512 KB is thousands of
/// lines of terminal output while staying trivial next to a session's other state.
const RAW_HISTORY_BYTES: usize = 512 * 1024;

pub struct Term {
    /// The shell has exited; the pane shows a notice until the user closes it.
    pub exited: bool,
    /// Where the shell was spawned (the work journal's cwd). The shell may
    /// `cd` later — a PTY can't see that without shell integration, so this
    /// is honest spawn-time truth, not a live value.
    pub spawn_cwd: Option<std::path::PathBuf>,
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
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
    let reader_raw_tap = raw_tap.clone();
    let reader_raw_tap_on = raw_tap_on.clone();
    let reader_raw_history = raw_history.clone();
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
                        p.process(&buf[..n]);
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
        exited: false,
        spawn_cwd,
        parser,
        writer,
        master: pair.master,
        kill_tx,
        startup_input: Some(StartupInput {
            bytes: Vec::new(),
            marker,
            probe_interval: startup_probe_interval,
            last_probe: None,
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
        if self.prompt_visible() {
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

    fn write_input(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// The shell's exit code, if it has exited and the OS reported one —
    /// available once the process watcher has reported exit.
    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code.lock().ok().and_then(|code| *code)
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
    pub fn history_ansi(&self, lines: usize) -> (Vec<u8>, usize, usize) {
        let Ok(h) = self.raw_history.lock() else { return (Vec::new(), 0, 0) };
        let all: Vec<u8> = h.iter().copied().collect();
        let total = all.iter().filter(|b| **b == b'\n').count();
        // Walk back from the end until we have `lines` newlines, then cut on that boundary.
        let mut seen = 0usize;
        let mut start = all.len();
        for (i, b) in all.iter().enumerate().rev() {
            if *b == b'\n' {
                seen += 1;
                if seen > lines { start = i + 1; break; }
            }
            start = i;
        }
        (all[start..].to_vec(), seen.min(lines), total)
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
