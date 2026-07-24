//! Inert syntax-highlighting stub — compiled in place of `syntax.rs` when the
//! `syntax` feature is off. Everything renders plain; syntect isn't a dependency.
//! Callers never learn highlighting is unavailable (the deletion-proof seam).

use ratatui::style::Style;

use crate::tuning::Palette;

pub fn highlight(_code: &str, _ext: &str, _palette: &Palette) -> Option<Vec<Vec<(Style, String)>>> {
    None
}
