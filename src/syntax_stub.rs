//! Inert syntax-highlighting stub — compiled in place of `syntax.rs` when the
//! `syntax` feature is off. Everything renders plain; syntect isn't a dependency.
//! Callers never learn highlighting is unavailable (the deletion-proof seam).

use std::sync::mpsc::Sender;

use ratatui::style::Style;

use crate::tuning::Palette;

pub fn highlight(_code: &str, _ext: &str, _palette: &Palette) -> Option<Vec<Vec<(Style, String)>>> {
    None
}

/// No-op without the `syntax` feature: report an empty (plain) highlight so the
/// caller's in-flight marker clears; nothing ever colorizes.
pub fn highlight_stream(job: crate::app::SyntaxJob, tx: Sender<crate::app::SyntaxEvent>) {
    let _ = tx.send(crate::app::SyntaxEvent::Chunk {
        buf_id: job.buf_id, rev: job.rev, palette_id: job.palette_id,
        start_line: 0, styles: Vec::new(), complete: true,
    });
}
