//! Code syntax highlighting via syntect (pure-Rust `fancy-regex` — no C toolchain).
//!
//! Colors are synthesized from the active theme `Palette`, so a theme restyles code
//! by construction — a keyword is `accent`, a string is `success`, a comment is
//! `text-faint`, etc. Grammars are syntect's bundled set plus any `.sublime-syntax`
//! a user drops in `~/.mars/syntaxes/` (the runtime "language pack" seam — no
//! rebuild). Without the `syntax` cargo feature, `syntax_stub.rs` replaces this and
//! everything renders plain.

use std::str::FromStr;
use std::sync::mpsc::Sender;
use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color as SynColor, FontStyle, ScopeSelectors, StyleModifier, Theme, ThemeItem,
};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

use crate::tuning::Palette;

/// The grammar set: syntect's bundled languages, plus any user grammars found in
/// `~/.mars/syntaxes/`. Built once, lazily.
/// Grammars bundled at compile time that syntect's default set lacks. TypeScript is
/// the notable gap — the defaults ship JavaScript but not TS, so `.ts`/`.tsx` would
/// render plain without this.
const BUNDLED_GRAMMARS: &[&str] = &[include_str!("syntaxes/typescript.sublime-syntax")];

fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(|| {
        let mut builder = SyntaxSet::load_defaults_newlines().into_builder();
        // Compile-time grammars beyond syntect's defaults (TypeScript, …).
        for src in BUNDLED_GRAMMARS {
            if let Ok(def) = syntect::parsing::SyntaxDefinition::load_from_str(src, true, None) {
                builder.add(def);
            }
        }
        // Runtime grammars: a user (or a future `mars lang add`) drops a
        // `.sublime-syntax` here and it highlights on next launch — no rebuild.
        if let Some(dir) = crate::sys::paths::home_dir() {
            let _ = builder.add_from_folder(dir.join(".mars").join("syntaxes"), true);
        }
        builder.build()
    })
}

fn syn(c: Color) -> SynColor {
    let [r, g, b] = crate::themes::rgb_of(c);
    SynColor { r, g, b, a: 0xff }
}

/// A syntect theme synthesized from the MARS palette — the whole point: highlighting
/// follows the active color theme. A handful of scope→token rules cover the common
/// categories (keyword, string, comment, number, type, function, punctuation).
fn theme_for(p: &Palette) -> Theme {
    let item = |scope: &str, color: Color| ThemeItem {
        scope: ScopeSelectors::from_str(scope).unwrap_or_default(),
        style: StyleModifier { foreground: Some(syn(color)), background: None, font_style: None },
    };
    let mut theme = Theme::default();
    theme.settings.foreground = Some(syn(p.text));
    // Order matters: later, more-specific rules win in syntect's selector matching.
    theme.scopes = vec![
        item("comment", p.text_faint),
        item("string, constant.character, constant.other.symbol", p.success),
        item("constant.numeric, constant.language, constant.character.escape", p.warning),
        item("keyword, storage, storage.type, storage.modifier", p.accent),
        item("entity.name.type, support.type, support.class, entity.other.inherited-class", p.info),
        item("entity.name.function, support.function, meta.function-call entity", p.accent_bright),
        item("variable, variable.parameter, punctuation, meta.brace", p.text_dim),
    ];
    theme
}

/// syntect's per-run style → a ratatui `Style` (fg + bold/italic).
fn ratatui_style(st: &syntect::highlighting::Style) -> Style {
    let mut style = Style::default().fg(Color::Rgb(st.foreground.r, st.foreground.g, st.foreground.b));
    if st.font_style.contains(FontStyle::BOLD) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if st.font_style.contains(FontStyle::ITALIC) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    style
}

/// Highlight one source line into a per-character style vector (newline stripped).
fn per_char_styles(ranges: Vec<(syntect::highlighting::Style, &str)>) -> Vec<Style> {
    let mut out = Vec::new();
    for (st, text) in ranges {
        let style = ratatui_style(&st);
        for _ in text.trim_end_matches(['\n', '\r']).chars() {
            out.push(style);
        }
    }
    out
}

/// Highlight `job.code` on this thread and stream per-line styles back over `tx` as a
/// sequence of `SyntaxEvent::Chunk`s. The **visible window** (`0..=viewport_bottom`)
/// is published first so on-screen code colorizes almost immediately; the rest streams
/// below it in blocks. syntect must parse from line 0 (multi-line state carries), so a
/// deep scroll pays for the lines above it — but the render never blocks either way,
/// and a stale cache stays on screen until the fresh chunks arrive.
pub fn highlight_stream(job: crate::app::SyntaxJob, tx: Sender<crate::app::SyntaxEvent>) {
    use crate::app::SyntaxEvent;
    const CHUNK: usize = 512;
    // Bound worst-case memory/time; a pathologically huge file just renders plain.
    if job.code.lines().count() > 50_000 {
        let _ = tx.send(SyntaxEvent::Chunk {
            buf_id: job.buf_id, rev: job.rev, palette_id: job.palette_id,
            start_line: 0, styles: Vec::new(), complete: true,
        });
        return;
    }
    std::thread::spawn(move || {
        let ss = syntax_set();
        let syntax = ss
            .find_syntax_by_extension(&job.ext)
            .or_else(|| ss.find_syntax_by_first_line(job.code.lines().next().unwrap_or("")));
        let Some(syntax) = syntax else {
            // Unknown language → empty (plain) cache; clears the in-flight marker.
            let _ = tx.send(SyntaxEvent::Chunk {
                buf_id: job.buf_id, rev: job.rev, palette_id: job.palette_id,
                start_line: 0, styles: Vec::new(), complete: true,
            });
            return;
        };
        let theme = theme_for(&job.palette);
        let mut hl = HighlightLines::new(syntax, &theme);
        let vp = job.viewport_bottom.max(1);
        let mut batch: Vec<Vec<Style>> = Vec::new();
        let mut batch_start = 0usize;
        for line in LinesWithEndings::from(&job.code) {
            let styles = match hl.highlight_line(line, ss) {
                Ok(ranges) => per_char_styles(ranges),
                Err(_) => break, // give up cleanly; what shipped so far stays cached
            };
            batch.push(styles);
            let covered = batch_start + batch.len();
            // First flush covers the visible window; thereafter every CHUNK lines.
            let publish = (batch_start < vp && covered >= vp) || batch.len() >= CHUNK;
            if publish {
                let n = batch.len();
                let styles = std::mem::take(&mut batch);
                if tx.send(SyntaxEvent::Chunk {
                    buf_id: job.buf_id, rev: job.rev, palette_id: job.palette_id,
                    start_line: batch_start, styles, complete: false,
                }).is_err() {
                    return; // receiver gone (app shutting down)
                }
                batch_start += n;
            }
        }
        let _ = tx.send(SyntaxEvent::Chunk {
            buf_id: job.buf_id, rev: job.rev, palette_id: job.palette_id,
            start_line: batch_start, styles: batch, complete: true,
        });
    });
}

/// Highlight `code` as the language for file extension `ext`, returning one vector of
/// styled runs per source line. `None` when the language is unknown or highlighting
/// fails — the caller then renders plain, never an error.
pub fn highlight(code: &str, ext: &str, palette: &Palette) -> Option<Vec<Vec<(Style, String)>>> {
    let ss = syntax_set();
    let syntax = ss
        .find_syntax_by_extension(ext)
        .or_else(|| ss.find_syntax_by_first_line(code.lines().next().unwrap_or("")))?;
    let theme = theme_for(palette);
    let mut hl = HighlightLines::new(syntax, &theme);
    let mut out = Vec::new();
    for line in LinesWithEndings::from(code) {
        let ranges = hl.highlight_line(line, ss).ok()?;
        let spans = ranges
            .into_iter()
            .map(|(st, text)| {
                let mut style = Style::default()
                    .fg(Color::Rgb(st.foreground.r, st.foreground.g, st.foreground.b));
                if st.font_style.contains(FontStyle::BOLD) {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if st.font_style.contains(FontStyle::ITALIC) {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                (style, text.trim_end_matches(['\n', '\r']).to_string())
            })
            .collect();
        out.push(spans);
    }
    Some(out)
}
