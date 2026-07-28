//! Every instruction the binary sends to a model, as editable Markdown under
//! `src/prompts/` — embedded at compile time (`include_str!`) so the
//! single-binary install still ships everything. Editing a prompt is editing
//! its `.md` file; no prompt text lives in code. `{name}` substrings are
//! placeholders the call sites substitute with `.replace()` (substitute
//! user/screen-derived content LAST, so injected text is never re-scanned for
//! placeholders). The selfcheck asserts each template still carries its
//! placeholders, so a stray edit can't silently break assembly.

pub const ASK_SYSTEM: &str = include_str!("prompts/ask_system.md");
pub const TRANSLATE_SYSTEM: &str = include_str!("prompts/translate_system.md");
pub const TRANSLATE_REASONING_CAP: &str = include_str!("prompts/translate_reasoning_cap.md");
pub const TRANSLATE_EXAMPLES: &str = include_str!("prompts/translate_examples.md");
pub const WATCH_SYSTEM: &str = include_str!("prompts/watch_system.md");
pub const WATCH_HINT_EXIT: &str = include_str!("prompts/watch_hint_exit.md");
pub const WATCH_HINT_QUIET: &str = include_str!("prompts/watch_hint_quiet.md");
pub const MISSION_SYSTEM: &str = include_str!("prompts/mission_system.md");
pub const AUTO_NAME_SYSTEM: &str = include_str!("prompts/auto_name_system.md");
pub const NAME_SESSION_SYSTEM: &str = include_str!("prompts/name_session_system.md");
#[cfg_attr(not(feature = "memory"), allow(dead_code))] // consumer is the docs corpus
pub const DOCS_CONTEXT_PREAMBLE: &str = include_str!("prompts/docs_context_preamble.md");
pub const CURSOR_INSERT: &str = include_str!("prompts/cursor_insert.md");
pub const EXPLAIN_THIS: &str = include_str!("prompts/explain_this.md");
pub const EXPLAIN_FAILURE: &str = include_str!("prompts/explain_failure.md");
pub const PERSONA_PREAMBLE: &str = include_str!("prompts/persona_preamble.md");
pub const PERSONA_DEFAULT: &str = include_str!("prompts/persona_default.md");
pub const SHIFT_BRIEF: &str = include_str!("prompts/shift_brief.md");
pub const CAPTURE_GOALS: &str = include_str!("prompts/capture_goals.md");
pub const ROVER_SUMMARY: &str = include_str!("prompts/rover_summary.md");
pub const ROVER_BRIEF: &str = include_str!("prompts/rover_brief.md");

/// Directory of runtime prompt overrides — `MARS_PROMPT_DIR` (e.g. the Rover dev repo's
/// `prompts/`, so the phone-facing prompts iterate alongside Rover) or `~/.mars/prompts`.
/// A `<name>.md` there replaces the compiled-in default. Mirrors the runtime seams for
/// `tiers.json` and `~/.mars/syntaxes/`.
fn override_dir() -> Option<std::path::PathBuf> {
    if let Ok(d) = std::env::var("MARS_PROMPT_DIR") {
        if !d.trim().is_empty() {
            return Some(std::path::PathBuf::from(d));
        }
    }
    crate::sys::paths::home_dir().map(|h| h.join(".mars").join("prompts"))
}

/// The prompt `name`, hot-loaded from the override dir (`<dir>/<name>.md`) if present, else the
/// compiled-in `embedded` default. Read PER CALL, so a dev iterates a prompt without a rebuild.
pub fn resolve(name: &str, embedded: &str) -> String {
    if let Some(dir) = override_dir() {
        if let Ok(text) = std::fs::read_to_string(dir.join(format!("{name}.md"))) {
            if !text.trim().is_empty() {
                return text;
            }
        }
    }
    embedded.to_string()
}
