/// The action palette behind mission control (the command bar): a fuzzy-searchable dropdown
/// that is the single "how do I do X?" entry point. Opened by Ctrl+Space / M-x.

use std::collections::HashMap;

// ── Actions ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Action {
    // A generated name the agent is RECOMMENDING. Accepting it renames; ignoring it costs
    // nothing. Naming used to apply itself, which moved the session's socket out from under
    // anything holding it by name (the Rover bridge, the manager repo) — so a helpful rename
    // became an outage. A recommendation cannot do that.
    AcceptSessionName,
    AcceptTabName,
    // windows / panes
    SplitHorizontal,
    SplitVertical,
    ClosePane,
    DeleteOtherWindows,
    NextPane,
    PrevPane,
    SwapPane,
    ZoomPane,
    RenamePane,
    // tabs
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    MoveTabLeft,
    MoveTabRight,
    RenameTab,
    TabMode,
    // files / buffers
    Save,
    ToggleFileTree,
    ManagerPane,
    ToggleMarkdown,
    /// Toggle code syntax highlighting for this session (off by default).
    ToggleSyntaxHighlight,
    RefreshIndex,
    RestoreKeybindings,
    KillBuffer,
    // edit
    Undo,
    Redo,
    UndoMode,
    AwayDigest,
    KillLine,
    KillRegion,
    CopyRegion,
    Yank,
    YankPop,
    Paste,
    KillWordForward,
    KillWordBackward,
    SelectAll,
    GoTop,
    GoBottom,
    GotoLine,
    JumpBlockPrev,
    JumpBlockNext,
    JumpSymbolPrev,
    JumpSymbolNext,
    MatchBracket,
    Recenter,
    // search / terminal / agent / app
    Search,
    QueryReplace,
    OpenTerminal,
    AskAgent,
    /// Ask the agent to explain what's on screen at the cursor.
    ExplainThis,
    /// Triage: "why did this fail?" grounded in the focused terminal.
    ExplainFailure,
    /// W6: watch this terminal — summarize it when it goes quiet or exits.
    WatchPane,
    /// Expand every pending notice into one digest (instead of Esc-ing each).
    ExpandNotices,
    /// Leave the session running and disconnect this client.
    Detach,
    RenameSession,
    /// Open the corrective-memory store in the editor (view/edit/forget lines).
    OpenCommandMemory,
    /// Erase the corrective-memory store (confirmation-gated).
    ClearCommandMemory,
    /// Open the prompt-redaction denylist in the editor.
    OpenDenylist,
    /// Open tuning.json — every behavioral knob, self-describing — in the editor.
    OpenTuning,
    /// Apply the named color theme live (beta) — selected from the Theme ▸ submenu.
    SetTheme(String),
    /// Open the assistant's voice file (~/.mars/persona.md) in the editor.
    OpenPersona,
    /// Detach when in a session (nothing is lost); actually exits standalone.
    Quit,
    /// End the session for good — the deleting verb (confirm-gated).
    KillSession,
}

impl Action {
    /// Resolve an LLM `RUN: <Name>` directive back into an `Action`.
    pub fn from_name(name: &str) -> Option<Action> {
        serde_json::from_str::<Action>(&format!("{:?}", name.trim())).ok()
    }

    /// Short human label — used by which-key hints and the graduation nudge.
    pub fn label(&self) -> &'static str {
        match self {
            Action::AcceptSessionName  => "rename session to the suggestion",
            Action::AcceptTabName      => "rename tab to the suggestion",
            Action::SplitHorizontal    => "split below",
            Action::SplitVertical      => "split right",
            Action::ClosePane          => "close pane",
            Action::DeleteOtherWindows => "only this pane",
            Action::NextPane           => "other pane",
            Action::PrevPane           => "prev pane",
            Action::SwapPane           => "move pane",
            Action::ZoomPane           => "zoom pane",
            Action::RenamePane         => "rename pane",
            Action::NewTab             => "new tab",
            Action::CloseTab           => "close tab",
            Action::NextTab            => "next tab",
            Action::PrevTab            => "prev tab",
            Action::MoveTabLeft        => "move tab left",
            Action::MoveTabRight       => "move tab right",
            Action::RenameTab          => "rename tab",
            Action::TabMode            => "space warp (tabs/panes)",
            Action::Save               => "save",
            Action::ToggleFileTree     => "navigator (browse & jump to files)",
            Action::ManagerPane        => "manager agent pane",
            Action::ToggleMarkdown     => "markdown view",
            Action::ToggleSyntaxHighlight => "syntax highlighting",
            Action::RefreshIndex       => "refresh file index",
            Action::RestoreKeybindings => "restore default keybindings",
            Action::KillBuffer         => "kill buffer",
            Action::Undo               => "undo",
            Action::Redo               => "redo",
            Action::UndoMode           => "time-travel (undo history)",
            Action::KillLine           => "kill line",
            Action::KillRegion         => "kill region",
            Action::CopyRegion         => "copy region",
            Action::Yank               => "yank",
            Action::YankPop            => "yank pop",
            Action::Paste              => "paste",
            Action::KillWordForward    => "kill word",
            Action::KillWordBackward   => "kill word back",
            Action::SelectAll          => "select all",
            Action::GoTop              => "top of file",
            Action::GoBottom           => "bottom of file",
            Action::GotoLine           => "go to line",
            Action::JumpBlockPrev      => "previous block",
            Action::JumpBlockNext      => "next block",
            Action::JumpSymbolPrev     => "previous definition",
            Action::JumpSymbolNext     => "next definition",
            Action::MatchBracket       => "matching bracket",
            Action::Recenter           => "recenter",
            Action::Search             => "search",
            Action::QueryReplace       => "search & replace",
            Action::OpenTerminal       => "terminal",
            Action::AskAgent           => "ask agent",
            Action::ExplainThis        => "explain this",
            Action::ExplainFailure     => "why did this fail?",
            Action::WatchPane          => "watch this pane",
            Action::ExpandNotices      => "show all notices",
            Action::AwayDigest         => "away digest (what happened)",
            Action::Detach             => "detach session",
            Action::RenameSession      => "rename session",
            Action::OpenCommandMemory  => "open command memory",
            Action::ClearCommandMemory => "forget all commands",
            Action::OpenDenylist       => "open redaction denylist",
            Action::OpenTuning         => "open tuning knobs",
            Action::SetTheme(_)        => "set theme",
            Action::OpenPersona        => "open persona (voice)",
            Action::Quit               => "quit (detach)",
            Action::KillSession        => "kill session",
        }
    }

    /// Destructive actions the agent's `RUN:` directive must confirm first.
    pub fn is_destructive(&self) -> bool {
        matches!(
            self,
            Action::Quit
                | Action::KillSession
                | Action::CloseTab
                | Action::KillBuffer
                | Action::ClosePane
                | Action::DeleteOtherWindows
                | Action::ClearCommandMemory
        )
    }
}

/// Keys that act INSIDE the bar, on an empty query (right after Ctrl+Space):
/// one keypress to a submode or action, so they outrank the global chords in
/// the dropdown's teaching column — a global chord needs the bar closed first.
/// Dispatch lives in `App::handle_bar`; the selfcheck pins each entry to its
/// real behavior so this table can't drift from what the keys do.
pub fn bar_quick_key(action: &Action) -> Option<char> {
    match action {
        Action::ToggleFileTree => Some('@'),
        Action::AskAgent       => Some('?'),
        _ => None,
    }
}

/// The full in-bar key legend (includes keys with no Action equivalent, like
/// `!` shell and Tab), shown on the bar line while the query is empty.
pub fn bar_quick_legend() -> &'static [(&'static str, &'static str)] {
    &[("!", "shell"), ("?", "ask"), ("@", "files"), ("⇥", "ask/cmd")]
}

// ── Menu structure ───────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ItemKind {
    Run(Action),
    Submenu(&'static str), // name passed to menu_for()
    /// A live workspace surface (Tier-2 board): a pane running work that needs
    /// you, injected ahead of the launcher when the query is empty. `↵` jumps to
    /// it. Built fresh each keystroke from the `pane_verdict` seam — never static.
    Surface(SurfaceRef),
}

/// A snapshot of one workspace surface, enough to rank it, render its row, and
/// jump to it. The verdict is snapshotted at build time so ranking and rendering
/// can never disagree within a frame.
#[derive(Debug, Clone)]
pub struct SurfaceRef {
    pub pane_id: crate::pane::PaneId,
    pub tab_index: usize,
    pub verdict: crate::briefing::Verdict,
    pub age_secs: u64,
}

pub struct MenuItem {
    pub label: std::borrow::Cow<'static, str>,
    pub kind: ItemKind,
    pub description: std::borrow::Cow<'static, str>,
}

impl MenuItem {
    fn run_desc(label: &'static str, action: Action, description: &'static str) -> Self {
        MenuItem { label: label.into(), kind: ItemKind::Run(action), description: description.into() }
    }
    /// Like `run_desc` but for runtime-built rows (e.g. the theme list read from disk).
    fn run_owned(label: String, action: Action, description: String) -> Self {
        MenuItem { label: label.into(), kind: ItemKind::Run(action), description: description.into() }
    }
    fn sub(label: &'static str, name: &'static str) -> Self {
        MenuItem { label: label.into(), kind: ItemKind::Submenu(name), description: "Open submenu".into() }
    }
}

// Keybindings are NOT hardcoded here — the UI shows each row's live binding
// via `KeyBindings::binding_for`, so hints stay honest after a remap.
fn root_menu() -> Vec<MenuItem> {
    vec![
        MenuItem::run_desc("Save",          Action::Save,           "Save the current buffer"),
        MenuItem::run_desc("Navigator — browse & jump to files", Action::ToggleFileTree, "Open the file sidebar; type to filter, Enter to jump (also @)"),
        MenuItem::run_desc("Manager — the agent that writes your feed", Action::ManagerPane, "Reveal the hidden pane where the manager agent runs; read what it decided and why"),
        MenuItem::run_desc("Markdown view", Action::ToggleMarkdown, "Toggle a richly-rendered read-only view of this Markdown buffer"),
        MenuItem::run_desc("Syntax highlighting", Action::ToggleSyntaxHighlight, "Toggle code coloring for the focused buffer — off by default; follows the active theme"),
        MenuItem::run_desc("Search",        Action::Search,       "Incremental search in this buffer"),
        MenuItem::run_desc("Search & replace…", Action::QueryReplace, "Find and replace, stepping y/n through each match"),
        MenuItem::run_desc("Undo",          Action::Undo,         "Undo the last edit"),
        MenuItem::run_desc("Redo",          Action::Redo,         "Redo the undone edit"),
        MenuItem::run_desc("Time-travel…",  Action::UndoMode,     "Scrub edit history: ←/→ back and forward, Home to the start (C-u)"),
        MenuItem::run_desc("Paste",         Action::Paste,        "Paste from the system clipboard"),
        MenuItem::run_desc("Select all",    Action::SelectAll,    "Select the whole buffer"),
        MenuItem::sub("Edit ▸",             "edit"),
        MenuItem::sub("Window ▸",           "window"),
        MenuItem::sub("Tab ▸",              "tab"),
        MenuItem::sub("Go ▸",               "go"),
        MenuItem::run_desc("Open terminal", Action::OpenTerminal, "Open or re-attach a shell in this pane"),
        MenuItem::run_desc("Ask agent",     Action::AskAgent,     "Ask the LLM how to do something"),
        MenuItem::run_desc("Explain this",  Action::ExplainThis,  "Explain what's on screen at the cursor"),
        MenuItem::run_desc("Why did this fail?", Action::ExplainFailure, "Triage the error in the focused terminal"),
        MenuItem::run_desc("Watch this pane", Action::WatchPane, "Summarize this terminal when it goes quiet or exits (even detached)"),
        MenuItem::run_desc("Show all notices", Action::ExpandNotices, "Expand every pending watch/agent notice into one digest and clear them"),
        MenuItem::run_desc("Away digest",   Action::AwayDigest,   "What happened while you were gone — runs, exits, changed files"),
        MenuItem::run_desc("Open command memory", Action::OpenCommandMemory, "See and edit everything the agent remembers (delete lines to forget)"),
        MenuItem::run_desc("Forget all commands", Action::ClearCommandMemory, "Erase the agent's remembered commands (asks first)"),
        MenuItem::run_desc("Open redaction denylist", Action::OpenDenylist, "Edit the strings always redacted from LLM prompts"),
        MenuItem::run_desc("Open tuning knobs", Action::OpenTuning, "Edit every behavioral knob (tuning.json) — each explains itself"),
        MenuItem::sub("Theme ▸ (beta)", "themes"),
        MenuItem::run_desc("Open persona", Action::OpenPersona, "Edit the voice the assistant replies in — style only, never behavior"),
        MenuItem::run_desc("Detach session", Action::Detach,      "Disconnect; the session keeps running (reattach: mars attach)"),
        MenuItem::run_desc("Rename session", Action::RenameSession, "Rename this session (also: mars rename <old> <new>)"),
        MenuItem::run_desc("Refresh file index", Action::RefreshIndex, "Re-scan the project for the file tree/picker"),
        MenuItem::run_desc("Restore default keys", Action::RestoreKeybindings, "Reset keybindings to defaults (backs up keys.json)"),
        MenuItem::run_desc("Quit (detach)", Action::Quit,         "Leave — the session keeps running (reattach: mars attach)"),
        MenuItem::run_desc("Kill session",  Action::KillSession,  "End this session for good — autosaves, then deletes it (asks first)"),
    ]
}

fn edit_menu() -> Vec<MenuItem> {
    vec![
        MenuItem::run_desc("Cut / kill region", Action::KillRegion,     "Cut the selection to the kill-ring"),
        MenuItem::run_desc("Copy region",       Action::CopyRegion,     "Copy the selection (or line) to the kill-ring + clipboard"),
        MenuItem::run_desc("Yank (paste)",      Action::Yank,           "Paste the last kill from the kill-ring"),
        MenuItem::run_desc("Yank-pop",          Action::YankPop,        "Cycle to an earlier kill after a yank"),
        MenuItem::run_desc("Kill to line end",  Action::KillLine,       "Cut from the cursor to the end of the line"),
        MenuItem::run_desc("Kill word forward", Action::KillWordForward, "Cut the word after the cursor"),
        MenuItem::run_desc("Kill word back",    Action::KillWordBackward, "Cut the word before the cursor"),
        MenuItem::run_desc("Close file (kill buffer)", Action::KillBuffer, "Close the current buffer (C-x k)"),
    ]
}

fn window_menu() -> Vec<MenuItem> {
    vec![
        MenuItem::run_desc("Split below ─",  Action::SplitHorizontal,    "Split the pane below"),
        MenuItem::run_desc("Split right │",  Action::SplitVertical,      "Split the pane right"),
        MenuItem::run_desc("Other window",   Action::NextPane,           "Focus the next pane (also Ctrl-arrows)"),
        MenuItem::run_desc("Previous window", Action::PrevPane,          "Focus the previous pane"),
        MenuItem::run_desc("Zoom pane",      Action::ZoomPane,           "Maximize this pane / restore (also C-t z)"),
        MenuItem::run_desc("Move pane",      Action::SwapPane,           "Swap this pane with the next"),
        MenuItem::run_desc("Rename pane",    Action::RenamePane,         "Set a custom title for this pane"),
        MenuItem::run_desc("Only this",      Action::DeleteOtherWindows, "Close the other panes"),
        MenuItem::run_desc("Close pane",     Action::ClosePane,          "Close this pane"),
    ]
}

fn tab_menu() -> Vec<MenuItem> {
    vec![
        MenuItem::run_desc("New tab",        Action::NewTab,       "Open a new tab with a scratch buffer"),
        MenuItem::run_desc("Close tab",      Action::CloseTab,     "Close the current tab"),
        MenuItem::run_desc("Rename tab",     Action::RenameTab,    "Name this tab (also r in C-t space warp)"),
        MenuItem::run_desc("Next tab",       Action::NextTab,      "Switch to the next tab (also M-1..9)"),
        MenuItem::run_desc("Prev tab",       Action::PrevTab,      "Switch to the previous tab"),
        MenuItem::run_desc("Move tab right", Action::MoveTabRight, "Reorder: move this tab right"),
        MenuItem::run_desc("Move tab left",  Action::MoveTabLeft,  "Reorder: move this tab left"),
        MenuItem::run_desc("Space warp mode…", Action::TabMode,    "One-key tab/pane verbs with an on-screen cheat panel (C-t)"),
    ]
}

fn go_menu() -> Vec<MenuItem> {
    vec![
        MenuItem::run_desc("Top of file",    Action::GoTop,    "Jump to the first line"),
        MenuItem::run_desc("Bottom of file", Action::GoBottom, "Jump to the last line"),
        MenuItem::run_desc("Go to line…",    Action::GotoLine, "Jump to a line number"),
        MenuItem::run_desc("Next definition", Action::JumpSymbolNext, "Jump to the next fn/def/class"),
        MenuItem::run_desc("Prev definition", Action::JumpSymbolPrev, "Jump to the previous fn/def/class"),
        MenuItem::run_desc("Next block",      Action::JumpBlockNext,  "Jump to the next blank line"),
        MenuItem::run_desc("Prev block",      Action::JumpBlockPrev,  "Jump to the previous blank line"),
        MenuItem::run_desc("Matching bracket", Action::MatchBracket,  "Jump to the matching ( [ {"),
        MenuItem::run_desc("Recenter",       Action::Recenter, "Center the view on the cursor"),
    ]
}

pub fn menu_for(name: &str) -> Vec<MenuItem> {
    match name {
        "edit"   => edit_menu(),
        "window" => window_menu(),
        "tab"    => tab_menu(),
        "go"     => go_menu(),
        "themes" => themes_menu(),
        _        => root_menu(),
    }
}

/// The Theme ▸ picker — one row per available theme, read live from the bundled set
/// and your `~/.mars/themes/` folder (so a dropped-in theme just appears). Selecting
/// one applies it live. The "Theme:" prefix makes them findable by typing "theme".
fn themes_menu() -> Vec<MenuItem> {
    crate::themes::list()
        .into_iter()
        .map(|t| {
            let about = if t.user { format!("{} (yours)", t.about) } else { t.about };
            MenuItem::run_owned(t.display, Action::SetTheme(t.name), about)
        })
        .collect()
}

/// Flattened list of all leaf (Run) items used for fuzzy search.
fn all_items() -> Vec<(String, ItemKind, String)> {
    let mut result = Vec::new();
    for item in root_menu() {
        match item.kind.clone() {
            ItemKind::Run(_) => {
                result.push((item.label.to_string(), item.kind, item.description.to_string()));
            }
            ItemKind::Submenu(sub_name) => {
                // The submenu entry itself is searchable (so typing "theme" finds
                // "Theme ▸" and opens it), plus its leaves (so "eclipse" applies directly).
                result.push((item.label.to_string(), ItemKind::Submenu(sub_name), item.description.to_string()));
                for subitem in menu_for(sub_name) {
                    result.push((subitem.label.to_string(), subitem.kind, subitem.description.to_string()));
                }
            }
            ItemKind::Surface(_) => {} // never appears in a static menu (built live in app.rs)
        }
    }
    result
}

/// A textual catalog of every runnable action (name + description) that the
/// LLM agent receives as context, so its answers cite real Ares commands and
/// can emit a matching `RUN: <Name>` directive.
pub fn registry_context() -> String {
    let mut out = String::new();
    for (label, kind, description) in all_items() {
        if let ItemKind::Run(a) = kind {
            out.push_str(&format!("- {:?}: {} — {}\n", a, label.trim(), description));
        }
    }
    out
}

/// Frecency weight for a row (0 for submenus / never-used actions).
fn row_frecency(row: &PaletteRow, frecency: &HashMap<String, u32>) -> u32 {
    if let ItemKind::Run(a) = &row.kind {
        *frecency.get(&format!("{:?}", a)).unwrap_or(&0)
    } else {
        0
    }
}

// ── Fuzzy scoring ────────────────────────────────────────────────────────────

/// Returns Some(score) if every char in `query` appears (in order) in
/// `candidate`; None if no subsequence match.
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.to_lowercase().chars().collect();
    let c: Vec<char> = candidate.to_lowercase().chars().collect();

    let mut score: i64 = 0;
    let mut qi = 0;
    let mut prev_matched = false;

    for (ci, &ch) in c.iter().enumerate() {
        if qi >= q.len() {
            break;
        }
        if ch == q[qi] {
            score += 1;
            if prev_matched {
                score += 2; // contiguous run bonus
            }
            if ci == 0 || c[ci - 1] == ' ' || c[ci - 1] == '_' {
                score += 3; // word-boundary bonus
            }
            qi += 1;
            prev_matched = true;
        } else {
            prev_matched = false;
        }
    }

    if qi < q.len() {
        None
    } else {
        Some(score)
    }
}

// ── Bar mode ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum BarMode {
    Command,
    Ask,
    /// `!` prefix — the query is a shell command to run in the terminal pane.
    Shell,
}

/// Which column of the two-pane command board holds focus. Workspaces (left) are a
/// live status board + switcher; Commands (right) is the launcher. ←/→ cross between
/// them, ↑/↓ move within, so panes and commands stay two separate ontologies.
#[derive(Debug, Clone, PartialEq)]
pub enum BarColumn {
    Workspaces,
    Commands,
}

// ── Palette state ─────────────────────────────────────────────────────────────

pub struct PaletteRow {
    pub label: String,
    pub kind: ItemKind,
    pub description: String,
}

pub struct Palette {
    /// Submenu breadcrumb — "root" is always the first entry.
    pub stack: Vec<&'static str>,
    pub query: String,
    pub selected: usize,
    pub bar_mode: BarMode,
    /// The user explicitly arrowed into the menu (↑/↓/C-p/C-n). In a terminal
    /// the composer is shell-first: Enter runs the query as a command UNLESS a
    /// suggestion was deliberately selected. Reset on open and on query edits.
    pub navigated: bool,
    /// Which column of the two-pane board has focus (Enter acts on it).
    pub column: BarColumn,
    /// Selection index within the Workspaces column (independent of `selected`,
    /// which is the Commands column — so the detail strip tracks the highlighted
    /// workspace even while focus is on Commands).
    pub sel_ws: usize,
}

impl Palette {
    pub fn root() -> Self {
        Palette {
            stack: vec!["root"],
            query: String::new(),
            selected: 0,
            bar_mode: BarMode::Command,
            navigated: false,
            column: BarColumn::Commands,
            sel_ws: 0,
        }
    }

    pub fn current_menu(&self) -> &'static str {
        self.stack.last().copied().unwrap_or("root")
    }

    /// Items to display given current menu level + query text.
    /// Empty query → **fixed** curated order (positional memory can form).
    /// Non-empty → fuzzy score, with frecency as a tiebreaker.
    pub fn visible_items(&self, frecency: &HashMap<String, u32>) -> Vec<PaletteRow> {
        if self.query.is_empty() {
            // Fixed order — do NOT reorder by frecency (spatial stability, §2.2).
            menu_for(self.current_menu())
                .into_iter()
                .map(|item| PaletteRow {
                    label: item.label.to_string(),
                    kind: item.kind,
                    description: item.description.to_string(),
                })
                .collect()
        } else {
            let mut scored: Vec<(i64, u32, PaletteRow)> = all_items()
                .into_iter()
                .filter_map(|(label, kind, description)| {
                    fuzzy_score(&self.query, &label).map(|s| {
                        let row = PaletteRow { label, kind, description };
                        let f = row_frecency(&row, frecency);
                        (s, f, row)
                    })
                })
                .collect();
            // Fuzzy score first, frecency as tiebreaker.
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
            scored.into_iter().map(|(_, _, row)| row).collect()
        }
    }

    /// Move selection up (wrapping).
    pub fn select_up(&mut self, total: usize) {
        if total == 0 {
            return;
        }
        // First arrow just engages the menu (highlights the current row).
        if !std::mem::replace(&mut self.navigated, true) {
            return;
        }
        self.selected = if self.selected == 0 { total - 1 } else { self.selected - 1 };
    }

    /// Move selection down (wrapping).
    pub fn select_down(&mut self, total: usize) {
        if total == 0 {
            return;
        }
        if !std::mem::replace(&mut self.navigated, true) {
            return;
        }
        self.selected = (self.selected + 1) % total;
    }

    /// Push into a submenu, reset query + selection.
    pub fn push(&mut self, name: &'static str) {
        self.stack.push(name);
        self.query.clear();
        self.selected = 0;
    }

    /// Pop one level. Returns `true` if the palette should stay open, `false` if it should close.
    pub fn pop(&mut self) -> bool {
        if self.stack.len() > 1 {
            self.stack.pop();
            self.query.clear();
            self.selected = 0;
            true
        } else {
            false // at root — caller should close
        }
    }
}
