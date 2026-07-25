#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    /// Non-modal text editing: typing inserts, chords/prefixes run commands.
    Edit,
    /// Mission control (Ctrl+Space / M-x) — fuzzy actions + Ask agent + `!` shell.
    Bar,
    /// Minibuffer prompt (find-file, switch-buffer, save-as, isearch, confirms).
    Prompt,
    /// C-t travel mode: one-char tab/pane verbs with an on-screen cheat panel.
    Tab,
    /// Focused terminal pane owns the keyboard.
    Terminal,
    /// Left file-tree sidebar owns the keyboard (browse + type-to-filter).
    Tree,
    /// Undo time-travel: ←/→ step backward/forward through edit history.
    Undo,
}

impl Mode {
    pub fn label(&self) -> &'static str {
        match self {
            Mode::Edit     => "EDIT",
            Mode::Bar      => "MC",   // mission control
            Mode::Prompt   => "MINI",
            Mode::Tab      => "WARP",
            Mode::Terminal => "TERM",
            Mode::Tree     => "NAV",
            Mode::Undo     => "TIME",
        }
    }

    /// Short hint pairs (key, action) shown in the status bar.
    /// Edit-mode hints are derived live from the keymap in `ui::render_status`
    /// (so they stay honest after a remap) — hence the empty slice here.
    pub fn hints(&self) -> &'static [(&'static str, &'static str)] {
        match self {
            Mode::Edit => &[],
            Mode::Bar => &[
                ("Tab", "cmd/ask"),
                ("!",   "shell"),
                ("?",   "ask"),
                ("↑↓",  "move/scroll"),
                ("⏎",   "run"),
                ("C-l", "new chat"),
                ("Esc", "close"),
            ],
            Mode::Prompt => &[
                ("⏎",   "accept"),
                ("C-g", "cancel"),
            ],
            Mode::Tab => &[
                ("t",   "new tab"),
                ("←→",  "switch"),
                ("1-9", "jump"),
                ("|/-", "split"),
                ("o",   "pane"),
                ("Esc", "done"),
            ],
            // NOTE: the status bar renders Terminal hints live (ui::status_hints)
            // so the bar-open chord stays honest; this static copy is a fallback.
            // "to editor" (not "detach") — C-g leaves the pane; session detach is
            // C-x C-d.
            Mode::Terminal => &[
                ("C-Spc", "commands"),
                ("C-t",   "warp"),
                ("C-g",   "editor"),
            ],
            Mode::Tree => &[
                ("↑↓",   "move"),
                ("→",    "expand/preview"),
                ("⏎",    "open"),
                ("←",    "collapse"),
                ("type", "filter"),
                ("Esc",  "close"),
            ],
            Mode::Undo => &[
                ("←",    "undo"),
                ("→",    "redo"),
                ("Home", "undo all"),
                ("End",  "redo all"),
                ("Esc",  "done"),
            ],
        }
    }
}
