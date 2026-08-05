/// Behavioral tuning knobs — loaded from ~/.config/mars/tuning.json.
///
/// Every knob is stored as `{ "value": ..., "description": "..." }` so that a
/// human or an agent editing the file can see what each number does. Defaults
/// are written on first run; user values are layered over defaults, so new
/// knobs appear in old files and unknown keys are ignored.

use std::collections::HashMap;

use ratatui::style::Color;
use serde::{Deserialize, Serialize};

/// The resolved color palette — every colored cell in the UI reads exactly one of
/// these semantic tokens (never a raw `Color::` or a `theme_*` knob). A theme swaps
/// the whole set; the default below is "Mission Control", byte-identical to the
/// shipped look. Neutrals default to ANSI-named colors so they honor the terminal
/// palette exactly as before.
#[derive(Clone, Copy, Hash)]
pub struct Palette {
    pub accent: Color,        // primary brand (focus, active chrome)
    pub accent_bright: Color, // bright brand (teaching, emphasis)
    pub accent_dark: Color,   // deep brand (secondary/receded emphasis)
    pub on_accent: Color,     // legible fg on a saturated fill
    pub info: Color,          // the teal "mission-control" hue; also the DONE status
    pub success: Color,       // RUNNING/healthy status; the go/confirm affordance
    pub warning: Color,       // BLOCKED / waiting-on-you status
    pub danger: Color,        // FAILED status — the loudest hue
    pub text: Color,          // primary foreground
    pub text_dim: Color,      // secondary foreground; the IDLE status
    pub text_faint: Color,    // tertiary: hints, rules, "N more", stars
    pub border: Color,        // idle / unfocused pane + panel borders
    pub surface: Color,       // panel/pane background; "no fill"
    pub select_row_bg: Color, // highlighted row fill in the command dropdown
    pub selection_bg: Color,  // active selection highlight bg
    pub search_bg: Color,     // isearch match bg
    pub current_line: Color,  // the cursor-line tint
}

impl Palette {
    /// The compiled default — the MARS house look. Every other theme starts from
    /// this and overrides tokens, so an under-specified theme still renders.
    pub fn mission_control() -> Self {
        let rgb = |r, g, b| Color::Rgb(r, g, b);
        Palette {
            accent: rgb(217, 119, 87),
            accent_bright: rgb(233, 161, 120),
            accent_dark: rgb(183, 65, 14),
            on_accent: rgb(31, 20, 16),
            info: rgb(13, 115, 119),
            success: rgb(61, 174, 114),
            warning: rgb(230, 165, 60),
            danger: rgb(217, 83, 79),
            text: Color::White,
            text_dim: Color::Gray,
            text_faint: Color::DarkGray,
            border: Color::DarkGray,
            surface: Color::Reset,
            select_row_bg: Color::DarkGray,
            selection_bg: rgb(74, 42, 31),
            search_bg: rgb(138, 84, 20),
            current_line: rgb(42, 37, 34),
        }
    }
}

#[derive(Clone)]
pub struct Tuning {
    pub poll_interval_ms: u64,
    pub which_key_delay_ms: u64,
    pub nudge_threshold: u32,
    pub max_panes: usize,
    pub scroll_margin: usize,
    pub page_overlap: usize,
    pub wheel_scroll_lines: usize,
    pub multi_click_ms: u64,
    pub dropdown_max_rows: u16,
    pub panel_max_height_pct: u16,
    pub ask_panel_max_pct: u16,
    pub spinner_speed_ticks: u64,
    pub which_key_panel_width: u16,
    pub travel_panel_width: u16,
    pub binding_badge_width: usize,
    pub selection_bg: [u8; 3],
    pub search_match_bg: [u8; 3],
    // ── Theme (Mars palette: Claude-Code clay + rust) ──
    pub theme_accent: [u8; 3],
    pub theme_accent_bright: [u8; 3],
    pub theme_accent_dark: [u8; 3],
    pub theme_chip_fg: [u8; 3],
    pub theme_terminal: [u8; 3],
    pub theme_healthy: [u8; 3],
    pub theme_status_failed: [u8; 3],
    pub theme_status_blocked: [u8; 3],
    /// Show the line-number gutter in editor panes (position always lives in
    /// the status bar).
    pub line_numbers: bool,
    /// Syntax-highlight code in editor panes (0 = off, 1 = on). Colors come from the
    /// active theme; unknown languages render plain. No-op without the `syntax` build.
    pub syntax_highlight: u64,
    /// Milliseconds of edit-quiet before code is re-highlighted. Colors track edits
    /// live via cache splicing; this only governs how soon a full reparse catches up
    /// after you pause typing.
    pub syntax_recolor_ms: u64,
    /// Show the host-health line at the top of the SPACES panel (0 = off, 1 = on):
    /// session uptime, load, memory %, free disk, and GPU % where available.
    pub health_line: u64,
    /// How often (seconds) the health probes refresh. Memory is smoothed across a few
    /// minutes regardless; this only sets the sampling cadence.
    pub health_sample_secs: u64,
    /// Tint the cursor's line with a subtle background (0 = off, 1 = on).
    pub highlight_current_line: u64,
    /// The current-line tint color (used when highlight_current_line = 1).
    pub current_line_bg: [u8; 3],
    /// Max reflow width for the Markdown reading-mode, in columns (0 = full width).
    pub reading_width: u64,
    /// Paint the theme's `surface` as a solid background (1) or stay transparent and
    /// honor the terminal's own background (0). A theme with a `Reset` surface (the
    /// default) is transparent either way; a colored surface (Paper, Hacker) is
    /// solid when this is on.
    pub opaque_background: u64,
    /// Seconds before the agent may auto-name a default-named tab (0 = off).
    pub auto_name_secs: u64,
    pub agent_max_tokens: u32,
    pub agent_temperature: f64,
    pub terminal_default_rows: u16,
    pub terminal_default_cols: u16,
    pub terminal_scrollback_lines: usize,
    pub terminal_line_log_bytes: usize,
    pub terminal_startup_probe_ms: u64,
    pub autosave_secs: u64,
    pub project_index_max: usize,
    pub project_ignore: Vec<String>,
    pub tree_width: u16,
    pub tree_show_dotfiles: u64,
    pub watch_quiet_secs: u64,
    pub stall_secs: u64,
    pub agent_scrollback_context: usize,
    pub memory_cwd_boost: f64,
    pub memory_recency_boost: f64,
    pub memory_recency_halflife_days: f64,
    pub mission_refresh_secs: u64,
    pub worklog_max_lines: u64,
    pub mission_briefing: u64,
    pub mission_briefing_animate: u64,
    pub mission_briefing_type_ms: u64,
    pub mission_briefing_prose_rows: u64,
    pub auto_watch: u64,
    pub watch_min_active_secs: u64,
    pub goal_tracking: u64,
    /// How often (milliseconds) the session daemon pushes the structured board +
    /// briefing to a subscribed mobile client (the Rover phone bridge). The main
    /// loop ticks far faster; this throttles the LAN push to a sane cadence.
    pub mobile_push_interval_ms: u64,
    /// How often the WATCHED PANE's screen is pushed to the phone, separately from the board.
    /// The board is a status summary and a second is fine; the pane is what you type into, and
    /// at the board's cadence a keystroke can take a full second to appear. Identical screens
    /// are never sent, so a tight interval costs nothing while the pane is idle.
    pub mobile_pane_interval_ms: u64,
    pub manager_snapshot_secs: u64,
    pub manager_snapshot_keep: u64,
    pub manager_detail_min_secs: u64,
    /// Seconds between manager-agent turns. This is the ONLY model-touching cadence in the
    /// manager; everything below it is arithmetic. A newly-blocked workspace bypasses it, since
    /// that is the state where the engineer is the bottleneck. 0 = the agent never runs.
    pub manager_agent_secs: u64,
    /// The command typed into the manager's hidden pane to start the agent.
    pub manager_agent_command: String,
    /// Seconds after which agent-written prose is considered stale and the phone stops
    /// preferring it. Guards against a dead agent silently serving an old world forever.
    pub manager_agent_stale_secs: u64,
    /// Minimum seconds between Rover MAP calls (the per-workspace summary). One
    /// changed pane is (re)summarized per interval, so a busy board fans out over
    /// several intervals instead of hammering the model. Only active while a phone
    /// is subscribed and a key is configured.
    pub rover_map_min_secs: u64,
    /// Minimum seconds between samples of each board pane's FOREGROUND COMMAND (what the
    /// phone's board labels an agent pane with). Every sample spawns `ps`, so this is a
    /// process-per-pane-per-interval cost and must not be read per board push — the push
    /// cadence is far faster than anything a running command changes at.
    pub foreground_sample_secs: u64,
    /// Minimum seconds between Rover REDUCE calls (the mission briefing composed
    /// from the summaries). It only re-runs when a summary/verdict actually
    /// changed; this bounds how often a burst of map updates re-briefs.
    pub rover_brief_min_secs: u64,
    /// The resolved color palette (theme + per-token knob overrides). Read by the
    /// render layer; the `theme_*`/`selection_bg`/… knobs above are its override
    /// surface, not read directly by `ui.rs`.
    pub palette: Palette,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            poll_interval_ms: 16,
            which_key_delay_ms: 200,
            nudge_threshold: 3,
            max_panes: 4,
            scroll_margin: 3,
            page_overlap: 2,
            wheel_scroll_lines: 3,
            multi_click_ms: 400,
            dropdown_max_rows: 20,
            panel_max_height_pct: 60,
            ask_panel_max_pct: 30,
            spinner_speed_ticks: 3,
            which_key_panel_width: 30,
            travel_panel_width: 46,
            binding_badge_width: 9,
            selection_bg: [74, 42, 31],      // deep rust-brown
            search_match_bg: [138, 84, 20],  // amber
            theme_accent: [217, 119, 87],        // #D97757 terracotta/clay
            theme_accent_bright: [233, 161, 120], // #E9A178 light sand
            theme_accent_dark: [183, 65, 14],     // #B7410E rust
            theme_chip_fg: [31, 20, 16],          // dark text on accent chips
            theme_terminal: [13, 115, 119],       // #0D7377 dark teal
            theme_healthy: [61, 174, 114],        // #3DAE72 healthy green (working/running)
            theme_status_failed: [217, 83, 79],   // #D9534F conventional error red (failed)
            theme_status_blocked: [230, 165, 60], // #E6A53C conventional amber (blocked / waiting on you)
            line_numbers: false,
            syntax_highlight: 1,
            syntax_recolor_ms: 150,
            health_line: 1,
            health_sample_secs: 2,
            highlight_current_line: 1,
            current_line_bg: [42, 37, 34],   // subtle warm tint on the cursor's row
            reading_width: 90,
            opaque_background: 1,
            auto_name_secs: 45,
            agent_max_tokens: 1024, // headroom for reasoning models (Qwen3/R1)
            agent_temperature: 0.3,
            terminal_default_rows: 24,
            terminal_default_cols: 80,
            terminal_scrollback_lines: 10_000,
            terminal_line_log_bytes: 8 * 1024 * 1024,
            terminal_startup_probe_ms: 5000,
            autosave_secs: 30,
            project_index_max: 20_000,
            project_ignore: ["target", "node_modules", ".git", "dist", "build", ".venv"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            tree_width: 30,
            tree_show_dotfiles: 1,
            watch_quiet_secs: 20,
            stall_secs: 300,
            agent_scrollback_context: 200,
            memory_cwd_boost: 0.25,
            memory_recency_boost: 0.15,
            memory_recency_halflife_days: 14.0,
            mission_refresh_secs: 600,
            worklog_max_lines: 4000,
            mission_briefing: 2,
            mission_briefing_animate: 1,
            mission_briefing_type_ms: 13,
            mission_briefing_prose_rows: 13,
            auto_watch: 1,
            watch_min_active_secs: 10,
            goal_tracking: 1,
            mobile_push_interval_ms: 1000,
            mobile_pane_interval_ms: 40,
            manager_snapshot_secs: 60,
            manager_snapshot_keep: 20,
            manager_detail_min_secs: 300,
            manager_agent_secs: 1200,
            manager_agent_command:
                "env -u ANTHROPIC_API_KEY -u ANTHROPIC_AUTH_TOKEN claude \
                  --model claude-sonnet-5 --effort medium \
                  --add-dir ~/.mars/sessions --permission-mode acceptEdits"
                    .into(),
            manager_agent_stale_secs: 2700,
            rover_map_min_secs: 1,
            foreground_sample_secs: 4,
            rover_brief_min_secs: 12,
            palette: Palette::mission_control(),
        }
    }
}

impl Tuning {
    /// which-key delay expressed in frame ticks of the main loop.
    pub fn which_key_delay_ticks(&self) -> u64 {
        (self.which_key_delay_ms / self.poll_interval_ms.max(1)).max(1)
    }
}

// ── File format ───────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct Knob {
    value: serde_json::Value,
    description: String,
}

fn knob(value: serde_json::Value, description: &str) -> Knob {
    Knob { value, description: description.to_string() }
}

/// The tunable knobs as self-contained, actionable retrieval lines — each names
/// the knob, WHERE to set it, and the default — so the agent answers
/// self-reconfiguration questions with the exact `knob = value in tuning.json`
/// rather than hallucinating the file or the knob name.
#[cfg_attr(not(feature = "memory"), allow(dead_code))] // sole consumer is the docs corpus
pub fn knob_descriptions() -> Vec<String> {
    default_knobs()
        .into_iter()
        .map(|(name, k)| {
            format!(
                "To change {}: set `{name}` in ~/.config/mars/tuning.json (default {}).",
                k.description, k.value
            )
        })
        .collect()
}

/// The default knob map, with the semantic explanations that make the file
/// safely editable by a human or an agent.
fn default_knobs() -> Vec<(&'static str, Knob)> {
    use serde_json::json;
    let d = Tuning::default();
    vec![
        ("poll_interval_ms", knob(json!(d.poll_interval_ms),
            "Main loop tick in milliseconds. Lower = snappier UI + spinner, higher CPU.")),
        ("which_key_delay_ms", knob(json!(d.which_key_delay_ms),
            "Hesitation on a prefix (C-x…) before the which-key panel pops. \
             Lower teaches eagerly; higher keeps it invisible to fast typists.")),
        ("nudge_threshold", knob(json!(d.nudge_threshold),
            "How many times an action is run from the command bar before the \
             '💡 next time: <key>' graduation hint appears in the status line.")),
        ("max_panes", knob(json!(d.max_panes),
            "Maximum panes per tab. Splits beyond this are refused.")),
        ("scroll_margin", knob(json!(d.scroll_margin),
            "Lines kept visible above/below the cursor when scrolling.")),
        ("page_overlap", knob(json!(d.page_overlap),
            "Lines of overlap kept on PageUp/PageDown so context isn't lost.")),
        ("wheel_scroll_lines", knob(json!(d.wheel_scroll_lines),
            "Lines moved per mouse-wheel step.")),
        ("multi_click_ms", knob(json!(d.multi_click_ms),
            "How long after a click a second one still counts as a double-click \
             (milliseconds). Terminals report no click count — two clicks arrive as \
             two independent presses — so Mars times them itself: double selects the \
             word under the pointer, triple selects the line. Raise it if your \
             double-clicks are being read as two single clicks.")),
        ("dropdown_max_rows", knob(json!(d.dropdown_max_rows),
            "Maximum visible rows in the command-bar dropdown.")),
        ("panel_max_height_pct", knob(json!(d.panel_max_height_pct),
            "Maximum height of pop-up panels (dropdown) as % of the editor area.")),
        ("ask_panel_max_pct", knob(json!(d.ask_panel_max_pct),
            "Maximum height of the ask/chat panel as % of the workspace — it hugs the bottom; scroll up (Up key or wheel) for older turns.")),
        ("spinner_speed_ticks", knob(json!(d.spinner_speed_ticks),
            "Frame ticks per spinner animation step while the agent thinks. Lower = faster spin.")),
        ("which_key_panel_width", knob(json!(d.which_key_panel_width),
            "Width (columns) of the which-key continuation panel.")),
        ("travel_panel_width", knob(json!(d.travel_panel_width),
            "Width (columns) of the C-t travel-mode cheat panel.")),
        ("binding_badge_width", knob(json!(d.binding_badge_width),
            "Column width reserved for keybinding badges in the dropdown.")),
        ("selection_bg", knob(json!(d.selection_bg),
            "RGB background of the active selection highlight.")),
        ("search_match_bg", knob(json!(d.search_match_bg),
            "RGB background of isearch match highlights.")),
        ("theme_accent", knob(json!(d.theme_accent),
            "RGB brand accent (Mars terracotta): focused borders, active tab, \
             command bar, selected rows, EDIT chip.")),
        ("theme_accent_bright", knob(json!(d.theme_accent_bright),
            "RGB bright accent (sand): which-key keys, keybinding badges, \
             teaching surfaces.")),
        ("theme_accent_dark", knob(json!(d.theme_accent_dark),
            "RGB dark accent (rust): splash gradient, secondary emphasis.")),
        ("theme_chip_fg", knob(json!(d.theme_chip_fg),
            "RGB text color on accent-colored chips/badges.")),
        ("theme_terminal", knob(json!(d.theme_terminal),
            "RGB for live terminal panes: focused border + TERM mode chip.")),
        ("theme_healthy", knob(json!(d.theme_healthy),
            "RGB for a HEALTHY / running surface in the workspace monitor and \
             briefing — a calm green you can dismiss at a glance, distinct from \
             teal (done/win) and the warm danger hues.")),
        ("theme_status_failed", knob(json!(d.theme_status_failed),
            "RGB for a FAILED surface (✗) — conventional error red, the loudest \
             status hue.")),
        ("theme_status_blocked", knob(json!(d.theme_status_blocked),
            "RGB for a BLOCKED surface (⏸) waiting on your input — conventional \
             amber, distinct from the red of an outright failure.")),
        ("line_numbers", knob(json!(d.line_numbers),
            "Show the line-number gutter in editor panes. The cursor position \
             is always in the status bar, so this defaults to off for width.")),
        ("syntax_highlight", knob(json!(d.syntax_highlight),
            "Start with syntax highlighting on (0/1). On by default; toggle per session \
             from the command bar. Highlighting runs on a background thread and is cached, so it \
             never blocks: a file paints plain immediately, then colorizes line-by-line \
             as the worker delivers. Colors follow the active theme; unknown languages \
             render plain. Needs the `syntax` build feature.")),
        ("syntax_recolor_ms", knob(json!(d.syntax_recolor_ms),
            "Milliseconds of edit-quiet before a full re-highlight runs. Lower = colors \
             catch up sooner after you pause; higher = fewer background reparses while \
             typing fast. Colors already track edits live via cache splicing, so this \
             is just the fallback reparse cadence.")),
        ("health_line", knob(json!(d.health_line),
            "Show the host-health line atop the SPACES panel (0/1): session uptime, 1-min \
             load, host memory %, free disk on the working dir, and — on machines with \
             `nvidia-smi` — GPU memory %. Any probe the OS can't answer is dropped, so the \
             line self-trims. Sampled only while the panel is open (GPU polled off-thread).")),
        ("health_sample_secs", knob(json!(d.health_sample_secs),
            "How often (seconds) the health probes refresh. Memory is smoothed over a few \
             minutes regardless; this is just the sampling cadence.")),
        ("highlight_current_line", knob(json!(d.highlight_current_line),
            "Tint the cursor's line with a subtle background (0 = off, 1 = on) \
             for focus. The color is `current_line_bg`.")),
        ("current_line_bg", knob(json!(d.current_line_bg),
            "Background tint for the cursor's line (RGB). Keep it subtle — a few \
             shades above the terminal background.")),
        ("reading_width", knob(json!(d.reading_width),
            "Max reflow width for the Markdown reading-mode, in columns; the \
             document is centered within the pane. 0 = use the full pane width.")),
        ("opaque_background", knob(json!(d.opaque_background),
            "Paint the theme's background as solid (1) or stay transparent and let \
             the terminal's own background show through (0). The default theme is \
             transparent regardless; colored themes (Paper, Hacker) turn solid.")),
        ("auto_name_secs", knob(json!(d.auto_name_secs),
            "With an agent configured, tabs still wearing their default numeric \
             name get an auto-generated label after this many seconds (0 = off). \
             Renaming a tab yourself always wins and opts it out.")),
        ("agent_max_tokens", knob(json!(d.agent_max_tokens),
            "Max tokens the ask-agent may generate per answer.")),
        ("agent_temperature", knob(json!(d.agent_temperature),
            "Sampling temperature for the ask-agent. Lower = more deterministic.")),
        ("terminal_default_rows", knob(json!(d.terminal_default_rows),
            "Initial PTY rows before the first render sizes the terminal pane.")),
        ("terminal_default_cols", knob(json!(d.terminal_default_cols),
            "Initial PTY columns before the first render sizes the terminal pane.")),
        ("terminal_scrollback_lines", knob(json!(d.terminal_scrollback_lines),
            "Scrollback history kept per terminal pane. Scroll with the wheel or \
             Shift+PageUp/PageDown; any keystroke snaps back to live.")),
        ("terminal_startup_probe_ms", knob(json!(d.terminal_startup_probe_ms),
            "Delay and retry interval for the readiness probe used when a fresh \
             shell has an empty or unrecognized prompt. Commands and typing stay \
             buffered until the shell accepts the probe.")),
        ("autosave_secs", knob(json!(d.autosave_secs),
            "Seconds between silent autosaves of modified buffers that have a file \
             path (also fires on session detach/disconnect). 0 disables.")),
        ("project_index_max", knob(json!(d.project_index_max),
            "Max files the `@` picker indexes from the project (bounds memory on huge \
             trees).")),
        ("project_ignore", knob(json!(d.project_ignore),
            "Directory names the file index/tree skip (plus all dotdirs). Does not yet \
             read a repo's .gitignore.")),
        ("tree_width", knob(json!(d.tree_width),
            "Column width of the left file-tree sidebar (@ / C-x d).")),
        ("tree_show_dotfiles", knob(json!(d.tree_show_dotfiles),
            "Show dotfiles in the navigator by default (0/1). `.` toggles at runtime; \
             the project_ignore list (.git, .venv, …) stays hidden regardless.")),
        ("watch_quiet_secs", knob(json!(d.watch_quiet_secs),
            "Seconds a watched terminal (C-t w) must be silent before Mars summarizes it \
             (W6). Also fires immediately on process exit. Generous by design — a false \
             'done' costs more than the feature earns.")),
        ("stall_secs", knob(json!(d.stall_secs),
            "Seconds a pane may have a command executing and print NOTHING before it reads \
             `stalled` instead of `running`. Much longer than watch_quiet_secs on purpose: \
             quiet is normal (a compile, a download), wedged is not, and only time tells \
             them apart. 0 disables the verdict entirely.")),
        ("agent_scrollback_context", knob(json!(d.agent_scrollback_context),
            "Lines of a watched/focused terminal's screen sent to the agent for a summary \
             or triage.")),
        ("memory_cwd_boost", knob(json!(d.memory_cwd_boost),
            "How much a remembered command from the CURRENT working directory outranks a \
             lexical tie from elsewhere (0 = off). Same-project memories answer \
             project-specific requests.")),
        ("memory_recency_boost", knob(json!(d.memory_recency_boost),
            "How much a RECENT remembered command outranks a lexical tie from long ago \
             (0 = off); decays with memory_recency_halflife_days.")),
        ("memory_recency_halflife_days", knob(json!(d.memory_recency_halflife_days),
            "Days for the recency boost to halve. Smaller = the agent prefers this \
             week's habits; larger = long memory.")),
        ("mission_refresh_secs", knob(json!(d.mission_refresh_secs),
            "How often (at most) the agent re-infers your one-line mission from the \
             work journal of watch verdicts; shown by `mars ls`. 0 disables.")),
        ("worklog_max_lines", knob(json!(d.worklog_max_lines),
            "Work-journal size bound (~/.mars/worklog.jsonl): past twice this many \
             lines it is compacted to the newest this-many at startup. 0 = never.")),
        ("mission_briefing", knob(json!(d.mission_briefing),
            "What reattach shows: 2 = the full-screen Mission Briefing (a centered, \
             plain-English summary of what happened while you were away — any key \
             resumes), 1 = a one-line notice, 0 = nothing.")),
        ("mission_briefing_animate", knob(json!(d.mission_briefing_animate),
            "1 = the Mission Briefing boots up on reattach (elements reveal in ~0.4s, \
             failures first); 0 = it appears instantly (thin SSH links, reduced motion).")),
        ("mission_briefing_type_ms", knob(json!(d.mission_briefing_type_ms),
            "Milliseconds per character as the briefing prose types itself in behind a \
             cursor (animate=1 only). Higher is slower; e.g. 13 ≈ 75 chars/sec.")),
        ("mission_briefing_prose_rows", knob(json!(d.mission_briefing_prose_rows),
            "Rows reserved for the briefing prose so the layout is a fixed vessel the \
             text fills top-down — the manifest below never shifts as prose streams. \
             Raise it if long briefings push the systems board down.")),
        ("auto_watch", knob(json!(d.auto_watch),
            "1 = panes that stay busy past watch_min_active_secs are watched \
             automatically (verdicts without arming a watch); 0 = only C-x w \
             watches.")),
        ("watch_min_active_secs", knob(json!(d.watch_min_active_secs),
            "Seconds of continuous output before auto-watch arms a pane — \
             filters one-shot commands so only real runs earn verdicts.")),
        ("goal_tracking", knob(json!(d.goal_tracking),
            "1 = when you detach, the agent captures what you were working \
             toward, so the reattach briefing can report progress on it; \
             0 = off.")),
        ("terminal_line_log_bytes", knob(json!(d.terminal_line_log_bytes),
            "Bytes of transcript each terminal pane keeps for the phone: every line that has \
             scrolled off, numbered, as formatted ANSI. This is what the phone pages through, \
             so it sets how far back scrollback reaches. Oldest lines are dropped past it.")),
        ("manager_snapshot_secs", knob(json!(d.manager_snapshot_secs),
            "How often (seconds) the daemon CHECKS whether anything changed. It writes nothing \
             unless a workspace appeared, vanished, changed state, or changed what it says, so a \
             quiet session costs one file read per tick and nothing else. 0 = off.")),
        ("manager_snapshot_keep", knob(json!(d.manager_snapshot_keep),
            "How many stimulus files to keep per session under \
             ~/.mars/manager/sessions/<name>/snapshots/. Older ones are pruned each tick so the \
             directory cannot grow without limit.")),
        ("manager_detail_min_secs", knob(json!(d.manager_detail_min_secs),
            "Minimum seconds between rewrites caused only by a workspace's DESCRIPTION changing. \
             A pane running an agent changes its description every time it prints, and rewriting \
             the tree for that would churn the phone continuously. A state change (failed, \
             blocked, a pane appearing) still lands immediately, whatever this is set to.")),
        ("manager_agent_secs", knob(json!(d.manager_agent_secs),
            "Seconds between manager-AGENT turns — the only cadence in the manager that spends \
             tokens. Everything faster than this is arithmetic and free. A newly-blocked \
             workspace wakes the agent immediately regardless. Delete \
             ~/.mars/manager/memory/last_run to force a run on the next tick. 0 = agent off.")),
        ("manager_agent_command", knob(json!(d.manager_agent_command),
            "The command run in the manager's hidden pane for each turn; the prompt is appended \
             as `-p '<line>'`. ANTHROPIC_API_KEY is scrubbed deliberately — when it is set it \
             takes precedence over the claude.ai login, so the agent would bill credits instead \
             of using the subscription (and fail outright when that key is empty). \
             --add-dir is required, not optional: session artifacts live in ~/.mars/sessions \
             while the repo is ~/.mars/manager, so without it the agent can read neither the \
             snapshots nor the directories it must write. acceptEdits lets it write files \
             without a human at the keyboard, which is the whole point, while still gating \
             everything that is not an edit. Sonnet at medium effort: the work is reading \
             prepared files and writing short prose.")),
        ("manager_agent_stale_secs", knob(json!(d.manager_agent_stale_secs),
            "How old agent-written prose may be before the phone stops trusting it and falls \
             back. Should comfortably exceed manager_agent_secs, or a healthy agent reads as \
             stale between its own turns.")),
        ("mobile_pane_interval_ms", knob(json!(d.mobile_pane_interval_ms),
            "How often the watched pane's live screen is pushed to the phone, in ms. Separate \
             from the board's cadence because this one is felt as typing latency. Identical \
             screens are suppressed, so a tight value is free while the pane is idle.")),
        ("mobile_push_interval_ms", knob(json!(d.mobile_push_interval_ms),
            "How often (ms) the session daemon pushes the workspace board + \
             briefing to a subscribed phone (the Rover bridge). The tick loop \
             runs far faster; this throttles the LAN push. Only active while a \
             phone is subscribed.")),
        ("rover_map_min_secs", knob(json!(d.rover_map_min_secs),
            "Minimum seconds between Rover per-workspace summary (MAP) calls. One \
             changed workspace is re-summarized per interval; idle workspaces are \
             never re-summarized. Only fires while a phone is subscribed.")),
        ("foreground_sample_secs", knob(json!(d.foreground_sample_secs),
            "Minimum seconds between samples of each board pane's foreground command \
             (used to label agent panes on the phone). Each sample spawns `ps` per \
             pane, so lowering this costs processes; raising it only delays how fast a \
             pane's label catches up. Only sampled while a phone is subscribed.")),
        ("rover_brief_min_secs", knob(json!(d.rover_brief_min_secs),
            "Minimum seconds between Rover mission-briefing (REDUCE) calls, which \
             compose the briefing from the cached summaries. Re-runs only when a \
             summary or verdict changed. Only fires while a phone is subscribed.")),
    ]
}

// ── load() ────────────────────────────────────────────────────────────────────

pub fn tuning_path() -> Option<std::path::PathBuf> {
    crate::config::state_path().map(|p| p.with_file_name("tuning.json"))
}

/// Write the annotated default knobs to `path` (used on first run and reset).
fn write_default_knobs(path: &std::path::Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let map: serde_json::Map<String, serde_json::Value> = default_knobs()
        .into_iter()
        .map(|(k, v)| (k.to_string(), serde_json::to_value(v).unwrap()))
        .collect();
    if let Ok(json) = serde_json::to_string_pretty(&map) {
        let _ = std::fs::write(path, json);
    }
}

/// Restore tuning.json to defaults, backing up the current file.
pub fn reset() {
    if let Some(p) = tuning_path() {
        if p.exists() {
            let _ = std::fs::rename(&p, p.with_extension("json.bak"));
        }
        write_default_knobs(&p);
    }
}

pub fn load() -> Tuning {
    let path = tuning_path();
    let user: Option<HashMap<String, Knob>> = path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok());

    if user.is_none() {
        // First run: write the annotated defaults so they're discoverable/editable.
        if let Some(p) = &path {
            write_default_knobs(p);
        }
    }

    let mut t = Tuning::default();
    if let Some(map) = user {
        let get_u64 = |m: &HashMap<String, Knob>, k: &str, d: u64| {
            m.get(k).and_then(|e| e.value.as_u64()).unwrap_or(d)
        };
        let get_f64 = |m: &HashMap<String, Knob>, k: &str, d: f64| {
            m.get(k).and_then(|e| e.value.as_f64()).unwrap_or(d)
        };
        let get_rgb = |m: &HashMap<String, Knob>, k: &str, d: [u8; 3]| {
            m.get(k)
                .and_then(|e| e.value.as_array())
                .and_then(|a| {
                    if a.len() == 3 {
                        Some([a[0].as_u64()? as u8, a[1].as_u64()? as u8, a[2].as_u64()? as u8])
                    } else {
                        None
                    }
                })
                .unwrap_or(d)
        };
        t.poll_interval_ms      = get_u64(&map, "poll_interval_ms", t.poll_interval_ms).max(1);
        t.which_key_delay_ms    = get_u64(&map, "which_key_delay_ms", t.which_key_delay_ms);
        t.nudge_threshold       = get_u64(&map, "nudge_threshold", t.nudge_threshold as u64) as u32;
        t.max_panes             = get_u64(&map, "max_panes", t.max_panes as u64) as usize;
        t.scroll_margin         = get_u64(&map, "scroll_margin", t.scroll_margin as u64) as usize;
        t.page_overlap          = get_u64(&map, "page_overlap", t.page_overlap as u64) as usize;
        t.wheel_scroll_lines    = get_u64(&map, "wheel_scroll_lines", t.wheel_scroll_lines as u64) as usize;
        t.multi_click_ms        = get_u64(&map, "multi_click_ms", t.multi_click_ms);
        t.dropdown_max_rows     = get_u64(&map, "dropdown_max_rows", t.dropdown_max_rows as u64) as u16;
        t.panel_max_height_pct  = get_u64(&map, "panel_max_height_pct", t.panel_max_height_pct as u64) as u16;
        t.ask_panel_max_pct     = get_u64(&map, "ask_panel_max_pct", t.ask_panel_max_pct as u64) as u16;
        t.spinner_speed_ticks   = get_u64(&map, "spinner_speed_ticks", t.spinner_speed_ticks).max(1);
        t.which_key_panel_width = get_u64(&map, "which_key_panel_width", t.which_key_panel_width as u64) as u16;
        t.travel_panel_width    = get_u64(&map, "travel_panel_width", t.travel_panel_width as u64) as u16;
        t.binding_badge_width   = get_u64(&map, "binding_badge_width", t.binding_badge_width as u64) as usize;
        t.selection_bg          = get_rgb(&map, "selection_bg", t.selection_bg);
        t.search_match_bg       = get_rgb(&map, "search_match_bg", t.search_match_bg);
        t.theme_accent          = get_rgb(&map, "theme_accent", t.theme_accent);
        t.theme_accent_bright   = get_rgb(&map, "theme_accent_bright", t.theme_accent_bright);
        t.theme_accent_dark     = get_rgb(&map, "theme_accent_dark", t.theme_accent_dark);
        t.theme_chip_fg         = get_rgb(&map, "theme_chip_fg", t.theme_chip_fg);
        t.theme_terminal        = get_rgb(&map, "theme_terminal", t.theme_terminal);
        t.theme_healthy         = get_rgb(&map, "theme_healthy", t.theme_healthy);
        t.theme_status_failed   = get_rgb(&map, "theme_status_failed", t.theme_status_failed);
        t.theme_status_blocked  = get_rgb(&map, "theme_status_blocked", t.theme_status_blocked);
        t.line_numbers = map
            .get("line_numbers")
            .and_then(|e| e.value.as_bool())
            .unwrap_or(t.line_numbers);
        t.syntax_highlight = get_u64(&map, "syntax_highlight", t.syntax_highlight);
        t.syntax_recolor_ms = get_u64(&map, "syntax_recolor_ms", t.syntax_recolor_ms);
        t.health_line = get_u64(&map, "health_line", t.health_line);
        t.health_sample_secs = get_u64(&map, "health_sample_secs", t.health_sample_secs);
        t.highlight_current_line = get_u64(&map, "highlight_current_line", t.highlight_current_line);
        t.current_line_bg = get_rgb(&map, "current_line_bg", t.current_line_bg);
        t.reading_width = get_u64(&map, "reading_width", t.reading_width);
        t.opaque_background = get_u64(&map, "opaque_background", t.opaque_background);
        t.auto_name_secs = get_u64(&map, "auto_name_secs", t.auto_name_secs);
        t.agent_max_tokens      = get_u64(&map, "agent_max_tokens", t.agent_max_tokens as u64) as u32;
        t.agent_temperature     = get_f64(&map, "agent_temperature", t.agent_temperature);
        t.terminal_default_rows = get_u64(&map, "terminal_default_rows", t.terminal_default_rows as u64) as u16;
        t.terminal_default_cols = get_u64(&map, "terminal_default_cols", t.terminal_default_cols as u64) as u16;
        t.terminal_scrollback_lines =
            get_u64(&map, "terminal_scrollback_lines", t.terminal_scrollback_lines as u64) as usize;
        t.terminal_startup_probe_ms =
            get_u64(&map, "terminal_startup_probe_ms", t.terminal_startup_probe_ms).max(1);
        t.autosave_secs = get_u64(&map, "autosave_secs", t.autosave_secs);
        t.project_index_max = get_u64(&map, "project_index_max", t.project_index_max as u64) as usize;
        t.tree_width = get_u64(&map, "tree_width", t.tree_width as u64) as u16;
        t.tree_show_dotfiles = get_u64(&map, "tree_show_dotfiles", t.tree_show_dotfiles);
        t.watch_quiet_secs = get_u64(&map, "watch_quiet_secs", t.watch_quiet_secs);
        t.stall_secs = get_u64(&map, "stall_secs", t.stall_secs);
        t.agent_scrollback_context =
            get_u64(&map, "agent_scrollback_context", t.agent_scrollback_context as u64) as usize;
        t.memory_cwd_boost = get_f64(&map, "memory_cwd_boost", t.memory_cwd_boost);
        t.memory_recency_boost = get_f64(&map, "memory_recency_boost", t.memory_recency_boost);
        t.memory_recency_halflife_days =
            get_f64(&map, "memory_recency_halflife_days", t.memory_recency_halflife_days);
        t.mission_refresh_secs = get_u64(&map, "mission_refresh_secs", t.mission_refresh_secs);
        t.worklog_max_lines = get_u64(&map, "worklog_max_lines", t.worklog_max_lines);
        // Renamed from `shift_report`; honor the old key if a config predates it.
        t.mission_briefing =
            get_u64(&map, "mission_briefing", get_u64(&map, "shift_report", t.mission_briefing));
        t.mission_briefing_animate =
            get_u64(&map, "mission_briefing_animate", t.mission_briefing_animate);
        t.mission_briefing_type_ms =
            get_u64(&map, "mission_briefing_type_ms", t.mission_briefing_type_ms);
        t.mission_briefing_prose_rows =
            get_u64(&map, "mission_briefing_prose_rows", t.mission_briefing_prose_rows);
        t.auto_watch = get_u64(&map, "auto_watch", t.auto_watch);
        t.watch_min_active_secs = get_u64(&map, "watch_min_active_secs", t.watch_min_active_secs);
        t.goal_tracking = get_u64(&map, "goal_tracking", t.goal_tracking);
        t.mobile_push_interval_ms =
            get_u64(&map, "mobile_push_interval_ms", t.mobile_push_interval_ms).max(1);
        t.mobile_pane_interval_ms =
            get_u64(&map, "mobile_pane_interval_ms", t.mobile_pane_interval_ms).max(1);
        t.manager_snapshot_secs = get_u64(&map, "manager_snapshot_secs", t.manager_snapshot_secs);
        t.manager_snapshot_keep =
            get_u64(&map, "manager_snapshot_keep", t.manager_snapshot_keep).max(1);
        t.manager_detail_min_secs =
            get_u64(&map, "manager_detail_min_secs", t.manager_detail_min_secs);
        t.manager_agent_secs = get_u64(&map, "manager_agent_secs", t.manager_agent_secs);
        t.manager_agent_stale_secs =
            get_u64(&map, "manager_agent_stale_secs", t.manager_agent_stale_secs);
        if let Some(c) = map.get("manager_agent_command").and_then(|v| v.value.as_str()) {
            if !c.trim().is_empty() {
                t.manager_agent_command = c.to_string();
            }
        }
        t.terminal_line_log_bytes =
            get_u64(&map, "terminal_line_log_bytes", t.terminal_line_log_bytes as u64).max(64 * 1024) as usize;
        t.rover_map_min_secs = get_u64(&map, "rover_map_min_secs", t.rover_map_min_secs).max(1);
        t.rover_brief_min_secs = get_u64(&map, "rover_brief_min_secs", t.rover_brief_min_secs).max(1);
        t.foreground_sample_secs = get_u64(&map, "foreground_sample_secs", t.foreground_sample_secs).max(1);
        if let Some(list) = map.get("project_ignore").and_then(|e| e.value.as_array()) {
            let dirs: Vec<String> =
                list.iter().filter_map(|v| v.as_str().map(String::from)).collect();
            if !dirs.is_empty() {
                t.project_ignore = dirs;
            }
        }
    }

    // Resolve the color palette: the selected theme (in ~/.mars/config.json) supplies
    // every token. A legacy `theme_*`/`selection_bg`/… knob overrides its token ONLY
    // when the user actually changed it from the Mission Control default — so themes
    // apply out of the box, yet a real customization still wins.
    let d = Tuning::default();
    let mut pal = crate::themes::resolve(crate::config::selected_theme().as_deref());
    let ov = |cur: [u8; 3], def: [u8; 3], base: Color| -> Color {
        if cur != def { Color::Rgb(cur[0], cur[1], cur[2]) } else { base }
    };
    pal.accent        = ov(t.theme_accent,         d.theme_accent,         pal.accent);
    pal.accent_bright = ov(t.theme_accent_bright,  d.theme_accent_bright,  pal.accent_bright);
    pal.accent_dark   = ov(t.theme_accent_dark,    d.theme_accent_dark,    pal.accent_dark);
    pal.on_accent     = ov(t.theme_chip_fg,        d.theme_chip_fg,        pal.on_accent);
    pal.info          = ov(t.theme_terminal,       d.theme_terminal,       pal.info);
    pal.success       = ov(t.theme_healthy,        d.theme_healthy,        pal.success);
    pal.warning       = ov(t.theme_status_blocked, d.theme_status_blocked, pal.warning);
    pal.danger        = ov(t.theme_status_failed,  d.theme_status_failed,  pal.danger);
    pal.selection_bg  = ov(t.selection_bg,         d.selection_bg,         pal.selection_bg);
    pal.search_bg     = ov(t.search_match_bg,      d.search_match_bg,      pal.search_bg);
    pal.current_line  = ov(t.current_line_bg,      d.current_line_bg,      pal.current_line);
    t.palette = pal;
    t
}
