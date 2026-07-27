use std::{collections::HashMap, io, sync::mpsc, time::Duration};

use anyhow::Result;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{backend::CrosstermBackend, layout::Rect, Terminal};

use crate::{
    agent::{self, AgentEvent},
    buffer::{Buffer, BufferId},
    config::{self, chord_of, KeyBindings, KeyChord},
    layout::PaneLayout,
    mode::Mode,
    palette::{self, Action, BarMode, ItemKind, Palette},
    pane::{Pane, PaneContent, PaneId},
    project,
    tab::{Tab, TabId},
    terminal::{self, Term, TermEvent, TermId},
    tuning::{self, Tuning},
    ui,
};

/// One unit of user input, source-agnostic: the real TTY in standalone mode,
/// or deserialized frames from a session client.
pub enum InputEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
    Paste(String),
    /// New client viewport size — handled by the session server (standalone
    /// mode relies on ratatui autoresize).
    Resize(u16, u16),
}

impl InputEvent {
    /// Whether this event, by itself, warrants a repaint. Crossterm enables
    /// any-event mouse tracking, so a pointer sweep delivers a `Moved` per cell —
    /// blanket-repainting on those streams a full rendered frame per motion over
    /// a session socket, which is exactly the traffic `needs_redraw` exists to
    /// avoid. Motion earns its repaint by changing something (`handle_mouse` sets
    /// the flag itself when it does); everything else repaints as before.
    pub fn forces_redraw(&self) -> bool {
        !matches!(self, InputEvent::Mouse(m) if matches!(m.kind, MouseEventKind::Moved))
    }
}

/// Which indexed list a clicked row belongs to. Rows are the one thing a chord
/// can't address directly — the keyboard reaches them by moving a selection, so
/// a click resolves to "select index N, then do what Enter does."
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RowKind {
    Tab,
    Tree,
    Command,
    Workspace,
}

/// What a screen region does when clicked. `Act` is the common case and the
/// point of the whole design: the mouse reaches the action registry through the
/// same `run_action` funnel as chords, the bar, travel mode, and the agent — so
/// the confirm gate and frecency apply to clicking without a line of new code.
/// `PartialEq` so hover/pressed can ask "am I the region under the pointer?".
#[derive(Clone, Debug, PartialEq)]
pub enum HitTarget {
    Act(Action),
    Row(RowKind, usize),
    /// Open mission control — the bar has no `Action` (it is the surface every
    /// action is reached *through*), so it gets its own target.
    OpenBar,
    /// Leave a focused terminal for the editor — the click twin of C-g in a terminal,
    /// which is hardcoded behavior in `handle_terminal` rather than a remappable
    /// action, so (like `OpenBar`) it gets its own target.
    FocusEditor,
    DismissNotice,
    /// A split boundary. `path` addresses the split from the layout root;
    /// `origin`/`span` are the parent area's extent along the split axis, which
    /// is what turns a pointer position into a ratio. Pressing here starts a
    /// drag rather than acting — the only target that does.
    Divider { path: Vec<u8>, vertical: bool, origin: u16, span: u16 },
}

/// One clickable rectangle, recorded by the renderer that drew it.
#[derive(Clone, Debug)]
pub struct HitRegion {
    pub rect: Rect,
    pub target: HitTarget,
}

/// The left file-tree sidebar's state (@ / C-x d).
pub struct FileTree {
    /// Directory the tree is rooted at (`../` re-roots to the parent).
    pub root: std::path::PathBuf,
    /// Folders the user has expanded (full paths).
    pub expanded: std::collections::HashSet<std::path::PathBuf>,
    pub selected: usize,
    /// Type-to-filter query; non-empty switches the sidebar to a fuzzy shortlist.
    pub filter: String,
    /// Show dotfiles (`.env`, `.github`, …). Toggled with `.` on an empty filter;
    /// initial value from `tuning.tree_show_dotfiles`. The `project_ignore` list
    /// (`.git`, `.venv`, `node_modules`, …) stays hidden either way.
    pub show_dotfiles: bool,
}

/// One flattened, visible line in the tree sidebar.
pub struct TreeRow {
    pub path: std::path::PathBuf,
    pub label: String,
    pub depth: usize,
    pub is_dir: bool,
    pub expanded: bool,
    /// The `../` go-up row.
    pub updir: bool,
}

/// A minibuffer prompt (find-file, switch-buffer, incremental search).
#[derive(Clone)]
pub struct Prompt {
    pub label: String,
    pub input: String,
    pub kind: PromptKind,
}

#[derive(Clone, PartialEq)]
pub enum PromptKind {
    SaveAs,
    GotoLine,
    RenameTab,
    RenamePane,
    RenameSession,
    /// Live incremental search (C-s / C-r navigate, Enter accepts, C-g restores).
    Search,
    /// Query-replace, stage 1: the string to find.
    ReplaceFrom,
    /// Query-replace, stage 2: the replacement string.
    ReplaceTo,
    /// Query-replace, stage 3: interactive y/n/! all/q stepping through matches.
    QueryReplace,
    /// Quit with modified buffers: s = save all & quit, q = quit anyway.
    ConfirmQuit,
    /// Confirm a destructive agent-proposed action: y runs it, anything else cancels.
    ConfirmAction(Action),
}

/// Per-terminal watch state (W6): the daemon summarizes a watched pane when it
/// goes quiet or its process exits — even while you're detached.
#[derive(Default)]
pub struct WatchState {
    pub watched: bool,
    pub last_output_tick: u64,
    /// Quiet/exit already fired → don't re-fire until new output arrives.
    pub triggered: bool,
    /// The last one-line verdict (kept for the W7 reattach diff later).
    pub verdict: Option<String>,
    /// `frame_tick` when the current run's output first began (0 = idle) — for the
    /// away-digest duration ("build took 4m12s").
    pub run_started_tick: u64,
    /// Armed by auto-watch (not an explicit C-x w). Auto-watched panes stay
    /// SILENT on the boring lifecycle of an interactive shell — idle at a
    /// prompt, or a clean user-initiated exit — and only produce a verdict when
    /// something is noteworthy (a failure, a blocked prompt, a real exit).
    /// Manual watches summarize everything, as the user explicitly asked.
    pub auto: bool,
    /// The last command mars itself sent to this pane (composer / TYPE:) —
    /// the work journal's `command` field. Raw-typed commands are opaque
    /// PTY bytes; this is what mars honestly knows.
    pub last_command: Option<String>,
    /// Stashed at watch-fire time for the journal: the deterministic evidence
    /// under the LLM verdict (redacted tail excerpt) and the PTY exit code.
    pub fired_excerpt: Option<String>,
    pub fired_exit: Option<i32>,
    /// On-demand summary (workspaces-panel `s`) is in flight — the anti-excess-fire
    /// guard: one at a time per surface.
    pub summ_inflight: bool,
    /// `last_output_tick` captured at the last on-demand summary. A re-summary only
    /// fires once new output has arrived past this point (freshness guard).
    pub summ_output_tick: u64,
}

/// Why a watch fired.
#[derive(Clone, Copy)]
pub enum WatchReason { Exit, Quiet }

/// A request to highlight a buffer, handed to the background syntax worker. Carries a
/// content snapshot so the worker never touches live app state on its thread.
// Without the `syntax` feature the stub worker ignores the payload — don't warn.
#[cfg_attr(not(feature = "syntax"), allow(dead_code))]
pub struct SyntaxJob {
    pub buf_id: BufferId,
    /// The buffer revision this snapshot is of — the result is discarded if the
    /// buffer has moved on by the time it lands.
    pub rev: u64,
    /// Identity of the theme palette the colors were computed for.
    pub palette_id: u64,
    pub code: String,
    pub ext: String,
    pub palette: crate::tuning::Palette,
    /// Last visible line — the worker publishes the chunk covering `0..=this` first.
    pub viewport_bottom: usize,
}

/// A slice of highlight results streaming back from the worker. Chunks for one job
/// arrive in order and tile the buffer (`start_line` = lines already delivered).
pub enum SyntaxEvent {
    Chunk {
        buf_id: BufferId,
        rev: u64,
        palette_id: u64,
        start_line: usize,
        styles: Vec<Vec<ratatui::style::Style>>,
        complete: bool,
    },
}

/// The cached highlight for one buffer: per-character styles per line, tagged with the
/// `(rev, palette)` they were computed for. Rendered as-is even when stale — the old
/// colors bridge the ~½s until a fresh pass for the current revision lands.
pub struct SyntaxCache {
    pub rev: u64,
    pub palette_id: u64,
    pub lines: Vec<Vec<ratatui::style::Style>>,
}

/// A human-readable language name for a file extension, so the agent can be told
/// what language to write. Empty-mapped extensions fall back to "plain text".
fn lang_label(ext: &str) -> &'static str {
    match ext.to_ascii_lowercase().as_str() {
        "rs" => "Rust",
        "py" | "pyi" => "Python",
        "js" | "mjs" | "cjs" => "JavaScript",
        "ts" => "TypeScript",
        "tsx" | "jsx" => "React/JSX",
        "go" => "Go",
        "c" | "h" => "C",
        "cpp" | "cc" | "cxx" | "hpp" => "C++",
        "rb" => "Ruby",
        "java" => "Java",
        "kt" | "kts" => "Kotlin",
        "swift" => "Swift",
        "sh" | "bash" | "zsh" => "shell",
        "json" => "JSON",
        "toml" => "TOML",
        "yaml" | "yml" => "YAML",
        "md" | "markdown" => "Markdown",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "sql" => "SQL",
        "lua" => "Lua",
        "php" => "PHP",
        _ => "plain text",
    }
}

/// Pull the selected cells from a terminal screen as text — reading order,
/// trailing spaces trimmed per row (linear/text-flow, like a normal terminal
/// copy). `a`/`b` are normalized (a ≤ b) screen cells; `last_col` bounds
/// intermediate full rows. Free function so it's unit-testable off a real screen.
pub(crate) fn selection_text_from_screen(
    screen: &vt100::Screen,
    a: (u16, u16),
    b: (u16, u16),
    last_col: u16,
) -> String {
    let mut out = String::new();
    for row in a.0..=b.0 {
        let c0 = if row == a.0 { a.1 } else { 0 };
        let c1 = if row == b.0 { b.1 } else { last_col };
        let mut line = String::new();
        for col in c0..=c1 {
            let ch = screen.cell(row, col).map(|c| c.contents()).unwrap_or_default();
            line.push_str(if ch.is_empty() { " " } else { &ch });
        }
        out.push_str(line.trim_end());
        if row < b.0 {
            out.push('\n');
        }
    }
    out
}

/// A live mouse drag-selection inside a terminal pane. Cells are in the pane's
/// visible screen coordinates (row, col); `ox`/`oy` map a screen cell to the
/// absolute terminal position, `vw`/`vh` bound it. Copied to the clipboard on
/// release; highlighted while dragging.
#[derive(Clone, Copy)]
pub struct TermSel {
    pub tid: TermId,
    pub ox: u16,
    pub oy: u16,
    pub vw: u16,
    pub vh: u16,
    pub anchor: (u16, u16),
    pub end: (u16, u16),
}

/// Coalesces consecutive edits into one undo step: a run of typed characters is
/// one undo, a run of backspaces another; any motion or command breaks the run so
/// the next keystroke starts a fresh checkpoint.
#[derive(Clone, Copy, PartialEq)]
enum EditRun { None, Insert, Delete }

/// A pull-rendered proactive notice — the agent's only path to the screen. The
/// renderer reads it; the agent never pushes. Failures sort before info.
pub struct Notice {
    pub text: String,
    pub kind: NoticeKind,
}

#[derive(PartialEq, PartialOrd, Eq, Ord)]
pub enum NoticeKind { Failure, Blocked, Info }

/// A cheap counts-and-flags snapshot taken at detach; diffed at reattach (W7).
/// Deterministic — the facts (what exited, what changed) are the value; no LLM.
#[derive(Default)]
pub struct Snapshot {
    dirty: std::collections::HashSet<String>,
}

/// One notable thing the daemon observed — accumulated always, rendered as the
/// reattach "Away Digest" (the events since detach). Deterministic by
/// construction; the LLM only ever fills a watch verdict's `text`, via the
/// existing `watch_summary → chat → AgentConfig` seam — so a keyless (or future
/// broker-proxied) box still produces the full digest. This is also the episodic
/// Tier-1 log the memory system will later read.
#[derive(Clone)]
pub struct AwayEvent {
    pub tick: u64,
    pub kind: AwayKind,
    pub text: String,
    /// Run duration in ticks, when known (verdict events).
    pub dur_ticks: Option<u64>,
}

/// Digest section — failures first.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum AwayKind { NeedsYou, Done, Context }

pub struct App {
    pub buffers: HashMap<BufferId, Buffer>,
    pub panes: HashMap<PaneId, Pane>,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    pub mode: Mode,
    pub palette: Option<Palette>,
    pub status_msg: Option<String>,
    pub should_quit: bool,
    pub keys: KeyBindings,
    pub frecency: HashMap<String, u32>,
    // ── Non-modal editing state ──
    pub pending_prefix: Vec<KeyChord>,
    /// frame_tick when the prefix was armed — which-key pops after a short delay.
    pub prefix_tick: u64,
    pub prompt: Option<Prompt>,
    pub kill_ring: Vec<String>,
    /// (buffer, start char idx, len, kill_ring index) of the last yank — M-y target.
    last_yank: Option<(BufferId, usize, usize, usize)>,
    // ── Incremental search ──
    pub search_origin: Option<(usize, usize)>,
    /// Highlighted matches as (row, col, len) — rendered like selections.
    pub search_hl: Vec<(usize, usize, usize)>,
    /// Teleport labels over matches (row, col, label) while picking (Tab).
    pub search_labels: Vec<(usize, usize, char)>,
    /// True when the next key selects a labeled match instead of extending the query.
    pub search_pick: bool,
    // ── Command bar ──
    /// Mode to return to when the bar closes (Terminal keeps its focus).
    pub bar_return: Mode,
    /// Per-action bar-invocation counts — drives the graduation nudge.
    pub bar_uses: HashMap<String, u32>,
    // ── Mouse ──
    /// Pane screen rects from the last render (pane_id, rect).
    pub pane_rects: Vec<(PaneId, Rect)>,
    /// Clickable chrome from the last render, in paint order. Refilled every
    /// frame by `ui::render`; hit-tested back-to-front so overlays drawn last
    /// win over what they cover — the same ordering the renderer already relies
    /// on, rather than a second z-index to keep in sync. A `RefCell` because the
    /// renderers that draw chrome take `&App` (see `Pane::md_rendered_total` for
    /// the same render-derived-through-a-shared-reference pattern).
    pub hits: std::cell::RefCell<Vec<HitRegion>>,
    /// The clickable region the pointer is currently over (resolved against the
    /// last frame's registry). Renderers light it; `None` = over dead space. Only
    /// a *change* earns a repaint — bare motion is otherwise dropped
    /// (`InputEvent::forces_redraw`), so hover costs nothing while the pointer sits.
    pub hovered: Option<HitTarget>,
    /// The region under a held left button, drawn pressed until release — the
    /// tactile "I clicked that" beat. Set on left-down over a hit, cleared on up.
    pub pressed: Option<HitTarget>,
    /// A one-line tooltip a renderer wants shown in the status bar this frame
    /// (e.g. a clipped tab's full name on hover). Refilled every frame like `hits`.
    pub hover_tip: std::cell::RefCell<Option<String>>,
    /// Focused pane's cursor screen position from the last render — anchors the
    /// W3 shell-translate overlay.
    pub cursor_screen: Option<(u16, u16)>,
    // ── System clipboard (None if unavailable, e.g. headless) ──
    clipboard: Option<arboard::Clipboard>,
    /// OSC 52 escape queued by a copy, drained by the driver loop into the
    /// real terminal — the only clipboard that works when the App runs in a
    /// remote daemon (arboard writes to the daemon's machine, which over ssh
    /// is the wrong one, or none at all).
    pending_osc: Option<String>,
    // ── Behavioral tuning knobs (~/.config/mars/tuning.json) ──
    pub tuning: Tuning,
    /// Host-health probes (uptime, load, memory, disk, GPU) for the SPACES line.
    pub health: crate::health::Health,
    /// Show the MARS banner in the empty scratch until the first keypress.
    pub show_splash: bool,
    /// Directory new terminals open in — the parent of the first opened file.
    startup_cwd: Option<std::path::PathBuf>,
    /// Directory `mars` was launched from — the terminal's cwd when no file set one.
    run_cwd: Option<std::path::PathBuf>,
    /// Lazily-built project file index (feeds the tree's type-to-filter).
    project_index: Option<project::Index>,
    /// How often each file has been opened — ranks the filter shortlist.
    pub file_frecency: HashMap<String, u32>,
    /// Left file-tree sidebar (@ / C-x d); visible whenever `tree_open`.
    pub file_tree: Option<FileTree>,
    pub tree_open: bool,
    /// Flattened visible rows, recomputed on every tree mutation.
    pub tree_rows: Vec<TreeRow>,
    // ── Session (daemon) state ──
    /// Set when running inside a session daemon (`mars --session <name>`).
    pub session_name: Option<String>,
    /// Immutable daemon identity used by PTY children across display-name changes.
    pub session_instance_id: Option<String>,
    /// Action::Detach sets this; the session server consumes it.
    pub detach_requested: bool,
    /// Action::RenameSession sets this; the session server consumes it.
    pub rename_session_to: Option<String>,
    // ── LLM agent ──
    pub agent_tx: mpsc::Sender<AgentEvent>,
    pub agent_rx: mpsc::Receiver<AgentEvent>,
    pub agent_pending: bool,
    /// The in-progress streamed reply (rendered live in the ask panel); the
    /// final `Answer` event replaces it with the directive-parsed text.
    pub agent_partial: Option<String>,
    /// Transient notices only (errors, no-key) — answers live in the history.
    pub agent_answer: Option<String>,
    /// Confirm-gated action the model proposed (RUN:/TYPE: directive).
    pub agent_directive: Option<agent::AgentDirective>,
    /// The selection (buf, start, end) captured when an agent query was asked —
    /// the target a proposed refactor would replace.
    pub refactor_target: Option<(BufferId, usize, usize)>,
    /// A code-block the agent returned to replace `refactor_target` (confirm-gated).
    pub refactor_replacement: Option<String>,
    /// The last question asked, replayed verbatim when the model emits a `NEED:`.
    last_question: String,
    /// How many `NEED:` expansions this ask has done (hard cap 1 — never a loop).
    need_depth: u8,
    // ── Watch / notices (W6) ──
    /// Per-terminal watch state, keyed by TermId.
    pub watches: HashMap<TermId, WatchState>,
    /// Proactive notices the renderer reads (failures first). The agent can only append.
    pub notices: Vec<Notice>,
    /// An exit trigger queued from the term_rx drain, fired next tick.
    /// Exit triggers queued from the term_rx drain, fired one per tick. A Vec,
    /// not an Option: in a fleet, several panes can conclude while detached —
    /// a single slot silently dropped all but the last (the fleet bug).
    pending_watch: Vec<(TermId, WatchReason)>,
    /// The save-state restore: built at reattach when things happened while
    /// away, rendered as a full-panel overlay, dismissed by any key.
    pub shift_report: Option<crate::briefing::ShiftReport>,
    /// Whether a client is looking at this session right now (set by the
    /// session server via on_attach/on_detach; standalone is always attached).
    /// Gates the focused-pane LLM skip: never summarize what's being watched.
    client_attached: bool,
    /// State captured at detach; diffed on reattach for the "where was I?" briefing (W7).
    detach_snapshot: Option<Snapshot>,
    /// Append-only ring of notable events (bounded) — the Away Digest source.
    pub away_log: Vec<AwayEvent>,
    /// `frame_tick` at the last detach — the start of the "while away" window.
    detach_tick: Option<u64>,
    /// `frame_tick` of the last actual work keystroke (typing into an editor or
    /// terminal — NOT navigation or the detach chord). The shift report's window
    /// starts here: "since your keyboard went silent," not since a formal detach,
    /// so a job that ran while you sat idle is still summarized.
    last_input_tick: u64,
    /// Window start for a re-summonable digest ("away digest" action).
    pub digest_from_tick: Option<u64>,
    /// The conversation: ("user"/"assistant", text). Survives bar close; C-l clears.
    pub agent_history: Vec<(String, String)>,
    /// Ask-panel scroll: lines scrolled up from the bottom of the transcript.
    pub ask_scroll: usize,
    /// Auto-naming state: one request in flight; tabs already tried.
    pub bg_busy: bool,
    auto_name_attempted: std::collections::HashSet<TabId>,
    /// Shell composer: the query is a ready-to-run command (translated or
    /// typed literally with no key) — the next Enter runs it.
    pub shell_ready: bool,
    /// Eval instrumentation: the `call_id` of the pending shell translation and the
    /// English request that produced it, so accept/edit/reject is logged (and the
    /// accepted command is remembered for corrective memory).
    translate_call_id: Option<u64>,
    translate_request: Option<String>,
    /// Session auto-naming: fired once per still-numeric session.
    session_name_attempted: bool,
    /// Undo coalescing: the kind of edit run currently in progress.
    edit_run: EditRun,
    /// One-shot bypass for the live-terminal close gate: set by the confirm
    /// prompt's `y`, consumed by the close_* functions (so the confirmed action
    /// doesn't re-prompt).
    close_confirmed: bool,
    /// One-shot "always confirm this close, even with no live terminal" — set by
    /// space-warp's d/q/0 (destructive keys adjacent to navigation), consumed at
    /// the top of every close_* fn so it never leaks to a later close.
    force_close_confirm: bool,
    // ── Query-replace (M-%) ──
    replace_from: String,
    replace_to: String,
    /// Char index of the match currently being offered.
    replace_idx: Option<usize>,
    /// Whether the one undo checkpoint for this replace has been taken.
    replace_checkpointed: bool,
    /// Live terminal mouse drag-selection (copied on release).
    pub term_sel: Option<TermSel>,
    /// A left button held down in an editor pane: (pane, press row, press col).
    /// The anchor is NOT set until the pointer actually moves — several call
    /// sites read "anchor is Some" as "a region exists" (Tab indents it, Esc
    /// clears it instead of dismissing a notice), so a plain click must leave
    /// exactly the caret it always did. Unlike a terminal drag this copies
    /// nothing on release: the selection is the same object Shift+arrows makes,
    /// and C-w/M-w still decide what leaves it.
    editor_drag: Option<(PaneId, usize, usize)>,
    /// (when, column, row, consecutive count) of the last press. Terminals report
    /// no click count, so double/triple clicks are timed here against
    /// `tuning.multi_click_ms`.
    last_click: Option<(std::time::Instant, u16, u16, u8)>,
    /// The split boundary currently being dragged: (path, vertical, origin, span).
    border_drag: Option<(Vec<u8>, bool, u16, u16)>,
    pub frame_tick: u64,
    /// Render only when something visible changed. Set on input and by `tick()`
    /// when it moves visible state (terminal output, agent events, the spinner,
    /// a pending which-key panel). Keeps an idle screen from flushing 60×/s —
    /// invisible locally, but pure noise (and input contention) over SSH.
    pub needs_redraw: bool,
    // ── Terminal panes ──
    pub terms: HashMap<TermId, Term>,
    pub term_tx: mpsc::Sender<TermEvent>,
    pub term_rx: mpsc::Receiver<TermEvent>,
    // ── Syntax highlighting (background worker + per-buffer cache) ──
    /// Whether highlighting is showing this session — a per-session toggle
    /// (C-x C-h) seeded from `tuning.syntax_highlight`. Off by default.
    pub syntax_on: bool,
    pub syntax_tx: mpsc::Sender<SyntaxEvent>,
    pub syntax_rx: mpsc::Receiver<SyntaxEvent>,
    pub syntax_cache: HashMap<BufferId, SyntaxCache>,
    /// Buffer → the `(rev, palette_id)` pass we currently want displayed. Set when a
    /// highlight is requested; the drain applies only chunks matching it, so a worker
    /// superseded by a newer edit (or theme change) is ignored — its stale colors are
    /// dropped rather than clobbering the current pass.
    pub syntax_want: HashMap<BufferId, (u64, u64)>,
    next_buffer_id: usize,
    next_pane_id: usize,
    next_tab_id: usize,
    next_term_id: usize,
}

impl App {
    pub fn new(file: Option<String>) -> Result<Self> {
        let keys = config::load();
        let state = PersistedState::load();
        let (agent_tx, agent_rx) = mpsc::channel();
        let (term_tx, term_rx) = mpsc::channel();
        let (syntax_tx, syntax_rx) = mpsc::channel();
        let mut app = App {
            buffers: HashMap::new(),
            panes: HashMap::new(),
            tabs: vec![],
            active_tab: 0,
            mode: Mode::Edit,
            palette: None,
            status_msg: None,
            should_quit: false,
            keys,
            frecency: state.frecency,
            pending_prefix: Vec::new(),
            prefix_tick: 0,
            prompt: None,
            kill_ring: Vec::new(),
            last_yank: None,
            search_origin: None,
            search_hl: Vec::new(),
            search_labels: Vec::new(),
            search_pick: false,
            bar_return: Mode::Edit,
            bar_uses: state.bar_uses,
            pane_rects: Vec::new(),
            hits: std::cell::RefCell::new(Vec::new()),
            hovered: None,
            pressed: None,
            hover_tip: std::cell::RefCell::new(None),
            editor_drag: None,
            last_click: None,
            border_drag: None,
            cursor_screen: None,
            // Env gate keeps selfchecks from touching the user's real clipboard.
            clipboard: if std::env::var("MARS_NO_SYSTEM_CLIPBOARD").is_ok()
                || std::env::var("ARES_NO_SYSTEM_CLIPBOARD").is_ok()
            {
                None
            } else {
                arboard::Clipboard::new().ok()
            },
            pending_osc: None,
            tuning: tuning::load(),
            health: crate::health::Health::new(2),
            show_splash: file.is_none(),
            startup_cwd: file
                .as_ref()
                .and_then(|f| std::path::Path::new(f).parent().map(|p| p.to_path_buf()))
                .filter(|p| !p.as_os_str().is_empty()),
            run_cwd: std::env::current_dir().ok(),
            project_index: None,
            file_frecency: state.file_frecency,
            file_tree: None,
            tree_open: false,
            tree_rows: Vec::new(),
            session_name: None,
            session_instance_id: None,
            detach_requested: false,
            rename_session_to: None,
            agent_tx,
            agent_rx,
            agent_pending: false,
            agent_partial: None,
            agent_answer: None,
            agent_directive: None,
            refactor_target: None,
            refactor_replacement: None,
            last_question: String::new(),
            need_depth: 0,
            watches: HashMap::new(),
            notices: Vec::new(),
            pending_watch: Vec::new(),
            shift_report: None,
            client_attached: true,
            detach_snapshot: None,
            away_log: Vec::new(),
            detach_tick: None,
            last_input_tick: 0,
            digest_from_tick: None,
            agent_history: Vec::new(),
            ask_scroll: 0,
            bg_busy: false,
            auto_name_attempted: std::collections::HashSet::new(),
            shell_ready: false,
            translate_call_id: None,
            translate_request: None,
            session_name_attempted: false,
            close_confirmed: false,
            force_close_confirm: false,
            term_sel: None,
            needs_redraw: true, // draw the first frame
            edit_run: EditRun::None,
            replace_from: String::new(),
            replace_to: String::new(),
            replace_idx: None,
            replace_checkpointed: false,
            frame_tick: 0,
            terms: HashMap::new(),
            term_tx,
            term_rx,
            syntax_on: false,
            syntax_tx,
            syntax_rx,
            syntax_cache: HashMap::new(),
            syntax_want: HashMap::new(),
            next_buffer_id: 0,
            next_pane_id: 0,
            next_tab_id: 0,
            next_term_id: 0,
        };
        app.syntax_on = app.tuning.syntax_highlight == 1;
        app.health = crate::health::Health::new(app.tuning.health_sample_secs.max(1));
        let buf_id = match file {
            Some(ref path) => app.open_file(path)?,
            None => app.new_scratch(),
        };
        let pane_id = app.alloc_pane(buf_id);
        let tab = Tab::new(app.alloc_tab_id(), "1".into(), pane_id);
        app.tabs.push(tab);
        crate::worklog::compact(app.tuning.worklog_max_lines as usize);
        Ok(app)
    }

    // ── ID allocators ────────────────────────────────────────────────────────

    fn alloc_buf_id(&mut self) -> BufferId {
        let id = self.next_buffer_id;
        self.next_buffer_id += 1;
        id
    }
    fn alloc_pane_id(&mut self) -> PaneId {
        let id = self.next_pane_id;
        self.next_pane_id += 1;
        id
    }
    fn alloc_tab_id(&mut self) -> TabId {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        id
    }

    // ── Buffer management ────────────────────────────────────────────────────

    pub fn new_scratch(&mut self) -> BufferId {
        let id = self.alloc_buf_id();
        self.buffers.insert(id, Buffer::new_scratch(id));
        id
    }

    pub fn open_file(&mut self, path: &str) -> Result<BufferId> {
        let id = self.alloc_buf_id();
        let buf = Buffer::from_file(id, std::path::PathBuf::from(path))?;
        self.buffers.insert(id, buf);
        // First file opened sets the cwd new terminals inherit.
        if self.startup_cwd.is_none() {
            self.startup_cwd = std::path::Path::new(path)
                .parent()
                .map(|p| p.to_path_buf())
                .filter(|p| !p.as_os_str().is_empty());
        }
        *self.file_frecency.entry(path.to_string()).or_insert(0) += 1;
        Ok(id)
    }

    /// Seed the project index directly (selfcheck only — bypasses the fs walk).
    pub fn set_project_index_for_test(&mut self, root: std::path::PathBuf, files: Vec<String>) {
        self.project_index = Some(project::Index { root, files });
    }

    /// Build the project index on first use (lazy); returns its root + files.
    fn ensure_project_index(&mut self) -> &project::Index {
        if self.project_index.is_none() {
            let root = self
                .startup_cwd
                .clone()
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let root = project::project_root(&root);
            let idx = project::Index::build(
                root,
                self.tuning.project_index_max,
                &self.tuning.project_ignore,
            );
            self.project_index = Some(idx);
        }
        self.project_index.as_ref().unwrap()
    }

    // ── Pane management ──────────────────────────────────────────────────────

    fn alloc_pane(&mut self, buffer_id: BufferId) -> PaneId {
        let id = self.alloc_pane_id();
        self.panes.insert(id, Pane::new(buffer_id));
        id
    }

    // ── Focus helpers ────────────────────────────────────────────────────────

    pub fn tab(&self) -> &Tab {
        &self.tabs[self.active_tab]
    }
    pub fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.active_tab]
    }
    pub fn focused_pane_id(&self) -> PaneId {
        self.tabs[self.active_tab].focused_pane
    }
    pub fn focused_pane(&self) -> &Pane {
        let id = self.focused_pane_id();
        self.panes.get(&id).unwrap()
    }
    pub fn focused_pane_mut(&mut self) -> &mut Pane {
        let id = self.focused_pane_id();
        self.panes.get_mut(&id).unwrap()
    }
    /// The status of one pane's surface — the single source every view (tab label,
    /// pane border, board row) reads, so no two surfaces of the monitor can ever
    /// disagree. A terminal's status is its watch verdict (LLM- or tier-0-authored),
    /// else Running while it produces output, else its exit outcome, else idle.
    /// Editors carry no run status.
    pub fn pane_verdict(&self, pane_id: PaneId) -> crate::briefing::Verdict {
        use crate::briefing::Verdict;
        let Some(pane) = self.panes.get(&pane_id) else { return Verdict::Context };
        let tid = match pane.content {
            PaneContent::Terminal(tid) => tid,
            PaneContent::Editor(_) => return Verdict::Context,
        };
        if let Some(w) = self.watches.get(&tid) {
            if let Some(v) = &w.verdict {
                return crate::briefing::classify(v, Verdict::Running);
            }
            // Running means producing output RIGHT NOW — not "has ever produced
            // output." run_started_tick stays set forever (it anchors the duration
            // clock), so gate on recent activity using the watch's own quiet window;
            // a shell idling at a prompt has gone quiet and reads as Context, not a
            // green "running" lie.
            if w.run_started_tick > 0 {
                let quiet_ticks =
                    self.tuning.watch_quiet_secs * 1000 / self.tuning.poll_interval_ms.max(1);
                if self.frame_tick.saturating_sub(w.last_output_tick) < quiet_ticks.max(1) {
                    return Verdict::Running;
                }
            }
        }
        if let Some(t) = self.terms.get(&tid) {
            if t.exited {
                return match t.exit_code() {
                    Some(0) | None => Verdict::Done,
                    Some(_) => Verdict::Failed,
                };
            }
        }
        Verdict::Context // idle / quiet
    }
    /// A tab's aggregate status: worst-wins across its panes, needs-you first — so a
    /// tab with any blocked/failed pane reads warm even if its other panes are fine.
    pub fn tab_status(&self, tab: &Tab) -> crate::briefing::Verdict {
        use crate::briefing::Verdict;
        tab.layout
            .pane_ids()
            .into_iter()
            .map(|id| self.pane_verdict(id))
            .max_by_key(|v| v.rank())
            .unwrap_or(Verdict::Context)
    }

    /// Show the Workspaces column of the command board when there is more than one
    /// workspace, or something needs you — a solo single-tab session keeps the plain
    /// launcher (scale prominence with fleet size).
    pub fn bar_show_workspaces(&self) -> bool {
        use crate::briefing::Verdict;
        self.tabs.len() >= 2
            || self
                .tabs
                .iter()
                .any(|t| matches!(self.tab_status(t), Verdict::Blocked | Verdict::Failed))
    }

    /// The Workspaces column: ONE row per workspace (tab), ranked needs-you first,
    /// colored by the tab's aggregate status. A switcher AND a status board — it
    /// lists ALL workspaces (jump anywhere). Deliberately NOT filtered by the query,
    /// so the panel stays a static, fixed-height box while the command list filters.
    pub fn bar_workspace_rows(&self) -> Vec<crate::palette::PaletteRow> {
        use crate::palette::{ItemKind, PaletteRow, SurfaceRef};
        let ms = self.tuning.poll_interval_ms.max(1);
        let now = self.frame_tick;
        let mut ranked: Vec<(u8, usize, PaletteRow)> = Vec::new();
        for (ti, tab) in self.tabs.iter().enumerate() {
            // The informative workspace name (falls back to a numbered terminal /
            // filename for unnamed or numeric tabs), so it reads "terminal 2" /
            // "main.rs", never a bare "1".
            let name = crate::ui::workspace_name(self, tab, ti + 1);
            let v = self.tab_status(tab);
            // The tab's most-severe pane carries the why-line and the jump target.
            let worst = tab
                .layout
                .pane_ids()
                .into_iter()
                .max_by_key(|pid| self.pane_verdict(*pid).rank())
                .unwrap_or(tab.focused_pane);
            let (_pname, why, age) = self.surface_row_parts(worst, now, ms);
            ranked.push((v.rank(), ti, PaletteRow {
                label: name,
                kind: ItemKind::Surface(SurfaceRef {
                    pane_id: worst,
                    tab_index: ti,
                    verdict: v,
                    age_secs: age,
                }),
                description: why,
            }));
        }
        ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1))); // needs-you first, else index
        ranked.into_iter().map(|(_, _, r)| r).collect()
    }

    /// The mobile board as JSON — the Rover seam's `WorkspaceRow[]` plus the
    /// session name and a timestamp. Built from `bar_workspace_rows()` so the
    /// phone and the command bar read one ranking and one verdict. Pure; only
    /// serialized when a phone is actually subscribed.
    pub fn mobile_board_json(&self) -> String {
        use crate::briefing::Verdict;
        use crate::palette::{ItemKind, PaletteRow};
        let ts = crate::worklog::now_secs();
        let rows: Vec<serde_json::Value> = self
            .bar_workspace_rows()
            .into_iter()
            .filter_map(|r| {
                let PaletteRow { label, description, kind } = r;
                let ItemKind::Surface(s) = kind else { return None };
                let blocked = s.verdict == Verdict::Blocked;
                let prompt = description.clone();
                let mut row = serde_json::json!({
                    "id": s.tab_index.to_string(),
                    "name": label,
                    "verdict": s.verdict.label(), // failed|blocked|done|running|idle
                    "why": description,
                    "ageSecs": s.age_secs,
                });
                if blocked {
                    row["blocked"] = serde_json::json!({
                        "prompt": prompt,
                        "paneId": s.pane_id.to_string(),
                    });
                }
                Some(row)
            })
            .collect();
        serde_json::json!({
            "session": self.session_name.clone().unwrap_or_default(),
            "rows": rows,
            "ts": ts,
        })
        .to_string()
    }

    /// The reattach briefing as JSON — the seam's `Briefing` — when a shift
    /// report exists, else `None` (the phone keeps its last board).
    pub fn mobile_briefing_json(&self) -> Option<String> {
        let report = self.shift_report.as_ref()?;
        let rows: Vec<serde_json::Value> = report
            .rows
            .iter()
            .map(|r| {
                serde_json::json!({
                    "verdict": r.verdict.label(),
                    "tab": r.tab,
                    "text": r.text,
                    "agoSecs": r.ago_secs,
                })
            })
            .collect();
        Some(
            serde_json::json!({
                "awaySecs": report.away_secs,
                "mission": report.mission,
                "narrative": report.narrative,
                "rows": rows,
                "suggestion": report.suggestion,
                "ts": crate::worklog::now_secs(),
            })
            .to_string(),
        )
    }

    /// Name / why-snippet / age for one surface, from its pane + terminal + watch.
    fn surface_row_parts(&self, pid: crate::pane::PaneId, now: u64, ms: u64) -> (String, String, u64) {
        let tid = match self.panes.get(&pid).map(|p| &p.content) {
            Some(PaneContent::Terminal(t)) => Some(*t),
            _ => None,
        };
        let name = crate::ui::pane_name(self, pid);
        let w = tid.and_then(|t| self.watches.get(&t));
        // Honest content: a summary earns its line only when it says something the
        // status doesn't. The LLM verdict when present; else the exit outcome, the
        // running command, or NOTHING for an idle prompt (never "working…").
        let cmd = w.and_then(|w| w.last_command.as_ref()).map(|c| c.trim().to_string());
        let why = if w.map(|w| w.summ_inflight).unwrap_or(false) {
            "summarizing…".to_string()
        } else if let Some(v) = w.and_then(|w| w.verdict.as_ref()) {
            v.lines().next().unwrap_or("").trim().to_string()
        } else if let Some(t) = tid.and_then(|t| self.terms.get(&t)) {
            if t.exited {
                match t.exit_code() {
                    Some(c) if c != 0 => format!("exited · code {c}"),
                    _ => cmd.clone().unwrap_or_default(),
                }
            } else if w.map(|w| w.run_started_tick > 0).unwrap_or(false) {
                cmd.clone().unwrap_or_default() // running → the command, not "working…"
            } else {
                String::new() // idle at a prompt → no summary line
            }
        } else {
            String::new()
        };
        let age = w
            .map(|w| w.run_started_tick)
            .filter(|s| *s > 0)
            .map(|s| now.saturating_sub(s) * ms / 1000)
            .unwrap_or(0);
        (name, why, age)
    }

    /// The Commands column: the launcher / fuzzy results (query-filtered). The
    /// Workspaces column is separate — two ontologies, never interleaved.
    pub fn bar_rows(&self) -> Vec<crate::palette::PaletteRow> {
        self.palette
            .as_ref()
            .map(|p| p.visible_items(&self.frecency))
            .unwrap_or_default()
    }

    /// `↵` (or a click) on a surface row — the safe universal verb: focus its tab +
    /// pane and land in it (cursor in the terminal, ready to answer a blocked
    /// prompt). It never auto-answers or reruns — those stay behind the confirm gate,
    /// per the destructive-action rule; jump the user TO the answer, don't press it.
    fn jump_to_surface(&mut self, s: crate::palette::SurfaceRef) {
        self.palette = None;
        if s.tab_index < self.tabs.len() {
            self.active_tab = s.tab_index;
            if self.tabs[s.tab_index].layout.pane_ids().contains(&s.pane_id) {
                self.tabs[s.tab_index].focused_pane = s.pane_id;
            }
        }
        self.mode = self.mode_for_focused_pane();
    }

    pub fn focused_buf_id(&self) -> BufferId {
        match self.focused_pane().content {
            PaneContent::Editor(buf_id) => buf_id,
            PaneContent::Terminal(_) => {
                // Return first available buffer id for terminal panes
                *self.buffers.keys().next().unwrap_or(&0)
            }
        }
    }
    pub fn focused_buf(&self) -> &Buffer {
        let id = self.focused_buf_id();
        self.buffers.get(&id).unwrap()
    }
    pub fn focused_buf_mut(&mut self) -> &mut Buffer {
        let id = self.focused_buf_id();
        self.buffers.get_mut(&id).unwrap()
    }

    // ── Cursor movement ──────────────────────────────────────────────────────

    pub fn move_up(&mut self) {
        let pane = self.focused_pane();
        if let PaneContent::Terminal(_) = pane.content { return; }
        let (row, affinity, buf_id) = (pane.cursor_row, pane.col_affinity, match pane.content { PaneContent::Editor(id) => id, _ => return });
        if row == 0 {
            return;
        }
        let new_row = row - 1;
        let len = self.buffers[&buf_id].line_len(new_row);
        let p = self.focused_pane_mut();
        p.cursor_row = new_row;
        p.cursor_col = affinity.min(len);
    }

    pub fn move_down(&mut self) {
        let pane = self.focused_pane();
        if let PaneContent::Terminal(_) = pane.content { return; }
        let (row, affinity, buf_id) = (pane.cursor_row, pane.col_affinity, match pane.content { PaneContent::Editor(id) => id, _ => return });
        let line_count = self.buffers[&buf_id].line_count();
        if row + 1 >= line_count {
            return;
        }
        let new_row = row + 1;
        let len = self.buffers[&buf_id].line_len(new_row);
        let p = self.focused_pane_mut();
        p.cursor_row = new_row;
        p.cursor_col = affinity.min(len);
    }

    pub fn move_left(&mut self) {
        let (row, col) = { let p = self.focused_pane(); (p.cursor_row, p.cursor_col) };
        if col > 0 {
            let p = self.focused_pane_mut();
            p.cursor_col = col - 1;
            p.col_affinity = p.cursor_col;
        } else if row > 0 {
            // Wrap to the end of the previous line.
            let buf_id = match self.focused_pane().content { PaneContent::Editor(id) => id, _ => return };
            let prev_len = self.buffers[&buf_id].line_len(row - 1);
            let p = self.focused_pane_mut();
            p.cursor_row = row - 1;
            p.cursor_col = prev_len;
            p.col_affinity = prev_len;
        }
    }

    pub fn move_right(&mut self) {
        let pane = self.focused_pane();
        if let PaneContent::Terminal(_) = pane.content { return; }
        let (row, col, buf_id) = (pane.cursor_row, pane.cursor_col, match pane.content { PaneContent::Editor(id) => id, _ => return });
        let len = self.buffers[&buf_id].line_len(row);
        if col < len {
            let p = self.focused_pane_mut();
            p.cursor_col = col + 1;
            p.col_affinity = p.cursor_col;
        } else {
            // Wrap to the start of the next line, if there is one.
            let last = self.buffers[&buf_id].line_count().saturating_sub(1);
            if row < last {
                let p = self.focused_pane_mut();
                p.cursor_row = row + 1;
                p.cursor_col = 0;
                p.col_affinity = 0;
            }
        }
    }

    pub fn move_line_start(&mut self) {
        let p = self.focused_pane_mut();
        p.cursor_col = 0;
        p.col_affinity = 0;
    }

    pub fn move_line_end(&mut self) {
        let pane = self.focused_pane();
        if let PaneContent::Terminal(_) = pane.content { return; }
        let (row, buf_id) = (pane.cursor_row, match pane.content { PaneContent::Editor(id) => id, _ => return });
        let len = self.buffers[&buf_id].line_len(row);
        let p = self.focused_pane_mut();
        p.cursor_col = len;
        p.col_affinity = len;
    }

    pub fn move_file_start(&mut self) {
        let p = self.focused_pane_mut();
        p.cursor_row = 0;
        p.cursor_col = 0;
        p.col_affinity = 0;
    }

    pub fn move_file_end(&mut self) {
        let pane = self.focused_pane();
        if let PaneContent::Terminal(_) = pane.content { return; }
        let buf_id = match pane.content { PaneContent::Editor(id) => id, _ => return };
        let line_count = self.buffers[&buf_id].line_count();
        let last = line_count.saturating_sub(1);
        let len = self.buffers[&buf_id].line_len(last);
        let p = self.focused_pane_mut();
        p.cursor_row = last;
        p.cursor_col = len;
        p.col_affinity = len;
    }

    // ── Text editing ─────────────────────────────────────────────────────────

    fn insert_char_at_cursor(&mut self, c: char) {
        self.last_input_tick = self.frame_tick; // active work — anchors the away window
        let pane = self.focused_pane();
        let buf_id = match pane.content { PaneContent::Editor(id) => id, _ => return };
        let (row, col) = (pane.cursor_row, pane.cursor_col);
        let char_idx = self.buffers[&buf_id].char_at(row, col);
        {
            let buf = self.buffers.get_mut(&buf_id).unwrap();
            buf.rope.insert_char(char_idx, c);
            buf.mark_edited();
        }
        if c == '\n' {
            self.syntax_split_line(buf_id, row, col); // keep colors aligned across the split
        }
        let p = self.focused_pane_mut();
        if c == '\n' {
            p.cursor_row += 1;
            p.cursor_col = 0;
        } else {
            p.cursor_col += 1;
        }
        p.col_affinity = p.cursor_col;
    }

    /// After a newline, copy the previous line's leading whitespace so the cursor
    /// lands at the same indent (the near-universal editor expectation).
    fn auto_indent(&mut self) {
        let (row, buf_id) = match self.editor_pos() { Some((r, _, id)) => (r, id), None => return };
        if row == 0 {
            return;
        }
        let indent: String = self.buffers[&buf_id]
            .rope
            .line(row - 1)
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        for c in indent.chars() {
            self.insert_char_at_cursor(c);
        }
    }

    fn delete_before_cursor(&mut self) {
        let pane = self.focused_pane();
        let buf_id = match pane.content { PaneContent::Editor(id) => id, _ => return };
        let (row, col) = (pane.cursor_row, pane.cursor_col);
        if col == 0 && row == 0 {
            return;
        }
        let char_idx = self.buffers[&buf_id].char_at(row, col);
        if char_idx == 0 {
            return;
        }
        let new_pos = if col > 0 {
            (row, col - 1)
        } else {
            let prev_len = self.buffers[&buf_id].line_len(row - 1);
            (row - 1, prev_len)
        };
        {
            let buf = self.buffers.get_mut(&buf_id).unwrap();
            buf.rope.remove(char_idx - 1..char_idx);
            buf.mark_edited();
        }
        if col == 0 {
            self.syntax_join_line(buf_id, row); // joined a line up — merge its colors
        }
        let p = self.focused_pane_mut();
        p.cursor_row = new_pos.0;
        p.cursor_col = new_pos.1;
        p.col_affinity = new_pos.1;
    }

    // ── Position helpers ─────────────────────────────────────────────────────

    /// (row, col, buffer) for the focused pane, or None if it hosts a terminal.
    fn editor_pos(&self) -> Option<(usize, usize, BufferId)> {
        let p = self.focused_pane();
        match p.content {
            PaneContent::Editor(id) => Some((p.cursor_row, p.cursor_col, id)),
            PaneContent::Terminal(_) => None,
        }
    }

    fn rowcol_of(&self, buf_id: BufferId, idx: usize) -> (usize, usize) {
        let rope = &self.buffers[&buf_id].rope;
        let idx = idx.min(rope.len_chars());
        let line = rope.char_to_line(idx);
        (line, idx - rope.line_to_char(line))
    }

    fn set_cursor(&mut self, row: usize, col: usize) {
        let p = self.focused_pane_mut();
        p.cursor_row = row;
        p.cursor_col = col;
        p.col_affinity = col;
    }

    // ── Selection ────────────────────────────────────────────────────────────

    fn clear_selection(&mut self) {
        let id = self.focused_pane_id();
        if let Some(p) = self.panes.get_mut(&id) { p.selection_anchor = None; }
    }

    fn begin_or_keep_selection(&mut self) {
        let (r, c) = { let p = self.focused_pane(); (p.cursor_row, p.cursor_col) };
        let p = self.focused_pane_mut();
        if p.selection_anchor.is_none() { p.selection_anchor = Some((r, c)); }
    }

    pub fn selection_range(&self) -> Option<(BufferId, usize, usize)> {
        let p = self.focused_pane();
        let anchor = p.selection_anchor?;
        let buf_id = match p.content { PaneContent::Editor(id) => id, _ => return None };
        let buf = &self.buffers[&buf_id];
        let a = buf.char_at(anchor.0, anchor.1);
        let b = buf.char_at(p.cursor_row, p.cursor_col);
        let (s, e) = if a <= b { (a, b) } else { (b, a) };
        if s == e { None } else { Some((buf_id, s, e)) }
    }

    // Selection-aware movement wrappers.
    fn move_left_sel(&mut self, extend: bool)  { if extend { self.begin_or_keep_selection(); } else { self.clear_selection(); } self.move_left(); }
    fn move_right_sel(&mut self, extend: bool) { if extend { self.begin_or_keep_selection(); } else { self.clear_selection(); } self.move_right(); }
    fn move_up_sel(&mut self, extend: bool)    { if extend { self.begin_or_keep_selection(); } else { self.clear_selection(); } self.move_up(); }
    fn move_down_sel(&mut self, extend: bool)  { if extend { self.begin_or_keep_selection(); } else { self.clear_selection(); } self.move_down(); }
    fn move_line_start_sel(&mut self, extend: bool) { if extend { self.begin_or_keep_selection(); } else { self.clear_selection(); } self.move_line_start(); }
    fn move_line_end_sel(&mut self, extend: bool)   { if extend { self.begin_or_keep_selection(); } else { self.clear_selection(); } self.move_line_end(); }

    /// One page ≈ viewport height minus overlap (fallback before first render).
    fn page_len(&self) -> usize {
        let h = self.focused_pane().view_h;
        if h == 0 { 18 } else { h.saturating_sub(self.tuning.page_overlap).max(1) }
    }
    fn page_up(&mut self) {
        self.clear_selection();
        for _ in 0..self.page_len() { self.move_up(); }
    }
    fn page_down(&mut self) {
        self.clear_selection();
        for _ in 0..self.page_len() { self.move_down(); }
    }

    // ── Word motion (M-f / M-b) ──────────────────────────────────────────────

    fn move_word_forward(&mut self) {
        let (row, col, buf_id) = match self.editor_pos() { Some(x) => x, None => return };
        let (len, mut idx) = {
            let b = &self.buffers[&buf_id];
            (b.rope.len_chars(), b.char_at(row, col))
        };
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        {
            let rope = &self.buffers[&buf_id].rope;
            while idx < len && !is_word(rope.char(idx)) { idx += 1; }
            while idx < len && is_word(rope.char(idx)) { idx += 1; }
        }
        let (r, c) = self.rowcol_of(buf_id, idx);
        self.set_cursor(r, c);
    }

    fn move_word_backward(&mut self) {
        let (row, col, buf_id) = match self.editor_pos() { Some(x) => x, None => return };
        let mut idx = self.buffers[&buf_id].char_at(row, col);
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        {
            let rope = &self.buffers[&buf_id].rope;
            while idx > 0 && !is_word(rope.char(idx - 1)) { idx -= 1; }
            while idx > 0 && is_word(rope.char(idx - 1)) { idx -= 1; }
        }
        let (r, c) = self.rowcol_of(buf_id, idx);
        self.set_cursor(r, c);
    }

    // ── Code-token motion (⌘←/→) ─────────────────────────────────────────────
    // A token is a maximal run of one class — word (alnum/`_`) or punctuation —
    // with whitespace skipped. So `foo.bar(baz)` stops at foo · . · bar · ( · baz
    // · ), which tracks how code reads (identifiers and operators as atoms).

    /// 0 = whitespace, 1 = word (alnum/underscore), 2 = punctuation.
    fn token_class(c: char) -> u8 {
        if c.is_whitespace() { 0 } else if c.is_alphanumeric() || c == '_' { 1 } else { 2 }
    }

    pub fn move_token_forward(&mut self) {
        let (row, col, buf_id) = match self.editor_pos() { Some(x) => x, None => return };
        let (len, mut idx) = {
            let b = &self.buffers[&buf_id];
            (b.rope.len_chars(), b.char_at(row, col))
        };
        {
            let rope = &self.buffers[&buf_id].rope;
            // Consume the current token's run, then any whitespace, landing on the
            // start of the next token.
            if idx < len {
                let c0 = Self::token_class(rope.char(idx));
                if c0 != 0 {
                    while idx < len && Self::token_class(rope.char(idx)) == c0 { idx += 1; }
                }
            }
            while idx < len && Self::token_class(rope.char(idx)) == 0 { idx += 1; }
        }
        let (r, c) = self.rowcol_of(buf_id, idx);
        self.set_cursor(r, c);
    }

    pub fn move_token_backward(&mut self) {
        let (row, col, buf_id) = match self.editor_pos() { Some(x) => x, None => return };
        let mut idx = self.buffers[&buf_id].char_at(row, col);
        {
            let rope = &self.buffers[&buf_id].rope;
            while idx > 0 && Self::token_class(rope.char(idx - 1)) == 0 { idx -= 1; }
            if idx > 0 {
                let c0 = Self::token_class(rope.char(idx - 1));
                while idx > 0 && Self::token_class(rope.char(idx - 1)) == c0 { idx -= 1; }
            }
        }
        let (r, c) = self.rowcol_of(buf_id, idx);
        self.set_cursor(r, c);
    }

    fn move_token_sel(&mut self, forward: bool, extend: bool) {
        if extend { self.begin_or_keep_selection(); } else { self.clear_selection(); }
        if forward { self.move_token_forward(); } else { self.move_token_backward(); }
    }

    // ── Structural jumps (C-x [ ] { } m) ─────────────────────────────────────

    fn line_is_blank(b: &Buffer, r: usize) -> bool {
        b.rope.line(r).chars().all(|c| c.is_whitespace())
    }

    /// Jump to the next/prev blank line — fly between code blocks.
    pub fn jump_block(&mut self, forward: bool) {
        let (row, _c, buf_id) = match self.editor_pos() { Some(x) => x, None => return };
        let n = self.buffers[&buf_id].line_count();
        let target = {
            let b = &self.buffers[&buf_id];
            if forward {
                let mut r = row + 1;
                while r < n && Self::line_is_blank(b, r) { r += 1; }
                while r < n && !Self::line_is_blank(b, r) { r += 1; }
                r.min(n.saturating_sub(1))
            } else {
                let mut r = row.saturating_sub(1);
                while r > 0 && Self::line_is_blank(b, r) { r -= 1; }
                while r > 0 && !Self::line_is_blank(b, r) { r -= 1; }
                r
            }
        };
        self.clear_selection();
        self.set_cursor(target, 0);
    }

    /// Jump to the next/prev top-level definition (column-0 keyword heuristic).
    pub fn jump_symbol(&mut self, forward: bool) {
        let (row, _c, buf_id) = match self.editor_pos() { Some(x) => x, None => return };
        let n = self.buffers[&buf_id].line_count();
        const KWS: &[&str] = &[
            "fn ", "pub fn", "pub(", "pub struct", "pub enum", "def ", "class ", "impl",
            "struct ", "enum ", "trait ", "mod ", "type ", "func ", "function ",
            "interface ", "async fn", "export ", "const fn",
        ];
        let is_def = |b: &Buffer, r: usize| -> bool {
            let line: String = b.rope.line(r).chars().collect();
            let t = line.trim_start();
            KWS.iter().any(|k| t.starts_with(k))
        };
        let target = {
            let b = &self.buffers[&buf_id];
            if forward {
                let mut r = row + 1;
                while r < n && !is_def(b, r) { r += 1; }
                (r < n).then_some(r)
            } else if row == 0 {
                None
            } else {
                let mut r = row - 1;
                loop {
                    if is_def(b, r) { break Some(r); }
                    if r == 0 { break None; }
                    r -= 1;
                }
            }
        };
        if let Some(r) = target {
            self.clear_selection();
            self.set_cursor(r, 0);
        }
    }

    /// Jump to the bracket matching the one at (or just before) the cursor.
    pub fn match_bracket(&mut self) {
        let (row, col, buf_id) = match self.editor_pos() { Some(x) => x, None => return };
        const OPENS: [char; 3] = ['(', '[', '{'];
        const CLOSES: [char; 3] = [')', ']', '}'];
        let target = {
            let rope = &self.buffers[&buf_id].rope;
            let len = rope.len_chars();
            let cur = self.buffers[&buf_id].char_at(row, col);
            // Find a bracket: the char under the cursor, scanning to end of line;
            // else the char just before the cursor.
            let mut found = None;
            let mut j = cur;
            while j < len {
                let c = rope.char(j);
                if c == '\n' { break; }
                if OPENS.contains(&c) || CLOSES.contains(&c) { found = Some((j, c)); break; }
                j += 1;
            }
            if found.is_none() && cur > 0 {
                let c = rope.char(cur - 1);
                if OPENS.contains(&c) || CLOSES.contains(&c) { found = Some((cur - 1, c)); }
            }
            found.and_then(|(pos, c)| {
                if let Some(oi) = OPENS.iter().position(|&o| o == c) {
                    let (open, close) = (c, CLOSES[oi]);
                    let mut depth = 1i32;
                    let mut k = pos + 1;
                    while k < len {
                        let ch = rope.char(k);
                        if ch == open { depth += 1; }
                        else if ch == close { depth -= 1; if depth == 0 { return Some(k); } }
                        k += 1;
                    }
                    None
                } else if let Some(ci) = CLOSES.iter().position(|&cc| cc == c) {
                    let (open, close) = (OPENS[ci], c);
                    let mut depth = 1i32;
                    let mut k = pos;
                    while k > 0 {
                        k -= 1;
                        let ch = rope.char(k);
                        if ch == close { depth += 1; }
                        else if ch == open { depth -= 1; if depth == 0 { return Some(k); } }
                    }
                    None
                } else {
                    None
                }
            })
        };
        if let Some(idx) = target {
            let (r, c) = self.rowcol_of(buf_id, idx);
            self.clear_selection();
            self.set_cursor(r, c);
        }
    }

    /// The bracket at (or just before) the cursor and its match, as `(row, col)`
    /// pairs — for passive highlighting in the editor render. Non-mutating; `None`
    /// when the cursor isn't on/next to a bracket or the pair is unbalanced.
    pub fn bracket_pair(&self) -> Option<((usize, usize), (usize, usize))> {
        let (row, col, buf_id) = self.editor_pos()?;
        const OPENS: [char; 3] = ['(', '[', '{'];
        const CLOSES: [char; 3] = [')', ']', '}'];
        let rope = &self.buffers.get(&buf_id)?.rope;
        let len = rope.len_chars();
        let cur = self.buffers[&buf_id].char_at(row, col);
        // A bracket under the cursor (scan to end of line), else the char just before.
        let mut found = None;
        let mut j = cur;
        while j < len {
            let c = rope.char(j);
            if c == '\n' { break; }
            if OPENS.contains(&c) || CLOSES.contains(&c) { found = Some((j, c)); break; }
            j += 1;
        }
        if found.is_none() && cur > 0 {
            let c = rope.char(cur - 1);
            if OPENS.contains(&c) || CLOSES.contains(&c) { found = Some((cur - 1, c)); }
        }
        let (pos, c) = found?;
        let match_idx = if let Some(oi) = OPENS.iter().position(|&o| o == c) {
            let (open, close) = (c, CLOSES[oi]);
            let (mut depth, mut k) = (1i32, pos + 1);
            loop {
                if k >= len { return None; }
                let ch = rope.char(k);
                if ch == open { depth += 1; } else if ch == close { depth -= 1; if depth == 0 { break k; } }
                k += 1;
            }
        } else if let Some(ci) = CLOSES.iter().position(|&cc| cc == c) {
            let (open, close) = (OPENS[ci], c);
            let (mut depth, mut k) = (1i32, pos);
            loop {
                if k == 0 { return None; }
                k -= 1;
                let ch = rope.char(k);
                if ch == close { depth += 1; } else if ch == open { depth -= 1; if depth == 0 { break k; } }
            }
        } else {
            return None;
        };
        Some((self.rowcol_of(buf_id, pos), self.rowcol_of(buf_id, match_idx)))
    }

    // ── Kill-ring editing (C-d / C-k / C-w / M-w / C-y) ──────────────────────

    /// Every kill/copy lands in the kill-ring AND the system clipboard —
    /// copy in Ares, paste in the browser.
    fn push_kill(&mut self, text: String) {
        self.clipboard_export(&text);
        self.kill_ring.push(text);
    }

    /// Set the system clipboard by every channel available: arboard for the
    /// local case, and an OSC 52 escape for the terminal itself — which rides
    /// the rendered output through the session socket and ssh, so a copy in a
    /// remote daemon lands on the clipboard of the machine the user is
    /// actually sitting at.
    fn clipboard_export(&mut self, text: &str) {
        if let Some(cb) = self.clipboard.as_mut() {
            let _ = cb.set_text(text.to_string());
        }
        use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
        self.pending_osc = Some(format!("\x1b]52;c;{}\x07", B64.encode(text.as_bytes())));
    }

    /// Drain the queued OSC 52 escape — called by the driver loops after each
    /// draw, written raw to whatever the frames themselves go to.
    pub fn take_osc(&mut self) -> Option<String> {
        self.pending_osc.take()
    }

    /// Insert a block of text at the cursor (one undo chunk, replaces selection).
    fn insert_text(&mut self, text: &str) {
        if self.editor_pos().is_none() {
            return;
        }
        self.focused_buf_mut().checkpoint();
        self.delete_selection();
        for ch in text.chars() {
            self.insert_char_at_cursor(ch);
        }
    }

    /// C-v — paste from the system clipboard (kill-ring head as fallback).
    fn paste_clipboard(&mut self) {
        let text = self
            .clipboard
            .as_mut()
            .and_then(|cb| cb.get_text().ok())
            .filter(|t| !t.is_empty())
            .or_else(|| self.kill_ring.last().cloned());
        match text {
            Some(t) => self.insert_text(&t),
            None => self.status_msg = Some("Clipboard empty".into()),
        }
    }

    /// Bracketed paste from the host terminal (Cmd+V etc.) — routed by mode.
    /// Write raw bytes to a specific pane's terminal (the Rover phone answering a
    /// prompt). Pane-targeted and non-takeover — the desktop client's focus is
    /// untouched, and a non-terminal pane is a silent no-op.
    pub fn write_to_pane(&mut self, pane_id: crate::pane::PaneId, data: &str) {
        let tid = match self.panes.get(&pane_id).map(|p| &p.content) {
            Some(PaneContent::Terminal(t)) => *t,
            _ => return,
        };
        if let Some(t) = self.terms.get_mut(&tid) {
            t.send_bytes(data.as_bytes());
        }
    }

    pub fn paste_text(&mut self, s: &str) {
        match self.mode {
            Mode::Terminal => {
                if let PaneContent::Terminal(tid) = self.focused_pane().content {
                    if let Some(t) = self.terms.get_mut(&tid) {
                        // Re-wrap if the inner app requested bracketed paste.
                        let wrap = t.screen().bracketed_paste();
                        if wrap { t.send_bytes(b"\x1b[200~"); }
                        t.send_bytes(s.as_bytes());
                        if wrap { t.send_bytes(b"\x1b[201~"); }
                    }
                }
            }
            Mode::Bar => {
                let clean: String = s.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
                if let Some(p) = self.palette.as_mut() {
                    p.query.push_str(&clean);
                }
            }
            Mode::Prompt => {
                let clean: String = s.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
                let is_search = if let Some(p) = self.prompt.as_mut() {
                    p.input.push_str(&clean);
                    p.kind == PromptKind::Search
                } else {
                    false
                };
                if is_search {
                    let q = self.prompt.as_ref().map(|p| p.input.clone()).unwrap_or_default();
                    self.update_isearch(&q);
                }
            }
            _ => self.insert_text(s),
        }
    }

    fn delete_char_forward(&mut self) {
        let (row, col, buf_id) = match self.editor_pos() { Some(x) => x, None => return };
        let buf = self.buffers.get_mut(&buf_id).unwrap();
        let idx = buf.char_at(row, col);
        if idx < buf.rope.len_chars() {
            buf.checkpoint();
            buf.rope.remove(idx..idx + 1);
            buf.mark_edited();
        }
    }

    fn kill_line(&mut self) {
        let (row, col, buf_id) = match self.editor_pos() { Some(x) => x, None => return };
        let killed = {
            let buf = self.buffers.get_mut(&buf_id).unwrap();
            buf.checkpoint();
            let start = buf.char_at(row, col);
            let eol = buf.line_len(row);
            let end = if col >= eol { (start + 1).min(buf.rope.len_chars()) } else { buf.char_at(row, eol) };
            if end > start {
                let k = buf.rope.slice(start..end).to_string();
                buf.rope.remove(start..end);
                buf.mark_edited();
                k
            } else {
                String::new()
            }
        };
        if !killed.is_empty() { self.push_kill(killed); }
    }

    fn kill_region(&mut self) {
        if let Some((buf_id, s, e)) = self.selection_range() {
            let killed = {
                let buf = self.buffers.get_mut(&buf_id).unwrap();
                buf.checkpoint();
                let k = buf.rope.slice(s..e).to_string();
                buf.rope.remove(s..e);
                buf.mark_edited();
                k
            };
            self.push_kill(killed);
            let (r, c) = self.rowcol_of(buf_id, s);
            self.set_cursor(r, c);
            self.clear_selection();
        }
    }

    fn copy_region(&mut self) {
        if let Some((buf_id, s, e)) = self.selection_range() {
            let text = self.buffers[&buf_id].rope.slice(s..e).to_string();
            self.push_kill(text);
            self.status_msg = Some("Copied".into());
        } else if let Some((row, _, buf_id)) = self.editor_pos() {
            // No selection → copy the whole current line (VS Code behavior).
            let line = self.buffers[&buf_id].line_str(row);
            if !line.is_empty() {
                self.push_kill(line);
                self.status_msg = Some("Copied line".into());
            }
        }
        self.clear_selection();
    }

    fn yank(&mut self) {
        if let Some(text) = self.kill_ring.last().cloned() {
            let start = match self.editor_pos() {
                Some((r, c, id)) => (id, self.buffers[&id].char_at(r, c)),
                None => return,
            };
            self.focused_buf_mut().checkpoint();
            for ch in text.chars() { self.insert_char_at_cursor(ch); }
            self.last_yank =
                Some((start.0, start.1, text.chars().count(), self.kill_ring.len() - 1));
        }
    }

    /// M-y — replace the text just yanked with the previous kill (rotating).
    fn yank_pop(&mut self) {
        let (buf_id, start, len, ridx) = match self.last_yank {
            Some(x) => x,
            None => {
                self.status_msg = Some("Previous command was not a yank".into());
                return;
            }
        };
        // Only valid while the cursor still sits at the end of the yanked text.
        let at_end = self
            .editor_pos()
            .map(|(r, c, id)| id == buf_id && self.buffers[&id].char_at(r, c) == start + len)
            .unwrap_or(false);
        if !at_end || self.kill_ring.len() < 2 {
            self.status_msg = Some("Previous command was not a yank".into());
            return;
        }
        let new_ridx = if ridx == 0 { self.kill_ring.len() - 1 } else { ridx - 1 };
        let text = self.kill_ring[new_ridx].clone();
        {
            let buf = self.buffers.get_mut(&buf_id).unwrap();
            buf.rope.remove(start..start + len);
            buf.mark_edited();
        }
        let (r, c) = self.rowcol_of(buf_id, start);
        self.set_cursor(r, c);
        for ch in text.chars() { self.insert_char_at_cursor(ch); }
        self.last_yank = Some((buf_id, start, text.chars().count(), new_ridx));
    }

    /// M-d / M-Backspace — kill from the cursor to a word boundary.
    fn kill_word(&mut self, forward: bool) {
        let (row, col, buf_id) = match self.editor_pos() { Some(x) => x, None => return };
        let from = self.buffers[&buf_id].char_at(row, col);
        if forward { self.move_word_forward(); } else { self.move_word_backward(); }
        let (row2, col2, _) = match self.editor_pos() { Some(x) => x, None => return };
        let to = self.buffers[&buf_id].char_at(row2, col2);
        let (s, e) = if from <= to { (from, to) } else { (to, from) };
        if s == e { return; }
        let killed = {
            let buf = self.buffers.get_mut(&buf_id).unwrap();
            buf.checkpoint();
            let k = buf.rope.slice(s..e).to_string();
            buf.rope.remove(s..e);
            buf.mark_edited();
            k
        };
        let (r, c) = self.rowcol_of(buf_id, s);
        self.set_cursor(r, c);
        self.push_kill(killed);
    }

    /// C-l — center the viewport on the cursor line.
    fn recenter(&mut self) {
        let p = self.focused_pane_mut();
        let half = (p.view_h / 2).max(1);
        p.scroll_row = p.cursor_row.saturating_sub(half);
    }

    /// C-x h — select the whole buffer (anchor at start, cursor at end).
    fn select_all(&mut self) {
        if self.editor_pos().is_none() { return; }
        self.focused_pane_mut().selection_anchor = Some((0, 0));
        self.move_file_end();
    }

    // ── Buffers & windows ────────────────────────────────────────────────────

    fn kill_buffer(&mut self) {
        let buf_id = match self.editor_pos() { Some((_, _, id)) => id, None => return };
        if self.buffers.len() <= 1 {
            self.status_msg = Some("Only buffer".into());
            return;
        }
        let other = self.buffers.keys().copied().find(|&id| id != buf_id);
        self.buffers.remove(&buf_id);
        if let Some(o) = other {
            // Retarget EVERY pane showing the killed buffer, not just the
            // focused one — a stale BufferId would panic on next focus.
            for pane in self.panes.values_mut() {
                if matches!(pane.content, PaneContent::Editor(id) if id == buf_id) {
                    pane.content = PaneContent::Editor(o);
                    pane.buffer_id = o;
                    pane.cursor_row = 0; pane.cursor_col = 0; pane.scroll_row = 0;
                    pane.selection_anchor = None;
                }
            }
        }
    }

    fn delete_other_windows(&mut self) {
        let force = std::mem::take(&mut self.force_close_confirm);
        let focused = self.focused_pane_id();
        let victims: Vec<PaneId> =
            self.tab().layout.pane_ids().into_iter().filter(|id| *id != focused).collect();
        if self.confirm_close(
            self.live_terms_in(&victims),
            force,
            Action::DeleteOtherWindows,
            "the other panes",
        ) {
            return;
        }
        self.reap_panes(&victims);
        let tab = self.tab_mut();
        tab.layout = PaneLayout::Single(focused);
        tab.focused_pane = focused;
    }

    // ── Incremental search ───────────────────────────────────────────────────

    /// All char indices where `needle` occurs in the focused buffer.
    fn find_matches(&self, buf_id: BufferId, needle: &str) -> Vec<usize> {
        let text: Vec<char> = self.buffers[&buf_id].rope.chars().collect();
        let pat: Vec<char> = needle.chars().collect();
        if pat.is_empty() || pat.len() > text.len() {
            return Vec::new();
        }
        (0..=text.len() - pat.len())
            .filter(|&i| text[i..i + pat.len()] == pat[..])
            .collect()
    }

    /// Refresh match highlights for the live query and jump to the first match
    /// at or after the search origin (wrapping).
    fn update_isearch(&mut self, needle: &str) {
        let buf_id = match self.editor_pos() { Some((_, _, id)) => id, None => return };
        let matches = self.find_matches(buf_id, needle);
        self.search_hl = matches
            .iter()
            .map(|&i| {
                let (r, c) = self.rowcol_of(buf_id, i);
                (r, c, needle.chars().count())
            })
            .collect();
        if needle.is_empty() {
            if let Some((r, c)) = self.search_origin {
                self.set_cursor(r, c);
            }
            return;
        }
        let origin_idx = self
            .search_origin
            .map(|(r, c)| self.buffers[&buf_id].char_at(r, c))
            .unwrap_or(0);
        match matches.iter().find(|&&i| i >= origin_idx).or(matches.first()) {
            Some(&idx) => {
                let (r, c) = self.rowcol_of(buf_id, idx);
                self.set_cursor(r, c);
            }
            None => self.status_msg = Some(format!("Failing I-search: {}", needle)),
        }
    }

    /// C-s / C-r inside isearch — jump to the next/previous match from the cursor.
    fn isearch_step(&mut self, needle: &str, forward: bool) {
        let (row, col, buf_id) = match self.editor_pos() { Some(x) => x, None => return };
        let matches = self.find_matches(buf_id, needle);
        if matches.is_empty() {
            self.status_msg = Some(format!("Failing I-search: {}", needle));
            return;
        }
        let cur = self.buffers[&buf_id].char_at(row, col);
        let idx = if forward {
            *matches.iter().find(|&&i| i > cur).unwrap_or(&matches[0]) // wrap
        } else {
            *matches.iter().rev().find(|&&i| i < cur).unwrap_or(matches.last().unwrap())
        };
        let (r, c) = self.rowcol_of(buf_id, idx);
        self.set_cursor(r, c);
    }

    fn start_isearch(&mut self) {
        let (row, col, _) = match self.editor_pos() {
            Some(x) => x,
            None => {
                self.status_msg = Some("No editor pane here".into());
                return;
            }
        };
        self.search_origin = Some((row, col));
        self.search_hl.clear();
        self.start_prompt(PromptKind::Search, "I-search: ");
    }

    fn end_isearch(&mut self, restore_origin: bool) {
        if restore_origin {
            if let Some((r, c)) = self.search_origin {
                self.set_cursor(r, c);
            }
        }
        self.search_origin = None;
        self.search_hl.clear();
        self.search_labels.clear();
        self.search_pick = false;
    }

    // ── Undo / redo ──────────────────────────────────────────────────────────

    fn do_undo(&mut self) {
        let did = self.focused_buf_mut().undo();
        if did {
            self.status_msg = Some("Undo".into());
        } else {
            self.status_msg = Some("Nothing to undo".into());
        }
        self.clamp_cursor_after_edit();
    }

    fn do_redo(&mut self) {
        let did = self.focused_buf_mut().redo();
        if did {
            self.status_msg = Some("Redo".into());
        } else {
            self.status_msg = Some("Nothing to redo".into());
        }
        self.clamp_cursor_after_edit();
    }

    /// Enter the undo time-travel mode: ←/→ scrub backward/forward, Esc exits.
    /// Only meaningful in an editor pane.
    fn enter_undo_mode(&mut self) {
        if self.editor_pos().is_none() {
            self.status_msg = Some("Undo history works in an editor pane".into());
            return;
        }
        self.edit_run = EditRun::None;
        self.mode = Mode::Undo;
        self.undo_status();
    }

    /// Status line shown in undo mode: how far back / forward you can go.
    fn undo_status(&mut self) {
        let (back, fwd) = self
            .editor_pos()
            .and_then(|(_, _, id)| self.buffers.get(&id))
            .map(|b| b.undo_depth())
            .unwrap_or((0, 0));
        self.status_msg =
            Some(format!("TIME-TRAVEL ◂ {back} back · {fwd} forward ▸   ←/→ step · Home/End all · Esc done"));
    }

    fn handle_undo_mode(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Left | KeyCode::Up | KeyCode::Char('u') => { self.do_undo(); self.undo_status(); }
            KeyCode::Right | KeyCode::Down | KeyCode::Char('r') => { self.do_redo(); self.undo_status(); }
            KeyCode::Home => { while self.focused_buf_mut().undo() {} self.clamp_cursor_after_edit(); self.undo_status(); }
            KeyCode::End  => { while self.focused_buf_mut().redo() {} self.clamp_cursor_after_edit(); self.undo_status(); }
            _ => { self.mode = Mode::Edit; self.status_msg = Some("Undo history closed".into()); }
        }
    }

    fn clamp_cursor_after_edit(&mut self) {
        let pane = self.focused_pane();
        let buf_id = match pane.content { PaneContent::Editor(id) => id, _ => return };
        let line_count = self.buffers[&buf_id].line_count();
        let (row, col) = (pane.cursor_row, pane.cursor_col);
        let new_row = row.min(line_count.saturating_sub(1));
        let new_col = col.min(self.buffers[&buf_id].line_len(new_row));
        let p = self.focused_pane_mut();
        p.cursor_row = new_row;
        p.cursor_col = new_col;
        p.col_affinity = new_col;
    }

    // ── Save ─────────────────────────────────────────────────────────────────

    fn do_save(&mut self) {
        if self.focused_buf().path.is_none() {
            self.start_prompt(PromptKind::SaveAs, "Save as: ");
            return;
        }
        let name = self.focused_buf().name.clone();
        match self.focused_buf_mut().save() {
            Ok(_) => self.status_msg = Some(format!("Saved  {}", name)),
            Err(e) => self.status_msg = Some(format!("Save error: {}", e)),
        }
    }

    /// Quit, but never silently discard unsaved work.
    fn request_quit(&mut self) {
        let dirty = self.buffers.values().filter(|b| b.modified).count();
        if dirty == 0 {
            self.should_quit = true;
        } else {
            self.start_prompt(
                PromptKind::ConfirmQuit,
                &format!("{} modified buffer(s):  s save all & quit · q quit anyway · C-g cancel ", dirty),
            );
        }
    }

    /// Crash-safety: quietly save every modified buffer that has a real path.
    /// Scratch buffers are never touched. Called on a timer and on detach.
    pub fn autosave(&mut self) {
        let mut failed: Vec<String> = Vec::new();
        for buf in self.buffers.values_mut() {
            if buf.modified && buf.path.is_some() {
                if buf.save().is_err() {
                    failed.push(buf.name.clone());
                }
            }
        }
        // Autosave is silent when it works — but a failing write (disk full,
        // permission, path gone) must survive: the status line is cleared by the
        // next keypress, so a fast typist would never see it. Route it to the
        // notices queue, which persists until Esc-dismissed. De-dupe so repeated
        // autosave ticks don't stack the same warning.
        if !failed.is_empty() {
            let text = format!("⚠ autosave FAILED: {} — check disk/permissions", failed.join(", "));
            if !self.notices.iter().any(|n| n.text == text) {
                self.notices.push(Notice { text, kind: NoticeKind::Failure });
                self.notices.sort_by(|a, b| a.kind.cmp(&b.kind));
            }
        }
    }

    /// Save every modified buffer that has a path. Returns names left unsaved.
    fn save_all(&mut self) -> Vec<String> {
        let mut unsaved = Vec::new();
        for buf in self.buffers.values_mut() {
            if buf.modified {
                if buf.path.is_some() {
                    if buf.save().is_err() {
                        unsaved.push(buf.name.clone());
                    }
                } else {
                    unsaved.push(buf.name.clone());
                }
            }
        }
        unsaved
    }

    // ── Split panes ──────────────────────────────────────────────────────────

    pub fn split_horizontal(&mut self) {
        if self.tab().layout.count() >= self.tuning.max_panes {
            self.status_msg = Some(format!("Max {} panes", self.tuning.max_panes));
            return;
        }
        let focused = self.focused_pane_id();
        let buf_id = match self.focused_pane().content { PaneContent::Editor(id) => id, _ => self.new_scratch() };
        let new_id = self.alloc_pane(buf_id);
        let (r, c, s) = {
            let p = &self.panes[&focused];
            (p.cursor_row, p.cursor_col, p.scroll_row)
        };
        {
            let p = self.panes.get_mut(&new_id).unwrap();
            p.cursor_row = r;
            p.cursor_col = c;
            p.scroll_row = s;
        }
        let tab = self.tab_mut();
        tab.layout.hsplit(focused, new_id);
        tab.focused_pane = new_id;
        self.status_msg = Some("Split ─".into());
    }

    pub fn split_vertical(&mut self) {
        if self.tab().layout.count() >= self.tuning.max_panes {
            self.status_msg = Some(format!("Max {} panes", self.tuning.max_panes));
            return;
        }
        let focused = self.focused_pane_id();
        let buf_id = match self.focused_pane().content { PaneContent::Editor(id) => id, _ => self.new_scratch() };
        let new_id = self.alloc_pane(buf_id);
        let (r, c, s) = {
            let p = &self.panes[&focused];
            (p.cursor_row, p.cursor_col, p.scroll_row)
        };
        {
            let p = self.panes.get_mut(&new_id).unwrap();
            p.cursor_row = r;
            p.cursor_col = c;
            p.scroll_row = s;
        }
        let tab = self.tab_mut();
        tab.layout.vsplit(focused, new_id);
        tab.focused_pane = new_id;
        self.status_msg = Some("Split │".into());
    }

    /// How many still-running shells live inside these panes.
    fn live_terms_in(&self, pane_ids: &[PaneId]) -> usize {
        pane_ids
            .iter()
            .filter_map(|pid| self.panes.get(pid))
            .filter_map(|p| match p.content {
                PaneContent::Terminal(tid) => self.terms.get(&tid),
                _ => None,
            })
            .filter(|t| !t.exited)
            .count()
    }

    /// Remove panes AND everything they own: their terminals (killing the shell
    /// via Term::drop) and any watch state — never orphan a running PTY.
    fn reap_panes(&mut self, pane_ids: &[PaneId]) {
        for pid in pane_ids {
            if let Some(p) = self.panes.remove(pid) {
                if let PaneContent::Terminal(tid) = p.content {
                    self.terms.remove(&tid);
                    self.watches.remove(&tid);
                }
            }
        }
    }

    /// Gate a close behind one y/n prompt. Fires when it would kill `live`
    /// running shells (data-loss guard) OR when `force` is set (space-warp's
    /// motor-slip guard). Returns true when the caller should stop and wait.
    fn confirm_close(&mut self, live: usize, force: bool, action: Action, what: &str) -> bool {
        if std::mem::take(&mut self.close_confirmed) || (live == 0 && !force) {
            return false;
        }
        let msg = if live > 0 {
            let plural = if live == 1 { "" } else { "s" };
            format!("Close {what} — {live} running terminal{plural} will be killed. y/n ")
        } else {
            format!("Close {what}? y/n ")
        };
        self.start_prompt(PromptKind::ConfirmAction(action), &msg);
        true
    }

    pub fn close_pane(&mut self) {
        let force = std::mem::take(&mut self.force_close_confirm);
        if self.tab().layout.count() <= 1 {
            return;
        }
        let focused = self.focused_pane_id();
        if self.confirm_close(self.live_terms_in(&[focused]), force, Action::ClosePane, "this pane") {
            return;
        }
        let next = self.tab().layout.next_pane(focused);
        let tab = self.tab_mut();
        tab.layout.remove(focused);
        tab.focused_pane = next;
        self.reap_panes(&[focused]);
    }

    pub fn focus_next_pane(&mut self) {
        let focused = self.focused_pane_id();
        let next = self.tab().layout.next_pane(focused);
        self.tab_mut().focused_pane = next;
    }

    /// M-arrows — focus the nearest pane in a screen direction, using the
    /// real geometry from the last render.
    fn focus_direction(&mut self, dx: i32, dy: i32) {
        let cur = self.focused_pane_id();
        let cur_rect = match self.pane_rects.iter().find(|(id, _)| *id == cur) {
            Some((_, r)) => *r,
            None => { self.focus_next_pane(); return; } // no geometry yet
        };
        let (cx, cy) = (
            cur_rect.x as i32 + cur_rect.width as i32 / 2,
            cur_rect.y as i32 + cur_rect.height as i32 / 2,
        );
        let mut best: Option<(i32, PaneId)> = None;
        for (id, r) in &self.pane_rects {
            if *id == cur { continue; }
            let px = r.x as i32 + r.width as i32 / 2;
            let py = r.y as i32 + r.height as i32 / 2;
            let (ddx, ddy) = (px - cx, py - cy);
            let aligned = if dx != 0 {
                ddx.signum() == dx && ddx.abs() >= ddy.abs()
            } else {
                ddy.signum() == dy && ddy.abs() >= ddx.abs()
            };
            if aligned {
                let dist = ddx.abs() + ddy.abs();
                if best.map(|(d, _)| dist < d).unwrap_or(true) {
                    best = Some((dist, *id));
                }
            }
        }
        if let Some((_, id)) = best {
            self.tab_mut().focused_pane = id;
        }
    }

    /// Grow/shrink the boundary nearest the focused pane (travel +/-).
    fn resize_pane(&mut self, delta: i16) {
        let focused = self.focused_pane_id();
        self.tab_mut().layout.resize(focused, delta);
    }

    /// Toggle zoom on the focused pane (travel z / tmux prefix-z).
    fn toggle_zoom(&mut self) {
        let focused = self.focused_pane_id();
        let tab = self.tab_mut();
        tab.zoomed = if tab.zoomed == Some(focused) { None } else { Some(focused) };
    }

    /// Space-warp movement — one directional grammar over the whole workspace.
    /// Steps to the nearest pane in a screen direction (real geometry); when there
    /// is no pane that way, a horizontal move spills into the adjacent tab. Panes
    /// and tabs are just views of one space, so one set of keys walks all of it.
    fn warp_move(&mut self, dx: i32, dy: i32) {
        let before = self.focused_pane_id();
        self.focus_direction(dx, dy);
        if self.focused_pane_id() == before && dx != 0 {
            if dx < 0 { self.prev_tab(); } else { self.next_tab(); }
        }
    }

    /// Space-warp delete — one key closes the focused view: the pane, or the whole
    /// tab when it is the tab's last pane. Behind the motor-slip confirm because the
    /// key sits right beside the navigation arrows.
    fn close_focused(&mut self) {
        // Short-circuit: a clean editor pane (no unsaved edits) closes without the
        // motor-slip confirm. Only a dirty buffer or a live terminal still gates.
        let clean_editor = match self.focused_pane().content {
            PaneContent::Editor(buf_id) => self.buffers.get(&buf_id).map(|b| !b.modified).unwrap_or(true),
            PaneContent::Terminal(_) => false,
        };
        self.force_close_confirm = !clean_editor;
        if self.tab().layout.count() > 1 {
            self.close_pane();
        } else {
            self.close_tab();
        }
    }

    /// C-x x — move this pane's content into the next pane slot (swap).
    fn swap_pane(&mut self) {
        let a = self.focused_pane_id();
        let b = self.tab().layout.next_pane(a);
        if a == b { return; }
        let snap_a = self.panes.get(&a).unwrap().clone();
        let snap_b = self.panes.get(&b).unwrap().clone();
        for (dst, src) in [(a, &snap_b), (b, &snap_a)] {
            let p = self.panes.get_mut(&dst).unwrap();
            p.content = src.content.clone();
            p.buffer_id = src.buffer_id;
            p.cursor_row = src.cursor_row;
            p.cursor_col = src.cursor_col;
            p.col_affinity = src.col_affinity;
            p.scroll_row = src.scroll_row;
            p.selection_anchor = src.selection_anchor;
        }
        // Focus follows the moved content.
        self.tab_mut().focused_pane = b;
        self.status_msg = Some("Pane moved".into());
    }

    pub fn focus_prev_pane(&mut self) {
        let focused = self.focused_pane_id();
        let prev = self.tab().layout.prev_pane(focused);
        self.tab_mut().focused_pane = prev;
    }

    // ── Tab management ───────────────────────────────────────────────────────

    pub fn new_tab(&mut self) {
        let buf_id = self.new_scratch();
        let pane_id = self.alloc_pane(buf_id);
        let n = self.tabs.len() + 1;
        let id = self.alloc_tab_id();
        let tab = Tab::new(id, n.to_string(), pane_id);
        self.tabs.push(tab);
        self.active_tab = self.tabs.len() - 1;
    }

    /// Open a file in a NEW tab and switch to it — used when a nested `mars <file>`
    /// (run from a terminal pane inside this session) routes the open here instead
    /// of launching a second Mars.
    pub fn open_file_in_new_tab(&mut self, path: &str) {
        match self.open_file(path) {
            Ok(buf_id) => {
                let pane_id = self.alloc_pane(buf_id);
                let id = self.alloc_tab_id();
                let name = std::path::Path::new(path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(str::to_string)
                    .unwrap_or_else(|| (self.tabs.len() + 1).to_string());
                let tab = Tab::new(id, name.clone(), pane_id);
                self.tabs.push(tab);
                self.active_tab = self.tabs.len() - 1;
                self.mode = Mode::Edit;
                self.status_msg = Some(format!("Opened {name}"));
            }
            Err(e) => self.status_msg = Some(format!("Open failed: {e}")),
        }
    }

    pub fn close_tab(&mut self) {
        let force = std::mem::take(&mut self.force_close_confirm);
        if self.tabs.len() == 1 {
            self.request_quit(); // quit has its own dirty-buffer gate
            return;
        }
        let pane_ids = self.tab().layout.pane_ids();
        if self.confirm_close(self.live_terms_in(&pane_ids), force, Action::CloseTab, "this tab") {
            return;
        }
        self.reap_panes(&pane_ids);
        self.tabs.remove(self.active_tab);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
    }

    pub fn next_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active_tab = (self.active_tab + 1) % self.tabs.len();
        }
    }

    pub fn prev_tab(&mut self) {
        if !self.tabs.is_empty() {
            self.active_tab = if self.active_tab == 0 {
                self.tabs.len() - 1
            } else {
                self.active_tab - 1
            };
        }
    }

    /// Reorder: move the active tab one slot left/right (no wrap).
    pub fn move_tab(&mut self, delta: i32) {
        let i = self.active_tab as i32;
        let j = i + delta;
        if j < 0 || j >= self.tabs.len() as i32 {
            return;
        }
        self.tabs.swap(i as usize, j as usize);
        self.active_tab = j as usize;
    }

    /// M-1..M-9 — jump straight to tab N.
    fn goto_tab(&mut self, n: usize) {
        if n >= 1 && n <= self.tabs.len() {
            self.active_tab = n - 1;
        }
    }


    // ── Key handlers ─────────────────────────────────────────────────────────

    pub fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        // The shift report: any key resumes exactly where you were (the key is
        // swallowed — it must never leak into a buffer). Enter with a suggestion
        // on screen prefills the shell composer instead, still confirm-gated.
        if let Some(rep) = self.shift_report.take() {
            crate::llm_log::event("shift_report_dismissed", serde_json::json!({
                "after_ms": rep.shown_at.elapsed().as_millis() as u64,
                "suggestion_shown": rep.suggestion.is_some(),
                "suggestion_typed":
                    rep.suggestion.is_some() && key.code == KeyCode::Enter,
            }));
            self.needs_redraw = true;
            if key.code == KeyCode::Enter {
                if let Some(cmd) = rep.suggestion {
                    self.open_bar(BarMode::Shell);
                    if let Some(p) = self.palette.as_mut() {
                        p.query = cmd;
                    }
                    self.shell_ready = true; // next Enter runs it — one confirm, as ever
                }
            }
            return Ok(());
        }
        self.show_splash = false; // any keypress dismisses the banner
        // One gesture rules everything (doctrine §2): the bar opens from any
        // mode. Edit/Terminal/Bar handle bar-open themselves (prefix-aware /
        // submode-toggling), and a focused minibuffer keeps the keystroke — but
        // the transient nav modes (space warp, tree, time-travel) used to swallow
        // it. Handle those centrally so C-Space never intermittently dies.
        let chord = chord_of(&key);
        let bar_open = self.pending_prefix.is_empty()
            && (self.keys.bar_open.contains(&chord) || matches!(key.code, KeyCode::Null));
        // In the navigator, the bar chord on a *folder* re-roots the tree into it —
        // the mirror of `../` (ascend), so you can drill back down after going up.
        // On a file or `../`, it still opens the command bar (falls through).
        if bar_open && matches!(self.mode, Mode::Tree) {
            let sel = self.file_tree.as_ref().map(|t| t.selected).unwrap_or(0);
            if let Some(row) = self.tree_rows.get(sel).filter(|r| r.is_dir && !r.updir) {
                let path = row.path.clone();
                if let Some(t) = self.file_tree.as_mut() {
                    t.root = path.clone();
                    t.selected = 0;
                    t.filter.clear();
                    t.expanded.clear();
                }
                self.refresh_tree_rows();
                self.status_msg = Some(format!("rooted at {}", path.file_name().and_then(|n| n.to_str()).unwrap_or("/")));
                return Ok(());
            }
        }
        if bar_open && matches!(self.mode, Mode::Tab | Mode::Tree | Mode::Undo) {
            self.open_bar(BarMode::Command);
            return Ok(());
        }
        match self.mode.clone() {
            Mode::Edit     => self.handle_edit(key),
            Mode::Bar      => self.handle_bar(key),
            Mode::Prompt   => self.handle_prompt(key),
            Mode::Tab      => self.handle_tab(key),
            Mode::Terminal => self.handle_terminal(key),
            Mode::Tree     => self.handle_tree(key),
            Mode::Undo     => self.handle_undo_mode(key),
        }
        Ok(())
    }

    // ── Non-modal editing (Emacs/Mac/Claude-Code feel) ───────────────────────

    fn handle_edit(&mut self, key: KeyEvent) {
        self.status_msg = None;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let chord = chord_of(&key);

        // Ctrl+Space (or NUL, which many terminals send for it) / M-x → bar.
        if self.pending_prefix.is_empty()
            && (self.keys.bar_open.contains(&chord) || matches!(key.code, KeyCode::Null))
        {
            self.open_bar(BarMode::Command);
            return;
        }

        // Terminal pane: Enter (no prefix) re-attaches — or dismisses a dead shell.
        if self.pending_prefix.is_empty() {
            if let PaneContent::Terminal(tid) = self.focused_pane().content {
                if matches!(key.code, KeyCode::Enter) && key.modifiers == KeyModifiers::NONE {
                    if self.terms.get(&tid).map(|t| t.exited).unwrap_or(true) {
                        self.close_terminal_pane(tid);
                    } else {
                        self.mode = Mode::Terminal;
                    }
                    return;
                }
            }
        }

        // C-g / Esc cancel a pending prefix / selection (Emacs quit, modern cancel).
        if (ctrl && matches!(key.code, KeyCode::Char('g')))
            || (matches!(key.code, KeyCode::Esc) && key.modifiers == KeyModifiers::NONE)
        {
            // Esc dismisses a proactive notice first (nothing else pending).
            if self.pending_prefix.is_empty()
                && self.focused_pane().selection_anchor.is_none()
                && self.dismiss_notice()
            {
                return;
            }
            let had_state = !self.pending_prefix.is_empty()
                || self.focused_pane().selection_anchor.is_some();
            self.pending_prefix.clear();
            self.clear_selection();
            if had_state {
                self.status_msg = Some("Quit".into());
            }
            return;
        }

        // Prefix-key state machine (C-x …).
        let mut seq = self.pending_prefix.clone();
        seq.push(chord.clone());
        if let Some(action) = self.keys.lookup(&seq) {
            self.pending_prefix.clear();
            self.run_action(action);
            return;
        }
        let extends = self.keys.edit.keys().any(|k| k.len() > seq.len() && k.starts_with(&seq));
        if extends {
            self.pending_prefix = seq;
            self.prefix_tick = self.frame_tick;
            return;
        }
        if !self.pending_prefix.is_empty() {
            let shown = crate::config::render_chords(&seq);
            self.pending_prefix.clear();
            self.status_msg = Some(format!("{} is undefined", shown));
            return;
        }

        // No binding matched → editing primitives.
        self.handle_edit_primitive(key);
    }

    fn handle_edit_primitive(&mut self, key: KeyEvent) {
        let ctrl  = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt   = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let cmd   = key.modifiers.contains(KeyModifiers::SUPER);

        // Read-only Markdown reading-mode: no cursor — the document scrolls. Keys mirror
        // the editor's own motion grammar so it's muscle-memory-consistent: C-n/C-p and
        // ↑/↓ step a line, ⌥↑/⌥↓ · ⌥v · PgUp/PgDn page, M-< / M-> jump to the ends. Every
        // other key is swallowed (the view is read-only).
        if self.md_view_active() {
            let vh = self.focused_pane().view_h.max(1);
            // Exact cap from the last render's measured length: the last page stays
            // visible at the bottom, and you can't scroll off into a blank void.
            let cap = self.focused_pane().md_rendered_total.get().saturating_sub(vh);
            let p = self.focused_pane_mut();
            match key.code {
                // page (editor: ⌥↑/⌥↓, ⌥v, PageUp/PageDown)
                KeyCode::Down if alt => p.md_scroll = (p.md_scroll + vh).min(cap),
                KeyCode::Up   if alt => p.md_scroll = p.md_scroll.saturating_sub(vh),
                KeyCode::Char('v') if alt => p.md_scroll = p.md_scroll.saturating_sub(vh),
                KeyCode::PageDown => p.md_scroll = (p.md_scroll + vh).min(cap),
                KeyCode::PageUp   => p.md_scroll = p.md_scroll.saturating_sub(vh),
                // ends (M-< / M->) are bound to GoTop/GoBottom → handled in run_action.
                // line (editor: ↑/↓, C-n/C-p)
                KeyCode::Down => p.md_scroll = (p.md_scroll + 1).min(cap),
                KeyCode::Up   => p.md_scroll = p.md_scroll.saturating_sub(1),
                KeyCode::Char('n') if ctrl => p.md_scroll = (p.md_scroll + 1).min(cap),
                KeyCode::Char('p') if ctrl => p.md_scroll = p.md_scroll.saturating_sub(1),
                _ => {}
            }
            return; // read-only document: swallow everything else
        }

        self.last_yank = None; // any primitive key breaks a C-y / M-y chain
        // Undo coalescing: remember the run in progress, then default to breaking
        // it — only the insert/backspace arms below renew it.
        let prev_run = self.edit_run;
        self.edit_run = EditRun::None;

        match key.code {
            // Emacs cursor chords
            KeyCode::Char('f') if ctrl => self.move_right_sel(false),
            KeyCode::Char('b') if ctrl => self.move_left_sel(false),
            KeyCode::Char('n') if ctrl => self.move_down_sel(false),
            KeyCode::Char('p') if ctrl => self.move_up_sel(false),
            KeyCode::Char('a') if ctrl => self.move_line_start_sel(false),
            KeyCode::Char('e') if ctrl => self.move_line_end_sel(false),
            KeyCode::Char('d') if ctrl => self.delete_char_forward(),
            KeyCode::Char('f') if alt  => self.move_word_forward(),
            KeyCode::Char('b') if alt  => self.move_word_backward(),
            KeyCode::Char('v') if alt  => self.page_up(),

            // M-1..M-9 — jump to tab N (browser standard).
            KeyCode::Char(c) if alt && c.is_ascii_digit() => {
                self.goto_tab((c as u8 - b'0') as usize);
            }

            // Fast motion — ⌘ (kitty terminals) OR Option/Alt (the universal
            // fallback where the OS eats ⌘): ⌥←/→ = code-token, ⌥↑/↓ = page;
            // Shift extends the selection.
            KeyCode::Left  if cmd || alt => self.move_token_sel(false, shift),
            KeyCode::Right if cmd || alt => self.move_token_sel(true, shift),
            KeyCode::Up    if cmd || alt => self.page_up(),
            KeyCode::Down  if cmd || alt => self.page_down(),

            // Ctrl+arrows — directional pane focus (C-o and C-t travel also work).
            KeyCode::Left  if ctrl => self.focus_direction(-1, 0),
            KeyCode::Right if ctrl => self.focus_direction(1, 0),
            KeyCode::Up    if ctrl => self.focus_direction(0, -1),
            KeyCode::Down  if ctrl => self.focus_direction(0, 1),

            // Arrows / nav (Shift extends the selection, Mac-style)
            KeyCode::Left  => self.move_left_sel(shift),
            KeyCode::Right => self.move_right_sel(shift),
            KeyCode::Up    => self.move_up_sel(shift),
            KeyCode::Down  => self.move_down_sel(shift),
            KeyCode::Home  => self.move_line_start_sel(shift),
            KeyCode::End   => self.move_line_end_sel(shift),
            KeyCode::PageUp   => self.page_up(),
            KeyCode::PageDown => self.page_down(),

            // Editing — an active selection is replaced/deleted (Mac contract).
            // Consecutive backspaces / typed chars coalesce into ONE undo step.
            KeyCode::Backspace => {
                if !self.delete_selection() {
                    if prev_run != EditRun::Delete { self.focused_buf_mut().checkpoint(); }
                    self.delete_before_cursor();
                    self.edit_run = EditRun::Delete;
                }
            }
            KeyCode::Delete => {
                if !self.delete_selection() { self.delete_char_forward(); }
            }
            KeyCode::Enter => {
                self.focused_buf_mut().checkpoint();
                self.delete_selection();
                self.insert_char_at_cursor('\n');
                self.auto_indent(); // carry the previous line's leading whitespace
            }
            KeyCode::Tab   => {
                if self.focused_pane().selection_anchor.is_some() {
                    self.indent_region(false); // Tab indents the selected lines
                } else {
                    if prev_run != EditRun::Insert { self.focused_buf_mut().checkpoint(); }
                    for _ in 0..4 { self.insert_char_at_cursor(' '); }
                    self.edit_run = EditRun::Insert;
                }
            }
            KeyCode::BackTab => self.indent_region(true), // Shift-Tab dedents
            KeyCode::Char(c) if !ctrl && !alt => {
                if self.delete_selection() {
                    // replace: the delete checkpointed; typing joins that step
                } else if prev_run != EditRun::Insert {
                    self.focused_buf_mut().checkpoint();
                }
                self.insert_char_at_cursor(c);
                self.edit_run = EditRun::Insert;
            }
            _ => {}
        }
    }

    /// Indent (+4 spaces) or dedent (≤4 leading spaces / one tab) every line the
    /// selection touches, or the current line if there's no selection. One undo
    /// step; the selection is preserved so Tab/Shift-Tab can repeat.
    fn indent_region(&mut self, dedent: bool) {
        let sel = self.selection_range();
        let (buf_id, start_row, end_row) = match sel {
            Some((id, s, e)) => {
                let (sr, _) = self.rowcol_of(id, s);
                let (er, ec) = self.rowcol_of(id, e);
                // A selection ending at column 0 doesn't include that trailing line.
                let er = if ec == 0 && er > sr { er - 1 } else { er };
                (id, sr, er)
            }
            None => match self.editor_pos() {
                Some((row, _, id)) => (id, row, row),
                None => return,
            },
        };
        self.focused_buf_mut().checkpoint();
        self.edit_run = EditRun::None;
        for row in start_row..=end_row {
            let line_start = self.buffers[&buf_id].char_at(row, 0);
            if dedent {
                let head: String = self.buffers[&buf_id].rope.line(row).chars().take(4).collect();
                let n = if head.starts_with('\t') { 1 } else { head.chars().take_while(|c| *c == ' ').count() };
                if n > 0 {
                    self.buffers.get_mut(&buf_id).unwrap().rope.remove(line_start..line_start + n);
                }
            } else {
                self.buffers.get_mut(&buf_id).unwrap().rope.insert(line_start, "    ");
            }
        }
        self.buffers.get_mut(&buf_id).unwrap().mark_edited();
        if sel.is_some() {
            // Re-select the affected lines so the block stays highlighted for repeats.
            let end_len = self.buffers[&buf_id].line_len(end_row);
            self.focused_pane_mut().selection_anchor = Some((start_row, 0));
            let p = self.focused_pane_mut();
            p.cursor_row = end_row;
            p.cursor_col = end_len;
            p.col_affinity = end_len;
        } else {
            self.clamp_cursor_after_edit();
        }
    }

    // ── Query-replace (M-%) ──────────────────────────────────────────────────

    fn begin_query_replace(&mut self) {
        self.replace_checkpointed = false;
        self.replace_idx = None;
        // Scan the whole buffer from the top (the "search & replace" expectation).
        if self.qr_show_next(0) {
            let label = format!("Replace '{}' → '{}'?  y / n / ! (all) / q ", self.replace_from, self.replace_to);
            self.start_prompt(PromptKind::QueryReplace, &label);
        } else {
            self.status_msg = Some(format!("No matches for '{}'", self.replace_from));
        }
    }

    /// Find the next match at/after `from_idx`; move the cursor there + highlight.
    fn qr_show_next(&mut self, from_idx: usize) -> bool {
        let from = self.replace_from.clone();
        let buf_id = match self.editor_pos() { Some((_, _, id)) => id, None => return false };
        match self.find_matches(buf_id, &from).into_iter().find(|&m| m >= from_idx) {
            Some(idx) => {
                self.replace_idx = Some(idx);
                let (r, c) = self.rowcol_of(buf_id, idx);
                self.set_cursor(r, c);
                self.search_hl = vec![(r, c, from.chars().count())];
                true
            }
            None => false,
        }
    }

    fn qr_replace_at(&mut self, idx: usize, flen: usize, to: &str) {
        let buf_id = match self.editor_pos() { Some((_, _, id)) => id, None => return };
        if !self.replace_checkpointed {
            self.focused_buf_mut().checkpoint(); // one undo step for the whole replace
            self.replace_checkpointed = true;
        }
        let buf = self.buffers.get_mut(&buf_id).unwrap();
        buf.rope.remove(idx..idx + flen);
        buf.rope.insert(idx, to);
        buf.mark_edited();
    }

    fn qr_finish(&mut self) {
        self.replace_idx = None;
        self.search_hl.clear();
        self.close_prompt();
    }

    fn handle_query_replace_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let to = self.replace_to.clone();
        let flen = self.replace_from.chars().count();
        let tlen = to.chars().count();
        let Some(idx) = self.replace_idx else { self.qr_finish(); return };
        match key.code {
            KeyCode::Char('y') | KeyCode::Char(' ') => {
                self.qr_replace_at(idx, flen, &to);
                if !self.qr_show_next(idx + tlen) {
                    self.qr_finish();
                    self.status_msg = Some("Replaced".into());
                }
            }
            KeyCode::Char('n') => {
                if !self.qr_show_next(idx + flen) { self.qr_finish(); }
            }
            KeyCode::Char('!') => {
                let mut count = 0;
                loop {
                    let m = self.replace_idx.unwrap();
                    self.qr_replace_at(m, flen, &to);
                    count += 1;
                    if !self.qr_show_next(m + tlen) { break; }
                }
                self.qr_finish();
                self.status_msg = Some(format!("Replaced {count}"));
            }
            KeyCode::Char('q') | KeyCode::Esc => self.qr_finish(),
            KeyCode::Char('g') if ctrl => self.qr_finish(),
            _ => {}
        }
    }

    /// Delete the active selection (no kill-ring). Returns true if one existed.
    fn delete_selection(&mut self) -> bool {
        if let Some((buf_id, s, e)) = self.selection_range() {
            {
                let buf = self.buffers.get_mut(&buf_id).unwrap();
                buf.checkpoint();
                buf.rope.remove(s..e);
                buf.mark_edited();
            }
            let (r, c) = self.rowcol_of(buf_id, s);
            self.set_cursor(r, c);
            self.clear_selection();
            true
        } else {
            self.clear_selection();
            false
        }
    }

    // ── Minibuffer prompt (find-file, switch-buffer, search) ──────────────────

    fn open_bar(&mut self, bar_mode: BarMode) {
        // Remember where to return: a terminal keeps its focus (seamless switch).
        self.bar_return = if self.mode == Mode::Terminal { Mode::Terminal } else { Mode::Edit };
        let mut p = Palette::root();
        p.bar_mode = bar_mode;
        // Editor bars are menu-first (a row is always selected); the terminal
        // composer opens unengaged — typing or arrowing selects the top match
        // (registry-first Enter), and an unengaged Enter is a no-op.
        p.navigated = self.bar_return != Mode::Terminal;
        // The bar always opens on the Commands launcher (its familiar behaviour). The
        // separate WORKSPACES panel appears beside it when there's a fleet to survey;
        // ← moves focus into it, → returns.
        p.column = crate::palette::BarColumn::Commands;
        p.sel_ws = 0;
        self.palette = Some(p);
        self.mode = Mode::Bar;
        self.shell_ready = false;
    }

    fn start_prompt(&mut self, kind: PromptKind, label: &str) {
        self.start_prompt_with(kind, label, "");
    }

    /// Prompt pre-filled with the current value (rename flows).
    fn start_prompt_with(&mut self, kind: PromptKind, label: &str, initial: &str) {
        self.prompt = Some(Prompt {
            label: label.to_string(),
            input: initial.to_string(),
            kind,
        });
        self.mode = Mode::Prompt;
    }

    fn handle_prompt(&mut self, key: KeyEvent) {
        let kind = match self.prompt.as_ref() {
            Some(p) => p.kind.clone(),
            None => { self.mode = Mode::Edit; return; }
        };
        match kind {
            PromptKind::Search => self.handle_isearch_key(key),
            PromptKind::ConfirmQuit => self.handle_confirm_quit_key(key),
            PromptKind::ConfirmAction(action) => self.handle_confirm_action_key(key, action),
            PromptKind::QueryReplace => self.handle_query_replace_key(key),
            _ => self.handle_line_prompt_key(key),
        }
    }

    fn close_prompt(&mut self) {
        self.prompt = None;
        self.mode = Mode::Edit;
    }

    fn handle_line_prompt_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('g')) {
            self.close_prompt();
            return;
        }
        match key.code {
            KeyCode::Esc => self.close_prompt(),
            KeyCode::Enter => {
                if let Some(p) = self.prompt.take() {
                    self.mode = Mode::Edit;
                    self.finish_prompt(p);
                }
            }
            KeyCode::Backspace => { if let Some(p) = self.prompt.as_mut() { p.input.pop(); } }
            KeyCode::Char(c) if !ctrl => { if let Some(p) = self.prompt.as_mut() { p.input.push(c); } }
            _ => {}
        }
    }

    /// Live isearch: typing filters immediately; C-s/C-r step; Enter accepts;
    /// C-g/Esc restores the origin.
    fn handle_isearch_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let query = self.prompt.as_ref().map(|p| p.input.clone()).unwrap_or_default();

        // Label-pick mode (after Tab): the next key jumps to a labeled match.
        if self.search_pick {
            self.search_pick = false;
            if let KeyCode::Char(c) = key.code {
                if let Some(&(r, col, _)) = self.search_labels.iter().find(|(_, _, l)| *l == c) {
                    self.set_cursor(r, col);
                    self.end_isearch(false);
                    self.close_prompt();
                    return;
                }
            }
            self.search_labels.clear(); // not a label → drop labels, handle normally
        }

        if ctrl && matches!(key.code, KeyCode::Char('g')) {
            self.end_isearch(true);
            self.close_prompt();
            return;
        }
        match key.code {
            KeyCode::Esc => { self.end_isearch(true); self.close_prompt(); }
            KeyCode::Enter => { self.end_isearch(false); self.close_prompt(); }
            KeyCode::Char('s') if ctrl => self.isearch_step(&query, true),
            KeyCode::Char('r') if ctrl => self.isearch_step(&query, false),
            // Tab → teleport: label the matches; the next key jumps to one.
            KeyCode::Tab => {
                if self.search_hl.len() >= 2 {
                    self.build_search_labels();
                    self.search_pick = true;
                }
            }
            KeyCode::Backspace => {
                if let Some(p) = self.prompt.as_mut() { p.input.pop(); }
                let q = self.prompt.as_ref().map(|p| p.input.clone()).unwrap_or_default();
                self.update_isearch(&q);
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(p) = self.prompt.as_mut() { p.input.push(c); }
                let q = self.prompt.as_ref().map(|p| p.input.clone()).unwrap_or_default();
                self.update_isearch(&q);
            }
            // Land-on-any-key: any other key accepts at the current match, then is
            // applied in edit mode — so search flows straight into editing.
            _ => {
                self.end_isearch(false);
                self.close_prompt();
                let _ = self.handle_key(key);
            }
        }
    }

    /// Assign home-row labels to the first matches (document order) for Tab-pick.
    fn build_search_labels(&mut self) {
        const ALPHA: &[u8] = b"asdfghjklqwertyuiopvbnm";
        self.search_labels = self
            .search_hl
            .iter()
            .take(ALPHA.len())
            .enumerate()
            .map(|(i, &(r, c, _))| (r, c, ALPHA[i] as char))
            .collect();
    }

    /// (1-based current, total) match index at the cursor, for the `n/m` counter.
    pub fn isearch_status(&self) -> Option<(usize, usize)> {
        let total = self.search_hl.len();
        if total == 0 {
            return None;
        }
        let pane = self.focused_pane();
        let (cr, cc) = (pane.cursor_row, pane.cursor_col);
        let cur = self
            .search_hl
            .iter()
            .position(|&(r, c, _)| r == cr && c == cc)
            .map(|i| i + 1)
            .unwrap_or(0);
        Some((cur, total))
    }

    fn handle_confirm_quit_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('g')) {
            self.close_prompt();
            return;
        }
        match key.code {
            KeyCode::Char('s') => {
                let unsaved = self.save_all();
                self.close_prompt();
                if unsaved.is_empty() {
                    self.should_quit = true;
                } else {
                    self.status_msg = Some(format!(
                        "No file for: {} — save it first (C-x C-s)",
                        unsaved.join(", ")
                    ));
                }
            }
            KeyCode::Char('q') | KeyCode::Char('!') => {
                self.close_prompt();
                self.should_quit = true;
            }
            KeyCode::Esc | KeyCode::Char('n') => self.close_prompt(),
            _ => {}
        }
    }

    fn handle_confirm_action_key(&mut self, key: KeyEvent, action: Action) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                self.close_prompt();
                self.close_confirmed = true; // the gated close_* runs once, un-re-prompted
                self.run_action(action);
                self.close_confirmed = false;
            }
            _ if ctrl || matches!(key.code, KeyCode::Esc | KeyCode::Char('n')) => {
                self.close_prompt();
                self.status_msg = Some("Cancelled".into());
            }
            _ => {}
        }
    }

    fn finish_prompt(&mut self, p: Prompt) {
        match p.kind {
            PromptKind::ReplaceFrom => {
                self.replace_from = p.input;
                if self.replace_from.is_empty() {
                    return;
                }
                self.start_prompt(PromptKind::ReplaceTo, "Replace with: ");
            }
            PromptKind::ReplaceTo => {
                self.replace_to = p.input;
                self.begin_query_replace();
            }
            PromptKind::GotoLine => {
                match p.input.trim().parse::<usize>() {
                    Ok(n) if n >= 1 => {
                        if let Some((_, _, buf_id)) = self.editor_pos() {
                            let last = self.buffers[&buf_id].line_count().saturating_sub(1);
                            let row = (n - 1).min(last);
                            self.set_cursor(row, 0);
                            self.recenter();
                        }
                    }
                    _ => self.status_msg = Some("Not a line number".into()),
                }
            }
            PromptKind::SaveAs => {
                let path = p.input.trim().to_string();
                if path.is_empty() { return; }
                let result = self
                    .focused_buf_mut()
                    .save_as(std::path::PathBuf::from(&path));
                self.status_msg = Some(match result {
                    Ok(_) => format!("Saved  {}", path),
                    Err(e) => format!("Save error: {}", e),
                });
            }
            PromptKind::RenameTab => {
                let name = p.input.trim().to_string();
                if !name.is_empty() {
                    let id = self.tab().id;
                    self.auto_name_attempted.insert(id); // manual name opts out
                    self.tab_mut().name = name;
                }
            }
            PromptKind::RenamePane => {
                let title = p.input.trim().to_string();
                let pid = self.focused_pane_id();
                if let Some(pane) = self.panes.get_mut(&pid) {
                    pane.title = if title.is_empty() { None } else { Some(title) };
                }
            }
            PromptKind::RenameSession => {
                let name = p.input.trim().to_string();
                if !name.is_empty() {
                    match crate::session::validate_session_name(&name) {
                        Ok(()) => self.rename_session_to = Some(name),
                        Err(e) => self.status_msg = Some(format!("Invalid session name: {e}")),
                    }
                }
            }
            // Search / confirms / query-replace are handled key-by-key, never via finish.
            PromptKind::Search | PromptKind::ConfirmQuit
            | PromptKind::ConfirmAction(_) | PromptKind::QueryReplace => {}
        }
    }

    /// C-t travel mode — one-char verbs for tabs and panes, with an on-screen
    /// cheat panel. Rule: creation exits the mode, navigation stays.
    fn handle_tab(&mut self, key: KeyEvent) {
        let ctrl  = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers == KeyModifiers::SHIFT;

        // Leave: Esc / Enter / C-g / C-t (back to whatever the pane hosts).
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter)
            || (ctrl && matches!(key.code, KeyCode::Char('g') | KeyCode::Char('t')))
        {
            self.mode = self.mode_for_focused_pane();
            return;
        }

        match key.code {
            // ── Create (creation exits — you'll want to type) ──
            KeyCode::Char('t') | KeyCode::Char('n') => {
                self.new_tab();
                self.mode = Mode::Edit;
            }
            KeyCode::Char('T') => {
                // Open a terminal in a NEW tab.
                self.new_tab();
                self.open_terminal(); // converts the new tab's pane; sets Mode::Terminal
            }
            KeyCode::Char('|') | KeyCode::Char('\\') | KeyCode::Char('v') => {
                self.split_vertical();
                self.mode = Mode::Edit;
            }
            KeyCode::Char('-') | KeyCode::Char('s') => {
                self.split_horizontal();
                self.mode = Mode::Edit;
            }

            // ── Move: ONE directional grammar over the whole workspace. Arrows (or
            //    hjkl) step between panes by real geometry; at the horizontal edge
            //    of the pane grid, focus spills into the adjacent tab — panes and
            //    tabs are just views of one space, so one set of keys walks it all.
            KeyCode::Left  | KeyCode::Char('h') if !shift => self.warp_move(-1, 0),
            KeyCode::Right | KeyCode::Char('l') if !shift => self.warp_move(1, 0),
            KeyCode::Up    | KeyCode::Char('k') => self.warp_move(0, -1),
            KeyCode::Down  | KeyCode::Char('j') => self.warp_move(0, 1),
            KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
                self.goto_tab((c as u8 - b'0') as usize);
                self.mode = self.mode_for_focused_pane(); // land ready to use
            }

            // ── Reorder the focused tab (moves the view itself, not the focus) ──
            KeyCode::Char('H') => self.move_tab(-1),
            KeyCode::Char('L') => self.move_tab(1),
            KeyCode::Left  if shift => self.move_tab(-1),
            KeyCode::Right if shift => self.move_tab(1),

            // ── Toggle: zoom (maximize) the focused pane; press again to restore ──
            KeyCode::Char('z') | KeyCode::Char(' ') => self.toggle_zoom(),

            // ── Delete: ONE key closes the focused view — the pane, or the whole
            //    tab when it is the tab's last pane. Behind a motor-slip prompt
            //    because it sits next to the navigation keys.
            KeyCode::Char('d') | KeyCode::Backspace | KeyCode::Delete => self.close_focused(),

            // ── Act on the focused pane ──
            KeyCode::Char('x') => self.swap_pane(),
            KeyCode::Char('>') | KeyCode::Char('+') | KeyCode::Char('=') => self.resize_pane(6),
            KeyCode::Char('<') => self.resize_pane(-6),
            KeyCode::Char('r') => self.run_action(Action::RenameTab), // → prompt, exits mode
            KeyCode::Char('?') => self.run_action(Action::ExplainFailure), // triage → Ask
            KeyCode::Char('w') => self.toggle_watch_pane(), // watch this pane
            KeyCode::Char('@') => self.toggle_file_tree(), // `@` opens/focuses the navigator

            // ── Session ──
            KeyCode::Char('D') => {
                self.run_action(Action::Detach);
                if !self.detach_requested {
                    self.mode = self.mode_for_focused_pane(); // standalone: just exit mode
                }
            }
            _ => {}
        }
    }

    // ── Command bar (was handle_palette) ─────────────────────────────────────

    fn handle_bar(&mut self, key: KeyEvent) {
        let ctrl  = key.modifiers.contains(KeyModifiers::CONTROL);
        let none  = key.modifiers == KeyModifiers::NONE;
        let shift = key.modifiers == KeyModifiers::SHIFT;

        // C-g cancels the bar from every submode — the one overlearned recovery
        // chord (doctrine §3.4) must be reliable here, the most-used surface.
        if ctrl && matches!(key.code, KeyCode::Char('g')) {
            self.close_bar();
            return;
        }

        // Ctrl+Space inside a sub-mode (shell / file) → the full command bar.
        let chord = chord_of(&key);
        if self.keys.bar_open.contains(&chord) || matches!(key.code, KeyCode::Null) {
            if let Some(p) = self.palette.as_mut() {
                if p.bar_mode == BarMode::Shell {
                    p.bar_mode = BarMode::Command;
                    p.query.clear();
                    p.selected = 0;
                    self.shell_ready = false;
                }
            }
            return;
        }

        // Tab: in shell mode it TRANSLATES the English query into a command;
        // elsewhere it toggles CMD ↔ ASK.
        if let KeyCode::Tab = key.code {
            let mode = self.palette.as_ref().map(|p| p.bar_mode.clone());
            match mode {
                Some(BarMode::Shell) => { self.translate_shell_query(); return; }
                _ => {
                    if let Some(p) = self.palette.as_mut() {
                        p.bar_mode = match p.bar_mode {
                            BarMode::Command => BarMode::Ask,
                            _ => BarMode::Command,
                        };
                    }
                    return;
                }
            }
        }

        // Leading '!' / '?' / '@' on an empty query switches mode instead of typing:
        // `!` shell, `?` ask, `@` file picker (VS Code Ctrl+P style).
        let empty_query = self.palette.as_ref().map(|p| p.query.is_empty()).unwrap_or(false);
        if (none || shift) && empty_query {
            match key.code {
                KeyCode::Char('!') => {
                    if let Some(p) = self.palette.as_mut() { p.bar_mode = BarMode::Shell; }
                    return;
                }
                KeyCode::Char('?') => {
                    if let Some(p) = self.palette.as_mut() { p.bar_mode = BarMode::Ask; }
                    return;
                }
                KeyCode::Char('@') => {
                    self.close_bar();
                    self.toggle_file_tree(); // `@` opens the left file tree
                    return;
                }
                _ => {}
            }
        }

        let bar_mode = self
            .palette
            .as_ref()
            .map(|p| p.bar_mode.clone())
            .unwrap_or(BarMode::Command);
        match bar_mode {
            BarMode::Command => self.handle_bar_command(key, ctrl, none, shift),
            BarMode::Ask     => self.handle_bar_ask(key, none, shift),
            BarMode::Shell   => self.handle_bar_shell(key, none, shift),
        }
    }

    /// Clear the bar and any pending agent state, returning to the mode the
    /// bar was opened from (Edit, or Terminal for seamless switching).
    /// The unified terminal composer's shell fallback: the query didn't match a
    /// command, so translate it (LLM) into a shell command for confirmation —
    /// or, with no agent key, run it directly.
    fn submit_terminal_shell(&mut self) {
        let cmd = self.palette.as_ref().map(|p| p.query.clone()).unwrap_or_default();
        if cmd.trim().is_empty() {
            return;
        }
        if self.shell_ready || !agent::AgentConfig::from_env().is_configured() {
            self.close_bar();
            self.run_shell_command(&cmd);
        } else {
            // Flip to the inline shell composer so the translated command shows,
            // anchored at the cursor, for a confirming second Enter.
            if let Some(p) = self.palette.as_mut() { p.bar_mode = BarMode::Shell; }
            self.translate_shell_query();
        }
    }

    fn close_bar(&mut self) {
        // A pending, un-accepted translation dismissed here = a reject signal.
        // (Accept clears translate_call_id first, so this only fires on cancel.)
        if let Some(id) = self.translate_call_id.take() {
            crate::llm_log::record_outcome(id, None, false, true);
        }
        self.translate_request = None;
        self.palette = None;
        self.mode = self.bar_return.clone();
        self.agent_answer = None;
        self.agent_partial = None;
        self.agent_directive = None;
        self.refactor_target = None;
        self.refactor_replacement = None;
        self.ask_scroll = 0;
        // agent_pending/agent_history survive — an in-flight answer lands in
        // the transcript and is there when the bar reopens.
    }

    /// Move the selection up/down within the focused board column (wrapping).
    fn bar_nav(&mut self, up: bool, on_ws: bool, ws_len: usize, cmd_len: usize) {
        if on_ws {
            if let Some(p) = self.palette.as_mut() {
                if ws_len > 0 {
                    p.sel_ws = if up { (p.sel_ws + ws_len - 1) % ws_len } else { (p.sel_ws + 1) % ws_len };
                }
            }
        } else if let Some(p) = self.palette.as_mut() {
            if up { p.select_up(cmd_len); } else { p.select_down(cmd_len); }
        }
    }

    fn handle_bar_command(&mut self, key: KeyEvent, ctrl: bool, none: bool, shift: bool) {
        use crate::palette::BarColumn;
        // Two-pane board: ↑/↓ move within the focused column, ←/→ cross between the
        // Workspaces board (left) and the Commands launcher (right).
        let show_ws = self.bar_show_workspaces();
        let cmd_len = self.bar_rows().len();
        let ws_len = self.bar_workspace_rows().len();
        let on_ws = show_ws
            && self.palette.as_ref().map(|p| p.column == BarColumn::Workspaces).unwrap_or(false);

        match key.code {
            KeyCode::Esc => {
                let close = if let Some(p) = self.palette.as_mut() { !p.pop() } else { true };
                if close { self.close_bar(); }
            }
            KeyCode::Left if show_ws => {
                if let Some(p) = self.palette.as_mut() { p.column = BarColumn::Workspaces; }
            }
            KeyCode::Right => {
                if let Some(p) = self.palette.as_mut() { p.column = BarColumn::Commands; }
            }
            KeyCode::Up | KeyCode::BackTab => self.bar_nav(true, on_ws, ws_len, cmd_len),
            KeyCode::Down => self.bar_nav(false, on_ws, ws_len, cmd_len),
            KeyCode::Char('p') if ctrl => self.bar_nav(true, on_ws, ws_len, cmd_len),
            KeyCode::Char('n') if ctrl => self.bar_nav(false, on_ws, ws_len, cmd_len),
            KeyCode::Enter if on_ws => {
                // Jump to the highlighted workspace — the safe universal verb (never
                // auto-answers a prompt; those stay behind the confirm gate).
                let sel = self.palette.as_ref().map(|p| p.sel_ws).unwrap_or(0);
                if let Some(crate::palette::ItemKind::Surface(s)) =
                    self.bar_workspace_rows().into_iter().nth(sel).map(|r| r.kind)
                {
                    self.jump_to_surface(s);
                }
            }
            KeyCode::Enter => {
                let selected = self.palette.as_ref().map(|p| p.selected).unwrap_or(0);
                let navigated = self.palette.as_ref().map(|p| p.navigated).unwrap_or(false);
                let kind = self.bar_rows().into_iter().nth(selected).map(|r| r.kind);
                // REGISTRY-FIRST (2026-07 ruling, reversing the earlier
                // shell-first one): typing pre-selects the top match and Enter
                // fires it — commands stay one keystroke away. Only when
                // nothing in the registry matches does the query fall through
                // to natural language: shell-translate in a terminal, an
                // editor-grounded ask elsewhere. `!` still forces pure shell.
                let has_query = self.palette.as_ref().map(|p| !p.query.trim().is_empty()).unwrap_or(false);
                if kind.is_some() && (navigated || has_query) {
                    self.activate_kind(kind);
                } else if has_query {
                    if self.bar_return == Mode::Terminal {
                        self.submit_terminal_shell();
                    } else {
                        if let Some(p) = self.palette.as_mut() { p.bar_mode = BarMode::Ask; }
                        self.submit_agent_query();
                    }
                }
                // Empty query, nothing highlighted → Enter is a no-op: it must
                // never fire a row the user can't see is selected.
            }
            KeyCode::Backspace => {
                let close = if let Some(p) = self.palette.as_mut() {
                    if p.query.is_empty() {
                        !p.pop()
                    } else {
                        p.query.pop();
                        p.selected = 0;
                        p.sel_ws = 0;
                        p.navigated = !p.query.is_empty();
                        false
                    }
                } else { false };
                if close { self.close_bar(); }
            }
            // On the Workspaces column, `s` pulls an on-demand summary for the
            // highlighted surface (not query input — workspaces are navigated, not
            // typed into).
            KeyCode::Char('s') if on_ws => self.request_summary_for_selected(),
            // Search-first (Claude-Code feel): typing filters the command list. Only
            // when the Commands column has focus — on Workspaces, letters are inert
            // (navigation mode) so `s` etc. stay shortcuts.
            KeyCode::Char(c) if (none || shift) && !on_ws => {
                if let Some(p) = self.palette.as_mut() {
                    p.query.push(c);
                    p.selected = 0;
                    p.sel_ws = 0;
                    p.navigated = true;
                }
            }
            _ => {}
        }
    }

    /// Ask-the-AI submode: text is a natural-language question; Enter sends it,
    /// and a second Enter fires any directive (RUN/TYPE) the model proposed.
    fn handle_bar_ask(&mut self, key: KeyEvent, none: bool, shift: bool) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // C-l starts a fresh conversation.
        if ctrl && matches!(key.code, KeyCode::Char('l')) {
            self.agent_history.clear();
            self.agent_answer = None;
            self.agent_partial = None;
            self.agent_directive = None;
            self.refactor_target = None;
            self.refactor_replacement = None;
            self.ask_scroll = 0;
            return;
        }

        match key.code {
            KeyCode::Esc => self.close_bar(),
            // Scroll the transcript.
            KeyCode::Up => self.ask_scroll = self.ask_scroll.saturating_add(1),
            KeyCode::Down => self.ask_scroll = self.ask_scroll.saturating_sub(1),
            KeyCode::Enter => {
                // A pending refactor is confirmed with Enter (unless you're typing
                // a follow-up question), applied as one reversible edit.
                let has_query = self.palette.as_ref().map(|p| !p.query.trim().is_empty()).unwrap_or(false);
                if self.refactor_replacement.is_some() && !has_query {
                    self.apply_refactor();
                    return;
                }
                match self.agent_directive.clone() {
                    Some(agent::AgentDirective::Run(name)) => {
                        self.agent_directive = None;
                        let Some(action) = Action::from_name(&name) else {
                            self.agent_answer = Some(format!("⚠ unknown action: {name}"));
                            return;
                        };
                        self.close_bar();
                        if action.is_destructive() {
                            // Never let the model fire a destructive action unconfirmed.
                            self.start_prompt(
                                PromptKind::ConfirmAction(action.clone()),
                                &format!("Agent wants to run “{}” — y run · n cancel ", action.label()),
                            );
                        } else {
                            self.run_action(action);
                        }
                    }
                    Some(agent::AgentDirective::Type(cmd)) => {
                        self.agent_directive = None;
                        self.close_bar();
                        self.run_shell_command(&cmd);
                    }
                    Some(agent::AgentDirective::Open(loc)) => {
                        self.agent_directive = None;
                        self.close_bar();
                        self.open_at(&loc);
                    }
                    // NEED is auto-satisfied in tick and never surfaced here.
                    Some(agent::AgentDirective::Need(_)) => { self.agent_directive = None; }
                    None => self.submit_agent_query(),
                }
            }
            KeyCode::Backspace => {
                let close = if let Some(p) = self.palette.as_mut() {
                    if p.query.is_empty() {
                        true
                    } else {
                        p.query.pop();
                        false
                    }
                } else { false };
                if close {
                    self.close_bar();
                } else {
                    self.agent_directive = None; // a new edit invalidates the suggestion
                }
            }
            KeyCode::Char(c) if none || shift => {
                if let Some(p) = self.palette.as_mut() { p.query.push(c); }
                self.agent_directive = None;
            }
            _ => {}
        }
    }

    /// Inline natural-language shell composer. Enter translates the English
    /// request into a shell command via the agent (shown for confirmation),
    /// then a second Enter runs it. With no API key it runs the text literally.
    fn handle_bar_shell(&mut self, key: KeyEvent, none: bool, shift: bool) {
        match key.code {
            KeyCode::Esc => self.close_bar(),
            KeyCode::Enter => {
                let cmd = self.palette.as_ref().map(|p| p.query.clone()).unwrap_or_default();
                if cmd.trim().is_empty() {
                    return;
                }
                if self.shell_ready || !agent::AgentConfig::from_env().is_configured() {
                    // Command is ready (translated), or there's no key to
                    // translate with → run what's shown. Record accept BEFORE
                    // close_bar clears the correlation state, and remember the
                    // (request → accepted command) pair for corrective memory.
                    if let Some(id) = self.translate_call_id.take() {
                        if let Some(req) = self.translate_request.take() {
                            crate::retrieval::remember_command(&req, &cmd);
                        }
                        crate::llm_log::record_outcome(id, Some(&cmd), false, false);
                    }
                    self.close_bar();
                    self.run_shell_command(&cmd);
                } else {
                    // Translate the English request; the command lands in the
                    // pill (shell_ready) and the next Enter runs it.
                    self.translate_shell_query();
                }
            }
            KeyCode::Backspace => {
                self.on_translation_edited();
                self.shell_ready = false; // an edit invalidates the translation
                self.agent_answer = None; // and clears any stale error
                if let Some(p) = self.palette.as_mut() {
                    if p.query.is_empty() {
                        p.bar_mode = BarMode::Command;
                    } else {
                        p.query.pop();
                    }
                }
            }
            KeyCode::Char(c) if none || shift => {
                self.on_translation_edited();
                self.shell_ready = false;
                self.agent_answer = None;
                if let Some(p) = self.palette.as_mut() { p.query.push(c); }
            }
            _ => {}
        }
    }

    /// `OPEN: path:line` — open a file at a line (from a cited stack trace).
    /// If the focused pane is a terminal, split first so it stays visible.
    fn open_at(&mut self, loc: &str) {
        // Parse "path:line" — line optional, trailing ":col" tolerated.
        let (path, line) = match loc.rsplit_once(':') {
            Some((p, n)) if n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty() => {
                (p.to_string(), n.parse::<usize>().unwrap_or(1))
            }
            _ => (loc.to_string(), 1),
        };
        let path = path.trim();
        if path.is_empty() {
            return;
        }
        // Keep a visible terminal by opening the file beside it.
        if matches!(self.focused_pane().content, PaneContent::Terminal(_))
            && self.tab().layout.count() < self.tuning.max_panes
        {
            self.split_vertical();
        }
        match self.open_file(path) {
            Ok(buf_id) => {
                let pid = self.focused_pane_id();
                if let Some(pane) = self.panes.get_mut(&pid) {
                    pane.content = PaneContent::Editor(buf_id);
                }
                let last = self.buffers[&buf_id].line_count().saturating_sub(1);
                let row = line.saturating_sub(1).min(last);
                self.set_cursor(row, 0);
                self.recenter();
                self.mode = Mode::Edit;
                self.status_msg = Some(format!("Opened {}:{}", path, line));
            }
            Err(e) => self.status_msg = Some(format!("Can't open {}: {}", path, e)),
        }
    }

    // ── Left file-tree sidebar (@ / C-x d) ───────────────────────────────────

    /// Open/focus/hide the tree (tri-state): closed → open+focus; open+focused →
    /// hide; open+unfocused → focus. Keeps the sidebar persistent across opens.
    pub fn toggle_file_tree(&mut self) {
        if !self.tree_open {
            self.ensure_project_index();
            if self.file_tree.is_none() {
                let root = self
                    .project_index
                    .as_ref()
                    .map(|i| i.root.clone())
                    .unwrap_or_else(|| std::path::PathBuf::from("."));
                // Absolute path so `../` (parent) navigation works — a relative
                // "." has an empty parent and would blank the tree.
                let root = std::fs::canonicalize(&root).unwrap_or(root);
                self.file_tree = Some(FileTree {
                    root,
                    expanded: std::collections::HashSet::new(),
                    selected: 0,
                    filter: String::new(),
                    show_dotfiles: self.tuning.tree_show_dotfiles == 1,
                });
            }
            self.tree_open = true;
            self.mode = Mode::Tree;
            self.refresh_tree_rows();
        } else if self.mode == Mode::Tree {
            self.close_tree();
        } else {
            self.mode = Mode::Tree;
            self.refresh_tree_rows();
        }
    }

    /// Hide the sidebar and forget its navigation state, so the next open starts
    /// fresh at the project root (not wherever `../` last wandered to).
    fn close_tree(&mut self) {
        self.tree_open = false;
        self.mode = Mode::Edit;
        self.file_tree = None;
        self.tree_rows.clear();
    }

    /// Recompute the flattened visible rows after any tree mutation.
    fn refresh_tree_rows(&mut self) {
        let rows = self.compute_tree_rows();
        let n = rows.len();
        self.tree_rows = rows;
        if let Some(t) = self.file_tree.as_mut() {
            if t.selected >= n {
                t.selected = n.saturating_sub(1);
            }
        }
    }

    /// The rows shown in the sidebar. Empty filter → the browse tree (folders
    /// expand in place); a filter → a flat fuzzy shortlist over the index.
    fn compute_tree_rows(&self) -> Vec<TreeRow> {
        let Some(tree) = self.file_tree.as_ref() else { return Vec::new() };
        if !tree.filter.is_empty() {
            // Shortlist: fuzzy over the project index (relative paths).
            let mut scored: Vec<(i64, u32, String)> = self
                .project_index
                .as_ref()
                .map(|i| {
                    i.files
                        .iter()
                        .filter_map(|f| {
                            palette::fuzzy_score(&tree.filter, f)
                                .map(|s| (s, *self.file_frecency.get(f).unwrap_or(&0), f.clone()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
            return scored
                .into_iter()
                .take(300)
                .map(|(_, _, rel)| TreeRow {
                    path: tree.root.join(&rel),
                    label: rel,
                    depth: 0,
                    is_dir: false,
                    expanded: false,
                    updir: false,
                })
                .collect();
        }
        // Browse: `../` (if the root has a parent), then the expanded tree.
        let mut rows = Vec::new();
        if tree.root.parent().is_some() {
            rows.push(TreeRow {
                path: tree.root.clone(),
                label: "../".into(),
                depth: 0,
                is_dir: true,
                expanded: false,
                updir: true,
            });
        }
        self.push_dir_rows(&tree.root, 0, &tree.expanded, &mut rows);
        rows
    }

    /// Append a directory's entries (dirs first, alpha), recursing into expanded
    /// folders — the expand-in-place tree.
    fn push_dir_rows(
        &self,
        dir: &std::path::Path,
        depth: usize,
        expanded: &std::collections::HashSet<std::path::PathBuf>,
        rows: &mut Vec<TreeRow>,
    ) {
        for (name, is_dir) in self.read_dir_entries(dir) {
            let path = dir.join(&name);
            let is_expanded = is_dir && expanded.contains(&path);
            rows.push(TreeRow {
                path: path.clone(),
                label: name,
                depth,
                is_dir,
                expanded: is_expanded,
                updir: false,
            });
            if is_expanded {
                self.push_dir_rows(&path, depth + 1, expanded, rows);
            }
        }
    }

    /// One directory's entries, dirs first. Dotfiles are skipped unless the tree's
    /// `show_dotfiles` toggle is on; the `project_ignore` list is always skipped.
    fn read_dir_entries(&self, dir: &std::path::Path) -> Vec<(String, bool)> {
        let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
        let show_dot = self.file_tree.as_ref().map(|t| t.show_dotfiles).unwrap_or(false);
        let mut entries: Vec<(String, bool)> = rd
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if (name.starts_with('.') && !show_dot)
                    || self.tuning.project_ignore.iter().any(|i| i == &name)
                {
                    return None;
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                Some((name, is_dir))
            })
            .collect();
        entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.to_lowercase().cmp(&b.0.to_lowercase())));
        entries
    }

    fn handle_tree(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let none = key.modifiers.is_empty();
        let shift = key.modifiers == KeyModifiers::SHIFT;
        let len = self.tree_rows.len();
        let filter_empty = self.file_tree.as_ref().map(|t| t.filter.is_empty()).unwrap_or(true);
        match key.code {
            // `.` on an empty filter toggles dotfiles (`.env`, `.github`, …); with a
            // filter active it falls through to type-to-filter below.
            KeyCode::Char('.') if none && filter_empty => {
                let now = self.file_tree.as_mut().map(|t| { t.show_dotfiles = !t.show_dotfiles; t.selected = 0; t.show_dotfiles }).unwrap_or(false);
                self.refresh_tree_rows();
                self.status_msg = Some(if now { "dotfiles shown".into() } else { "dotfiles hidden".into() });
            }
            KeyCode::Esc | KeyCode::Char('g') if key.code == KeyCode::Esc || ctrl => {
                // Esc / C-g: clear an active filter, else close the sidebar.
                let cleared = self.file_tree.as_mut().map(|t| {
                    if t.filter.is_empty() { false } else { t.filter.clear(); t.selected = 0; true }
                }).unwrap_or(false);
                if cleared { self.refresh_tree_rows(); }
                else { self.close_tree(); }
            }
            KeyCode::Up | KeyCode::BackTab => {
                if let Some(t) = self.file_tree.as_mut() { t.selected = t.selected.saturating_sub(1); }
            }
            KeyCode::Down => {
                if let Some(t) = self.file_tree.as_mut() {
                    if t.selected + 1 < len { t.selected += 1; }
                }
            }
            KeyCode::Char('p') if ctrl => {
                if let Some(t) = self.file_tree.as_mut() { t.selected = t.selected.saturating_sub(1); }
            }
            KeyCode::Char('n') if ctrl => {
                if let Some(t) = self.file_tree.as_mut() {
                    if t.selected + 1 < len { t.selected += 1; }
                }
            }
            KeyCode::Enter => self.tree_activate(true),  // open + focus editor
            KeyCode::Right => self.tree_activate(false), // expand / preview (stay in tree)
            KeyCode::Left => self.tree_collapse(),
            KeyCode::Backspace => {
                let changed = self.file_tree.as_mut().map(|t| {
                    if t.filter.is_empty() { false } else { t.filter.pop(); t.selected = 0; true }
                }).unwrap_or(false);
                if changed { self.refresh_tree_rows(); }
            }
            KeyCode::Char(c) if none || shift => {
                if let Some(t) = self.file_tree.as_mut() { t.filter.push(c); t.selected = 0; }
                self.refresh_tree_rows();
            }
            _ => {}
        }
    }

    /// Enter/→ on a row. Folders expand and `../` re-roots for both. For a file,
    /// `commit` (Enter) opens it and focuses the editor; a preview (→) shows it
    /// but keeps you in the tree, reversibly — arrow to another file to re-preview.
    fn tree_activate(&mut self, commit: bool) {
        let Some(row) = self.tree_rows.get(self.file_tree.as_ref().map(|t| t.selected).unwrap_or(0)) else { return };
        let (path, is_dir, updir) = (row.path.clone(), row.is_dir, row.updir);
        if updir {
            if let Some(parent) = path.parent().map(|p| p.to_path_buf()) {
                if let Some(t) = self.file_tree.as_mut() { t.root = parent; t.selected = 0; }
            }
            self.refresh_tree_rows();
        } else if is_dir {
            if let Some(t) = self.file_tree.as_mut() {
                if !t.expanded.remove(&path) { t.expanded.insert(path); }
            }
            self.refresh_tree_rows();
        } else if commit {
            // Enter opens the file in a NEW TAB — a clean, non-destructive open
            // (no split, no replacing the current pane).
            self.open_file_in_new_tab(&path.to_string_lossy());
        } else {
            // → previews the file in the current pane (reversible peek).
            self.show_file_in_pane(&path, false);
        }
    }

    /// A navigator *click*: like arrowing to the row (highlight + stay in the tree),
    /// but a file previews into a single reusable tab instead of piling up one tab
    /// per click. Folders expand / `../` re-roots, exactly as the keyboard does.
    fn tree_click_open(&mut self) {
        let sel = self.file_tree.as_ref().map(|t| t.selected).unwrap_or(0);
        let Some(row) = self.tree_rows.get(sel) else { return };
        if row.updir || row.is_dir {
            self.tree_activate(false); // the dir/updir branches ignore `commit`
            return;
        }
        let path = row.path.clone();
        self.preview_file_from_tree(&path);
    }

    /// Show `path` in the preview tab, reusing it across clicks. The tab is pinned
    /// the moment its buffer is edited: after that the dirtied tab is left alone and
    /// the next click starts a fresh preview — VS Code's "italic tab" rule.
    fn preview_file_from_tree(&mut self, path: &std::path::Path) {
        if let Some(idx) = self.tabs.iter().position(|t| t.preview) {
            let pid = self.tabs[idx].focused_pane;
            let reusable = match self.panes.get(&pid).map(|p| &p.content) {
                Some(PaneContent::Editor(b)) => !self.buffers.get(b).map(|b| b.modified).unwrap_or(true),
                _ => false, // no longer a clean editor pane → don't reuse
            };
            if reusable {
                self.active_tab = idx;
                self.swap_preview_file(idx, path);
                return;
            }
            self.tabs[idx].preview = false; // edited (or repurposed) → promote to a real tab
        }
        self.open_preview_tab(path);
    }

    /// Swap the file shown in preview tab `idx`, reusing an already-open buffer and
    /// discarding the outgoing preview buffer when it's clean and unreferenced — so
    /// exploring a directory leaves neither extra tabs nor orphan buffers behind.
    fn swap_preview_file(&mut self, idx: usize, path: &std::path::Path) {
        let existing = self.buffers.values().find(|b| b.path.as_deref() == Some(path)).map(|b| b.id);
        let new_buf = match existing {
            Some(id) => id,
            None => match self.open_file(&path.to_string_lossy()) {
                Ok(id) => id,
                Err(e) => { self.status_msg = Some(format!("Can't open {}: {e}", path.display())); return; }
            },
        };
        let pid = self.tabs[idx].focused_pane;
        let old_buf = match self.panes.get(&pid).map(|p| p.content.clone()) {
            Some(PaneContent::Editor(b)) => Some(b),
            _ => None,
        };
        if let Some(pane) = self.panes.get_mut(&pid) {
            pane.content = PaneContent::Editor(new_buf);
            pane.buffer_id = new_buf;
            pane.cursor_row = 0; pane.cursor_col = 0; pane.scroll_row = 0;
            pane.selection_anchor = None;
        }
        self.tabs[idx].name = Self::file_label(path);
        if let Some(old) = old_buf {
            if old != new_buf { self.gc_orphan_buffer(old); }
        }
    }

    /// Open `path` in a NEW tab flagged as the preview slot, staying in the
    /// navigator — unlike `open_file_in_new_tab`, which jumps focus to the editor.
    fn open_preview_tab(&mut self, path: &std::path::Path) {
        match self.open_file(&path.to_string_lossy()) {
            Ok(buf_id) => {
                let pane_id = self.alloc_pane(buf_id);
                let id = self.alloc_tab_id();
                let mut tab = crate::tab::Tab::new(id, Self::file_label(path), pane_id);
                tab.preview = true;
                self.tabs.push(tab);
                self.active_tab = self.tabs.len() - 1;
            }
            Err(e) => self.status_msg = Some(format!("Can't open {}: {e}", path.display())),
        }
    }

    /// Drop a buffer no pane shows anymore — but never a modified one, and never the
    /// last buffer standing.
    fn gc_orphan_buffer(&mut self, buf: BufferId) {
        if self.buffers.len() <= 1 { return; }
        let clean = !self.buffers.get(&buf).map(|b| b.modified).unwrap_or(true);
        let referenced = self.panes.values().any(|p| matches!(p.content, PaneContent::Editor(id) if id == buf));
        if clean && !referenced {
            self.buffers.remove(&buf);
        }
    }

    /// A file's display name (its basename), falling back to the full path.
    fn file_label(path: &std::path::Path) -> String {
        path.file_name().and_then(|s| s.to_str()).map(str::to_string)
            .unwrap_or_else(|| path.to_string_lossy().to_string())
    }

    /// ←: collapse an expanded folder, else jump selection to the parent row.
    fn tree_collapse(&mut self) {
        let sel = self.file_tree.as_ref().map(|t| t.selected).unwrap_or(0);
        let Some(row) = self.tree_rows.get(sel) else { return };
        if row.is_dir && row.expanded {
            let path = row.path.clone();
            if let Some(t) = self.file_tree.as_mut() { t.expanded.remove(&path); }
            self.refresh_tree_rows();
        } else if row.depth > 0 {
            let target_depth = row.depth - 1;
            let parent = self.tree_rows[..sel].iter().rposition(|r| r.depth == target_depth);
            if let (Some(idx), Some(t)) = (parent, self.file_tree.as_mut()) { t.selected = idx; }
        }
    }

    /// Show a tree file in the focused pane. `commit` (Enter) focuses the editor;
    /// otherwise (→) it's a preview and focus stays in the tree. Reuses an already
    /// open buffer so repeated previews don't pile up duplicates.
    fn show_file_in_pane(&mut self, path: &std::path::Path, commit: bool) {
        let existing = self
            .buffers
            .values()
            .find(|b| b.path.as_deref() == Some(path))
            .map(|b| b.id);
        // Keep a visible terminal by opening the file beside it.
        if matches!(self.focused_pane().content, PaneContent::Terminal(_))
            && self.tab().layout.count() < self.tuning.max_panes
        {
            self.split_vertical();
        }
        let buf = match existing {
            Some(id) => Ok(id),
            None => self.open_file(&path.to_string_lossy()),
        };
        match buf {
            Ok(buf_id) => {
                let pid = self.focused_pane_id();
                if let Some(pane) = self.panes.get_mut(&pid) {
                    pane.content = PaneContent::Editor(buf_id);
                    pane.cursor_row = 0; pane.cursor_col = 0; pane.scroll_row = 0;
                    pane.selection_anchor = None;
                }
                if commit {
                    self.mode = Mode::Edit; // focus the editor; sidebar stays open
                    let name = path.file_name().map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string_lossy().to_string());
                    self.status_msg = Some(format!("Opened {name}"));
                }
            }
            Err(e) => self.status_msg = Some(format!("Can't open {}: {e}", path.display())),
        }
    }

    /// W3: send the shell-bar's English text to be turned into one command,
    /// which replaces the query when it returns (`ShellTranslation` event).
    fn translate_shell_query(&mut self) {
        let text = self.palette.as_ref().map(|p| p.query.clone()).unwrap_or_default();
        if text.trim().is_empty() {
            return;
        }
        let cfg = agent::AgentConfig::from_env();
        if !cfg.is_configured() {
            self.status_msg = Some("No LLM key — run `mars setup` for a free key to translate".into());
            return;
        }
        self.agent_pending = true;
        self.translate_request = Some(text.clone()); // for the corrective-memory pair
        agent::translate_shell(cfg, text, self.screen_context(), self.agent_tx.clone());
    }

    /// Editing a ready translation invalidates it — log it as an "edited" outcome
    /// (once) so the accept/edit/reject split is measurable.
    fn on_translation_edited(&mut self) {
        if self.shell_ready {
            if let Some(id) = self.translate_call_id.take() {
                crate::llm_log::record_outcome(id, None, true, false);
            }
        }
    }

    /// Run `cmd` in a terminal pane: reuse one in this tab, else open one here.
    fn run_shell_command(&mut self, cmd: &str) {
        // Prefer an existing terminal pane in the current tab.
        let term_pane = self
            .tab()
            .layout
            .pane_ids()
            .into_iter()
            .find(|id| matches!(self.panes.get(id).map(|p| &p.content), Some(PaneContent::Terminal(_))));
        if let Some(pid) = term_pane {
            self.tab_mut().focused_pane = pid;
            self.mode = Mode::Terminal;
        } else {
            self.open_terminal();
        }
        if let PaneContent::Terminal(tid) = self.focused_pane().content {
            if let Some(t) = self.terms.get_mut(&tid) {
                let mut input = cmd.as_bytes().to_vec();
                // What a keyboard Enter sends (key_to_bytes): every shell takes
                // \r as accept-line; ConPTY apps do NOT take a bare \n.
                input.push(b'\r');
                t.send_bytes(&input);
                // The work journal's `command`: the last thing mars itself ran
                // in this pane (composer + TYPE: both funnel through here).
                self.watches.entry(tid).or_default().last_command = Some(cmd.to_string());
            }
        }
    }

    /// W1/W2: open the Ask bar with a canned question and submit it at once —
    /// a zero-typing "explain / triage" gesture grounded in the live screen.
    /// The Explain-failure query, enriched with the focused terminal's failing
    /// command and exit code — facts the model needs to triage but that may have
    /// scrolled off the visible screen (or, for the exit code, never appear there).
    fn explain_failure_prompt(&self) -> String {
        let base = crate::prompts::EXPLAIN_FAILURE.trim_end();
        let tid = match self.focused_pane().content {
            PaneContent::Terminal(t) => t,
            _ => return base.to_string(),
        };
        let cmd = self
            .watches
            .get(&tid)
            .and_then(|w| w.last_command.as_ref())
            .map(|c| c.trim())
            .filter(|c| !c.is_empty());
        let exit = self.terms.get(&tid).and_then(|t| t.exit_code());
        match (cmd, exit) {
            (Some(c), Some(code)) => format!("{base}\n\nLAST COMMAND: {c} (exit {code})"),
            (Some(c), None) => format!("{base}\n\nLAST COMMAND: {c}"),
            _ => base.to_string(),
        }
    }

    fn ask_prefilled(&mut self, question: &str) {
        self.open_bar(BarMode::Ask);
        if let Some(p) = self.palette.as_mut() {
            p.query = question.to_string();
        }
        self.submit_agent_query();
    }

    /// Fire off the current Ask query to the LLM on a background thread —
    /// grounded in the live screen, with the conversation history attached.
    fn submit_agent_query(&mut self) {
        let question = self.palette.as_ref().map(|p| p.query.clone()).unwrap_or_default();
        if question.trim().is_empty() {
            return;
        }
        let mut cfg = agent::AgentConfig::from_env();
        cfg.max_tokens = self.tuning.agent_max_tokens;
        cfg.temperature = self.tuning.agent_temperature;
        if !cfg.is_configured() {
            self.agent_answer = Some(
                "⚠ No LLM key set — agent features are off. Quit and run `mars setup` for a \
                 free key (Groq/Gemini), or set GROQ_API_KEY, then relaunch."
                    .into(),
            );
            self.agent_directive = None;
            return;
        }
        self.agent_pending = true;
        self.agent_answer = None;
        self.agent_directive = None;
        self.refactor_replacement = None;
        self.last_question = question.clone();
        self.need_depth = 0;
        self.ask_scroll = 0; // snap to the newest turn
        let history = self.agent_history.clone();
        self.agent_history.push(("user".into(), question.clone()));
        if let Some(p) = self.palette.as_mut() {
            p.query.clear();
        }
        // Selection-aware: a live selection becomes precise context, and marks the
        // range a proposed refactor would replace ("translate this to French").
        // With no selection but the cursor in an editor, the target is an empty
        // range at point, so a reply's code block INSERTS there ("write a
        // limerick about potatoes").
        let mut context = self.screen_context();
        self.refactor_target = self.selection_range();
        if let Some(sel) = self.selected_text() {
            context.push_str(&sel);
        } else if let PaneContent::Editor(buf_id) = self.focused_pane().content {
            let (row, col) = {
                let p = self.focused_pane();
                (p.cursor_row, p.cursor_col)
            };
            let at = self.buffers[&buf_id].char_at(row, col);
            self.refactor_target = Some((buf_id, at, at));
            let ext = self.buffers[&buf_id]
                .path
                .as_ref()
                .and_then(|p| p.extension().and_then(|e| e.to_str()))
                .or_else(|| self.buffers[&buf_id].name.rsplit('.').nth(0))
                .unwrap_or("");
            context.push_str(
                &crate::prompts::CURSOR_INSERT
                    .replace("{line}", &(row + 1).to_string())
                    .replace("{file}", &self.buffers[&buf_id].name)
                    .replace("{lang}", lang_label(ext)),
            );
        }
        agent::ask(
            cfg,
            question,
            palette::registry_context(),
            context,
            history,
            self.agent_tx.clone(),
        );
    }

    /// Apply a confirm-gated refactor: replace the captured selection with the
    /// agent's code block, as ONE undo step (C-/ reverts the whole AI edit).
    pub fn apply_refactor(&mut self) {
        let (Some((buf_id, s, e)), Some(code)) =
            (self.refactor_target, self.refactor_replacement.take())
        else {
            return;
        };
        self.refactor_target = None;
        // Clamp both endpoints to the current buffer length — the range was
        // captured at query time and the buffer may have changed since.
        let len = self.buffers.get(&buf_id).map(|b| b.rope.len_chars()).unwrap_or(0);
        let (s, e) = (s.min(len), e.min(len));
        if let Some(buf) = self.buffers.get_mut(&buf_id) {
            buf.checkpoint(); // one reversible chunk
            buf.rope.remove(s..e);
            buf.rope.insert(s, &code);
            buf.mark_edited();
        }
        let (r, c) = self.rowcol_of(buf_id, s + code.chars().count());
        self.close_bar();
        self.clear_selection();
        self.set_cursor(r, c);
        self.mode = Mode::Edit;
        self.status_msg = Some("Refactor applied — C-/ to undo".into());
    }

    /// W4/W5: replay the last question with an extra context source the model asked
    /// for via `NEED:`. One expansion per ask (capped in `tick`), never surfaced.
    fn reask_with_need(&mut self, kind: agent::NeedKind) {
        let mut cfg = agent::AgentConfig::from_env();
        cfg.max_tokens = self.tuning.agent_max_tokens;
        cfg.temperature = self.tuning.agent_temperature;
        if !cfg.is_configured() {
            self.agent_pending = false;
            return;
        }
        let extra = self.expand_context(&kind);
        let context = format!("{}\n\n### expanded ###\n{}", self.screen_context(), extra);
        let history = self.agent_history.clone();
        let q = self.last_question.clone();
        self.agent_pending = true; // keep the spinner; the re-ask is the same turn
        agent::ask(cfg, q, palette::registry_context(), context, history, self.agent_tx.clone());
    }

    /// Render the extra source a `NEED:` asked for (full scrollback, or another tab).
    fn expand_context(&self, kind: &agent::NeedKind) -> String {
        match kind {
            agent::NeedKind::Scrollback => {
                if let PaneContent::Terminal(id) = self.focused_pane().content {
                    if let Some(t) = self.terms.get(&id) {
                        let cap = self.tuning.terminal_scrollback_lines.min(2000);
                        return format!("FULL TERMINAL SCROLLBACK:\n{}", t.history_tail(cap));
                    }
                }
                String::new()
            }
            agent::NeedKind::Tab(name) => {
                let low = name.to_lowercase();
                let Some(tab) = self.tabs.iter().find(|t| t.name.to_lowercase().contains(&low)) else {
                    return format!("(no tab matching '{name}')");
                };
                let mut out = format!("TAB {}:\n", tab.name);
                for pid in tab.layout.pane_ids() {
                    let Some(p) = self.panes.get(&pid) else { continue };
                    match p.content {
                        PaneContent::Terminal(tid) => {
                            if let Some(t) = self.terms.get(&tid) {
                                out.push_str(t.screen().contents().trim_end());
                                out.push('\n');
                            }
                        }
                        PaneContent::Editor(bid) => {
                            if let Some(b) = self.buffers.get(&bid) {
                                out.push_str(&format!("[{}]\n", b.name));
                                for line in b.rope.to_string().lines().take(120) {
                                    out.push_str(line);
                                    out.push('\n');
                                }
                            }
                        }
                    }
                }
                out
            }
        }
    }

    /// The highlighted code as a labeled context block, telling the model that a
    /// refactor request should reply with ONLY the replacement in a ``` block.
    fn selected_text(&self) -> Option<String> {
        let (buf_id, s, e) = self.selection_range()?;
        let buf = self.buffers.get(&buf_id)?;
        let text = buf.rope.slice(s..e).to_string();
        let (sr, _) = self.rowcol_of(buf_id, s);
        let (er, _) = self.rowcol_of(buf_id, e);
        Some(format!(
            "\n\nSELECTED CODE — {} lines {}-{} (the user has this highlighted). If they ask \
             to refactor/rewrite/fix/simplify it, reply with ONLY the replacement inside one \
             ``` code block and no prose:\n```\n{}\n```\n",
            buf.name,
            sr + 1,
            er + 1,
            text
        ))
    }

    /// The context-bus slice: what the user is looking at, as text the model
    /// can ground its answers in. Capped so huge buffers can't blow the prompt.
    fn screen_context(&self) -> String {
        const CAP: usize = 6 * 1024;
        let mut out = String::new();
        if let Some(s) = &self.session_name {
            out.push_str(&format!("session: {s}\n"));
        }
        let tab_names: Vec<String> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if i == self.active_tab { format!("[{}]", t.name) } else { t.name.clone() }
            })
            .collect();
        out.push_str(&format!("tabs: {}\n", tab_names.join(" ")));

        let focused = self.focused_pane_id();
        for pid in self.tab().layout.pane_ids() {
            let Some(pane) = self.panes.get(&pid) else { continue };
            let marker = if pid == focused { " (focused)" } else { "" };
            match pane.content {
                PaneContent::Editor(buf_id) => {
                    if let Some(buf) = self.buffers.get(&buf_id) {
                        out.push_str(&format!(
                            "\n--- editor pane: {}{marker}, cursor at line {} ---\n",
                            buf.name,
                            pane.cursor_row + 1
                        ));
                        // The visible window plus a little margin.
                        let from = pane.scroll_row;
                        let to = (from + pane.view_h.max(20) + 10).min(buf.line_count());
                        for row in from..to {
                            out.push_str(&buf.line_str(row));
                            out.push('\n');
                        }
                    }
                }
                PaneContent::Terminal(tid) => {
                    if let Some(t) = self.terms.get(&tid) {
                        out.push_str(&format!("\n--- terminal pane{marker} ---\n"));
                        out.push_str(t.screen().contents().trim_end());
                        out.push('\n');
                    }
                }
            }
            if out.len() > CAP {
                break;
            }
        }
        if out.len() > CAP {
            // Keep the head (layout) and the tail (most recent output).
            let head: String = out.chars().take(CAP / 3).collect();
            let tail: String = out
                .chars()
                .rev()
                .take(2 * CAP / 3)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            out = format!("{head}\n…(truncated)…\n{tail}");
        }
        out
    }

    /// Dispatch a palette ItemKind.
    fn activate_kind(&mut self, kind: Option<ItemKind>) {
        match kind {
            Some(ItemKind::Submenu(name)) => {
                if let Some(p) = self.palette.as_mut() {
                    p.push(name);
                }
            }
            Some(ItemKind::Run(action)) => {
                self.palette = None;
                self.mode = self.bar_return.clone();
                self.run_action_from_bar(action);
            }
            Some(ItemKind::Surface(s)) => self.jump_to_surface(s),
            None => {}
        }
    }

    /// Run an action chosen in the bar, and — once it's clearly a habit —
    /// nudge toward its direct keybinding (subtle, one status line, never blocks).
    fn run_action_from_bar(&mut self, action: Action) {
        let key = format!("{:?}", action);
        let uses = self.bar_uses.entry(key).or_insert(0);
        *uses += 1;
        let nudge = if *uses >= self.tuning.nudge_threshold {
            self.keys
                .binding_for(&action)
                .map(|b| format!("💡 next time: {}  ({})", b, action.label()))
        } else {
            None
        };
        self.run_action(action);
        if let Some(n) = nudge {
            self.status_msg = Some(n);
        }
    }

    /// Execute a palette action.
    /// Flip the focused editor pane into (or out of) the read-only rendered
    /// Markdown view. No-op with a hint on a terminal pane.
    fn toggle_markdown(&mut self) {
        if !matches!(self.focused_pane().content, PaneContent::Editor(_)) {
            self.status_msg = Some("Markdown view applies to editor panes only".into());
            return;
        }
        let p = self.focused_pane_mut();
        p.md_view = !p.md_view;
        p.md_scroll = 0;
        let on = p.md_view;
        self.status_msg = Some(if on {
            "Markdown view on (read-only) — toggle again to edit".into()
        } else {
            "Markdown view off".into()
        });
    }

    /// Apply a color theme live — beta. Updates the resolved palette in place
    /// (repaints immediately) and persists the choice, sidestepping the
    /// new-session-only caveat of the `mars theme` CLI.
    fn set_theme_live(&mut self, name: &str) {
        self.tuning.palette = crate::themes::resolve(Some(name));
        let _ = crate::config::set_theme(name);
        self.status_msg = Some(format!("Theme: {name} (beta)"));
        self.needs_redraw = true;
    }

    /// A cheap identity for the current theme palette — the syntax cache invalidates
    /// when it changes (a live theme switch recolors code).
    fn palette_id(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.tuning.palette.hash(&mut h);
        h.finish()
    }

    /// Toggle syntax highlighting for this session (C-x C-h). The cache persists
    /// across a toggle, so flipping it back on is instant; turning it on kicks an
    /// immediate highlight of the focused buffer rather than waiting for the idle pass.
    fn toggle_syntax_highlight(&mut self) {
        self.syntax_on = !self.syntax_on;
        if self.syntax_on {
            if let PaneContent::Editor(buf_id) = self.focused_pane().content {
                let vp = self.focused_pane().scroll_row + 80;
                self.request_highlight(buf_id, vp);
            }
            self.status_msg = Some("Syntax highlighting on".into());
        } else {
            self.status_msg = Some("Syntax highlighting off".into());
        }
        self.needs_redraw = true;
    }

    /// Snapshot the buffer and hand it to the background highlight worker, unless a
    /// pass for the current `(rev, palette)` is already cached or in flight. Never
    /// blocks: the worker runs off-thread and streams results back via `syntax_rx`.
    fn request_highlight(&mut self, buf_id: BufferId, viewport_bottom: usize) {
        if !self.syntax_on {
            return;
        }
        let Some(buf) = self.buffers.get(&buf_id) else { return };
        let Some(ext) = buf
            .path
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .map(str::to_string)
        else {
            return; // no extension → nothing to key a language on
        };
        let rev = buf.rev;
        let palette_id = self.palette_id();
        // Already displaying this exact pass, or already requested it → nothing to do.
        if self
            .syntax_cache
            .get(&buf_id)
            .map(|c| c.rev == rev && c.palette_id == palette_id)
            .unwrap_or(false)
        {
            return;
        }
        if self.syntax_want.get(&buf_id) == Some(&(rev, palette_id)) {
            return;
        }
        self.syntax_want.insert(buf_id, (rev, palette_id));
        let job = SyntaxJob {
            buf_id,
            rev,
            palette_id,
            code: buf.rope.to_string(),
            ext,
            palette: self.tuning.palette,
            viewport_bottom,
        };
        crate::syntax::highlight_stream(job, self.syntax_tx.clone());
    }

    /// Merge one streamed highlight chunk into the cache by **overwriting its line
    /// range in place** — the cache is never emptied, so on-screen cells only ever go
    /// color→color as a new pass streams in, never color→white→color. Chunks from a
    /// superseded pass (an older `(rev, palette)` than we now want) are dropped. The
    /// stale tail (if the file shrank) is trimmed only when the pass `complete`s, and
    /// the cache's `(rev, palette)` advances then too — one clean swap.
    fn apply_syntax_chunk(
        &mut self,
        buf_id: BufferId,
        rev: u64,
        palette_id: u64,
        start_line: usize,
        styles: Vec<Vec<ratatui::style::Style>>,
        complete: bool,
    ) {
        // Only accept the pass we're currently waiting for.
        if self.syntax_want.get(&buf_id) != Some(&(rev, palette_id)) {
            return;
        }
        let c = self.syntax_cache.entry(buf_id).or_insert_with(|| SyntaxCache {
            rev, palette_id, lines: Vec::new(),
        });
        let end = start_line + styles.len();
        if c.lines.len() < end {
            c.lines.resize(end, Vec::new());
        }
        for (k, s) in styles.into_iter().enumerate() {
            c.lines[start_line + k] = s;
        }
        if complete {
            c.lines.truncate(end); // drop any stale tail from the previous, longer pass
            c.rev = rev;
            c.palette_id = palette_id;
        }
        self.needs_redraw = true;
    }

    /// Keep the color cache aligned with a newline INSERT: split the cached line at
    /// the cursor column so the colors travel with their characters — the tail keeps
    /// its own colors on the new line instead of every line below showing the wrong
    /// (shifted) colors until the next re-highlight. A no-op if the line isn't cached.
    fn syntax_split_line(&mut self, buf_id: BufferId, row: usize, col: usize) {
        if let Some(c) = self.syntax_cache.get_mut(&buf_id) {
            if row < c.lines.len() {
                let at = col.min(c.lines[row].len());
                let tail = c.lines[row].split_off(at);
                c.lines.insert(row + 1, tail);
            }
        }
    }

    /// The newline-DELETE counterpart (Backspace at column 0, joining `row` up into
    /// `row - 1`): merge the two cached color-lines so nothing below shifts out of
    /// alignment. A no-op at the top of the buffer or if the line isn't cached.
    fn syntax_join_line(&mut self, buf_id: BufferId, row: usize) {
        if row == 0 {
            return;
        }
        if let Some(c) = self.syntax_cache.get_mut(&buf_id) {
            if row < c.lines.len() {
                let tail = c.lines.remove(row);
                c.lines[row - 1].extend(tail);
            }
        }
    }

    /// True when the focused pane is showing the read-only Markdown view.
    fn md_view_active(&self) -> bool {
        let p = self.focused_pane();
        p.md_view && matches!(p.content, PaneContent::Editor(_))
    }

    /// Actions that write to the buffer — blocked while the Markdown view is up.
    fn action_mutates_buffer(action: &Action) -> bool {
        matches!(
            action,
            Action::Undo
                | Action::Redo
                | Action::KillLine
                | Action::KillRegion
                | Action::Yank
                | Action::YankPop
                | Action::Paste
                | Action::KillWordForward
                | Action::KillWordBackward
        )
    }

    pub fn run_action(&mut self, action: Action) {
        self.edit_run = EditRun::None; // a command breaks the typing/backspace undo run
        // Track frecency
        let key = format!("{:?}", action);
        *self.frecency.entry(key).or_insert(0) += 1;

        // Any action other than yank/yank-pop breaks the M-y chain.
        if !matches!(action, Action::Yank | Action::YankPop) {
            self.last_yank = None;
        }

        // Read-only Markdown view: buffer-mutating commands are inert (the same
        // contract the edit-primitive guard enforces for typed keys).
        if self.md_view_active() && Self::action_mutates_buffer(&action) {
            self.status_msg = Some("Markdown view is read-only — toggle it off to edit".into());
            return;
        }

        match action {
            Action::SplitHorizontal    => self.split_horizontal(),
            Action::SplitVertical      => self.split_vertical(),
            Action::ClosePane          => self.close_pane(),
            Action::DeleteOtherWindows => self.delete_other_windows(),
            Action::NextPane           => self.focus_next_pane(),
            Action::PrevPane           => self.focus_prev_pane(),
            Action::SwapPane           => self.swap_pane(),
            Action::ZoomPane           => self.toggle_zoom(),
            Action::NewTab             => self.new_tab(),
            Action::CloseTab           => self.close_tab(),
            Action::NextTab            => self.next_tab(),
            Action::PrevTab            => self.prev_tab(),
            Action::MoveTabLeft        => self.move_tab(-1),
            Action::MoveTabRight       => self.move_tab(1),
            Action::RenameTab          => {
                let current = self.tab().name.clone();
                self.start_prompt_with(PromptKind::RenameTab, "Rename tab: ", &current);
            }
            Action::RenamePane         => {
                let current = self.focused_pane().title.clone().unwrap_or_default();
                self.start_prompt_with(PromptKind::RenamePane, "Rename pane: ", &current);
            }
            Action::RenameSession      => {
                if self.session_name.is_some() {
                    let current = self.session_name.clone().unwrap_or_default();
                    self.start_prompt_with(PromptKind::RenameSession, "Rename session: ", &current);
                } else {
                    self.status_msg =
                        Some("Not in a session — start one with: mars new <name>".into());
                }
            }
            Action::TabMode            => self.mode = Mode::Tab,
            Action::Save               => self.do_save(),
            Action::ToggleFileTree     => self.toggle_file_tree(),
            Action::ToggleMarkdown     => self.toggle_markdown(),
            Action::ToggleSyntaxHighlight => self.toggle_syntax_highlight(),
            Action::SetTheme(name)     => self.set_theme_live(&name),
            Action::RefreshIndex       => {
                self.project_index = None;
                self.ensure_project_index();
                if self.tree_open { self.refresh_tree_rows(); }
                self.status_msg = Some("File index refreshed".into());
            }
            Action::RestoreKeybindings => {
                match config::reset_keys() {
                    Ok(_) => {
                        self.keys = config::load(); // apply immediately, no restart
                        self.status_msg = Some("Default keybindings restored (old file → keys.json.bak)".into());
                    }
                    Err(e) => self.status_msg = Some(format!("Reset failed: {e}")),
                }
            }
            Action::KillBuffer         => self.kill_buffer(),
            Action::Undo               => self.do_undo(),
            Action::Redo               => self.do_redo(),
            Action::UndoMode           => self.enter_undo_mode(),
            Action::KillLine           => self.kill_line(),
            Action::KillRegion         => self.kill_region(),
            Action::CopyRegion         => self.copy_region(),
            Action::Yank               => self.yank(),
            Action::YankPop            => self.yank_pop(),
            Action::Paste              => self.paste_clipboard(),
            Action::KillWordForward    => self.kill_word(true),
            Action::KillWordBackward   => self.kill_word(false),
            Action::SelectAll          => self.select_all(),
            // In the Markdown reading-mode there's no cursor — the editor's top/bottom
            // motions scroll the document to its ends instead (same chord, M-< / M->).
            Action::GoTop if self.md_view_active() => self.focused_pane_mut().md_scroll = 0,
            Action::GoBottom if self.md_view_active() => {
                let cap = self.focused_pane().md_rendered_total.get()
                    .saturating_sub(self.focused_pane().view_h.max(1));
                self.focused_pane_mut().md_scroll = cap;
            }
            Action::GoTop              => self.move_file_start(),
            Action::GoBottom           => self.move_file_end(),
            Action::GotoLine           => self.start_prompt(PromptKind::GotoLine, "Go to line: "),
            Action::JumpBlockPrev      => self.jump_block(false),
            Action::JumpBlockNext      => self.jump_block(true),
            Action::JumpSymbolPrev     => self.jump_symbol(false),
            Action::JumpSymbolNext     => self.jump_symbol(true),
            Action::MatchBracket       => self.match_bracket(),
            Action::Recenter           => self.recenter(),
            Action::Search             => self.start_isearch(),
            Action::QueryReplace       => self.start_prompt(PromptKind::ReplaceFrom, "Query replace: "),
            Action::OpenTerminal       => self.open_terminal(),
            Action::AskAgent           => self.open_bar(BarMode::Ask),
            Action::ExplainThis        => self.ask_prefilled(crate::prompts::EXPLAIN_THIS.trim_end()),
            Action::ExplainFailure     => { let q = self.explain_failure_prompt(); self.ask_prefilled(&q); }
            Action::WatchPane          => self.toggle_watch_pane(),
            Action::ExpandNotices      => self.expand_notices(),
            Action::AwayDigest         => self.show_away_digest(),
            Action::Detach             => {
                if self.session_name.is_some() {
                    self.detach_requested = true;
                } else {
                    self.status_msg =
                        Some("Not in a session — start one with: mars --session <name>".into());
                }
            }
            Action::OpenCommandMemory  => self.open_command_memory(),
            Action::ClearCommandMemory => self.clear_command_memory(),
            Action::OpenDenylist       => self.open_denylist(),
            Action::OpenTuning         => self.open_tuning(),
            Action::OpenPersona        => self.open_persona(),
            // Quit = detach (2026-07 ruling): leaving mars never ends a
            // session — kill is the deleting verb (KillSession here, `mars
            // kill`/`mars killall` outside). Standalone has nothing to keep
            // running, so Quit still exits there (dirty-guarded).
            Action::Quit               => {
                if self.session_name.is_some() {
                    self.detach_requested = true;
                } else {
                    self.request_quit();
                }
            }
            Action::KillSession        => self.request_quit(),
        }
    }

    // ── Memory management ────────────────────────────────────────────────────
    // The stores are plain local files, so "manage memory" is "edit a buffer":
    // ownership means the user can read, edit, and delete what the agent knows.

    fn open_command_memory(&mut self) {
        match crate::retrieval::command_memory_path() {
            Some(p) if p.exists() => {
                let path = p.to_string_lossy().into_owned();
                if let Err(e) = self.open_file(&path) {
                    self.status_msg = Some(format!("couldn't open {path}: {e}"));
                }
            }
            _ => {
                self.status_msg =
                    Some("no command memory yet — accept a translated command first".into());
            }
        }
    }

    fn clear_command_memory(&mut self) {
        let n = crate::retrieval::load_command_records().len();
        if n == 0 {
            self.status_msg = Some("command memory is already empty".into());
            return;
        }
        if !self.close_confirmed {
            self.start_prompt(
                PromptKind::ConfirmAction(Action::ClearCommandMemory),
                &format!("Forget all {n} remembered command(s)?  y forget · n cancel "),
            );
            return;
        }
        if let Some(p) = crate::retrieval::command_memory_path() {
            let _ = std::fs::write(&p, "");
        }
        self.status_msg = Some(format!("forgot {n} remembered command(s)"));
    }

    fn open_denylist(&mut self) {
        let Some(p) = crate::retrieval::denylist_path() else {
            self.status_msg = Some("no HOME — can't locate ~/.mars/denylist".into());
            return;
        };
        if !p.exists() {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(
                &p,
                "# One entry per line. Any line's text appearing in command memory or\n\
                 # shell history is replaced with [REDACTED] before entering an LLM prompt.\n",
            );
        }
        let path = p.to_string_lossy().into_owned();
        if let Err(e) = self.open_file(&path) {
            self.status_msg = Some(format!("couldn't open {path}: {e}"));
        }
    }

    fn open_persona(&mut self) {
        let Some(p) = crate::persona::seed_if_missing() else {
            self.status_msg = Some("no HOME — can't locate ~/.mars/persona.md".into());
            return;
        };
        let path = p.to_string_lossy().into_owned();
        if let Err(e) = self.open_file(&path) {
            self.status_msg = Some(format!("couldn't open {path}: {e}"));
        } else {
            self.status_msg = Some("style only — empty file turns the voice off".into());
        }
    }

    fn open_tuning(&mut self) {
        let Some(p) = crate::tuning::tuning_path() else {
            self.status_msg = Some("no config dir — can't locate tuning.json".into());
            return;
        };
        if !p.exists() {
            let _ = crate::tuning::load(); // seeds the annotated defaults file
        }
        let path = p.to_string_lossy().into_owned();
        if let Err(e) = self.open_file(&path) {
            self.status_msg = Some(format!("couldn't open {path}: {e}"));
        } else {
            self.status_msg = Some("edits apply on next start (or new session)".into());
        }
    }

    // ── Terminal pane ────────────────────────────────────────────────────────

    pub fn open_terminal(&mut self) {
        // If this pane is already a terminal, just re-attach.
        if let PaneContent::Terminal(_) = self.focused_pane().content {
            self.mode = Mode::Terminal;
            return;
        }
        let id = self.next_term_id;
        self.next_term_id += 1;
        let (rows, cols) = (self.tuning.terminal_default_rows, self.tuning.terminal_default_cols);
        let scrollback = self.tuning.terminal_scrollback_lines;
        let startup_probe =
            std::time::Duration::from_millis(self.tuning.terminal_startup_probe_ms);
        // The first opened file's dir if any, else where `mars` was launched —
        // never portable-pty's default (which lands the shell at /).
        let cwd = self.startup_cwd.clone().or_else(|| self.run_cwd.clone());
        match terminal::spawn(
            id,
            rows,
            cols,
            scrollback,
            cwd,
            self.session_name.as_deref(),
            self.session_instance_id.as_deref(),
            startup_probe,
            self.term_tx.clone(),
        ) {
            Ok(term) => {
                self.terms.insert(id, term);
                let pid = self.focused_pane_id();
                if let Some(p) = self.panes.get_mut(&pid) {
                    p.content = PaneContent::Terminal(id);
                }
                self.mode = Mode::Terminal;
                self.status_msg = Some("Terminal — Ctrl+g back to editor".into());
            }
            Err(e) => {
                self.status_msg = Some(format!("Terminal failed: {}", e));
            }
        }
    }

    /// The mode a pane's content wants when it has focus.
    fn mode_for_focused_pane(&self) -> Mode {
        match self.focused_pane().content {
            PaneContent::Terminal(_) => Mode::Terminal,
            PaneContent::Editor(_) => Mode::Edit,
        }
    }

    /// Chrome layer: navigation chords are global — they mean the same thing
    /// inside a terminal pane as in the editor. Editing chords (C-k, C-c,
    /// C-x…) are NOT intercepted; they keep their shell meanings.
    fn is_chrome_action(a: &Action) -> bool {
        matches!(
            a,
            Action::NextPane | Action::PrevPane | Action::SwapPane
                | Action::NextTab | Action::PrevTab | Action::MoveTabLeft
                | Action::MoveTabRight | Action::NewTab | Action::TabMode
                | Action::SplitHorizontal | Action::SplitVertical
        )
    }

    fn handle_terminal(&mut self, key: KeyEvent) {
        // Ctrl+Space from a terminal opens the unified composer, one keystroke: a
        // red inline overlay anchored at the cursor (type in place, the terminal
        // stays visible) AND a ↑/↓ menu of matching Mars commands above the bar.
        // Enter runs a picked command; with no match it's a shell command
        // (LLM-translated + confirmed). `!` forces pure-shell; `?` asks the agent.
        let chord = chord_of(&key);
        if self.keys.bar_open.contains(&chord) || matches!(key.code, KeyCode::Null) {
            self.open_bar(BarMode::Command);
            return;
        }
        // Ctrl+g detaches back to the editor.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            if let KeyCode::Char('g') = key.code {
                self.mode = Mode::Edit;
                return;
            }
        }
        // A proactive notice is up: Esc dismisses it here (one keypress budget)
        // rather than leaking 0x1b to the shell — so the notice's "Esc dismiss"
        // hint is honest even when it renders over a focused terminal pane.
        if matches!(key.code, KeyCode::Esc) && !self.notices.is_empty() {
            self.dismiss_notice();
            return;
        }
        // Global chrome chords (single-chord only — prefixes belong to the shell).
        if let Some(action) = self.keys.lookup(std::slice::from_ref(&chord)) {
            if Self::is_chrome_action(&action) {
                self.run_action(action);
                // Follow the (possibly new) focused pane — unless the action
                // opened a transient mode of its own (travel mode).
                if !matches!(self.mode, Mode::Tab | Mode::Bar) {
                    self.mode = self.mode_for_focused_pane();
                }
                return;
            }
        }
        // Chrome primitives: M-1..9 tab jump, M-/Ctrl+arrows pane focus.
        let alt  = key.modifiers.contains(KeyModifiers::ALT);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char(c) if alt && c.is_ascii_digit() => {
                self.goto_tab((c as u8 - b'0') as usize);
                self.mode = self.mode_for_focused_pane();
                return;
            }
            KeyCode::Left if alt || ctrl => {
                self.focus_direction(-1, 0);
                self.mode = self.mode_for_focused_pane();
                return;
            }
            KeyCode::Right if alt || ctrl => {
                self.focus_direction(1, 0);
                self.mode = self.mode_for_focused_pane();
                return;
            }
            KeyCode::Up if alt || ctrl => {
                self.focus_direction(0, -1);
                self.mode = self.mode_for_focused_pane();
                return;
            }
            KeyCode::Down if alt || ctrl => {
                self.focus_direction(0, 1);
                self.mode = self.mode_for_focused_pane();
                return;
            }
            _ => {}
        }
        let term_id = match self.focused_pane().content {
            PaneContent::Terminal(id) => id,
            _ => {
                self.mode = Mode::Edit;
                return;
            }
        };

        // Dead shell: the pane only waits to be dismissed.
        if self.terms.get(&term_id).map(|t| t.exited).unwrap_or(false) {
            if matches!(key.code, KeyCode::Enter | KeyCode::Char('q')) {
                self.close_terminal_pane(term_id);
            }
            return;
        }

        // Scrollback view: Shift+PageUp/PageDown page through history.
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if shift && matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
            let page = self.focused_pane().view_h.max(2) as i64 - 1;
            let delta = if key.code == KeyCode::PageUp { page } else { -page };
            if let Some(t) = self.terms.get_mut(&term_id) {
                t.scroll_view(delta);
            }
            return;
        }

        let bytes = key_to_bytes(&key);
        if !bytes.is_empty() {
            self.last_input_tick = self.frame_tick; // active work — anchors the away window
            if let Some(t) = self.terms.get_mut(&term_id) {
                t.scroll_to_live(); // typing snaps out of scrollback
                t.send_bytes(&bytes);
            }
        }
    }

    /// Dismiss an exited terminal: close the pane, or recycle the last pane
    /// back into an editor showing a scratch/existing buffer.
    fn close_terminal_pane(&mut self, term_id: TermId) {
        self.terms.remove(&term_id);
        if self.tab().layout.count() > 1 {
            self.close_pane();
        } else {
            let buf = match self.buffers.keys().next().copied() {
                Some(b) => b,
                None => self.new_scratch(),
            };
            let pid = self.focused_pane_id();
            if let Some(p) = self.panes.get_mut(&pid) {
                p.content = PaneContent::Editor(buf);
                p.cursor_row = 0;
                p.cursor_col = 0;
                p.scroll_row = 0;
            }
        }
        self.mode = self.mode_for_focused_pane();
    }

    // ── Main loop ────────────────────────────────────────────────────────────

    /// One housekeeping tick: animation counter + PTY/agent event drains.
    /// Called every loop iteration whether or not a client is attached.
    pub fn tick(&mut self) {
        self.frame_tick = self.frame_tick.wrapping_add(1);

        // Live elapsed on the workspaces board: while the command bar is open, repaint
        // ~once a second if any workstream is running, so its counter ticks. Cheap —
        // the board scan only runs once a second and only in bar mode.
        if matches!(self.mode, Mode::Bar) {
            let tps = (1000 / self.tuning.poll_interval_ms.max(1)).max(1);
            if self.frame_tick % tps == 0
                && self.bar_workspace_rows().iter().any(|r| {
                    matches!(&r.kind, crate::palette::ItemKind::Surface(s)
                        if s.verdict == crate::briefing::Verdict::Running)
                })
            {
                self.needs_redraw = true;
            }
        }

        // Host-health probes for the SPACES line. Cheap metrics sample continuously so
        // the memory average stays warm; the GPU poll (off-thread) and the repaint only
        // happen while the panel (bar) is open. `maybe_sample` self-throttles to the
        // configured cadence.
        if self.tuning.health_line == 1 {
            let vis = matches!(self.mode, Mode::Bar);
            let cwd = self
                .startup_cwd
                .clone()
                .or_else(|| self.run_cwd.clone())
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            if self.health.maybe_sample(&cwd, vis) && vis {
                self.needs_redraw = true;
            }
        }

        for term in self.terms.values_mut() {
            term.flush_startup_input();
        }

        // Drain terminal signals (repaint next frame); mark dead shells and feed
        // the watch clock (W6: output resets quiet, exit queues a verdict).
        let now = self.frame_tick;
        while let Ok(ev) = self.term_rx.try_recv() {
            self.needs_redraw = true; // terminal content moved → repaint
            match ev {
                TermEvent::Output(id) => {
                    if !self.terms.contains_key(&id) {
                        continue;
                    }
                    let auto = self.tuning.auto_watch == 1;
                    let min_ticks = (self.tuning.watch_min_active_secs * 1000
                        / self.tuning.poll_interval_ms.max(1))
                        .max(1);
                    let w = if auto {
                        Some(self.watches.entry(id).or_default())
                    } else {
                        self.watches.get_mut(&id)
                    };
                    if let Some(w) = w {
                        // A fresh run begins when output resumes after a quiet/fired
                        // gap (or on the very first output) — stamp its start.
                        if w.triggered || w.run_started_tick == 0 {
                            w.run_started_tick = now;
                        }
                        w.last_output_tick = now;
                        w.triggered = false;
                        // Auto-watch: sustained output = a real run, not an `ls` —
                        // arm a verdict without the user reaching for C-x w. Marked
                        // `auto` so it stays silent on idle-shell noise.
                        if auto && !w.watched && now.saturating_sub(w.run_started_tick) >= min_ticks {
                            w.watched = true;
                            w.auto = true;
                        }
                    }
                }
                TermEvent::Exited(id) => {
                    let Some(t) = self.terms.get_mut(&id) else { continue };
                    if t.exited {
                        continue;
                    }
                    t.exited = true;
                    let watched = self.watches.get(&id).map(|w| w.watched && !w.triggered).unwrap_or(false);
                    if watched {
                        if !self.pending_watch.iter().any(|(qid, _)| *qid == id) {
                            self.pending_watch.push((id, WatchReason::Exit)); // gets an LLM verdict
                        }
                    } else {
                        // An unwatched shell ending is context, not an accomplishment.
                        self.push_away(AwayKind::Context, "shell exited".into(), None);
                    }
                }
            }
        }

        // Silent autosave of modified, path-backed buffers.
        let secs = self.tuning.autosave_secs;
        if secs > 0 {
            let ticks_per_save = (secs * 1000 / self.tuning.poll_interval_ms.max(1)).max(1);
            if self.frame_tick % ticks_per_save == 0 {
                self.autosave();
            }
        }

        // Drain streamed syntax-highlight chunks into the per-buffer cache.
        let mut chunks = Vec::new();
        while let Ok(ev) = self.syntax_rx.try_recv() {
            chunks.push(ev);
        }
        for ev in chunks {
            match ev {
                SyntaxEvent::Chunk { buf_id, rev, palette_id, start_line, styles, complete } => {
                    self.apply_syntax_chunk(buf_id, rev, palette_id, start_line, styles, complete);
                }
            }
        }
        // Idle re-highlight: ~½s after the last edit, refresh the focused code buffer
        // so its colors catch up to the current revision. Debounced off `last_input_tick`
        // so it never fires mid-keystroke — the stale cache bridges the gap on screen.
        if self.syntax_on {
            let debounce = (self.tuning.syntax_recolor_ms / self.tuning.poll_interval_ms.max(1)).max(1);
            if self.frame_tick.saturating_sub(self.last_input_tick) >= debounce {
                if let PaneContent::Editor(buf_id) = self.focused_pane().content {
                    let vp = self.focused_pane().scroll_row + 80;
                    self.request_highlight(buf_id, vp);
                }
            }
        }

        // Drain background LLM-agent events.
        let mut events = Vec::new();
        while let Ok(ev) = self.agent_rx.try_recv() {
            events.push(ev);
        }
        if !events.is_empty() {
            self.needs_redraw = true; // an answer / verdict / rename landed
        }
        for ev in events {
            match ev {
                AgentEvent::Answer { text, directive } => {
                    // W4/W5: a NEED: request re-asks once with the extra source and
                    // is never surfaced (no history push, spinner keeps spinning).
                    if let Some(agent::AgentDirective::Need(kind)) = &directive {
                        if self.need_depth < 1 {
                            self.need_depth += 1;
                            self.reask_with_need(kind.clone());
                            continue;
                        }
                    }
                    self.agent_pending = false;
                    self.agent_partial = None;
                    // If the query targeted a selection and the reply carries a code
                    // block, offer it as a confirm-gated replacement (a refactor).
                    if self.refactor_target.is_some() {
                        self.refactor_replacement = extract_code_block(&text);
                    }
                    self.agent_history.push(("assistant".into(), text));
                    self.agent_directive = directive;
                    self.ask_scroll = 0; // show the new turn
                }
                AgentEvent::AnswerStart => {
                    self.agent_partial = Some(String::new());
                }
                AgentEvent::AnswerDelta { text } => {
                    self.agent_partial.get_or_insert_with(String::new).push_str(&text);
                }
                AgentEvent::AutoName { tab_id, name } => {
                    self.bg_busy = false;
                    // Apply only if the tab still wears its default numeric
                    // name — a user rename always wins the race.
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == tab_id) {
                        if tab.name.chars().all(|c| c.is_ascii_digit()) {
                            tab.name = name;
                        }
                    }
                }
                AgentEvent::SessionName { name } => {
                    self.bg_busy = false;
                    // Rename only if still numeric (user/explicit names win).
                    let numeric = self
                        .session_name
                        .as_ref()
                        .map(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
                        .unwrap_or(false);
                    if numeric {
                        match crate::session::validate_session_name(&name) {
                            Ok(()) => self.rename_session_to = Some(name),
                            Err(e) => {
                                self.status_msg =
                                    Some(format!("Ignored invalid generated session name: {e}"));
                            }
                        }
                    }
                }
                AgentEvent::ShellTranslation { command, call_id } => {
                    self.agent_pending = false;
                    // Only meaningful if still composing a shell command.
                    let is_shell = self
                        .palette
                        .as_ref()
                        .map(|p| p.bar_mode == BarMode::Shell)
                        .unwrap_or(false);
                    if is_shell {
                        if let Some(p) = self.palette.as_mut() {
                            p.query = command;
                        }
                        self.shell_ready = true; // Enter now runs the translated command
                        self.translate_call_id = Some(call_id); // correlate the outcome
                        self.agent_answer = None; // clear any prior error
                    }
                }
                AgentEvent::Error(e) => {
                    self.agent_pending = false;
                    self.agent_partial = None;
                    self.bg_busy = false;
                    self.agent_answer = Some(format!("⚠ {}", e));
                    self.agent_directive = None;
                }
                AgentEvent::BgDone => {
                    self.bg_busy = false;
                }
                AgentEvent::SurfaceSummary { term_id, text } => {
                    // On-demand summary: set the surface's verdict/summary (the panel
                    // shows it) and clear the in-flight + freshness guards. No notice
                    // side-effects — the user pulled it and is looking at it.
                    if let Some(w) = self.watches.get_mut(&term_id) {
                        w.verdict = Some(text);
                        w.summ_inflight = false;
                        w.summ_output_tick = w.last_output_tick;
                    }
                    self.needs_redraw = true;
                }
                AgentEvent::WatchSummary { term_id, verdict } => {
                    // NOT bg-done: a batched shift call emits several of these
                    // before its own BgDone; individual watch calls send BgDone
                    // separately. Never clear the gate from here.
                    let blocked = verdict.to_lowercase().starts_with("blocked");
                    let failed = !blocked
                        && (verdict.to_lowercase().contains("fail")
                            || verdict.to_lowercase().contains("error"));
                    let tab = self.tab_label_of_term(term_id);
                    let dur = self.watches.get(&term_id).and_then(|w| {
                        (w.run_started_tick > 0).then(|| now.saturating_sub(w.run_started_tick))
                    });
                    if let Some(w) = self.watches.get_mut(&term_id) {
                        w.verdict = Some(verdict.clone());
                    }
                    // Telemetry coming in: an on-screen report row absorbs the
                    // verdict directly and subsumes the notice.
                    let mut on_report = false;
                    if let Some(rep) = self.shift_report.as_mut() {
                        if let Some(row) =
                            rep.rows.iter_mut().find(|r| r.term_id == Some(term_id))
                        {
                            row.verdict = crate::briefing::classify(&verdict, row.verdict);
                            row.text = verdict.clone();
                            row.settling = false;
                            rep.sort_rows();
                            on_report = true;
                        }
                    }
                    if !on_report {
                        self.notices.push(Notice {
                            text: format!("{verdict}{tab}"),
                            kind: if failed {
                                NoticeKind::Failure
                            } else if blocked {
                                NoticeKind::Blocked
                            } else {
                                NoticeKind::Info
                            },
                        });
                        // Failures surface first.
                        self.notices.sort_by(|a, b| a.kind.cmp(&b.kind));
                    }
                    // Also record it for the Away Digest (with the run's duration).
                    self.push_away(
                        if failed || blocked { AwayKind::NeedsYou } else { AwayKind::Done },
                        format!("{verdict}{tab}"),
                        dur,
                    );
                    // And into the work journal — the persistent stream of
                    // what-was-happening snapshots (mission inference, mars ls).
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let cwd = self
                        .terms
                        .get(&term_id)
                        .and_then(|t| t.spawn_cwd.as_ref())
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    let (command, exit, error_excerpt) = self
                        .watches
                        .get_mut(&term_id)
                        .map(|w| (w.last_command.clone(), w.fired_exit.take(), w.fired_excerpt.take()))
                        .unwrap_or_default();
                    crate::worklog::record(&crate::worklog::WorkEntry {
                        ts,
                        session: self.session_label(),
                        tab: tab.trim_start_matches(" · ").trim().to_string(),
                        verdict: verdict.clone(),
                        failed,
                        dur_secs: dur.map(|t| t * self.tuning.poll_interval_ms / 1000),
                        cwd,
                        command,
                        exit,
                        // The excerpt is evidence for failures; successes keep
                        // the journal lean (verdict alone).
                        error_excerpt: failed.then_some(error_excerpt).flatten(),
                    });
                    self.maybe_infer_mission(ts);
                }
                AgentEvent::Mission { text } => {
                    self.bg_busy = false;
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    crate::worklog::save_mission(&self.session_label(), &text, ts);
                }
                AgentEvent::ShiftDelta { text } => {
                    // The persona-voiced briefing streaming in. The first delta
                    // replaces the deterministic template; the rest append —
                    // typewriter-in for free (stateless render diffs each frame).
                    if let Some(rep) = self.shift_report.as_mut() {
                        if !rep.narrative_from_model {
                            rep.narrative.clear();
                            rep.narrative_from_model = true;
                            // Anchor the typewriter clock to the first token, so the
                            // prose reveals at a steady pace no matter how bursty the
                            // network delivery is.
                            rep.stream_started_at = Some(std::time::Instant::now());
                        }
                        rep.narrative.push_str(&text);
                    }
                }
                AgentEvent::ShiftDone => {
                    // The enrichment call finished. If the model produced tokens, log
                    // the finalized briefing for continuity (the next return reports
                    // progress against it). If it produced nothing — errored, timed
                    // out, no key or tunnel reachable from the daemon — KEEP the
                    // deterministic briefing: the mission board (clock, manifest,
                    // greeting) IS the briefing, not a throwaway stub. Yanking it out
                    // from under the user was the "it flashes then vanishes" bug —
                    // and a detached daemon that can't reach the model hit it every
                    // reattach. Just settle the streaming state so the final frame
                    // renders stably. (Keyless sessions never fire the call; their
                    // deterministic briefing already stands.)
                    if let Some(rep) = self.shift_report.as_ref() {
                        if rep.narrative_from_model {
                            let ts = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            crate::worklog::log_briefing(
                                &self.session_label(), &rep.narrative, &rep.facts, rep.away_secs, ts,
                            );
                        }
                    }
                    if let Some(rep) = self.shift_report.as_mut() {
                        rep.narrative_streaming = false;
                    }
                    self.needs_redraw = true;
                }
                AgentEvent::Goals { goals } => {
                    self.bg_busy = false;
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    crate::worklog::save_goals(&self.session_label(), &goals, ts);
                }
            }
        }

        self.maybe_auto_name();
        self.maybe_auto_name_session();
        self.maybe_fire_watches();

        // Timer-driven surfaces that animate without input: the agent spinner
        // (every frame while thinking) and the which-key panel (appears a few
        // ticks after a prefix is held). Redraw while either is live; otherwise
        // an idle screen stays quiet — no draw, no flush, silent link.
        if self.agent_pending || !self.pending_prefix.is_empty() {
            self.needs_redraw = true;
        }
        // The Mission Briefing boots up on reattach: redraw while its ~0.5s
        // reveal animation is running (or while its narrative streams). Both are
        // self-terminating — once revealed and the stream is done, this goes
        // quiet and no idle frames are sent.
        if let Some(rep) = &self.shift_report {
            let animate = self.tuning.mission_briefing_animate == 1;
            let booting = animate && rep.shown_at.elapsed().as_millis() < crate::briefing::BOOT_TOTAL_MS;
            // The typewriter can still be catching up to the received text after the
            // stream itself has closed — keep redrawing until it lands the last char.
            let ms = self.tuning.mission_briefing_type_ms.max(1) as u128;
            let typing = animate
                && rep.stream_started_at
                    .map(|s| s.elapsed().as_millis() / ms < rep.narrative.chars().count() as u128)
                    .unwrap_or(false);
            if booting || typing || rep.narrative_streaming {
                self.needs_redraw = true;
            }
        }
    }

    /// Append an event to the bounded away-log ring (the Away Digest source).
    /// First-run nudge when no LLM key is configured: one dismissible notice. The
    /// editor and multiplexer work without a key — this only points the way to the
    /// agent features. Esc clears it like any other notice.
    pub fn notice_no_key(&mut self) {
        self.notices.push(Notice {
            text: "No LLM key set — agent features are off. Run `mars setup` for a free key.".into(),
            kind: NoticeKind::Info,
        });
    }

    pub fn push_away(&mut self, kind: AwayKind, text: String, dur_ticks: Option<u64>) {
        self.away_log.push(AwayEvent {
            tick: self.frame_tick,
            kind,
            text,
            dur_ticks,
        });
        const CAP: usize = 200;
        if self.away_log.len() > CAP {
            let drop = self.away_log.len() - CAP;
            self.away_log.drain(0..drop);
        }
    }

    /// Human duration from a tick span ("45s" / "4m12s" / "3h02m").
    fn fmt_dur(&self, ticks: u64) -> String {
        let secs = ticks * self.tuning.poll_interval_ms.max(1) / 1000;
        if secs < 60 {
            format!("{secs}s")
        } else if secs < 3600 {
            format!("{}m{:02}s", secs / 60, secs % 60)
        } else {
            format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
        }
    }

    /// Toggle watching the focused terminal pane (W6).
    fn toggle_watch_pane(&mut self) {
        let PaneContent::Terminal(id) = self.focused_pane().content else {
            self.status_msg = Some("Watch works on a terminal pane".into());
            return;
        };
        let now = self.frame_tick;
        let w = self.watches.entry(id).or_default();
        w.watched = !w.watched;
        w.auto = false; // an explicit watch summarizes everything, idle included
        w.last_output_tick = now;
        w.triggered = false;
        let watching = w.watched;
        self.status_msg = Some(if watching {
            if agent::AgentConfig::from_env().is_configured() {
                "Watching this pane — I'll summarize it when it quiets (~20s) or exits".into()
            } else {
                "Watching — but set GROQ_API_KEY/GEMINI_API_KEY for the AI summary".into()
            }
        } else {
            "Stopped watching this pane".into()
        });
    }

    /// Capture a cheap snapshot when the last client detaches; the away_log carries
    /// events, the snapshot only what isn't event-shaped (which buffers were dirty).
    pub fn on_detach(&mut self) {
        self.client_attached = false;
        self.detach_tick = Some(self.frame_tick);
        self.detach_snapshot = Some(Snapshot {
            dirty: self.buffers.values().filter(|b| b.modified).map(|b| b.name.clone()).collect(),
        });
        // Capture what the user was working toward, so the reattach briefing can
        // report progress against it. One low-tier call over the live panes +
        // recent journal; best-effort (a remote detach may find the tunnel gone).
        // Fire even if another background call is in flight — detach is the
        // one-shot moment we most need to capture, and a concurrent watch summary
        // must never cancel it. The daemon keeps ticking headless after the client
        // leaves (session.rs), so the call completes and its Goals event is
        // processed; the deterministic summary floor covers us if it still fails.
        if self.tuning.goal_tracking == 1 {
            let cfg = agent::AgentConfig::from_env();
            if cfg.is_configured() {
                let evidence = self.goal_evidence();
                if !evidence.trim().is_empty() {
                    // Mark the summary in flight so `mars ls` shows "…summarizing…"
                    // instead of a stale line until the fresh goals land.
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    crate::worklog::mark_summarizing(&self.session_label(), ts);
                    self.bg_busy = true;
                    agent::capture_goals(cfg, evidence, self.agent_tx.clone());
                }
            }
        }
    }

    /// What happened in EVERY pane while away — the raw material for the
    /// briefing. Not just watched panes: every terminal's recent tail (labeled
    /// by tab, redacted) plus each editor pane's file, so the narrative can
    /// summarize the whole workspace and surface anything that needs a decision.
    /// `active` panes (output moved since the away window opened) are marked so
    /// the model leads with them.
    fn all_pane_activity(&self, from: u64) -> String {
        let mut parts: Vec<String> = Vec::new();
        for tab in &self.tabs {
            for pid in tab.layout.pane_ids() {
                match self.panes.get(&pid).map(|p| &p.content) {
                    Some(&PaneContent::Terminal(tid)) => {
                        let tail = self.terminal_tail(tid, 40);
                        if tail.trim().is_empty() {
                            continue;
                        }
                        let moved = self
                            .watches
                            .get(&tid)
                            .map(|w| w.last_output_tick >= from)
                            .unwrap_or(false);
                        let mark = if moved { " (active while you were away)" } else { "" };
                        parts.push(format!(
                            "PANE [{}]{mark}:\n{}",
                            tab.name,
                            crate::retrieval::redact(&tail)
                        ));
                    }
                    Some(&PaneContent::Editor(bid)) => {
                        if let Some(b) = self.buffers.get(&bid) {
                            let dirty = if b.modified { " (unsaved edits)" } else { "" };
                            parts.push(format!("EDITOR [{}]{dirty}", b.name));
                        }
                    }
                    _ => {}
                }
            }
        }
        parts.join("\n\n")
    }

    /// Evidence for goal capture: the tail of each live terminal pane plus the
    /// last few work-journal verdicts — what's in flight right now.
    fn goal_evidence(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        for tab in &self.tabs {
            for pid in tab.layout.pane_ids() {
                if let Some(&PaneContent::Terminal(tid)) = self.panes.get(&pid).map(|p| &p.content) {
                    let tail = self.terminal_tail(tid, 24);
                    if !tail.trim().is_empty() {
                        parts.push(format!("[{}]\n{}", tab.name, crate::retrieval::redact(&tail)));
                    }
                }
            }
        }
        let recent = crate::worklog::recent(&self.session_label(), 5);
        if !recent.is_empty() {
            let lines: Vec<String> = recent.iter().map(|e| format!("- {}", e.verdict)).collect();
            parts.push(format!("recent:\n{}", lines.join("\n")));
        }
        parts.join("\n\n")
    }

    /// Build the shift report (the save-state restore). Returns true when there
    /// is something to show. Deterministic tier-0 rows render immediately;
    /// ambiguous rows are marked settling and covered by ONE batched low-tier
    /// call — the overlay frame is never blocked on a model.
    fn build_shift_report(&mut self, from: u64) -> bool {
        use crate::briefing::{self, ReportRow, Verdict};
        let now = self.frame_tick;
        let ms = self.tuning.poll_interval_ms.max(1);
        let t2s = |ticks: u64| ticks * ms / 1000;
        let mut rows: Vec<ReportRow> = Vec::new();
        // 1. Verdicts and context that already landed while away (precomputed at
        //    event time — the zero-latency path for keyed/local sessions).
        for e in self.away_log.iter().filter(|e| e.tick >= from) {
            let default = match e.kind {
                AwayKind::NeedsYou => Verdict::Failed,
                AwayKind::Done => Verdict::Done,
                AwayKind::Context => Verdict::Context,
            };
            rows.push(ReportRow {
                verdict: briefing::classify(&e.text, default),
                tab: String::new(), // away text already carries its tab suffix
                text: e.text.clone(),
                ago_secs: Some(t2s(now.saturating_sub(e.tick))),
                dur_secs: e.dur_ticks.map(t2s),
                term_id: None,
                cwd: None,
                exit: None,
                error_excerpt: None,
                settling: false,
            });
        }
        // 2. Watched panes with no verdict yet (held triggers, quiet panes,
        //    still-running work): tier-0 triage — deterministic, no per-pane LLM.
        let candidates: Vec<TermId> = self
            .watches
            .iter()
            .filter(|(_, w)| w.watched && !w.triggered)
            .map(|(id, _)| *id)
            .collect();
        for id in candidates {
            let Some(exited) = self.terms.get(&id).map(|t| t.exited) else { continue };
            let running = !exited;
            let exit = self.terms.get_mut(&id).and_then(|t| t.exited.then(|| t.exit_code()).flatten());
            let tail = self.terminal_tail(id, self.tuning.agent_scrollback_context);
            let tri = briefing::triage(&tail, exit, running);
            let (started, last_out) = self
                .watches
                .get(&id)
                .map(|w| (w.run_started_tick, w.last_output_tick))
                .unwrap_or((0, 0));
            let dur_secs = (started > 0).then(|| t2s(last_out.saturating_sub(started)));
            // An auto-watched pane that's just an idle shell (or a clean quit) isn't
            // a workstream — keep it off the report. BUT a genuine WIN earns its
            // place: a pane that ran real work past the good-news duration and
            // concluded without failing gets its teal row even though it exited
            // clean. This is the only way an auto-watched success reaches the board
            // (is_noteworthy passes only failures/blocks); the short-run threshold
            // keeps a bare `exit` from an idle shell out.
            let auto = self.watches.get(&id).map(|w| w.auto).unwrap_or(false);
            let notable_win = !running
                && tri.verdict == Verdict::Done
                && dur_secs.map(|d| d >= briefing::GOODNEWS_SECS).unwrap_or(false);
            if auto && !briefing::is_noteworthy(&tail, exit) && !notable_win {
                continue;
            }
            let tab = self.tab_label_of_term(id).trim_start_matches(" · ").trim().to_string();
            let cwd = self
                .terms
                .get(&id)
                .and_then(|t| t.spawn_cwd.as_ref())
                .map(|p| p.display().to_string());
            let excerpt: Option<String> = if matches!(tri.verdict, Verdict::Failed | Verdict::Blocked) {
                let e: String = tail
                    .lines()
                    .rev()
                    .take(5)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .map(crate::retrieval::redact)
                    .collect::<Vec<_>>()
                    .join("\n");
                (!e.trim().is_empty()).then_some(e)
            } else {
                None
            };
            rows.push(ReportRow {
                verdict: tri.verdict,
                tab: tab.clone(),
                text: tri.text.clone(),
                ago_secs: (!running).then(|| t2s(now.saturating_sub(last_out))),
                dur_secs,
                term_id: Some(id),
                cwd: cwd.clone(),
                exit,
                error_excerpt: excerpt.clone(),
                settling: false,
            });
            if !running {
                // The pane concluded: consume its trigger and journal the
                // deterministic verdict (no per-pane LLM — the narrative call
                // below does the prose in one shot).
                self.pending_watch.retain(|(qid, _)| *qid != id);
                if let Some(w) = self.watches.get_mut(&id) {
                    w.triggered = true;
                }
                let command = self.watches.get(&id).and_then(|w| w.last_command.clone());
                let failed = tri.verdict == Verdict::Failed;
                let prefix = if failed { "failed" } else { "done" };
                let ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                crate::worklog::record(&crate::worklog::WorkEntry {
                    ts,
                    session: self.session_label(),
                    tab,
                    verdict: format!("{prefix}: {}", tri.text),
                    failed,
                    dur_secs: rows.last().and_then(|r| r.dur_secs),
                    cwd: cwd.unwrap_or_default(),
                    command,
                    exit,
                    error_excerpt: failed.then_some(excerpt).flatten(),
                });
            }
        }
        // ITERATION MODE: always greet on return, even a quiet one — so the
        // briefing quality can be tuned. The eventful-only gate (the calm
        // "absence is the feature" default) is the scale-down lever for later:
        //   if rows.is_empty() && crate::worklog::load_goals(&self.session_label()).is_empty() {
        //       return false;
        //   }
        // The report subsumes notices queued while detached.
        self.notices.retain(|n| !rows.iter().any(|r| n.text.contains(r.text.as_str())));
        self.digest_from_tick = Some(from);
        let mission = crate::worklog::load_mission(&self.session_label()).map(|(m, _)| m);
        let mut report = briefing::ShiftReport {
            away_secs: t2s(now.saturating_sub(from)),
            mission: mission.clone(),
            rows,
            suggestion: None,
            narrative: String::new(),
            narrative_streaming: false,
            narrative_from_model: false,
            facts: String::new(),
            stream_started_at: None,
            shown_at: std::time::Instant::now(),
        };
        report.sort_rows();
        // A compact manifest distillation, logged when the briefing finalizes so
        // the NEXT return can report progress against it (the continuity spine).
        report.facts = report
            .rows
            .iter()
            .map(|r| {
                let w = match r.verdict {
                    Verdict::Failed => "failed",
                    Verdict::Blocked => "blocked",
                    Verdict::Done => "done",
                    Verdict::Running => "running",
                    Verdict::Context => "·",
                };
                format!("{w}: {}", r.text)
            })
            .collect::<Vec<_>>()
            .join(" · ");
        // The plain-English briefing: start with the deterministic one-liner
        // (instant, keyless-safe), then let the persona-voiced version stream in
        // and replace it. Evidence is the sorted rows' facts.
        report.narrative = report.deterministic_narrative();
        // The evidence the narrative summarizes: the deterministic verdicts (what
        // concluded / needs you) AND the raw activity in every pane (so nothing
        // that happened in an unwatched pane is missed — the "all quiet" bug).
        let verdicts = report.rows.iter().map(|r| r.evidence()).collect::<Vec<_>>().join("\n");
        let pane_activity = self.all_pane_activity(from);
        let mut evidence = String::new();
        if !verdicts.trim().is_empty() {
            evidence.push_str(&format!("Verdicts (what concluded or needs you):\n{verdicts}\n\n"));
        }
        if !pane_activity.trim().is_empty() {
            evidence.push_str(&format!("Everything on the panes right now:\n{pane_activity}\n\n"));
        }
        // The goals captured at detach: the briefing reports progress against
        // them by comparing to what actually happened on the panes above.
        let goals = crate::worklog::load_goals(&self.session_label());
        if !goals.is_empty() {
            let g = goals.iter().map(|g| format!("- {g}")).collect::<Vec<_>>().join("\n");
            evidence = format!("What they were working toward:\n{g}\n\n{evidence}");
        }
        crate::llm_log::event(
            "shift_report_shown",
            serde_json::json!({
                "rows": report.rows.len(),
                "failures": report.rows.iter().filter(|r| r.verdict == Verdict::Failed).count(),
                "away_secs": report.away_secs,
            }),
        );
        let away = crate::briefing::fmt_secs(report.away_secs);
        // Continuity: what the LAST briefing said, so this one can note progress.
        let prev = crate::worklog::load_last_briefing(&self.session_label())
            .map(|p| format!("{}: {}", crate::worklog::ago(p.ts), p.facts))
            .unwrap_or_default();
        // Captured for the keyless log path (no ShiftDone will fire without a key).
        let (det_narrative, facts, away_secs) =
            (report.narrative.clone(), report.facts.clone(), report.away_secs);
        self.shift_report = Some(report);
        // Fire the narrative AFTER the overlay exists, so the prose streams into
        // a screen already on display — the frame is never blocked on the model.
        let cfg = agent::AgentConfig::from_env();
        if cfg.is_configured() {
            self.bg_busy = true;
            if let Some(rep) = self.shift_report.as_mut() {
                rep.narrative_streaming = true; // template stays until the first delta
            }
            agent::shift_brief(cfg, away, mission.unwrap_or_default(), prev, evidence, self.agent_tx.clone());
        } else {
            // Keyless: log the deterministic briefing now, so continuity still
            // threads across returns even without a model.
            let ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            crate::worklog::log_briefing(&self.session_label(), &det_narrative, &facts, away_secs, ts);
        }
        true
    }

    /// W7 → Away Digest: on reattach, summarize everything logged since detach as
    /// one duration-anchored headline (failures first); `↵` opens the full digest.
    /// Deterministic — verdict text is the only LLM-derived part, and it was
    /// produced earlier through the normal `watch_summary` seam (broker-proxied in
    /// the future); a keyless box still gets the full digest. Quiet when idle.
    pub fn on_attach(&mut self) {
        self.client_attached = true;
        let Some(snap) = self.detach_snapshot.take() else { return };
        let from = self.detach_tick.take().unwrap_or(0);
        // Fold file changes since detach into the log as Context events, so the
        // digest owns the whole story (names, not just a count).
        let dirty: Vec<String> = self
            .buffers
            .values()
            .filter(|b| b.modified && !snap.dirty.contains(&b.name))
            .map(|b| b.name.clone())
            .collect();
        if !dirty.is_empty() {
            let text = format!(
                "{} file{} modified ({})",
                dirty.len(),
                if dirty.len() == 1 { "" } else { "s" },
                dirty.join(", ")
            );
            self.push_away(AwayKind::Context, text, None);
        }
        // Reattach briefing: mission + the last few work-journal snapshots as
        // an assistant turn — "where was I?" is answerable the moment the ask
        // panel opens, and the context rides along with follow-up questions
        // (build_messages sends the recent turns). Deterministic; no LLM call.
        let session = self.session_label();
        let mission = crate::worklog::load_mission(&session);
        let recent = crate::worklog::recent(&session, 5);
        if mission.is_some() || !recent.is_empty() {
            let mut brief = String::from("Where you left off\n");
            if let Some((m, _)) = &mission {
                brief.push_str(&format!("  mission: {m}\n"));
            }
            for e in &recent {
                let mark = if e.failed { "✗" } else { "✓" };
                brief.push_str(&format!(
                    "  {mark} {} [{}] {}\n",
                    crate::worklog::ago(e.ts),
                    e.tab,
                    e.verdict
                ));
            }
            self.agent_history.push(("assistant".into(), brief));
        }
        // The save-state restore: full overlay (2, default), classic notice (1),
        // or nothing (0). The overlay path owns dedupe/digest bookkeeping.
        match self.tuning.mission_briefing {
            2 => {
                // Window from when the keyboard last went silent for work, not the
                // formal detach — so a job that ran while you sat idle is covered.
                let window = if self.last_input_tick > 0 {
                    self.last_input_tick.min(from)
                } else {
                    from
                };
                self.build_shift_report(window);
                return;
            }
            0 => {
                self.digest_from_tick = Some(from);
                return;
            }
            _ => {} // 1 → the classic one-line notice below
        }
        // Headline items from the away window: failures lead, then the rest.
        let events: Vec<&AwayEvent> = self.away_log.iter().filter(|e| e.tick >= from).collect();
        let mut items: Vec<String> = Vec::new();
        let mut failed = false;
        for e in events.iter().filter(|e| e.kind == AwayKind::NeedsYou) {
            items.push(format!("✗ {}", e.text));
            failed = true;
        }
        let done = events.iter().filter(|e| e.kind == AwayKind::Done).count();
        for e in events.iter().filter(|e| e.kind == AwayKind::Done).take(2) {
            items.push(format!("✓ {}", e.text));
        }
        if done > 2 {
            items.push(format!("+{} more done", done - 2));
        }
        for e in events.iter().filter(|e| e.kind == AwayKind::Context) {
            items.push(e.text.clone());
        }
        if items.is_empty() {
            return; // nothing happened — no briefing
        }
        // The headline subsumes watch notices queued while detached — drop the
        // duplicates so reattach greets with ONE line, not a stack.
        self.notices.retain(|n| !events.iter().any(|e| n.text == e.text));
        self.digest_from_tick = Some(from); // the "away digest" action re-summons details
        let away = self.fmt_dur(self.frame_tick.saturating_sub(from));
        // Honesty invariant, situated: the digest hint shows the live binding —
        // but reattach usually lands in a terminal pane, where a prefix chord
        // like C-x g goes to the shell, not Mars. So prefix it with the
        // leave-terminal step (C-g) whenever the focus is a terminal.
        let in_term = matches!(self.focused_pane().content, PaneContent::Terminal(_));
        let hint = match self.keys.binding_for(&Action::AwayDigest) {
            Some(b) if in_term => format!(" · C-g then {b} · digest"),
            Some(b) => format!(" · {b} digest"),
            None => String::new(),
        };
        self.notices.push(Notice {
            text: format!("while away {away} — {}{hint}", items.join(" · ")),
            kind: if failed { NoticeKind::Failure } else { NoticeKind::Info },
        });
        self.notices.sort_by(|a, b| a.kind.cmp(&b.kind));
    }

    /// Render the Away Digest (events since the last detach window — or the whole
    /// log if never detached) into the ask transcript: sectioned, timestamped
    /// relative ("12m ago"), scrollable, re-summonable. Fully deterministic.
    fn show_away_digest(&mut self) {
        let from = self.digest_from_tick.unwrap_or(0);
        let events: Vec<AwayEvent> =
            self.away_log.iter().filter(|e| e.tick >= from).cloned().collect();
        if events.is_empty() {
            self.status_msg = Some("Away digest: nothing notable recorded".into());
            return;
        }
        let now = self.frame_tick;
        let mut out = String::from("While you were away\n");
        for (kind, title) in [
            (AwayKind::NeedsYou, "✗ needs you"),
            (AwayKind::Done, "✓ done"),
            (AwayKind::Context, "· context"),
        ] {
            let section: Vec<&AwayEvent> = events.iter().filter(|e| e.kind == kind).collect();
            if section.is_empty() {
                continue;
            }
            out.push_str(&format!("\n{title}\n"));
            for e in section {
                let ago = self.fmt_dur(now.saturating_sub(e.tick));
                let dur = e
                    .dur_ticks
                    .map(|d| format!(", ran {}", self.fmt_dur(d)))
                    .unwrap_or_default();
                out.push_str(&format!("  {ago} ago — {}{dur}\n", e.text));
            }
        }
        self.agent_history.push(("assistant".into(), out));
        self.ask_scroll = 0;
        self.open_bar(BarMode::Ask);
    }

    pub fn session_label(&self) -> String {
        self.session_name.clone().unwrap_or_else(|| "standalone".to_string())
    }

    /// Debounced background mission inference: at most one per
    /// `mission_refresh_secs`, only with enough journal signal, never while
    /// another background task holds the gate.
    fn maybe_infer_mission(&mut self, now: u64) {
        let refresh = self.tuning.mission_refresh_secs;
        if refresh == 0 || self.bg_busy {
            return;
        }
        let session = self.session_label();
        if let Some((_, as_of)) = crate::worklog::load_mission(&session) {
            if now.saturating_sub(as_of) < refresh {
                return;
            }
        }
        let entries = crate::worklog::recent(&session, 15);
        if entries.len() < 2 {
            return;
        }
        let cfg = agent::AgentConfig::from_env();
        if !cfg.is_configured() {
            return;
        }
        let lines: Vec<String> = entries
            .iter()
            .map(|e| {
                let mark = if e.failed { "✗" } else { "✓" };
                format!("{} {} [{}] {}", mark, crate::worklog::ago(e.ts), e.tab, e.verdict)
            })
            .collect();
        self.bg_busy = true;
        agent::infer_mission(cfg, lines, self.agent_tx.clone());
    }

    /// Expand every pending notice into one digest turn in the ask panel and
    /// clear the queue — the "read them all at once" alternative to Esc-ing
    /// through notices one by one.
    fn expand_notices(&mut self) {
        if self.notices.is_empty() {
            self.status_msg = Some("no pending notices".into());
            return;
        }
        let mut out = String::from("Pending notices\n");
        for n in &self.notices {
            let mark = if n.kind == NoticeKind::Failure { "✗" } else { "·" };
            out.push_str(&format!("  {mark} {}\n", n.text));
        }
        self.notices.clear();
        self.agent_history.push(("assistant".into(), out));
        self.ask_scroll = 0;
        self.open_bar(BarMode::Ask);
    }

    /// Dismiss the front (highest-priority) notice, if any. Returns true if one popped.
    pub fn dismiss_notice(&mut self) -> bool {
        if self.notices.is_empty() {
            false
        } else {
            self.notices.remove(0);
            true
        }
    }

    /// W6: summarize a watched terminal that just went quiet or exited. One global
    /// in-flight gate (`bg_busy`); a foreground ask always preempts. Runs inside the
    /// daemon's `tick`, so it fires even while detached.
    fn maybe_fire_watches(&mut self) {
        if self.bg_busy || self.agent_pending {
            return;
        }
        let quiet_ticks =
            self.tuning.watch_quiet_secs * 1000 / self.tuning.poll_interval_ms.max(1);
        let now = self.frame_tick;
        // The pane the user is looking at right now never earns a quiet-fire
        // LLM verdict — they ARE the verdict. (Exit fires still do: rare,
        // high-value, and the pane may die unnoticed behind a split.)
        let focused_term = match self.focused_pane().content {
            PaneContent::Terminal(id) if self.client_attached => Some(id),
            _ => None,
        };
        let quiet_ready = |id: &TermId, w: &WatchState| {
            w.watched
                && !w.triggered
                && now.saturating_sub(w.last_output_tick) > quiet_ticks
                && Some(*id) != focused_term
        };
        // Peek: is anything ready to fire? (don't consume the trigger yet.)
        let candidate = !self.pending_watch.is_empty()
            || self.watches.iter().any(|(id, w)| quiet_ready(id, w));
        if !candidate {
            return;
        }
        // Remote box, tunnel down (you're detached): HOLD every trigger — don't
        // consume it — so the verdict fires on reattach instead of being lost.
        let cfg = agent::AgentConfig::from_env();
        if cfg.provider == "broker" && !cfg.is_configured() {
            return;
        }
        // The oldest queued exit trigger wins; else the first quiet watched pane.
        let fire = if self.pending_watch.is_empty() {
            self.watches
                .iter()
                .find(|(id, w)| quiet_ready(id, w))
                .map(|(id, _)| (*id, WatchReason::Quiet))
        } else {
            Some(self.pending_watch.remove(0))
        };
        let Some((id, reason)) = fire else { return };
        if let Some(w) = self.watches.get_mut(&id) {
            w.triggered = true;
        }
        // Auto-watch stays silent on the boring shell lifecycle (idle prompt, a
        // clean user-initiated exit) — those aren't work, and summarizing them
        // fills the journal (hence the mission) with "user quit" noise. Consume
        // the trigger, spend no tokens, journal nothing. Manual watches always
        // summarize. The pane can re-fire once real output resumes.
        if self.watches.get(&id).map(|w| w.auto).unwrap_or(false) {
            let exit = self.terms.get_mut(&id).and_then(|t| t.exited.then(|| t.exit_code()).flatten());
            let tail = self.terminal_tail(id, 12);
            if !crate::briefing::is_noteworthy(&tail, exit) {
                return;
            }
        }
        if !cfg.is_configured() {
            // No key at all (not broker): the trigger is consumed, but tier-0
            // triage still owes the user a deterministic verdict — a keyless
            // mars watches with exit codes and heuristics instead of a model.
            let tail = self.terminal_tail(id, self.tuning.agent_scrollback_context);
            let (exited, exit) = self
                .terms
                .get_mut(&id)
                .map(|t| (t.exited, t.exited.then(|| t.exit_code()).flatten()))
                .unwrap_or((true, None));
            let tri = crate::briefing::triage(&tail, exit, !exited);
            let prefix = match tri.verdict {
                crate::briefing::Verdict::Failed => "failed",
                crate::briefing::Verdict::Blocked => "blocked",
                _ => "done",
            };
            if let Some(w) = self.watches.get_mut(&id) {
                w.fired_exit = exit;
            }
            let _ = self.agent_tx.send(agent::AgentEvent::WatchSummary {
                term_id: id,
                verdict: format!("{prefix}: {}", tri.text),
            });
            return;
        }
        let tail = self.terminal_tail(id, self.tuning.agent_scrollback_context);
        // Stash the deterministic outcome evidence for the journal now, while
        // the pane state is in hand: the redacted last lines of the tail and,
        // on exit, the shell's exit code (the verdict arrives async later).
        let excerpt: String = tail
            .lines()
            .rev()
            .take(5)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(crate::retrieval::redact)
            .collect::<Vec<_>>()
            .join("\n");
        let excerpt = excerpt.chars().take(400).collect::<String>();
        let exit = match reason {
            WatchReason::Exit => self.terms.get_mut(&id).and_then(|t| t.exit_code()),
            WatchReason::Quiet => None,
        };
        if let Some(w) = self.watches.get_mut(&id) {
            w.fired_excerpt = (!excerpt.trim().is_empty()).then_some(excerpt);
            w.fired_exit = exit;
        }
        self.bg_busy = true;
        agent::watch_summary(cfg, id, reason, tail, exit, self.tuning.watch_quiet_secs, self.agent_tx.clone());
    }

    /// Workspaces-panel `s`: pull an on-demand summary for the highlighted surface.
    fn request_summary_for_selected(&mut self) {
        let sel = self.palette.as_ref().map(|p| p.sel_ws).unwrap_or(0);
        let tid = match self.bar_workspace_rows().into_iter().nth(sel).map(|r| r.kind) {
            Some(crate::palette::ItemKind::Surface(s)) => {
                match self.panes.get(&s.pane_id).map(|p| &p.content) {
                    Some(PaneContent::Terminal(t)) => *t,
                    _ => { self.status_msg = Some("nothing to summarize — not a terminal".into()); return; }
                }
            }
            _ => return,
        };
        self.request_summary(tid);
    }

    /// Fire an on-demand LLM summary (low tier) for a terminal — the classifier, run
    /// manually. Guards against excessive firing: at most one in flight per surface,
    /// and no re-fire unless new output has arrived since the last summary.
    pub(crate) fn request_summary(&mut self, tid: TermId) {
        let last_out = self.watches.get(&tid).map(|w| w.last_output_tick).unwrap_or(0);
        let (inflight, fresh) = self
            .watches
            .get(&tid)
            .map(|w| (w.summ_inflight, w.verdict.is_some() && last_out <= w.summ_output_tick))
            .unwrap_or((false, false));
        if inflight { self.status_msg = Some("summarizing…".into()); return; }
        if fresh { self.status_msg = Some("summary is current".into()); return; } // freshness guard
        let tail = self.terminal_tail(tid, self.tuning.agent_scrollback_context);
        if tail.trim().is_empty() { self.status_msg = Some("nothing to summarize yet".into()); return; }
        let cfg = agent::AgentConfig::from_env();
        if !cfg.is_configured() {
            // Keyless: a deterministic triage line stands in for the LLM summary.
            let exited = self.terms.get(&tid).map(|t| t.exited).unwrap_or(false);
            let exit = self.terms.get(&tid).and_then(|t| t.exit_code());
            let tri = crate::briefing::triage(&tail, exit, !exited);
            let _ = self.agent_tx.send(agent::AgentEvent::SurfaceSummary { term_id: tid, text: tri.text });
            return;
        }
        if let Some(w) = self.watches.get_mut(&tid) {
            w.summ_inflight = true;
            w.summ_output_tick = last_out;
        }
        self.bg_busy = true;
        self.status_msg = Some("summarizing…".into());
        agent::summarize_surface(cfg, tid, tail, self.agent_tx.clone());
    }

    /// The last `lines` of a terminal pane's visible screen, for a watch summary.
    fn terminal_tail(&self, id: TermId, lines: usize) -> String {
        let Some(t) = self.terms.get(&id) else { return String::new() };
        let contents = t.screen().contents();
        let rows: Vec<&str> = contents.lines().collect();
        let start = rows.len().saturating_sub(lines);
        rows[start..].join("\n")
    }

    /// A " · <tab>/<n panes>" locator suffix for a watched terminal's notice.
    fn tab_label_of_term(&self, id: TermId) -> String {
        for tab in &self.tabs {
            for pid in tab.layout.pane_ids() {
                if let Some(p) = self.panes.get(&pid) {
                    if matches!(p.content, PaneContent::Terminal(tid) if tid == id) {
                        return format!("  · {}", tab.name);
                    }
                }
            }
        }
        String::new()
    }

    /// One-shot AI naming of a still-numeric session (numbered → AI → explicit).
    fn maybe_auto_name_session(&mut self) {
        if self.session_name_attempted || self.tuning.auto_name_secs == 0 {
            return;
        }
        let numeric = self
            .session_name
            .as_ref()
            .map(|s| !s.is_empty() && s.chars().all(|c| c.is_ascii_digit()))
            .unwrap_or(false);
        if !numeric {
            self.session_name_attempted = true; // explicitly named already
            return;
        }
        // Give it a little longer than tab-naming so there's real activity.
        let ticks = (self.tuning.auto_name_secs * 2 * 1000 / self.tuning.poll_interval_ms.max(1)).max(1);
        if self.frame_tick % ticks != 0 || self.bg_busy {
            return;
        }
        let cfg = agent::AgentConfig::from_env();
        if !cfg.is_configured() {
            return;
        }
        self.session_name_attempted = true;
        self.bg_busy = true;
        agent::name_session(cfg, self.screen_context(), self.agent_tx.clone());
    }

    /// With an agent configured, quietly name the active tab once it has
    /// content and is still called "1"/"2"/…. Manual renames opt a tab out.
    fn maybe_auto_name(&mut self) {
        let secs = self.tuning.auto_name_secs;
        if secs == 0 || self.bg_busy {
            return;
        }
        let ticks = (secs * 1000 / self.tuning.poll_interval_ms.max(1)).max(1);
        if self.frame_tick % ticks != 0 {
            return;
        }
        let tab = self.tab();
        let (tab_id, default_named) =
            (tab.id, tab.name.chars().all(|c| c.is_ascii_digit()));
        if !default_named || self.auto_name_attempted.contains(&tab_id) {
            return;
        }
        // Only bother once there's something to name.
        let has_content = self.tab().layout.pane_ids().iter().any(|pid| {
            match self.panes.get(pid).map(|p| &p.content) {
                Some(PaneContent::Editor(b)) => {
                    self.buffers.get(b).map(|b| b.rope.len_chars() > 40).unwrap_or(false)
                }
                Some(PaneContent::Terminal(_)) => true,
                None => false,
            }
        });
        if !has_content {
            return;
        }
        let cfg = agent::AgentConfig::from_env();
        if !cfg.is_configured() {
            return;
        }
        self.auto_name_attempted.insert(tab_id);
        self.bg_busy = true;
        agent::auto_name(cfg, tab_id, self.screen_context(), self.agent_tx.clone());
    }

    /// Apply one source-agnostic input event.
    pub fn apply_input(&mut self, ev: InputEvent) -> Result<()> {
        match ev {
            // Crossterm always reports key releases on Windows. Treating them
            // as input duplicates every character and command; held-key repeats
            // are intentional and remain active.
            InputEvent::Key(key) if key.kind != KeyEventKind::Release => self.handle_key(key)?,
            InputEvent::Key(_) => {}
            InputEvent::Mouse(m) => self.handle_mouse(m),
            InputEvent::Paste(s) => self.paste_text(&s),
            InputEvent::Resize(_, _) => {} // session server rebuilds its viewport
        }
        Ok(())
    }

    /// Standalone main loop: draw, tick, and consume events from `events`
    /// (fed by a TTY-reader thread) until quit.
    pub fn run<W: io::Write>(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<W>>,
        events: &mpsc::Receiver<InputEvent>,
    ) -> Result<()> {
        loop {
            self.tick();
            // Only redraw (and flush) when something visible changed — an idle
            // screen must not stream no-op frames, especially over SSH.
            if self.needs_redraw {
                terminal.draw(|f| ui::render(f, self))?;
                self.needs_redraw = false;
                if let Some(osc) = self.take_osc() {
                    use std::io::Write as _;
                    let w = terminal.backend_mut(); // CrosstermBackend forwards Write
                    let _ = w.write_all(osc.as_bytes());
                    let _ = w.flush();
                }
            }

            match events.recv_timeout(Duration::from_millis(self.tuning.poll_interval_ms)) {
                Ok(first) => {
                    // Apply the first event, then drain whatever else queued.
                    let mut visible = first.forces_redraw();
                    self.apply_input(first)?;
                    while let Ok(ev) = events.try_recv() {
                        visible |= ev.forces_redraw();
                        self.apply_input(ev)?;
                    }
                    if visible {
                        self.needs_redraw = true; // input → repaint
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break, // input source gone
            }

            if self.detach_requested {
                // Standalone has nothing to detach from; session servers use
                // their own loop and consume this flag before we get here.
                self.detach_requested = false;
                self.status_msg =
                    Some("Not in a session — start one with: mars --session <name>".into());
            }
            if self.should_quit {
                break;
            }
        }
        self.save_state();
        Ok(())
    }

    pub fn save_state_now(&self) {
        self.save_state();
    }

    // ── Mouse ────────────────────────────────────────────────────────────────

    /// Start a fresh frame's hit registry. Called by `ui::render` before drawing.
    pub fn hits_clear(&self) {
        self.hits.borrow_mut().clear();
        *self.hover_tip.borrow_mut() = None;
    }

    /// Record a clickable region. Called by whichever renderer drew it, which is
    /// the only place that knows the rectangle.
    pub fn hit(&self, rect: Rect, target: HitTarget) {
        self.hits.borrow_mut().push(HitRegion { rect, target });
    }

    /// Whether `t` is the region under the pointer / held down — the two questions
    /// a renderer asks to light a chip on hover or draw it pressed.
    pub fn is_hovered(&self, t: &HitTarget) -> bool { self.hovered.as_ref() == Some(t) }
    pub fn is_pressed(&self, t: &HitTarget) -> bool { self.pressed.as_ref() == Some(t) }

    /// Topmost region under a point: last drawn wins.
    fn hit_test(&self, col: u16, row: u16) -> Option<HitTarget> {
        self.hits
            .borrow()
            .iter()
            .rev()
            .find(|h| {
                col >= h.rect.x
                    && col < h.rect.x + h.rect.width
                    && row >= h.rect.y
                    && row < h.rect.y + h.rect.height
            })
            .map(|h| h.target.clone())
    }

    /// Resolve a clicked region. Row targets move the selection and then run the
    /// keyboard's own activation path, so a click can never mean something the
    /// keyboard doesn't already mean.
    fn dispatch_hit(&mut self, target: HitTarget) {
        // Click-to-teach: a click flashes the chord it stands in for, so the mouse
        // teaches the keyboard rather than replacing it (the on-ramp doctrine). Read
        // the chord before dispatch, show it after so `run_action` can't clobber it.
        let teach = match &target {
            HitTarget::Act(a) => self.keys.binding_for(a),
            HitTarget::FocusEditor => Some("C-g".to_string()),
            HitTarget::OpenBar => {
                let s = self
                    .keys
                    .bar_open
                    .iter()
                    .map(|c| crate::config::render_chords(std::slice::from_ref(c)))
                    .collect::<Vec<_>>()
                    .join(" / ");
                if s.is_empty() { None } else { Some(s) }
            }
            _ => None,
        };
        match target {
            HitTarget::Act(a) => self.run_action(a),
            HitTarget::OpenBar => {
                self.bar_return = self.mode.clone();
                self.open_bar(BarMode::Command);
            }
            HitTarget::FocusEditor => {
                // Exactly what C-g does from a terminal: hand the keyboard back to the
                // editor. The pane still shows the shell; you've just left its capture.
                self.mode = Mode::Edit;
            }
            HitTarget::DismissNotice => {
                self.dismiss_notice();
            }
            // Pressing a boundary arms a drag; the motion does the work.
            HitTarget::Divider { path, vertical, origin, span } => {
                self.border_drag = Some((path, vertical, origin, span));
            }
            HitTarget::Row(RowKind::Tab, i) => self.goto_tab(i + 1),
            HitTarget::Row(RowKind::Tree, i) => {
                // A navigator click mirrors keyboard navigation: move the highlight
                // to this row and stay IN the tree (Tree mode → the row renders
                // selected). A folder expands; a file previews into ONE reusable tab
                // rather than committing a new tab per click.
                if let Some(t) = self.file_tree.as_mut() {
                    t.selected = i;
                }
                self.mode = Mode::Tree;
                self.tree_click_open();
            }
            HitTarget::Row(RowKind::Command, i) => {
                if let Some(p) = self.palette.as_mut() {
                    p.selected = i;
                    p.navigated = true;
                }
                let kind = self.bar_rows().into_iter().nth(i).map(|r| r.kind);
                self.activate_kind(kind);
            }
            HitTarget::Row(RowKind::Workspace, i) => {
                if let Some(p) = self.palette.as_mut() {
                    p.sel_ws = i;
                }
                if let Some(crate::palette::ItemKind::Surface(s)) =
                    self.bar_workspace_rows().into_iter().nth(i).map(|r| r.kind)
                {
                    self.jump_to_surface(s);
                }
            }
        }
        if let Some(chord) = teach {
            self.status_msg = Some(format!("↦ {chord}"));
        }
        self.needs_redraw = true;
    }

    /// Click focuses a pane (and positions the cursor); wheel scrolls.
    /// Chrome registered in the hit registry is clickable from ANY mode — the
    /// tab bar and the navigator belong to Mars whatever owns the keyboard.
    /// Pane interiors keep their own path below, still Edit/Terminal only.
    pub fn handle_mouse(&mut self, m: MouseEvent) {
        // Hover, in every mode and before anything else: track the region under
        // the pointer (resolved against the last frame's still-valid registry) so
        // chrome can light it. Only a *change* repaints — bare motion is otherwise
        // dropped by `forces_redraw`, so a resting pointer costs nothing.
        if matches!(m.kind, MouseEventKind::Moved) {
            let now = self.hit_test(m.column, m.row);
            if now != self.hovered {
                self.hovered = now;
                self.needs_redraw = true;
            }
            return;
        }
        // Releasing the button ends the pressed-flash. Fall through so the border
        // drag's own `Up` handling below still runs.
        if matches!(m.kind, MouseEventKind::Up(MouseButton::Left)) && self.pressed.is_some() {
            self.pressed = None;
            self.needs_redraw = true;
        }
        // The ask transcript scrolls under the wheel too (same as the Up/Down
        // keys), so reviewing past turns doesn't require leaving the mouse.
        if matches!(self.mode, Mode::Bar)
            && matches!(self.palette.as_ref().map(|p| &p.bar_mode), Some(BarMode::Ask))
        {
            match m.kind {
                MouseEventKind::ScrollUp => {
                    self.ask_scroll = self.ask_scroll.saturating_add(self.tuning.wheel_scroll_lines);
                }
                MouseEventKind::ScrollDown => {
                    self.ask_scroll = self.ask_scroll.saturating_sub(self.tuning.wheel_scroll_lines);
                }
                _ => {}
            }
            return;
        }
        // Registry first, in every mode: chrome and overlays are Mars's, and a
        // click on them must work while a prompt, the bar, or the tree owns the
        // keyboard. Pane interiors register nothing, so they fall through.
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            if let Some(target) = self.hit_test(m.column, m.row) {
                self.pressed = Some(target.clone());
                self.dispatch_hit(target);
                return;
            }
        }
        // A boundary drag outlives the press: it must keep resizing in any mode,
        // and it outranks whatever the pointer is now over (you are allowed to
        // drag a divider straight across another pane).
        if let Some((path, vertical, origin, span)) = self.border_drag.clone() {
            match m.kind {
                // Drag only, never Moved: SGR reports held-button motion as Drag,
                // so accepting bare motion buys nothing and would make the panes
                // follow the pointer forever if a release were ever missed.
                MouseEventKind::Drag(MouseButton::Left) => {
                    if span > 0 {
                        let at = if vertical { m.column } else { m.row };
                        let ratio = (at.saturating_sub(origin) as u32 * 100 / span as u32) as u16;
                        let tab = self.tab_mut();
                        tab.layout.set_ratio(&path, ratio);
                        self.needs_redraw = true;
                    }
                    return;
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.border_drag = None;
                    return;
                }
                _ => {}
            }
        }
        // Pane interaction (focus, selection, wheel) also works FROM the navigator:
        // a click on a pane leaves the tree and focuses it (so you can type), and the
        // wheel scrolls the pane under the pointer without a click first. The tree
        // sidebar registers its own hit regions above, so a click there is dispatched
        // before we ever reach here — only clicks on an actual pane fall through.
        if !matches!(self.mode, Mode::Edit | Mode::Terminal | Mode::Tree) {
            return;
        }
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let hit = self
                    .pane_rects
                    .iter()
                    .find(|(_, r)| {
                        m.column >= r.x && m.column < r.x + r.width
                            && m.row >= r.y && m.row < r.y + r.height
                    })
                    .map(|(id, r)| (*id, *r));
                let (pane_id, rect) = match hit { Some(h) => h, None => return };
                self.tab_mut().focused_pane = pane_id;
                match self.panes.get(&pane_id).map(|p| p.content.clone()) {
                    Some(PaneContent::Terminal(tid)) => {
                        self.mode = Mode::Terminal;
                        // Begin a drag-selection at the clicked cell (screen coords).
                        let (rows, cols) = self
                            .terms
                            .get(&tid)
                            .map(|t| t.screen().size())
                            .unwrap_or((0, 0));
                        let vh = rows.min(rect.height.saturating_sub(2));
                        let vw = cols.min(rect.width.saturating_sub(2));
                        let (ox, oy) = (rect.x + 1, rect.y + 1);
                        let cell = (
                            m.row.saturating_sub(oy).min(vh.saturating_sub(1)),
                            m.column.saturating_sub(ox).min(vw.saturating_sub(1)),
                        );
                        self.term_sel = Some(TermSel { tid, ox, oy, vw, vh, anchor: cell, end: cell });
                    }
                    Some(PaneContent::Editor(buf_id)) => {
                        self.mode = Mode::Edit;
                        // Inner area = rect minus 1-cell border; text starts
                        // after the line-number gutter.
                        let inner_x = rect.x + 1 + crate::ui::gutter_width(&self.tuning);
                        let inner_y = rect.y + 1;
                        if m.row >= inner_y && m.column >= rect.x + 1 {
                            let scroll = self.panes[&pane_id].scroll_row;
                            let row = scroll + (m.row - inner_y) as usize;
                            let row = row.min(self.buffers[&buf_id].line_count().saturating_sub(1));
                            let col = (m.column.saturating_sub(inner_x)) as usize;
                            let col = col.min(self.buffers[&buf_id].line_len(row));
                            self.clear_selection();
                            let p = self.panes.get_mut(&pane_id).unwrap();
                            p.cursor_row = row;
                            p.cursor_col = col;
                            p.col_affinity = col;
                            match self.click_count(m.column, m.row) {
                                // Double: the word under the pointer. Triple: the
                                // whole line. Both leave a normal selection behind,
                                // so every existing verb (C-w, M-w, refactor) works
                                // on it unchanged.
                                2 => self.select_word_at(pane_id, buf_id, row, col),
                                n if n >= 3 => self.select_line_at(pane_id, buf_id, row),
                                // Single: remember where the press landed; a drag
                                // (not the press) is what turns it into a region.
                                _ => self.editor_drag = Some((pane_id, row, col)),
                            }
                        }
                    }
                    None => {}
                }
            }
            // Terminal wheel = tmux's three-way dispatch. Mars's own scrollback
            // is only ONE of the destinations: a full-screen app (alternate
            // screen — Claude Code, less, vim) has no scrollback at all, so the
            // wheel must become input to the app, not a silent no-op.
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(m.kind, MouseEventKind::ScrollUp);
                let n = self.tuning.wheel_scroll_lines;
                // A terminal UNDER THE POINTER scrolls even without focus or a click,
                // so a shell keeps scrolling while the navigator (or another pane) holds
                // the keyboard — "scroll the thing I'm pointing at". Editor panes keep
                // their focused/Edit-mode behavior in the fallback below.
                let ptr_term = self.pane_rects.iter()
                    .find(|(_, r)| m.column >= r.x && m.column < r.x + r.width
                        && m.row >= r.y && m.row < r.y + r.height)
                    .and_then(|(id, r)| match self.panes.get(id).map(|p| p.content.clone()) {
                        Some(PaneContent::Terminal(tid)) => Some((tid, *r)),
                        _ => None,
                    });
                if let Some((tid, rect)) = ptr_term {
                    let Some(t) = self.terms.get_mut(&tid) else { return };
                    let screen = t.screen();
                    let delta = if up { n as i64 } else { -(n as i64) };
                    if t.view_offset() > 0 {
                        t.scroll_view(delta);
                    } else if screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None {
                        let (mut x, mut y) = (1u16, 1u16);
                        if m.column > rect.x && m.row > rect.y {
                            x = (m.column - rect.x).min(rect.width.saturating_sub(2).max(1));
                            y = (m.row - rect.y).min(rect.height.saturating_sub(2).max(1));
                        }
                        let bytes = encode_wheel(&screen, up, x, y);
                        t.send_bytes(&bytes);
                    } else if screen.alternate_screen() {
                        let seq: &[u8] = match (up, screen.application_cursor()) {
                            (true, false) => b"\x1b[A",
                            (true, true) => b"\x1bOA",
                            (false, false) => b"\x1b[B",
                            (false, true) => b"\x1bOB",
                        };
                        for _ in 0..n { t.send_bytes(seq); }
                    } else {
                        t.scroll_view(delta);
                    }
                    return;
                }
                let fid = self.focused_pane_id();
                let rect = self.pane_rects.iter().find(|(id, _)| *id == fid).map(|(_, r)| *r);
                match self.focused_pane().content {
                    PaneContent::Terminal(tid) => {
                        let Some(t) = self.terms.get_mut(&tid) else { return };
                        let screen = t.screen();
                        let delta = if up { n as i64 } else { -(n as i64) };
                        if t.view_offset() > 0 {
                            // Already browsing mars scrollback: the wheel keeps
                            // operating on the view until it returns to live,
                            // so scrollback is always escapable.
                            t.scroll_view(delta);
                        } else if screen.mouse_protocol_mode() != vt100::MouseProtocolMode::None {
                            // The inner app owns the mouse — forward the wheel
                            // press in the app's own encoding, pane-relative,
                            // 1-based (inner area starts past the border cell).
                            let (mut x, mut y) = (1u16, 1u16);
                            if let Some(r) = rect {
                                if m.column > r.x && m.row > r.y {
                                    x = (m.column - r.x).min(r.width.saturating_sub(2).max(1));
                                    y = (m.row - r.y).min(r.height.saturating_sub(2).max(1));
                                }
                            }
                            let bytes = encode_wheel(&screen, up, x, y);
                            t.send_bytes(&bytes);
                        } else if screen.alternate_screen() {
                            // Full-screen app without mouse reporting: translate
                            // each notch into arrow keys, honoring DECCKM.
                            let seq: &[u8] = match (up, screen.application_cursor()) {
                                (true, false) => b"\x1b[A",
                                (true, true) => b"\x1bOA",
                                (false, false) => b"\x1b[B",
                                (false, true) => b"\x1bOB",
                            };
                            for _ in 0..n {
                                t.send_bytes(seq);
                            }
                        } else {
                            t.scroll_view(delta);
                        }
                    }
                    PaneContent::Editor(buf_id) if self.mode == Mode::Edit => {
                        if self.focused_pane().md_view {
                            // Reading-mode: the wheel scrolls the rendered document.
                            let vh = self.focused_pane().view_h.max(1);
                            let cap = self.focused_pane().md_rendered_total.get().saturating_sub(vh);
                            let p = self.focused_pane_mut();
                            p.md_scroll = if up { p.md_scroll.saturating_sub(n as usize) }
                                          else { (p.md_scroll + n as usize).min(cap) };
                        } else {
                            // Normal editor: scroll the viewport (not the cursor), then
                            // pull the cursor back into view so it stays valid.
                            let lc = self.buffers[&buf_id].line_count();
                            let vh = self.focused_pane().view_h.max(1);
                            {
                                let p = self.focused_pane_mut();
                                p.scroll_row = if up { p.scroll_row.saturating_sub(n as usize) }
                                               else { (p.scroll_row + n as usize).min(lc.saturating_sub(1)) };
                                if p.cursor_row < p.scroll_row { p.cursor_row = p.scroll_row; }
                                let bottom = (p.scroll_row + vh.saturating_sub(1)).min(lc.saturating_sub(1));
                                if p.cursor_row > bottom { p.cursor_row = bottom; }
                            }
                            let cr = self.focused_pane().cursor_row;
                            let ll = self.buffers[&buf_id].line_len(cr);
                            let p = self.focused_pane_mut();
                            if p.cursor_col > ll { p.cursor_col = ll; p.col_affinity = ll; }
                        }
                    }
                    _ => {}
                }
            }
            // Extend an in-progress terminal selection.
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(sel) = self.term_sel.as_mut() {
                    sel.end = (
                        m.row.saturating_sub(sel.oy).min(sel.vh.saturating_sub(1)),
                        m.column.saturating_sub(sel.ox).min(sel.vw.saturating_sub(1)),
                    );
                }
                // Editor drag: move the cursor; the anchor set on press makes
                // that a selection, exactly as Shift+arrows would.
                if let Some((pane_id, from_row, from_col)) = self.editor_drag {
                    let rect = self.pane_rects.iter().find(|(id, _)| *id == pane_id).map(|(_, r)| *r);
                    let buf_id = match self.panes.get(&pane_id).map(|p| p.content.clone()) {
                        Some(PaneContent::Editor(id)) => Some(id),
                        _ => None,
                    };
                    if let (Some(rect), Some(buf_id)) = (rect, buf_id) {
                        let inner_x = rect.x + 1 + crate::ui::gutter_width(&self.tuning);
                        let inner_y = rect.y + 1;
                        let scroll = self.panes[&pane_id].scroll_row;
                        // Clamp to the pane: dragging past an edge extends to it
                        // rather than dropping the event.
                        let vrow = m.row.max(inner_y).min(rect.y + rect.height.saturating_sub(2));
                        let row = (scroll + (vrow - inner_y) as usize)
                            .min(self.buffers[&buf_id].line_count().saturating_sub(1));
                        let col = (m.column.saturating_sub(inner_x) as usize)
                            .min(self.buffers[&buf_id].line_len(row));
                        let p = self.panes.get_mut(&pane_id).unwrap();
                        // First motion of this drag anchors it at the press point.
                        if p.selection_anchor.is_none() {
                            p.selection_anchor = Some((from_row, from_col));
                        }
                        p.cursor_row = row;
                        p.cursor_col = col;
                        p.col_affinity = col;
                    }
                }
            }
            // Release: copy the selected terminal text to the clipboard — but
            // only for a real drag. A plain click (anchor == end) is focus, not
            // a selection; copying a 1-char "selection" would silently clobber
            // the clipboard on every click.
            MouseEventKind::Up(MouseButton::Left) => {
                // An editor drag ends without copying: unlike a terminal, the text
                // is already reachable, and auto-copy is what made a plain click
                // clobber the clipboard (P1.4).
                self.editor_drag = None;
                if let Some(sel) = self.term_sel.take() {
                    let text = if sel.anchor == sel.end {
                        String::new()
                    } else {
                        self.term_selection_text(&sel)
                    };
                    if !text.is_empty() {
                        self.clipboard_export(&text);
                        self.kill_ring.push(text.clone());
                        self.status_msg = Some(format!("Copied {} chars", text.chars().count()));
                        self.needs_redraw = true;
                    }
                }
            }
            _ => {}
        }
    }

    /// How many consecutive clicks this press makes (1, 2, 3, …). A terminal
    /// sends two clicks as two independent presses with no count, so "double"
    /// means "same cell, within `multi_click_ms`."
    fn click_count(&mut self, col: u16, row: u16) -> u8 {
        let now = std::time::Instant::now();
        let window = std::time::Duration::from_millis(self.tuning.multi_click_ms);
        let n = match self.last_click {
            Some((t, c, r, n)) if c == col && r == row && now.duration_since(t) <= window => {
                n.saturating_add(1)
            }
            _ => 1,
        };
        self.last_click = Some((now, col, row, n));
        n
    }

    /// Select the word under (row, col) — the double-click verb. Word here is the
    /// identifier sense (alphanumeric + `_`), matching what `move_token_*` treats
    /// as one hop, so double-click and ⌘←/→ agree about where words end.
    fn select_word_at(&mut self, pane_id: PaneId, buf_id: BufferId, row: usize, col: usize) {
        let line: Vec<char> = self.buffers[&buf_id].line_str(row).chars().collect();
        if line.is_empty() {
            return;
        }
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        // A click past the last character selects the last word, not nothing.
        let at = col.min(line.len().saturating_sub(1));
        if !is_word(line[at]) {
            return;
        }
        let mut s = at;
        while s > 0 && is_word(line[s - 1]) {
            s -= 1;
        }
        let mut e = at;
        while e + 1 < line.len() && is_word(line[e + 1]) {
            e += 1;
        }
        let p = self.panes.get_mut(&pane_id).unwrap();
        p.selection_anchor = Some((row, s));
        p.cursor_col = e + 1;
        p.col_affinity = e + 1;
    }

    /// Select the whole line — the triple-click verb.
    fn select_line_at(&mut self, pane_id: PaneId, buf_id: BufferId, row: usize) {
        let len = self.buffers[&buf_id].line_len(row);
        let p = self.panes.get_mut(&pane_id).unwrap();
        p.selection_anchor = Some((row, 0));
        p.cursor_col = len;
        p.col_affinity = len;
    }

    /// Extract the text under a terminal drag-selection.
    fn term_selection_text(&self, sel: &TermSel) -> String {
        let Some(t) = self.terms.get(&sel.tid) else { return String::new() };
        let (mut a, mut b) = (sel.anchor, sel.end);
        if b < a {
            std::mem::swap(&mut a, &mut b);
        }
        selection_text_from_screen(&t.screen(), a, b, sel.vw.saturating_sub(1))
    }

    // ── Persisted state (frecency + nudge counters) ──────────────────────────

    fn save_state(&self) {
        let state = PersistedState {
            frecency: self.frecency.clone(),
            bar_uses: self.bar_uses.clone(),
            file_frecency: self.file_frecency.clone(),
        };
        if let Some(path) = config::state_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(&state) {
                let _ = std::fs::write(path, json);
            }
        }
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
struct PersistedState {
    #[serde(default)]
    frecency: HashMap<String, u32>,
    #[serde(default)]
    bar_uses: HashMap<String, u32>,
    #[serde(default)]
    file_frecency: HashMap<String, u32>,
}

impl PersistedState {
    fn load() -> Self {
        config::state_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
}

/// Extract the first fenced ``` code block's body (drops an optional language
/// tag on the opening fence). None if there's no complete block.
pub fn extract_code_block(text: &str) -> Option<String> {
    let start = text.find("```")?;
    let after = &text[start + 3..];
    // Skip the rest of the opening-fence line (e.g. ```rust).
    let body_start = after.find('\n').map(|i| i + 1).unwrap_or(after.len());
    let body = &after[body_start..];
    let end = body.find("```")?;
    Some(body[..end].trim_end_matches('\n').to_string())
}

/// Encode a wheel press for an inner app that enabled mouse reporting, in the
/// app's own negotiated encoding. Coordinates are 1-based, pane-relative.
fn encode_wheel(screen: &vt100::Screen, up: bool, x: u16, y: u16) -> Vec<u8> {
    let button: u32 = if up { 64 } else { 65 };
    match screen.mouse_protocol_encoding() {
        vt100::MouseProtocolEncoding::Sgr => format!("\x1b[<{button};{x};{y}M").into_bytes(),
        vt100::MouseProtocolEncoding::Utf8 => {
            let mut out = vec![0x1b, b'[', b'M'];
            for v in [32 + button, 32 + x as u32, 32 + y as u32] {
                let mut buf = [0u8; 4];
                out.extend_from_slice(
                    char::from_u32(v).unwrap_or(' ').encode_utf8(&mut buf).as_bytes(),
                );
            }
            out
        }
        // X10 bytes overflow past coordinate 223 (32 + 223 = 255); clamp.
        vt100::MouseProtocolEncoding::Default => {
            let b = |v: u32| (32 + v.min(223)) as u8;
            vec![0x1b, b'[', b'M', b(button), b(x as u32), b(y as u32)]
        }
    }
}

/// Translate a key event into the byte sequence a PTY expects.
fn key_to_bytes(key: &KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char(c) => {
            if ctrl && c.is_ascii_alphabetic() {
                // Ctrl-A..Ctrl-Z → 0x01..0x1a
                vec![(c.to_ascii_lowercase() as u8 - b'a') + 1]
            } else {
                let mut b = [0u8; 4];
                c.encode_utf8(&mut b).as_bytes().to_vec()
            }
        }
        KeyCode::Enter     => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab       => vec![b'\t'],
        KeyCode::BackTab   => vec![0x1b, b'[', b'Z'],
        KeyCode::Esc       => vec![0x1b],
        KeyCode::Left      => vec![0x1b, b'[', b'D'],
        KeyCode::Right     => vec![0x1b, b'[', b'C'],
        KeyCode::Up        => vec![0x1b, b'[', b'A'],
        KeyCode::Down      => vec![0x1b, b'[', b'B'],
        KeyCode::Home      => vec![0x1b, b'[', b'H'],
        KeyCode::End       => vec![0x1b, b'[', b'F'],
        KeyCode::PageUp    => vec![0x1b, b'[', b'5', b'~'],
        KeyCode::PageDown  => vec![0x1b, b'[', b'6', b'~'],
        KeyCode::Delete    => vec![0x1b, b'[', b'3', b'~'],
        _ => vec![],
    }
}
