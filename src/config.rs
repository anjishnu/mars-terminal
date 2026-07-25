/// Config-driven keybindings — loaded from ~/.config/mars/keys.json
/// or .mars/keys.json; defaults written on first run. `mars reset` restores them.
///
/// Bindings are **sequences** of chords, so Emacs prefixes like `C-x C-s`
/// work. Emacs notation (`C-x`, `M-x`, `S-tab`) and long form (`ctrl-x`) both parse.

use std::collections::HashMap;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};

use crate::palette::Action;

// ── KeyChord ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub modifiers: KeyModifiers,
    pub code: KeyCode,
}

/// Build a `KeyChord` from a live crossterm `KeyEvent` (normalizing away the
/// KEYPAD/other bits crossterm sometimes sets).
///
/// SHIFT is dropped for non-alphabetic chars: terminals report `M-<` as
/// ALT|SHIFT + '<', but the '<' already encodes the shift — keeping the bit
/// would make bindings like `M-<` never match.
pub fn chord_of(key: &KeyEvent) -> KeyChord {
    let mut m = KeyModifiers::empty();
    if key.modifiers.contains(KeyModifiers::CONTROL) { m |= KeyModifiers::CONTROL; }
    if key.modifiers.contains(KeyModifiers::ALT)     { m |= KeyModifiers::ALT; }
    if key.modifiers.contains(KeyModifiers::SHIFT)   { m |= KeyModifiers::SHIFT; }
    if key.modifiers.contains(KeyModifiers::SUPER)   { m |= KeyModifiers::SUPER; }
    if let KeyCode::Char(c) = key.code {
        if !c.is_alphabetic() {
            m -= KeyModifiers::SHIFT;
        }
    }
    KeyChord { modifiers: m, code: key.code }
}

/// Parse one chord. Accepts `C-`, `M-`, `S-` (Emacs), `cmd-`/`super-` (mac ⌘,
/// delivered only by kitty-protocol terminals) and `ctrl-`, `alt-`, `shift-`
/// prefixes, plus named keys (`space`, `esc`, `ret`, `tab`, …).
pub fn parse_key(s: &str) -> Option<KeyChord> {
    let s = s.trim();
    for (pfx, m) in [
        ("ctrl-", KeyModifiers::CONTROL), ("c-", KeyModifiers::CONTROL),
        ("alt-", KeyModifiers::ALT),      ("m-", KeyModifiers::ALT),
        ("cmd-", KeyModifiers::SUPER),    ("super-", KeyModifiers::SUPER),
        ("shift-", KeyModifiers::SHIFT),  ("s-", KeyModifiers::SHIFT),
    ] {
        if s.len() > pfx.len() && s[..pfx.len()].eq_ignore_ascii_case(pfx) {
            let inner = parse_key(&s[pfx.len()..])?;
            return Some(KeyChord { modifiers: inner.modifiers | m, code: inner.code });
        }
    }

    let code = match s.to_lowercase().as_str() {
        "esc" | "escape"     => KeyCode::Esc,
        "space" | "spc"      => KeyCode::Char(' '),
        "enter" | "return" | "ret" => KeyCode::Enter,
        "tab"                => KeyCode::Tab,
        "backspace" | "del"  => KeyCode::Backspace,
        "up"    => KeyCode::Up,   "down"  => KeyCode::Down,
        "left"  => KeyCode::Left, "right" => KeyCode::Right,
        "home"  => KeyCode::Home, "end"   => KeyCode::End,
        "pageup"   => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "delete"   => KeyCode::Delete,
        _ => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None; // unrecognized multi-char token
            }
            // Uppercase letter → SHIFT implied.
            let modifiers = if c.is_ascii_uppercase() { KeyModifiers::SHIFT } else { KeyModifiers::NONE };
            return Some(KeyChord { modifiers, code: KeyCode::Char(c) });
        }
    };
    Some(KeyChord { modifiers: KeyModifiers::NONE, code })
}

/// Parse a whitespace-separated sequence, e.g. `"C-x C-s"`.
pub fn parse_sequence(s: &str) -> Option<Vec<KeyChord>> {
    let seq: Vec<KeyChord> = s.split_whitespace().filter_map(parse_key).collect();
    if seq.is_empty() { None } else { Some(seq) }
}

// ── KeyBindings ───────────────────────────────────────────────────────────────

pub struct KeyBindings {
    /// Full chord-sequence → action map (Emacs prefixes included).
    pub edit: HashMap<Vec<KeyChord>, Action>,
    /// Single chords that open the command bar (Ctrl+Space, M-x).
    pub bar_open: Vec<KeyChord>,
    /// Definition order of each sequence — the canonical-vs-alias tiebreak, so
    /// `binding_for` prefers the first-listed chord (C-s over the C-r alias).
    order: HashMap<Vec<KeyChord>, usize>,
}

/// Capability tier of a chord sequence for DISPLAY preference: lower = more
/// universally deliverable. A hint must never advertise a chord the user's
/// terminal can't physically send (the honesty invariant is only satisfied if
/// the shown chord actually reaches Mars). This does not affect what's *bound* —
/// every chord still works where the terminal supports it; this only chooses
/// which of several equivalents to teach.
fn chord_tier(seq: &[KeyChord]) -> u8 {
    let mut tier = 0u8;
    for c in seq {
        // ⌘/super is forwarded only by kitty-protocol terminals; elsewhere the
        // OS/terminal consumes it and nothing reaches Mars.
        if c.modifiers.contains(KeyModifiers::SUPER) {
            tier = tier.max(2);
        }
        if c.modifiers.contains(KeyModifiers::CONTROL) {
            if let KeyCode::Char(ch) = c.code {
                match ch {
                    // Ctrl + shifted punctuation rides the kitty protocol only.
                    '|' | '{' | '}' => tier = tier.max(2),
                    // Ambiguous legacy encodings (C-- ⇒ C-_ ⇒ Undo on many
                    // terminals; C-\ ⇒ SIGQUIT in some shells) — prefer a
                    // cleaner equivalent when one exists.
                    '-' | '\\' | '_' => tier = tier.max(1),
                    _ => {}
                }
            }
        }
    }
    tier
}

impl KeyBindings {
    pub fn lookup(&self, seq: &[KeyChord]) -> Option<Action> {
        self.edit.get(seq).cloned()
    }
    /// The best chord sequence bound to `action`, rendered for display — the
    /// single source of truth for every hint surface. Preference order:
    /// capability tier (universal first), then length (shortest), then
    /// definition order (canonical over alias), then lexicographic.
    pub fn binding_for(&self, action: &Action) -> Option<String> {
        self.edit
            .iter()
            .filter(|(_, a)| *a == action)
            .map(|(seq, _)| {
                let order = self.order.get(seq).copied().unwrap_or(usize::MAX);
                (chord_tier(seq), seq.len(), order, render_chords(seq))
            })
            .min()
            .map(|(_, _, _, s)| s)
    }

    /// which-key continuations for a pending prefix: (tail keys, action).
    pub fn continuations(&self, prefix: &[KeyChord]) -> Vec<(String, Action)> {
        let mut out: Vec<(String, Action)> = self
            .edit
            .iter()
            .filter(|(seq, _)| seq.len() > prefix.len() && seq.starts_with(prefix))
            .map(|(seq, a)| (render_chords(&seq[prefix.len()..]), a.clone()))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

/// Human-readable chord sequence for hints, e.g. `[Ctrl+x, Ctrl+s] → "C-x C-s"`.
pub fn render_chords(seq: &[KeyChord]) -> String {
    seq.iter()
        .map(|c| {
            let mut s = String::new();
            if c.modifiers.contains(KeyModifiers::SUPER)   { s.push_str("⌘-"); }
            if c.modifiers.contains(KeyModifiers::CONTROL) { s.push_str("C-"); }
            if c.modifiers.contains(KeyModifiers::ALT)     { s.push_str("M-"); }
            match c.code {
                KeyCode::Char(' ') => s.push_str("Spc"),
                KeyCode::Char(ch) => s.push(ch),
                KeyCode::Enter => s.push_str("RET"),
                KeyCode::Tab => s.push_str("TAB"),
                KeyCode::Backspace => s.push_str("DEL"),
                other => s.push_str(&format!("{:?}", other)),
            }
            s
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ── Raw JSON representation ───────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct RawBindings {
    edit: HashMap<String, Action>,
    bar_open: Vec<String>,
}

/// The default bindings in canonical order — the array order is meaningful:
/// `binding_for` prefers earlier entries as the chord to teach (C-s before its
/// C-r alias). Kept as an ordered slice (not a map) so that order survives.
fn default_edit_pairs() -> Vec<(&'static str, Action)> {
    vec![
            // files / buffers
            ("C-x C-s", Action::Save),
            ("C-x C-c", Action::Quit),
            // C-x C-f / C-x p / C-x b are muscle-memory aliases for the one
            // find-file surface Mars actually has: the file tree.
            ("C-x C-f", Action::ToggleFileTree),
            ("C-x C-h", Action::ToggleSyntaxHighlight), // highlight — toggle code coloring
            ("C-x p",   Action::ToggleFileTree),
            ("C-x d",   Action::ToggleFileTree),
            ("C-x b",   Action::ToggleFileTree),
            ("C-x k",   Action::KillBuffer),
            // windows (panes) — the char IS the split direction: | right, - below
            ("C-x 2",   Action::SplitHorizontal),
            ("C-x 3",   Action::SplitVertical),
            ("C-\\",    Action::SplitVertical), // legacy byte for C-| (0x1c)
            ("C-|",     Action::SplitVertical), // kitty-protocol terminals
            ("C--",     Action::SplitHorizontal), // kitty-protocol (legacy sends C-_ → undo)
            ("M--",     Action::SplitHorizontal), // universal fallback
            ("C-x o",   Action::NextPane),
            ("C-o",     Action::NextPane),      // rapid pane cycling, no Meta needed
            ("M-o",     Action::NextPane),
            ("C-x x",   Action::SwapPane),
            ("C-x 1",   Action::DeleteOtherWindows),
            ("C-x 0",   Action::ClosePane),
            // tabs — C-t opens the travel hub (new tab = C-t t)
            ("C-t",     Action::TabMode),
            ("C-x t",   Action::TabMode),
            ("M-{",     Action::PrevTab),       // Alt+Shift+[  (mac Cmd+Shift+[ shape)
            ("M-}",     Action::NextTab),       // Alt+Shift+]
            ("C-{",     Action::PrevTab),       // kitty-protocol terminals
            ("C-}",     Action::NextTab),
            ("C-pageup",   Action::PrevTab),    // browser/VS Code standard
            ("C-pagedown", Action::NextTab),
            // terminal
            ("M-`",     Action::OpenTerminal),
            ("C-x C-t", Action::OpenTerminal),
            // session
            ("C-x C-d", Action::Detach), // detach (also: C-t D in space warp)
            ("C-x g",   Action::AwayDigest), // what happened while I was gone
            ("C-x w",   Action::WatchPane), // watch this pane (also: C-t w)
            // edit / kill-ring
            ("C-k",     Action::KillLine),
            ("C-w",     Action::KillRegion),
            ("M-w",     Action::CopyRegion),
            ("C-c",     Action::CopyRegion), // modern copy (no selection → copies the line)
            // mac ⌘ chords — delivered only by kitty-protocol terminals that
            // forward super; elsewhere the terminal app handles ⌘ itself
            // (⌘V still lands via bracketed paste).
            ("cmd-c",   Action::CopyRegion),
            ("cmd-v",   Action::Paste),
            ("cmd-s",   Action::Save),
            ("cmd-a",   Action::SelectAll),
            ("C-y",     Action::Yank),
            ("M-y",     Action::YankPop),
            ("C-v",     Action::Paste), // system clipboard (explicit ruling; not Emacs page-down)
            ("M-d",     Action::KillWordForward),
            ("M-backspace", Action::KillWordBackward),
            ("C-/",     Action::Undo),
            ("C-_",     Action::Undo), // many terminals send C-/ as C-_ (0x1f)
            ("C-x u",   Action::Undo),
            ("M-/",     Action::Redo), // redo mirrors undo (also: C-x C-u style via menu)
            ("C-x C-u", Action::Redo),
            ("cmd-z",   Action::Undo), // Mac muscle memory (kitty-class terminals)
            ("cmd-Z",   Action::Redo), // ⌘⇧Z
            ("C-u",     Action::UndoMode), // time-travel: ←/→ scrub through history
            ("M-%",     Action::QueryReplace),
            // navigation targets that are commands (not raw motions)
            ("M-<",     Action::GoTop),
            ("M->",     Action::GoBottom),
            ("M-g",     Action::GotoLine),
            // structural jumps (Emacs-style paragraph = C-x [ / ]; symbols + brackets)
            ("C-x [",   Action::JumpBlockPrev),
            ("C-x ]",   Action::JumpBlockNext),
            ("C-x {",   Action::JumpSymbolPrev),
            ("C-x }",   Action::JumpSymbolNext),
            ("C-x m",   Action::MatchBracket),
            ("C-l",     Action::Recenter),
            // agent
            ("C-x e",   Action::ExplainThis),
            ("C-x ?",   Action::ExplainFailure),
            ("C-x h",   Action::SelectAll),
            // search
            ("C-s",     Action::Search),
            ("C-r",     Action::Search), // reverse-isearch is not implemented; C-r = search
    ]
}

impl RawBindings {
    fn defaults() -> Self {
        let edit = default_edit_pairs()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();

        RawBindings {
            edit,
            bar_open: vec!["ctrl-space".into(), "M-x".into()],
        }
    }

    /// Layer defaults under user entries: new default bindings appear even in
    /// old config files; a user entry for the same sequence wins.
    fn into_bindings(self) -> KeyBindings {
        let mut edit: HashMap<Vec<KeyChord>, Action> = HashMap::new();
        let mut order: HashMap<Vec<KeyChord>, usize> = HashMap::new();
        // Defaults first, in their true array order (the canonical preference —
        // C-s before its C-r alias), user entries after; the first assignment
        // wins the order slot so the canonical chord keeps the lower index.
        let defaults = default_edit_pairs().into_iter().map(|(k, v)| (k.to_string(), v));
        for (i, (k, v)) in defaults.chain(self.edit).enumerate() {
            if let Some(seq) = parse_sequence(&k) {
                order.entry(seq.clone()).or_insert(i);
                edit.insert(seq, v);
            }
        }
        // bar_open is the command bar's opener — the recovery hatch. If a
        // hand-edited config empties or breaks it, fall back to defaults so the
        // bar (and thus Quit / reset) is never unreachable.
        let mut bar_open: Vec<KeyChord> = self.bar_open.iter().filter_map(|s| parse_key(s)).collect();
        if bar_open.is_empty() {
            bar_open = RawBindings::defaults().bar_open.iter().filter_map(|s| parse_key(s)).collect();
        }
        KeyBindings { edit, bar_open, order }
    }
}

// ── load() ────────────────────────────────────────────────────────────────────

pub fn load() -> KeyBindings {
    let config_path = app_config_dir().map(|d| d.join("keys.json"));
    let local_path = std::path::PathBuf::from(".mars/keys.json");

    let raw: Option<RawBindings> = config_path
        .as_ref()
        .and_then(|p| try_read(p))
        .or_else(|| try_read(&local_path));

    match raw {
        Some(r) => r.into_bindings(),
        None => {
            let defaults = RawBindings::defaults();
            if let Some(path) = &config_path {
                let _ = write_defaults(path, &defaults);
            }
            defaults.into_bindings()
        }
    }
}

/// Path of the persisted-state file (frecency, nudge counters).
pub fn state_path() -> Option<std::path::PathBuf> {
    app_config_dir().map(|d| d.join("state.json"))
}

/// The global MARS config file `~/.mars/config.json` (env overrides + theme).
pub fn config_json_path() -> Option<std::path::PathBuf> {
    crate::sys::paths::home_dir().map(|h| h.join(".mars").join("config.json"))
}

/// The active theme name from `~/.mars/config.json` (`None` ⇒ the default).
pub fn selected_theme() -> Option<String> {
    let p = config_json_path()?;
    let s = std::fs::read_to_string(&p).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get("theme").and_then(|t| t.as_str()).map(String::from)
}

/// Write `theme` into `~/.mars/config.json`, preserving every other key (env, …).
pub fn set_theme(name: &str) -> std::io::Result<()> {
    let p = config_json_path().ok_or_else(|| std::io::Error::other("no home directory"))?;
    let mut v: serde_json::Value = std::fs::read_to_string(&p)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    if !v.is_object() {
        v = serde_json::json!({});
    }
    v.as_object_mut().unwrap().insert("theme".into(), serde_json::json!(name));
    if let Some(parent) = p.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&p, format!("{}\n", serde_json::to_string_pretty(&v).unwrap()))
}

/// `~/.config/mars`, migrating a pre-rename `~/.config/ares` on first touch.
fn app_config_dir() -> Option<std::path::PathBuf> {
    let base = config_dir()?;
    let mars = base.join("mars");
    let ares = base.join("ares");
    if !mars.join("keys.json").exists() && ares.is_dir() {
        let _ = std::fs::create_dir_all(&mars);
        for f in ["keys.json", "tuning.json", "state.json"] {
            let src = ares.join(f);
            if src.exists() {
                let _ = std::fs::copy(&src, mars.join(f));
            }
        }
    }
    Some(mars)
}

fn config_dir() -> Option<std::path::PathBuf> {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return Some(std::path::PathBuf::from(xdg));
    }
    crate::sys::paths::home_dir().map(|h| h.join(".config"))
}

fn try_read(path: &std::path::Path) -> Option<RawBindings> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Restore keys.json to the compiled defaults, backing up the current file to
/// `keys.json.bak`. Returns the config path (for a status/CLI message).
pub fn reset_keys() -> anyhow::Result<std::path::PathBuf> {
    let path = app_config_dir()
        .map(|d| d.join("keys.json"))
        .ok_or_else(|| anyhow::anyhow!("no config directory"))?;
    if path.exists() {
        let _ = std::fs::rename(&path, path.with_extension("json.bak"));
    }
    write_defaults(&path, &RawBindings::defaults())?;
    Ok(path)
}

fn write_defaults(path: &std::path::Path, raw: &RawBindings) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(raw)?)?;
    Ok(())
}
