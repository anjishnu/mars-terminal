//! Code syntax highlighting via syntect (pure-Rust `fancy-regex` — no C toolchain).
//!
//! Colors are synthesized from the active theme `Palette`, so a theme restyles code
//! by construction — a keyword is `accent`, a string is `success`, a comment is
//! `text-faint`, etc. Grammars are syntect's bundled set plus any `.sublime-syntax`
//! a user drops in `~/.mars/syntaxes/` (the runtime "language pack" seam — no
//! rebuild). Without the `syntax` cargo feature, `syntax_stub.rs` replaces this and
//! everything renders plain.

use std::str::FromStr;
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
fn syntax_set() -> &'static SyntaxSet {
    static SET: OnceLock<SyntaxSet> = OnceLock::new();
    SET.get_or_init(|| {
        let mut builder = SyntaxSet::load_defaults_newlines().into_builder();
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
