use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::{
    app::App,
    layout::PaneLayout,
    mode::Mode,
    palette::{Action, BarMode, ItemKind},
    pane::{PaneContent, PaneId},
};

/// Width of the "NNNN│ " line-number prefix when `line_numbers` is on.
pub const LINE_NUM_W: u16 = 6;
/// Width of the default gutter: a 1-char cursor-line pointer + 1 space.
pub const POINTER_W: u16 = 2;

/// Default is a slim pointer gutter (current-line marker only); the full
/// line-number gutter is opt-in (`line_numbers` knob). Either way the live
/// line/col lives in the status bar.
pub fn gutter_width(tuning: &crate::tuning::Tuning) -> u16 {
    if tuning.line_numbers { LINE_NUM_W } else { POINTER_W }
}

/// Whether the resolved palette is the default Mission Control look — the baked
/// terracotta banner matches only this. Any other theme (or a customized accent)
/// gets the plain block wordmark in its own accent instead.
fn is_default_theme(app: &App) -> bool {
    app.tuning.palette.accent == Color::Rgb(217, 119, 87)
}

/// The solid background color to paint, or `None` for a transparent (terminal-bg)
/// look. Solid only when `opaque_background` is on AND the theme commits to a real
/// surface color — so Mission Control (Reset surface) stays clear either way.
fn opaque_bg(app: &App) -> Option<Color> {
    (app.tuning.opaque_background != 0 && app.tuning.palette.surface != Color::Reset)
        .then_some(app.tuning.palette.surface)
}

/// Clear a rect for an overlay panel, then fill it with the theme surface when the
/// background is opaque — so Paper/Hacker popups are solid, not see-through to the
/// terminal's own background. Transparent themes just clear (unchanged behavior).
fn clear_panel(frame: &mut Frame, app: &App, rect: Rect) {
    frame.render_widget(Clear, rect);
    if let Some(bg) = opaque_bg(app) {
        frame.render_widget(Block::default().style(Style::default().bg(bg)), rect);
    }
}

/// Brighten an RGB theme color by `amt` per channel — for readable variants of dark
/// theme hues (e.g. the dark teal used for code, which is near-invisible on a dark bg).

// ── Entry point ──────────────────────────────────────────────────────────────

pub fn render(frame: &mut Frame, app: &mut App) {
    let area = frame.area();

    // Clickable chrome is recorded as it's drawn; the registry is only ever as
    // current as this frame.
    app.hits_clear();

    // Paint the whole frame in the theme's surface first, so a committed background
    // (Paper's cream, Hacker's black) is consistent everywhere — not just where a
    // widget happens to set a bg. Transparent for Mission Control / a Reset surface
    // (or when `opaque_background = 0`), which honors the terminal's own background.
    if let Some(bg) = opaque_bg(app) {
        frame.render_widget(Block::default().style(Style::default().bg(bg)), area);
    }

    // Layout: tab-bar (1) | pane area (min) | status (1) | control bar (1)
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // tab bar
            Constraint::Min(1),    // pane area
            Constraint::Length(1), // status bar
            Constraint::Length(1), // control bar
        ])
        .split(area);

    let (tab_area, full_pane_area, status_area, bar_area) =
        (chunks[0], chunks[1], chunks[2], chunks[3]);

    // Carve a left sidebar for the file tree when it's open; panes take the rest.
    let pane_area = if app.tree_open {
        let tw = app.tuning.tree_width.min(full_pane_area.width.saturating_sub(20));
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(tw), Constraint::Min(1)])
            .split(full_pane_area);
        render_file_tree(frame, app, cols[0]);
        cols[1]
    } else {
        full_pane_area
    };

    render_tab_bar(frame, app, tab_area);
    render_panes(frame, app, pane_area);
    // Session-start splash: the MARS banner overlays the workspace (terminal or
    // editor) until the first keypress dismisses it.
    if app.show_splash {
        render_splash(frame, app, pane_area);
    }
    // The shift report: the save-state restore overlays the workspace on
    // reattach; any key resumes. Suppresses notice noise while up.
    if app.shift_report.is_some() {
        render_shift_report(frame, app, pane_area);
    }
    // Proactive notice (W6): one dim line at the bottom of the workspace, the
    // agent's only path to the screen. Failures first; Esc dismisses.
    if !app.notices.is_empty() && !app.show_splash && app.shift_report.is_none() {
        render_notice(frame, app, pane_area);
    }
    render_status(frame, app, status_area);
    render_control_bar(frame, app, bar_area);

    // Bar dropdown / ask-panel drawn last so it sits on top (grows upward).
    if app.palette.is_some() && matches!(app.mode, Mode::Bar) {
        match app.palette.as_ref().map(|p| p.bar_mode.clone()) {
            Some(BarMode::Ask)     => render_ask_panel(frame, app, pane_area, bar_area),
            Some(BarMode::Command) => {
                let dropdown = render_bar_dropdown(frame, app, pane_area, bar_area);
                // In a terminal, the unified composer also shows the red inline
                // overlay at the cursor (type-in-place) — but the menu outranks
                // it: when the two would collide, the overlay stays hidden.
                if app.bar_return == Mode::Terminal {
                    render_shell_overlay(frame, app, pane_area, dropdown);
                }
            }
            // Shell: an inline composer anchored at the cursor (no eye-jump).
            Some(BarMode::Shell)   => render_shell_overlay(frame, app, pane_area, None),
            None => {}
        }
    }

    // which-key: after a short hesitation on a prefix, show the continuations.
    if !app.pending_prefix.is_empty()
        && app.frame_tick.saturating_sub(app.prefix_tick) >= app.tuning.which_key_delay_ticks()
    {
        render_which_key(frame, app, pane_area, status_area);
    }

    // C-t travel mode: always-on cheat panel — the characters tell you what to do.
    if matches!(app.mode, Mode::Tab) {
        render_travel_panel(frame, app, pane_area, status_area);
    }
}

// ── C-t travel panel ─────────────────────────────────────────────────────────

fn render_travel_panel(frame: &mut Frame, app: &App, pane_area: Rect, status_area: Rect) {
    let panel_width = app.tuning.travel_panel_width;
    let rows: &[(&str, &str)] = &[
        ("← → ↑ ↓",  "move focus  ·  pane → tab at the edges"),
        ("1-9",      "jump to tab"),
        ("@",        "go to the navigator (file tree)"),
        ("z / Spc",  "zoom (maximize)  ·  toggle"),
        ("d / ⌫",    "close focused  ·  pane, or tab if last"),
        ("",         ""),
        ("t / n",    "new tab"),
        ("T",        "new terminal tab"),
        ("| / -",    "split right / below"),
        ("⇧ ← →",    "reorder tab"),
        ("< >",      "resize pane"),
        ("x",        "swap pane"),
        ("r",        "rename tab"),
        ("",         ""),
        ("?",        "why did this fail? (triage)"),
        ("w",        "watch this pane (summarize when done)"),
        ("D",        "detach session (keeps running)"),
        ("Esc ⏎",    "done  ·  creation exits, navigation stays"),
    ];

    let mut lines: Vec<Line> = Vec::new();
    for (keys, what) in rows {
        if keys.is_empty() {
            lines.push(Line::from(Span::raw("")));
            continue;
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {:<9}", keys),
                Style::default().fg(app.tuning.palette.accent_bright).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {}", what), Style::default().fg(app.tuning.palette.text_dim)),
        ]));
    }

    let panel_h = (lines.len() as u16 + 2).min(pane_area.height.saturating_sub(1)); // + top/bottom border
    let width = panel_width.min(status_area.width);
    let rect = Rect {
        x: status_area.x + status_area.width.saturating_sub(width),
        y: status_area.y.saturating_sub(panel_h),
        width,
        height: panel_h,
    };
    clear_panel(frame, app, rect);
    // A full box with " WARP " on a neutral grey/white line — a calm, non-teal chrome.
    let block = Block::default()
        .title(Span::styled(" WARP ", Style::default().fg(app.tuning.palette.text).add_modifier(Modifier::BOLD)))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.tuning.palette.text_dim));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

// ── which-key panel ──────────────────────────────────────────────────────────

fn render_which_key(frame: &mut Frame, app: &App, pane_area: Rect, status_area: Rect) {
    let conts = app.keys.continuations(&app.pending_prefix);
    if conts.is_empty() {
        return;
    }
    let prefix = crate::config::render_chords(&app.pending_prefix);

    let mut lines: Vec<Line> = Vec::new();
    for (tail, action) in &conts {
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {:<4}", tail),
                Style::default()
                    .fg(app.tuning.palette.accent_bright)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {}", action.label()),
                Style::default().fg(app.tuning.palette.text),
            ),
        ]));
    }

    let panel_h = (lines.len() as u16 + 1).min(pane_area.height.saturating_sub(1)); // +1 border
    let width = app.tuning.which_key_panel_width.min(status_area.width);
    let rect = Rect {
        x: status_area.x + status_area.width.saturating_sub(width),
        y: status_area.y.saturating_sub(panel_h),
        width,
        height: panel_h,
    };
    clear_panel(frame, app, rect);
    let block = Block::default()
        .title(Span::styled(
            format!(" {} - ", prefix),
            Style::default()
                .fg(app.tuning.palette.accent_bright)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::TOP | Borders::LEFT)
        .border_style(Style::default().fg(app.tuning.palette.border));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

// ── Tab bar ──────────────────────────────────────────────────────────────────

/// The one place a surface verdict becomes a glyph + semantic color. Tab labels,
/// pane borders, and the workspace summary all read this, so a status means the
/// same thing everywhere it shows. Returns None for idle (Context) — each caller
/// picks its own recede (a readable label, a dim border, or nothing at all).
/// The status bubble color for a verdict. ONE shape everywhere — a filled dot in a
/// fixed position — with color the only varying dimension, so status always reads in
/// the same place: amber=blocked, red=failed, green=running, teal=done, grey=idle.
fn verdict_color(app: &App, v: crate::briefing::Verdict) -> Color {
    use crate::briefing::Verdict;
    match v {
        Verdict::Blocked => app.tuning.palette.warning, // amber
        Verdict::Failed  => app.tuning.palette.danger,  // red
        Verdict::Running => app.tuning.palette.success,        // green
        Verdict::Done    => app.tuning.palette.info,       // teal
        Verdict::Context => app.tuning.palette.text_dim,                          // idle
    }
}

/// A pane's display name for the top bar and the board: an editor's filename (with a
/// dirty dot), or a terminal's title / running command / "shell" — never a bare
/// "terminal". This is what gives editor panes and split terminals a real identity.
pub(crate) fn pane_name(app: &App, pane_id: PaneId) -> String {
    let Some(pane) = app.panes.get(&pane_id) else { return "—".to_string() };
    match pane.content {
        PaneContent::Editor(buf_id) => {
            let b = app.buffers.get(&buf_id);
            let name = b.map(|b| b.name.clone()).unwrap_or_else(|| "buffer".to_string());
            if b.map(|b| b.modified).unwrap_or(false) { format!("{name} ●") } else { name }
        }
        PaneContent::Terminal(tid) => {
            if let Some(t) = pane.title.as_deref() {
                return t.to_string();
            }
            if let Some(cmd) = app.watches.get(&tid).and_then(|w| w.last_command.as_ref()) {
                if let Some(w0) = cmd.split_whitespace().next() {
                    return w0.rsplit('/').next().unwrap_or(w0).to_string();
                }
            }
            "shell".to_string()
        }
    }
}

/// The informative name for a workspace (tab), given its 1-based number. A real
/// custom name wins; otherwise the focused pane names it — an editor by filename, a
/// terminal by its title or a consistent "terminal N" default (never a bare number,
/// and numbered the same way for every terminal so two shells read "terminal 1" /
/// "terminal 2").
pub(crate) fn workspace_name(app: &App, tab: &crate::tab::Tab, num: usize) -> String {
    if !tab.name.is_empty() && tab.name.parse::<usize>().is_err() {
        return tab.name.clone();
    }
    match app.panes.get(&tab.focused_pane).map(|p| &p.content) {
        Some(PaneContent::Editor(buf_id)) => {
            // Just the filename — no trailing dirty dot; the one status bubble before
            // the name is the only status marker, in a consistent position.
            app.buffers.get(buf_id).map(|b| b.name.clone()).unwrap_or_else(|| "buffer".to_string())
        }
        _ => app
            .panes
            .get(&tab.focused_pane)
            .and_then(|p| p.title.clone())
            .unwrap_or_else(|| format!("terminal {num}")),
    }
}

fn render_tab_bar(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();
    // Running x so each tab can register the cells it actually occupies.
    let mut x = area.x;
    for (i, tab) in app.tabs.iter().enumerate() {
        // Every tab: a status BUBBLE in the same position (colored by the worst-pane
        // verdict; grey = idle) then the name. Consistent shape + position, colour the
        // only varying dimension.
        let verdict = app.tab_status(tab);
        let bubble = verdict_color(app, verdict);
        let name = workspace_name(app, tab, i + 1);
        let target = crate::app::HitTarget::Row(crate::app::RowKind::Tab, i);
        let hot = app.is_hovered(&target);
        // A preview tab (the reusable navigator slot) is italic — VS Code's cue that
        // it's ephemeral and will be swapped out by the next click.
        let ital = if tab.preview { Modifier::ITALIC } else { Modifier::empty() };
        if i == app.active_tab {
            // The active tab is inverted chrome (you're looking at it); its bubble
            // recedes into the chip color, but stays in the same slot.
            let chip = app.tuning.palette.on_accent;
            let bg = if app.is_pressed(&target) { app.tuning.palette.accent_dark } else { app.tuning.palette.accent };
            spans.push(Span::styled(format!(" ● {name} "),
                Style::default().fg(chip).bg(bg).add_modifier(Modifier::BOLD | ital)));
        } else if hot {
            // Hover previews the ACTIVE look — filling the same `● name` cells the
            // active chip fills (dot included), just a lighter bg — so the hover band
            // and the switched-to band line up exactly (not a name-only highlight).
            spans.push(Span::styled(format!(" ● {name} "),
                Style::default().fg(app.tuning.palette.on_accent).bg(app.tuning.palette.accent_bright).add_modifier(Modifier::BOLD | ital)));
        } else {
            spans.push(Span::styled(" ● ", Style::default().fg(bubble)));
            spans.push(Span::styled(format!("{name} "),
                Style::default().fg(app.tuning.palette.text_dim).add_modifier(ital)));
        }
        spans.push(Span::styled("│", Style::default().fg(app.tuning.palette.text_faint)));
        // " ● " + name + " " — the same cells the spans above just claimed.
        let w = (3 + name.chars().count() + 1) as u16;
        if x < area.right() {
            let vis = w.min(area.right() - x);
            app.hit(Rect { x, y: area.y, width: vis, height: 1 }, target);
            // Hovering a tab surfaces that workspace's status (and its full name, so a
            // clipped label is legible too) in the status bar, rendered just after.
            if hot {
                *app.hover_tip.borrow_mut() = Some(format!("{name} · {}", verdict.label()));
            }
        }
        x = x.saturating_add(w + 1); // + the │ separator
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
    // (The top-right status counter/beacon was removed: it counted finished-Done
    // surfaces that never clear, so it silted up into a persistent, glyph-garbled
    // "✓N ●N" in the corner. Per-tab colors carry status until a better aggregate is
    // designed. Status lives in the tab labels and the WORKSPACES panel.)
}

// ── Pane layout ───────────────────────────────────────────────────────────────

fn compute_rects(layout: &PaneLayout, area: Rect) -> Vec<(PaneId, Rect)> {
    match layout {
        PaneLayout::Single(id) => vec![(*id, area)],
        PaneLayout::HSplit { top, bottom, ratio } => {
            let halves = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(*ratio), Constraint::Percentage(100 - *ratio)])
                .split(area);
            let mut v = compute_rects(top, halves[0]);
            v.extend(compute_rects(bottom, halves[1]));
            v
        }
        PaneLayout::VSplit { left, right, ratio } => {
            let halves = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(*ratio), Constraint::Percentage(100 - *ratio)])
                .split(area);
            let mut v = compute_rects(left, halves[0]);
            v.extend(compute_rects(right, halves[1]));
            v
        }
    }
}

/// Walk the layout the same way `compute_rects` does, collecting each split's
/// draggable boundary. `origin`/`span` are the parent area's extent along the
/// split axis, so a drag can turn a pointer position straight into a ratio.
/// Mirrors `compute_rects`' arithmetic — the two must stay in step, which is why
/// they sit next to each other.
fn compute_dividers(
    layout: &PaneLayout,
    area: Rect,
    path: &mut Vec<u8>,
    out: &mut Vec<(Rect, Vec<u8>, bool, u16, u16)>,
) {
    match layout {
        PaneLayout::Single(_) => {}
        PaneLayout::HSplit { top, bottom, ratio } => {
            let halves = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(*ratio), Constraint::Percentage(100 - *ratio)])
                .split(area);
            // The seam: the first child's last row and the second's first, which
            // is where both panes paint their own border.
            let y = halves[0].bottom().saturating_sub(1);
            out.push((
                Rect { x: area.x, y, width: area.width, height: 2.min(area.bottom().saturating_sub(y)) },
                path.clone(), false, area.y, area.height,
            ));
            path.push(0); compute_dividers(top, halves[0], path, out); path.pop();
            path.push(1); compute_dividers(bottom, halves[1], path, out); path.pop();
        }
        PaneLayout::VSplit { left, right, ratio } => {
            let halves = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(*ratio), Constraint::Percentage(100 - *ratio)])
                .split(area);
            let x = halves[0].right().saturating_sub(1);
            out.push((
                Rect { x, y: area.y, width: 2.min(area.right().saturating_sub(x)), height: area.height },
                path.clone(), true, area.x, area.width,
            ));
            path.push(0); compute_dividers(left, halves[0], path, out); path.pop();
            path.push(1); compute_dividers(right, halves[1], path, out); path.pop();
        }
    }
}

fn render_panes(frame: &mut Frame, app: &mut App, area: Rect) {
    let focused_id = app.focused_pane_id();

    // Zoom follows focus: moving focus away (or closing the pane) unzooms.
    {
        let tab = &mut app.tabs[app.active_tab];
        let stale = match tab.zoomed {
            Some(z) => z != focused_id || !tab.layout.pane_ids().contains(&z),
            None => false,
        };
        if stale {
            tab.zoomed = None;
        }
    }
    let rects: Vec<(PaneId, Rect)> = {
        let tab = &app.tabs[app.active_tab];
        match tab.zoomed {
            Some(z) => vec![(z, area)],
            None => compute_rects(&tab.layout, area),
        }
    };

    // Remember pane rects for mouse hit-testing.
    app.pane_rects = rects.clone();

    // Split boundaries are draggable. Registered BEFORE the panes paint, so the
    // pane-interior path (which registers nothing) can't shadow them, and a
    // zoomed pane — which has no visible seams — contributes none.
    if app.tabs[app.active_tab].zoomed.is_none() {
        let mut dividers = Vec::new();
        compute_dividers(&app.tabs[app.active_tab].layout, area, &mut Vec::new(), &mut dividers);
        for (rect, path, vertical, origin, span) in dividers {
            app.hit(rect, crate::app::HitTarget::Divider { path, vertical, origin, span });
        }
    }

    // Update scroll offsets now that we know the real viewport heights.
    let margin = app.tuning.scroll_margin;
    for (pane_id, rect) in &rects {
        let inner_h = rect.height.saturating_sub(2) as usize;
        if let Some(p) = app.panes.get_mut(pane_id) {
            p.view_h = inner_h;
            p.ensure_scroll(inner_h, margin);
        }
    }

    // Keep terminal PTYs sized to their panes' inner area — except a pane Rover has taken
    // over (mobile_reflow), which stays at the phone's width so a wide TUI reflows narrow.
    let mobile_reflow = app.mobile_reflow;
    for (pane_id, rect) in &rects {
        let tid = match app.panes.get(pane_id).map(|p| p.content.clone()) {
            Some(PaneContent::Terminal(id)) => Some(id),
            _ => None,
        };
        if let Some(tid) = tid {
            let iw = rect.width.saturating_sub(2);
            let ih = rect.height.saturating_sub(2);
            if let Some(t) = app.terms.get_mut(&tid) {
                match mobile_reflow {
                    Some((mp, mrows, mcols)) if mp == *pane_id => t.resize(mrows, mcols),
                    _ => t.resize(ih, iw),
                }
            }
        }
    }

    let bar_open = app.palette.is_some() && matches!(app.mode, Mode::Bar);
    let mut cursor_screen: Option<(u16, u16)> = None;
    for (pane_id, rect) in &rects {
        let is_focused = *pane_id == focused_id;
        if let Some(pos) = render_pane(frame, app, *pane_id, *rect, is_focused) {
            cursor_screen = Some(pos);
        }
    }
    app.cursor_screen = cursor_screen; // anchors the shell-translate overlay

    // Place the terminal cursor — but not while the bar owns it.
    if !bar_open {
        if let Some((cx, cy)) = cursor_screen {
            if matches!(app.mode, Mode::Edit | Mode::Terminal) {
                frame.set_cursor_position((cx, cy));
            }
        }
    }
}

fn render_pane(
    frame: &mut Frame,
    app: &App,
    pane_id: PaneId,
    rect: Rect,
    focused: bool,
) -> Option<(u16, u16)> {
    let pane = app.panes.get(&pane_id)?;

    match pane.content.clone() {
        PaneContent::Editor(buf_id) => render_editor_pane(frame, app, pane_id, buf_id, rect, focused),
        PaneContent::Terminal(_term_id) => render_terminal_pane(frame, app, pane_id, rect, focused),
    }
}

fn render_editor_pane(
    frame: &mut Frame,
    app: &App,
    pane_id: PaneId,
    buf_id: usize,
    rect: Rect,
    focused: bool,
) -> Option<(u16, u16)> {
    let pane = app.panes.get(&pane_id)?;
    let buf  = app.buffers.get(&buf_id)?;

    if pane.md_view {
        return render_markdown_pane(frame, app, pane, buf, rect, focused);
    }

    let border_style = if focused {
        Style::default().fg(app.tuning.palette.accent)
    } else {
        Style::default().fg(app.tuning.palette.border)
    };
    let title_mod = if focused { Modifier::BOLD } else { Modifier::empty() };
    let marker    = if buf.modified { " ●" } else { "" };
    let shown     = pane.title.as_deref().unwrap_or(&buf.name);
    let title     = format!(" {}{} ", shown, marker);

    let block = Block::default()
        .title(Span::styled(title, Style::default().add_modifier(title_mod)))
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let vp_h = inner.height as usize;
    let line_count = buf.line_count();
    let mut lines: Vec<Line> = Vec::with_capacity(vp_h);

    // Ordered selection range (start ≤ end) for highlighting.
    let sel: Option<((usize, usize), (usize, usize))> = pane.selection_anchor.map(|a| {
        let c = (pane.cursor_row, pane.cursor_col);
        if a <= c { (a, c) } else { (c, a) }
    });
    // Teleport labels: high-contrast chip (accent bg, dark fg, bold).
    let label_style = Style::default()
        .bg(app.tuning.palette.accent_bright)
        .fg(app.tuning.palette.on_accent)
        .add_modifier(Modifier::BOLD);

    let numbers = app.tuning.line_numbers;
    // Passive matched-bracket pair (row, col) for both ends, computed once.
    let bracket = if focused { app.bracket_pair() } else { None };
    // Syntax highlighting reads a per-buffer cache populated off-thread by the
    // background worker (`App::request_highlight`). Never computes here — the render
    // path stays O(visible cells). The cache may be stale (a keystroke ahead) or
    // partial (still streaming); the per-cell lookup below clamps, so missing lines
    // and characters simply render plain until the fresh pass tiles in.
    let syntax_styles: Option<&Vec<Vec<Style>>> = if app.syntax_on {
        app.syntax_cache.get(&buf_id).map(|c| &c.lines)
    } else { None };
    for row_off in 0..vp_h {
        let row = pane.scroll_row + row_off;
        if row >= line_count {
            // Blank gutter beyond end-of-buffer.
            let blank = " ".repeat(gutter_width(&app.tuning) as usize);
            lines.push(Line::from(Span::styled(blank, Style::default().fg(app.tuning.palette.text_faint))));
        } else {
            let content = buf.line_str(row);
            let on_cursor = focused && row == pane.cursor_row;
            // Current-line tint: a subtle bg on the cursor's row (selection/search
            // backgrounds still win where they sit).
            let line_bg = if on_cursor && app.tuning.highlight_current_line == 1 {
                Some(app.tuning.palette.current_line)
            } else { None };
            let with_bg = |st: Style| -> Style { if let Some(bg) = line_bg { st.bg(bg) } else { st } };

            let mut spans = Vec::new();
            if numbers {
                let num_style = if on_cursor {
                    Style::default().fg(app.tuning.palette.accent_bright).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(app.tuning.palette.text_faint)
                };
                spans.push(Span::styled(format!("{:>4}│ ", row + 1), with_bg(num_style)));
            } else {
                // Slim pointer gutter: a marker on the cursor line, else blank.
                let (glyph, style) = if on_cursor {
                    ("▸ ", Style::default().fg(app.tuning.palette.accent).add_modifier(Modifier::BOLD))
                } else {
                    ("  ", Style::default())
                };
                spans.push(Span::styled(glyph, with_bg(style)));
            }
            let chars: Vec<char> = content.chars().collect();

            // Per-char highlight map: 0 none, 1 selection, 2 isearch, 3 label, 4 bracket.
            let mut hl: Vec<u8> = vec![0; chars.len()];
            if let Some(((sr, sc), (er, ec))) = sel {
                if row >= sr && row <= er {
                    let start = (if row == sr { sc } else { 0 }).min(chars.len());
                    let end = (if row == er { ec } else { chars.len() }).min(chars.len());
                    for h in hl.iter_mut().take(end).skip(start) { *h = 1; }
                }
            }
            let mut label_ch: Vec<Option<char>> = vec![None; chars.len()];
            if focused {
                for &(hr, hc, hlen) in &app.search_hl {
                    if hr == row {
                        let end = (hc + hlen).min(chars.len());
                        for h in hl.iter_mut().take(end).skip(hc.min(chars.len())) { *h = 2; }
                    }
                }
                // Teleport labels (kind 3) overwrite the first cell of each match.
                if app.search_pick {
                    for &(lr, lc, ch) in &app.search_labels {
                        if lr == row && lc < chars.len() {
                            hl[lc] = 3;
                            label_ch[lc] = Some(ch);
                        }
                    }
                }
            }
            // Bracket pair (kind 4) only where nothing stronger already sits.
            if let Some((a, b)) = bracket {
                for &(br, bc) in &[a, b] {
                    if br == row && bc < chars.len() && hl[bc] == 0 { hl[bc] = 4; }
                }
            }

            // Resolve each cell: the syntax foreground (or default) is the base, then
            // the bg layers by kind — selection/search win on background but keep the
            // syntax colour; a matched bracket takes the accent. Runs coalesce by
            // equal resolved style, so a plain line is still one span.
            let syn: Option<&Vec<Style>> = syntax_styles.and_then(|ls| ls.get(row));
            let cell_style = |i: usize| -> Style {
                // A character typed past the cached line length (Tab, mid-line insert)
                // holds its line's trailing color rather than flashing white — the
                // fresh highlight recolors it a moment later.
                let base = syn
                    .and_then(|cs| cs.get(i).copied().or_else(|| cs.last().copied()))
                    .unwrap_or_default();
                match hl[i] {
                    1 => base.bg(app.tuning.palette.selection_bg),
                    2 => base.bg(app.tuning.palette.search_bg),
                    4 => with_bg(Style::default().fg(app.tuning.palette.accent).add_modifier(Modifier::BOLD)),
                    _ => with_bg(base),
                }
            };
            let mut i = 0;
            while i < chars.len() {
                if hl[i] == 3 {
                    let ch = label_ch[i].unwrap_or(chars[i]);
                    spans.push(Span::styled(ch.to_string(), label_style));
                    i += 1;
                    continue;
                }
                let st = cell_style(i);
                let mut j = i + 1;
                while j < chars.len() && hl[j] != 3 && cell_style(j) == st { j += 1; }
                let text: String = chars[i..j].iter().collect();
                spans.push(Span::styled(text, st));
                i = j;
            }
            // Extend the tint across the whole row.
            if let Some(bg) = line_bg {
                let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                if used < inner.width as usize {
                    spans.push(Span::styled(" ".repeat(inner.width as usize - used), Style::default().bg(bg)));
                }
            }
            lines.push(Line::from(spans));
        }
    }

    frame.render_widget(Paragraph::new(Text::from(lines)), inner);

    if focused {
        let sy = inner.y + (pane.cursor_row.saturating_sub(pane.scroll_row)) as u16;
        let sx = inner.x + gutter_width(&app.tuning) + pane.cursor_col as u16;
        if sy < inner.y + inner.height && sx < inner.x + inner.width {
            return Some((sx, sy));
        }
    }
    None
}

// ── Markdown view (read-only rendered) ────────────────────────────────────────

/// A termimad skin dressed in MARS's palette, in a clear three-tier hierarchy on a
/// dark ground: **clay** (the brand terracotta) is primary — headings and bold;
/// **sandstone** (light accent) is secondary — italic and bullets; a **lightened**
/// teal is tertiary — code and table rules. Never the raw dark teal (invisible on a
/// dark background). termimad rides crossterm 0.29, so colors use `termimad::crossterm`.
fn mars_md_skin(app: &App) -> termimad::MadSkin {
    use termimad::crossterm::style::Color as TColor;
    let ct = |c: [u8; 3]| TColor::Rgb { r: c[0], g: c[1], b: c[2] };
    let lite = |c: [u8; 3], a: u8| TColor::Rgb {
        r: c[0].saturating_add(a), g: c[1].saturating_add(a), b: c[2].saturating_add(a),
    };
    let clay = ct(crate::themes::rgb_of(app.tuning.palette.accent));        // primary
    let sandstone = ct(crate::themes::rgb_of(app.tuning.palette.accent_bright)); // secondary
    let light_teal = lite(crate::themes::rgb_of(app.tuning.palette.info), 110); // tertiary (never raw dark teal)

    let mut s = termimad::MadSkin::default();
    s.set_headers_fg(clay);
    s.bold.set_fg(clay);
    s.italic.set_fg(sandstone);
    s.bullet.set_fg(sandstone);
    s.inline_code.set_fg(light_teal);
    s.code_block.compound_style.set_fg(light_teal);
    s.quote_mark.set_fg(light_teal);
    s.table.set_fg(light_teal);
    s
}

/// Prototype: render the buffer's Markdown with termimad (reflow, tables, wrapping),
/// windowed by `md_scroll`. termimad renders to ANSI; we re-parse it with vt100 into
/// a cell grid and convert to ratatui spans, so its full output lands in our frame.
fn render_markdown_termimad(frame: &mut Frame, app: &App, buf: &crate::buffer::Buffer, pane: &crate::pane::Pane, inner: Rect) {
    if inner.width == 0 || inner.height == 0 { return; }
    // Reading-mode: cap the reflow width and center the column, so wide panes don't
    // stretch prose to unreadable line lengths. 0 = use the full pane width.
    let cap = app.tuning.reading_width as u16;
    let width = if cap > 0 { inner.width.min(cap) } else { inner.width };
    let md_scroll = pane.md_scroll;
    let mut text = String::with_capacity(buf.line_count() * 40);
    for i in 0..buf.line_count() {
        if i > 0 { text.push('\n'); }
        text.push_str(&buf.line_str(i));
    }
    let skin = mars_md_skin(app);
    let fmt = skin.text(&text, Some(width as usize));
    let total = fmt.lines.len().max(1);
    // Record the true rendered length so the scroll handler clamps exactly (no
    // running off into a blank void) and the title can show a position %.
    pane.md_rendered_total.set(total);
    // termimad emits bare `\n` (relying on the terminal's ONLCR); vt100 has none, so
    // each line would inherit the previous line's end column. Force CRLF so every
    // line starts at column 0. +headroom rows so the trailing newline can't scroll.
    let ansi = fmt.to_string().replace('\n', "\r\n");
    let mut parser = vt100::Parser::new((total + 8) as u16, width, 0);
    parser.process(ansi.as_bytes());
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    let vw = cols.min(width);

    let start = md_scroll.min(total.saturating_sub(1));
    let mut lines: Vec<Line> = Vec::with_capacity(inner.height as usize);
    for r in 0..inner.height as usize {
        let row = (start + r) as u16;
        if row >= rows { lines.push(Line::from(Span::raw(""))); continue; }
        let mut spans: Vec<Span> = Vec::new();
        let mut run = String::new();
        let mut run_style: Option<Style> = None;
        for col in 0..vw {
            let (ch, style) = match screen.cell(row, col) {
                Some(cell) => {
                    let c = cell.contents();
                    let ch = if c.is_empty() { " ".to_string() } else { c };
                    let mut st = Style::default().fg(conv_fg(app, cell.fgcolor())).bg(conv_bg(app, cell.bgcolor()));
                    if cell.bold() { st = st.add_modifier(Modifier::BOLD); }
                    if cell.italic() { st = st.add_modifier(Modifier::ITALIC); }
                    if cell.underline() { st = st.add_modifier(Modifier::UNDERLINED); }
                    (ch, st)
                }
                None => (" ".to_string(), Style::default()),
            };
            if run_style == Some(style) {
                run.push_str(&ch);
            } else {
                if let Some(s) = run_style { spans.push(Span::styled(std::mem::take(&mut run), s)); }
                run = ch;
                run_style = Some(style);
            }
        }
        if let Some(s) = run_style { spans.push(Span::styled(run, s)); }
        lines.push(Line::from(spans));
    }
    // Center the (possibly narrower) reading column within the pane.
    let x_off = (inner.width - width) / 2;
    let text_rect = Rect { x: inner.x + x_off, y: inner.y, width, height: inner.height };
    frame.render_widget(Paragraph::new(Text::from(lines)), text_rect);
}

fn render_markdown_pane(
    frame: &mut Frame,
    app: &App,
    pane: &crate::pane::Pane,
    buf: &crate::buffer::Buffer,
    rect: Rect,
    focused: bool,
) -> Option<(u16, u16)> {
    let border_style = if focused {
        Style::default().fg(app.tuning.palette.accent)
    } else {
        Style::default().fg(app.tuning.palette.border)
    };
    let title_mod = if focused { Modifier::BOLD } else { Modifier::empty() };
    let shown = pane.title.as_deref().unwrap_or(&buf.name);
    // Position %: how far through the reflowed document. Reads the last frame's
    // measured total — a static doc never changes it.
    let pos = {
        let total = pane.md_rendered_total.get();
        let vh = pane.view_h.max(1);
        if total > vh {
            let cap = total - vh;
            let pct = (pane.md_scroll.min(cap) * 100) / cap.max(1);
            format!(" · {pct}%")
        } else { String::new() }
    };
    let title = format!(" {} — markdown{pos} ", shown);

    let block = Block::default()
        .title(Span::styled(title, Style::default().add_modifier(title_mod)))
        .borders(Borders::ALL)
        .border_style(border_style);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    // Reflow the whole buffer into a rendered document, windowed by the document
    // scroll. No cursor (columns don't map after reflow), so no on-screen cursor.
    render_markdown_termimad(frame, app, buf, pane, inner);
    None
}

// ── Splash (day-0 banner) ─────────────────────────────────────────────────────

/// Parse a truecolor-ANSI line (`\x1b[38;2;r;g;bm` fg + `\x1b[0m` reset) into a
/// ratatui `Line`, also returning its *visible* width (escapes excluded).
fn ansi_to_line(raw: &str) -> (Line<'static>, u16) {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut style = Style::default();
    let mut buf = String::new();
    let mut width: u16 = 0;
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if !buf.is_empty() {
                spans.push(Span::styled(std::mem::take(&mut buf), style));
            }
            if chars.peek() == Some(&'[') {
                chars.next();
            }
            let mut code = String::new();
            for nc in chars.by_ref() {
                if nc == 'm' {
                    break;
                }
                code.push(nc);
            }
            if code == "0" || code.is_empty() {
                style = Style::default();
            } else if let Some(rest) = code.strip_prefix("38;2;") {
                let p: Vec<u8> = rest.split(';').filter_map(|x| x.parse().ok()).collect();
                if p.len() == 3 {
                    style = Style::default().fg(Color::Rgb(p[0], p[1], p[2]));
                }
            }
        } else {
            buf.push(c);
            width += 1;
        }
    }
    if !buf.is_empty() {
        spans.push(Span::styled(buf, style));
    }
    (Line::from(spans), width)
}

fn render_splash(frame: &mut Frame, app: &App, inner: Rect) {
    let t = &app.tuning;
    // Overlay: wipe whatever's underneath (terminal shell or empty editor).
    clear_panel(frame, app, inner);

    // Parse the rich ANSI banner; fall back to a plain wordmark when narrow.
    let parsed: Vec<(Line, u16)> = crate::banner::BANNER_LINES
        .iter()
        .map(|l| ansi_to_line(l))
        .collect();
    let banner_w = parsed.iter().map(|(_, w)| *w).max().unwrap_or(0);
    let big = inner.width >= banner_w && inner.height >= (parsed.len() as u16 + 7);

    let mut lines: Vec<Line> = Vec::new();
    if big && is_default_theme(app) {
        // The baked terracotta banner — the default look only.
        let pad = (inner.width.saturating_sub(banner_w) / 2) as usize;
        for (line, _) in parsed {
            let mut spans = vec![Span::raw(" ".repeat(pad))];
            spans.extend(line.spans);
            lines.push(Line::from(spans));
        }
        lines.push(Line::raw(""));
    } else if big {
        // Any other theme: the plain block wordmark in the theme's accent.
        let style = Style::default().fg(t.palette.accent).add_modifier(Modifier::BOLD);
        let w = crate::banner::MARS_BLOCK.iter().map(|r| r.chars().count()).max().unwrap_or(0) as u16;
        let pad = " ".repeat((inner.width.saturating_sub(w) / 2) as usize);
        for row in crate::banner::MARS_BLOCK {
            lines.push(Line::from(vec![Span::raw(pad.clone()), Span::styled((*row).to_string(), style)]));
        }
        lines.push(Line::from(Span::styled(
            "mission control for your terminal",
            Style::default().fg(t.palette.accent_bright).add_modifier(Modifier::ITALIC),
        )).centered());
        lines.push(Line::raw(""));
    } else {
        lines.push(Line::from(Span::styled(
            "M A R S",
            Style::default().fg(t.palette.accent).add_modifier(Modifier::BOLD),
        )).centered());
        lines.push(Line::from(Span::styled(
            "mission control for your terminal",
            Style::default().fg(t.palette.accent_bright).add_modifier(Modifier::ITALIC),
        )).centered());
        lines.push(Line::raw(""));
    }

    // Key commands — rendered as one aligned block (keys right-justified into a
    // column, descriptions left-aligned), the whole block centered. Per-line
    // centering made these look ragged; a single left pad keeps the columns true.
    let cmds: &[(&str, &str)] = &[
        ("C-Space", "mission control — search actions · ! shell · ? ask the agent"),
        ("C-x C-f", "navigator — browse & jump to any project file"),
        ("C-t",     "space warp — tabs, panes, splits, open terminal"),
        ("C-u",     "time-travel — scrub back through undo history"),
        ("C-x C-d", "detach — work keeps running while you're gone"),
        ("C-g",     "cancel anything"),
    ];
    let keyw = cmds.iter().map(|(k, _)| k.chars().count()).max().unwrap_or(0);
    let block_w = cmds
        .iter()
        .map(|(_, d)| keyw + 3 + d.chars().count())
        .max()
        .unwrap_or(0) as u16;
    let lpad = " ".repeat((inner.width.saturating_sub(block_w) / 2) as usize);
    let key_style = Style::default().fg(t.palette.accent_bright).add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(app.tuning.palette.text_dim);
    for (k, d) in cmds {
        lines.push(Line::from(vec![
            Span::raw(lpad.clone()),
            Span::styled(format!("{k:>keyw$}"), key_style),
            Span::raw("   "),
            Span::styled(d.to_string(), desc_style),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "or just start typing",
        Style::default().fg(app.tuning.palette.text_dim).add_modifier(Modifier::ITALIC),
    )).centered());

    // Vertically center the banner block.
    let block_h = lines.len() as u16;
    let top_pad = inner.height.saturating_sub(block_h) / 2;
    let area = Rect {
        x: inner.x,
        y: inner.y + top_pad,
        width: inner.width,
        height: block_h.min(inner.height),
    };
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

/// Greedy word-wrap to `width` columns (char-count approximate; ASCII-dominant
/// terminal text). Overlong words are hard-split.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if line.is_empty() {
            line.push_str(word);
        } else if line.chars().count() + 1 + word.chars().count() <= width {
            line.push(' ');
            line.push_str(word);
        } else {
            out.push(std::mem::take(&mut line));
            line.push_str(word);
        }
        while line.chars().count() > width {
            let head: String = line.chars().take(width).collect();
            out.push(head);
            line = line.chars().skip(width).collect();
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}

/// The shift report — the save-state restore. The MARS wordmark up top (centered),
/// then a plain-English persona-voiced situation briefing (the star — it streams
/// in, justified within a centered measure), then a compact glyph manifest of the
/// workstreams. Splash pattern: Clear + one centered Paragraph. Any key resumes.
fn render_shift_report(frame: &mut Frame, app: &App, inner: Rect) {
    let Some(rep) = app.shift_report.as_ref() else { return };
    clear_panel(frame, app, inner);
    let accent = app.tuning.palette.accent;
    let bright = app.tuning.palette.accent_bright;
    let teal = app.tuning.palette.info;
    let green = app.tuning.palette.success;
    let dim = Style::default().fg(app.tuning.palette.text_faint);
    let white = Style::default().fg(app.tuning.palette.text);
    let cw = inner.width as usize;
    // The reading measure: one centered column the prose and manifest share, so
    // every element hangs off the same axis down the middle of the screen.
    let bw = (cw.saturating_sub(8)).clamp(24, 64);
    let block_pad = " ".repeat(cw.saturating_sub(bw) / 2);
    // Prepend padding to a span list so its visible width centers in the page.
    let centered = |spans: Vec<Span<'static>>, vis_len: usize| -> Line<'static> {
        let pad = " ".repeat(cw.saturating_sub(vis_len) / 2);
        let mut v = vec![Span::raw(pad)];
        v.extend(spans);
        Line::from(v)
    };
    let center1 = |s: String, style: Style| -> Line<'static> {
        let len = s.chars().count();
        centered(vec![Span::styled(s, style)], len)
    };

    // The boot-up reveal: elements come online over ~0.5s, worst-news-first.
    let animate = app.tuning.mission_briefing_animate == 1;
    let elapsed = if animate { rep.shown_at.elapsed().as_millis() } else { u128::MAX };
    let rev = crate::briefing::reveal_at(elapsed, rep.rows.len());

    let mut lines: Vec<Line> = Vec::new();
    // The MARS wordmark (instant — the console's always-on identity), centered as
    // a block. On the default look, the baked terracotta banner; under any other
    // theme, the plain block wordmark painted in the theme's accent so it matches.
    let logo_rows = &crate::banner::BANNER_LINES[1..=9];
    if inner.height as usize > rep.rows.len() + logo_rows.len() + 14 {
        if is_default_theme(app) {
            let logo: Vec<(Line, u16)> = logo_rows.iter().map(|r| ansi_to_line(r)).collect();
            let logo_w = logo.iter().map(|(_, wd)| *wd as usize).max().unwrap_or(0);
            let logo_pad = " ".repeat(cw.saturating_sub(logo_w) / 2);
            for (line, _) in logo {
                let mut spans = vec![Span::raw(logo_pad.clone())];
                spans.extend(line.spans);
                lines.push(Line::from(spans));
            }
        } else {
            let style = Style::default().fg(app.tuning.palette.accent).add_modifier(Modifier::BOLD);
            let w = crate::banner::MARS_BLOCK.iter().map(|r| r.chars().count()).max().unwrap_or(0);
            let pad = " ".repeat(cw.saturating_sub(w) / 2);
            for row in crate::banner::MARS_BLOCK {
                lines.push(Line::from(vec![Span::raw(pad.clone()), Span::styled((*row).to_string(), style)]));
            }
        }
        lines.push(Line::from(""));
    }
    // Caption: MISSION BRIEFING · T+HH:MM:SS mission clock · status ribbon.
    let (nf, nb, nd, nr) = (
        rep.rows.iter().filter(|r| r.verdict == crate::briefing::Verdict::Failed).count(),
        rep.rows.iter().filter(|r| r.verdict == crate::briefing::Verdict::Blocked).count(),
        rep.rows.iter().filter(|r| r.verdict == crate::briefing::Verdict::Done).count(),
        rep.rows.iter().filter(|r| r.verdict == crate::briefing::Verdict::Running).count(),
    );
    let ribbon = if rep.rows.is_empty() {
        " · all quiet".to_string()
    } else {
        let mut parts = Vec::new();
        if nf > 0 { parts.push(format!("✗{nf}")); }
        if nb > 0 { parts.push(format!("⏸{nb}")); }
        if nd > 0 { parts.push(format!("✓{nd}")); }
        if nr > 0 { parts.push(format!("●{nr}")); }
        format!(" · {}", parts.join(" "))
    };
    let title = "MISSION BRIEFING";
    let caption = format!("   T+ {}{ribbon}", crate::briefing::fmt_clock(rep.away_secs));
    let cap_len = title.chars().count() + caption.chars().count();
    lines.push(centered(
        vec![
            Span::styled(title.to_string(), Style::default().fg(accent).add_modifier(Modifier::BOLD)),
            Span::styled(caption, dim),
        ],
        cap_len,
    ));
    if let Some(m) = &rep.mission {
        lines.push(center1(format!("mission: {m}"), dim));
    }
    lines.push(Line::from(""));

    // The briefing prose. The model emits four blocks — greeting, summary, action
    // items, sign-off — and the first three fill a FIXED vessel above the manifest
    // (the sign-off is held for below it, the peak-end beat). Three moves make the
    // fill feel deliberate rather than jittery:
    //   · while the call is in flight, a mission-control word flashes in the slot
    //     the greeting will take — so the prose never visibly swaps a backup stub;
    //   · the model text is revealed at a steady rate behind a cursor (a typewriter),
    //     not in the ragged bursts the network delivers;
    //   · the prose region is padded to a reserved height, so it is a shaped vessel
    //     the text fills top-down and nothing below it shifts as more arrives.
    let type_ms = app.tuning.mission_briefing_type_ms.max(1) as u128;
    let prose_rows = (app.tuning.mission_briefing_prose_rows as usize)
        .min((inner.height as usize).saturating_sub(6))
        .max(2);
    let loading = animate && rep.narrative_streaming && !rep.narrative_from_model;
    let target_len = rep.narrative.chars().count();
    // How many chars of the model text have been revealed: everything, unless we're
    // animating an in-flight model stream, in which case it advances on the clock.
    let show_n = if !animate || !rep.narrative_from_model {
        target_len
    } else {
        rep.stream_started_at
            .map(|s| (s.elapsed().as_millis() / type_ms) as usize)
            .unwrap_or(0)
            .min(target_len)
    };
    let typing = animate && rep.narrative_from_model && (show_n < target_len || rep.narrative_streaming);
    let shown: String = rep.narrative.chars().take(show_n).collect();

    let mut prose: Vec<Line> = Vec::new();
    let mut signoff: Option<String> = None;
    if loading {
        let idx = (rep.shown_at.elapsed().as_millis() / crate::briefing::LOADING_FLASH_MS) as usize
            % crate::briefing::BRIEF_LOADING.len();
        // Anchored at the block's left edge — where the greeting's first letter
        // will land — so the swap to real text has no lateral jump.
        prose.push(Line::from(Span::styled(
            format!("{block_pad}{}…", crate::briefing::BRIEF_LOADING[idx]),
            Style::default().fg(accent).add_modifier(Modifier::ITALIC),
        )));
    } else {
        let cursor = if typing { "▏" } else { "" };
        let full = format!("{shown}{cursor}");
        let paras: Vec<&str> = full.split("\n\n").map(str::trim).filter(|p| !p.is_empty()).collect();
        let npar = paras.len();
        let has_signoff = npar >= 4;
        let above = if has_signoff { &paras[..npar - 1] } else { &paras[..] };
        signoff = has_signoff.then(|| paras[npar - 1].to_string());
        let above_n = above.len();
        for (pi, para) in above.iter().enumerate() {
            let style = if pi == 0 {
                Style::default().fg(accent).add_modifier(Modifier::BOLD) // greeting
            } else if above_n >= 3 && pi == above_n - 1 {
                Style::default().fg(bright).add_modifier(Modifier::BOLD) // action items
            } else {
                white // the summary
            };
            // Every prose line is anchored at the block's left edge and left ragged
            // — no word is ever moved to justify it, so nothing shifts as the
            // typewriter advances.
            for l in wrap(para, bw) {
                prose.push(Line::from(Span::styled(format!("{block_pad}{l}"), style)));
            }
            prose.push(Line::from(""));
        }
        // A quiet return still feels intentional: one dim radar line under the prose.
        if rep.rows.is_empty() {
            prose.push(center1("·   ·   ◜   ·   ·".to_string(), dim));
            prose.push(Line::from(""));
        }
    }
    // Pad the prose to its reserved height so the vessel is a fixed shape (only when
    // animating; instant mode packs tight). Overflow just grows the vessel.
    if animate {
        while prose.len() < prose_rows {
            prose.push(Line::from(""));
        }
    }
    lines.extend(prose);
    // Everything above here is a fixed height across frames; the manifest and
    // sign-off below reveal into reserved space without moving it.
    let fixed_head = lines.len();
    let manifest_full: usize = if rep.rows.is_empty() {
        0
    } else {
        1 + rep
            .rows
            .iter()
            .map(|r| {
                let needsyou = matches!(
                    r.verdict,
                    crate::briefing::Verdict::Failed | crate::briefing::Verdict::Blocked
                );
                let has_detail =
                    needsyou && (r.cwd.is_some() || r.exit.is_some() || r.error_excerpt.is_some());
                1 + usize::from(has_detail)
            })
            .sum::<usize>()
    };
    let signoff_full = if rep.rows.is_empty() { 2 } else { 6 };

    // The manifest as a systems board, hung off the same centered measure: a left
    // severity stripe, needs-you rows bright and concluded ones receding. Rows
    // cascade in, failures first. Wins render in teal — never the danger hue.
    if !rep.rows.is_empty() && rev.rows > 0 {
        lines.push(Line::from(Span::styled(format!("{block_pad}{}", "─".repeat(bw)), dim)));
    }
    for r in rep.rows.iter().take(rev.rows) {
        let needsyou = matches!(r.verdict, crate::briefing::Verdict::Failed | crate::briefing::Verdict::Blocked);
        let goodnews = r.verdict == crate::briefing::Verdict::Done
            && r.dur_secs.map(|d| d > crate::briefing::GOODNEWS_SECS).unwrap_or(false);
        // Danger keeps the warm hues; wins (done/running, and the good-news ★) are
        // always teal — a success never wears the failure colour.
        let hue = match r.verdict {
            crate::briefing::Verdict::Failed => bright,
            crate::briefing::Verdict::Blocked => accent,
            crate::briefing::Verdict::Running => green, // healthy work — calm, dismissible
            crate::briefing::Verdict::Done => teal,     // the win keeps its teal

            _ => app.tuning.palette.text_faint,
        };
        let body_style = if needsyou || goodnews { white } else { dim };
        let glyph = if goodnews { "★" } else { r.verdict.glyph() };
        let tab = if r.tab.is_empty() { String::new() } else { format!("[{}] ", r.tab) };
        let mut meta = Vec::new();
        if let Some(d) = r.dur_secs.filter(|d| *d > 0) {
            meta.push(format!("ran {}", crate::briefing::fmt_secs(d)));
        }
        if let Some(a) = r.ago_secs.filter(|a| *a > 0) {
            meta.push(format!("{} ago", crate::briefing::fmt_secs(a)));
        }
        let meta = if meta.is_empty() { String::new() } else { format!("  ({})", meta.join(", ")) };
        let body: String = format!("{tab}{}{meta}", r.text).chars().take(bw).collect();
        lines.push(Line::from(vec![
            Span::raw(block_pad.clone()),
            Span::styled("▎ ".to_string(), Style::default().fg(hue)),
            Span::styled(format!("{glyph} "), Style::default().fg(hue).add_modifier(Modifier::BOLD)),
            Span::styled(body, body_style),
        ]));
        // The "why" under failed/blocked rows: cwd · exit · first error line.
        if needsyou {
            let mut detail = Vec::new();
            if let Some(c) = &r.cwd { detail.push(c.clone()); }
            if let Some(x) = r.exit { detail.push(format!("exit {x}")); }
            if let Some(e) = &r.error_excerpt {
                if let Some(first) = e.lines().find(|l| !l.trim().is_empty()) {
                    detail.push(format!("“{}”", first.trim()));
                }
            }
            if !detail.is_empty() {
                let d: String = format!("{block_pad}   {}", detail.join(" · ")).chars().take(cw).collect();
                lines.push(Line::from(Span::styled(d, dim)));
            }
        }
    }

    // The sign-off (the last word) and footer arrive once the board is up.
    if rev.signoff {
        if !rep.rows.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(format!("{block_pad}{}", "─".repeat(bw)), dim)));
        }
        if let Some(s) = &signoff {
            // Left-anchored like the prose, so the last word types in left-to-right.
            let so = Style::default().fg(accent).add_modifier(Modifier::ITALIC);
            for l in wrap(s, bw) {
                lines.push(Line::from(Span::styled(format!("{block_pad}{l}"), so)));
            }
            lines.push(Line::from(""));
        }
        lines.push(center1("any key resumes exactly where you left off".to_string(), dim));
    }

    // Center once against the RESERVED height (fixed head + full manifest + tail),
    // not the live line count — so the composition holds still while the manifest
    // cascades and the prose types in. In instant mode there's no reveal, so the
    // live height is exact.
    let total = if animate {
        (fixed_head + manifest_full + signoff_full) as u16
    } else {
        lines.len() as u16
    };
    let top_pad = inner.height.saturating_sub(total) / 2;
    let area = Rect {
        x: inner.x,
        y: inner.y + top_pad,
        width: inner.width,
        height: (lines.len() as u16).max(total).min(inner.height),
    };
    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn render_terminal_pane(
    frame: &mut Frame,
    app: &App,
    pane_id: PaneId,
    rect: Rect,
    focused: bool,
) -> Option<(u16, u16)> {
    let pane = app.panes.get(&pane_id)?;
    let term_id = match pane.content {
        PaneContent::Terminal(id) => id,
        _ => return None,
    };

    let exited = app.terms.get(&term_id).map(|t| t.exited).unwrap_or(true);
    let offset = app.terms.get(&term_id).map(|t| t.view_offset()).unwrap_or(0);
    // The pane border/title carry NO status glyph — status lives in the tab bar, so
    // the divider line stays uncluttered. Border color is focus/exited only.
    let border_style = if exited {
        Style::default().fg(app.tuning.palette.accent_dark)
    } else if focused {
        Style::default().fg(app.tuning.palette.info)
    } else {
        Style::default().fg(app.tuning.palette.text_faint)
    };
    let base = pane.title.as_deref().unwrap_or("terminal");
    let title = if exited {
        format!(" {base} · exited ")
    } else if offset > 0 {
        format!(" {base} ↑{offset} ")
    } else {
        format!(" {base} ")
    };
    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default().add_modifier(if focused { Modifier::BOLD } else { Modifier::empty() }),
        ))
        .borders(Borders::ALL)
        .border_style(border_style);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let term = match app.terms.get(&term_id) {
        Some(t) => t,
        None => {
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "(terminal closed)",
                    Style::default().fg(app.tuning.palette.text_faint),
                )),
                inner,
            );
            return None;
        }
    };

    // Render the vt100 screen grid into the pane.
    let screen = term.screen();
    let (rows, cols) = screen.size();
    let vh = inner.height.min(rows);
    let vw = inner.width.min(cols);

    // A live mouse selection in THIS terminal → highlight its cells.
    let sel = app.term_sel.filter(|s| s.tid == term_id).map(|s| {
        let (mut a, mut b) = (s.anchor, s.end);
        if b < a { std::mem::swap(&mut a, &mut b); }
        (a, b, s.vw.saturating_sub(1))
    });
    let sel_bg = app.tuning.palette.selection_bg;

    let mut lines: Vec<Line> = Vec::with_capacity(vh as usize);
    for row in 0..vh {
        let mut spans: Vec<Span> = Vec::with_capacity(vw as usize);
        for col in 0..vw {
            if let Some(cell) = screen.cell(row, col) {
                let contents = cell.contents();
                let ch = if contents.is_empty() { " ".to_string() } else { contents };
                let mut style = Style::default()
                    .fg(conv_fg(app, cell.fgcolor()))
                    .bg(conv_bg(app, cell.bgcolor()));
                if cell.bold()      { style = style.add_modifier(Modifier::BOLD); }
                if cell.italic()    { style = style.add_modifier(Modifier::ITALIC); }
                if cell.underline() { style = style.add_modifier(Modifier::UNDERLINED); }
                if cell.inverse()   { style = style.add_modifier(Modifier::REVERSED); }
                if let Some((a, b, last)) = sel {
                    let c0 = if row == a.0 { a.1 } else { 0 };
                    let c1 = if row == b.0 { b.1 } else { last };
                    if row >= a.0 && row <= b.0 && col >= c0 && col <= c1 {
                        style = style.bg(sel_bg);
                    }
                }
                spans.push(Span::styled(ch, style));
            } else {
                spans.push(Span::raw(" "));
            }
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);

    // Dead shell: overlay the dismissal hint on the bottom row.
    if exited && inner.height > 0 {
        let notice = Rect { x: inner.x, y: inner.y + inner.height - 1, width: inner.width, height: 1 };
        frame.render_widget(
            Paragraph::new(Span::styled(
                " process exited — Enter closes this pane ",
                Style::default()
                    .fg(app.tuning.palette.on_accent)
                    .bg(app.tuning.palette.accent_dark)
                    .add_modifier(Modifier::BOLD),
            )),
            notice,
        );
        return None;
    }

    // Report the terminal's own cursor position when focused.
    if focused && !screen.hide_cursor() {
        let (cr, cc) = screen.cursor_position();
        let cx = inner.x + cc.min(vw.saturating_sub(1));
        let cy = inner.y + cr.min(vh.saturating_sub(1));
        return Some((cx, cy));
    }
    None
}

/// Map a vt100 cell color to a ratatui color.
fn conv_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default    => Color::Reset,
        vt100::Color::Idx(i)     => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// Terminal-cell foreground with a themed fallback: under an opaque theme, the
/// child's *default* fg follows `text` so the shell's base color matches the theme;
/// otherwise the terminal's own default (Reset) is honored. Explicit 16-color and
/// truecolor cells the program sets always pass through unchanged.
fn conv_fg(app: &App, c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default if opaque_bg(app).is_some() => app.tuning.palette.text,
        other => conv_color(other),
    }
}
/// Terminal-cell background — the child's *default* bg follows the theme surface
/// under an opaque theme, so terminal panes match the rest of the UI.
fn conv_bg(app: &App, c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => opaque_bg(app).unwrap_or(Color::Reset),
        other => conv_color(other),
    }
}

// ── Status bar ───────────────────────────────────────────────────────────────

/// One bottom-bar hint: a key badge, its label, and — when the chord maps to a
/// real action in this mode — the click target that makes the chip a button.
/// `None` targets (e.g. Terminal's "type to shell") render as plain, inert labels.
type Hint = (String, String, Option<crate::app::HitTarget>);

/// Render one hint chip into `spans`, register its click region when it has a
/// target, and light it on hover / recess it on press. Returns the advanced x.
/// A chip is ` key :label ` followed by a two-cell gap; the clickable rect covers
/// the badge and label but not the gap. Widths are `chars().count()` to match how
/// the tab bar measures its own cells (all hint glyphs are single-width).
fn push_chip(
    app: &App,
    spans: &mut Vec<Span<'static>>,
    x: u16,
    y: u16,
    key: &str,
    label: &str,
    target: Option<crate::app::HitTarget>,
    key_bg: Color,
    key_fg: Color,
) -> u16 {
    let badge_w = key.chars().count() as u16 + 2; // " key "
    let label_w = label.chars().count() as u16 + 2; // ":label "
    // Interactive feedback rides the shared hover/pressed state: brighter when the
    // pointer is over the chip, deeper when it's held down — the "raised" then
    // "pushed in" read a terminal can manage with color alone.
    let (bg, fg) = match &target {
        Some(t) if app.is_pressed(t) => (app.tuning.palette.accent_dark, app.tuning.palette.on_accent),
        Some(t) if app.is_hovered(t) => (app.tuning.palette.accent_bright, app.tuning.palette.on_accent),
        _ => (key_bg, key_fg),
    };
    spans.push(Span::styled(
        format!(" {key} "),
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::styled(
        format!(":{label} "),
        Style::default().fg(app.tuning.palette.text).add_modifier(Modifier::BOLD),
    ));
    if let Some(t) = target {
        app.hit(Rect { x, y, width: badge_w + label_w, height: 1 }, t);
    }
    spans.push(Span::styled("  ", Style::default()));
    x + badge_w + label_w + 2
}

/// Hint pairs for the status bar. Edit/Terminal hints are derived live from the
/// keymap so they stay honest after a remap; other modes are fixed UI keys. Each
/// hint carries its click target (or `None` for a plain label).
fn status_hints(app: &App) -> Vec<Hint> {
    use crate::app::HitTarget;
    if matches!(app.mode, Mode::Edit) {
        let mut v: Vec<Hint> = vec![(bar_open_keys(app), "⌕ commands".to_string(), Some(HitTarget::OpenBar))];
        // WARP (space warp — tabs/panes/splits): high-value and, until now, invisible
        // on the persistent bar. Honest here — C-t is in the edit chord map.
        if let Some(b) = app.keys.binding_for(&Action::TabMode) {
            v.push((b, "warp".to_string(), Some(HitTarget::Act(Action::TabMode))));
        }
        for (action, label) in [
            (Action::Save, "save"),
            (Action::ToggleFileTree, "open"),
            (Action::Search, "search"),
        ] {
            if let Some(b) = app.keys.binding_for(&action) {
                v.push((b, label.to_string(), Some(HitTarget::Act(action))));
            }
        }
        v
    } else if matches!(app.mode, Mode::Terminal) {
        // Three honest, clickable affordances that make sense from a shell: commands
        // (the bar), warp (C-t — a single-chord chrome action `handle_terminal` runs),
        // and editor (C-g — hand the keyboard back to the editor; a real click target
        // now, not a dead label). No "open" — its C-x prefix goes to the shell here.
        let mut v: Vec<Hint> = vec![(bar_open_keys(app), "commands".to_string(), Some(HitTarget::OpenBar))];
        if let Some(b) = app.keys.binding_for(&Action::TabMode) {
            v.push((b, "warp".to_string(), Some(HitTarget::Act(Action::TabMode))));
        }
        v.push(("C-g".to_string(), "editor".to_string(), Some(HitTarget::FocusEditor)));
        v
    } else {
        app.mode
            .hints()
            .iter()
            .map(|(k, a)| (k.to_string(), a.to_string(), None))
            .collect()
    }
}

/// The chords that open the command bar, rendered (e.g. "C-Spc / M-x").
fn bar_open_keys(app: &App) -> String {
    app.keys
        .bar_open
        .iter()
        .map(|c| crate::config::render_chords(std::slice::from_ref(c)))
        .collect::<Vec<_>>()
        .join(" / ")
}

fn render_status(frame: &mut Frame, app: &App, area: Rect) {
    let accent = app.tuning.palette.accent;
    let sand   = app.tuning.palette.accent_bright;
    let chipfg = app.tuning.palette.on_accent;
    // Brand lives in chrome; green stays semantic (a live shell process).
    let (mode_fg, mode_bg, key_bg, key_fg) = match &app.mode {
        Mode::Edit     => (chipfg, accent,       accent, chipfg),
        Mode::Prompt   => (chipfg, sand,         sand,   chipfg),
        Mode::Tab      => (chipfg, accent,       accent, chipfg),
        Mode::Bar      => (chipfg, accent,       accent, chipfg),
        Mode::Tree     => (chipfg, accent,       accent, chipfg),
        Mode::Undo     => (chipfg, sand,         sand,   chipfg),
        Mode::Terminal => {
            let teal = app.tuning.palette.info;
            (app.tuning.palette.text, teal, teal, app.tuning.palette.text)
        }
    };

    // Compute the right-aligned readout first, so the chips know how much room to
    // leave: the position readout must never be pushed off or overwritten.
    let pane = app.focused_pane();
    let session = app.session_name.as_ref().map(|s| format!("  ⚡{s}")).unwrap_or_default();
    let readout = match pane.content {
        PaneContent::Editor(buf_id) => {
            let name = app
                .buffers
                .get(&buf_id)
                .map(|b| format!("{}{}", b.name, if b.modified { " ●" } else { "" }))
                .unwrap_or_default();
            format!("{name}   Ln {}, Col {}{session} ", pane.cursor_row + 1, pane.cursor_col + 1)
        }
        PaneContent::Terminal(_) => format!("terminal{session} "),
    };
    let reserve = readout.chars().count() as u16 + 1;
    let budget_right = area.right().saturating_sub(reserve.min(area.width));

    // Left side: mode label (keyboard-first entry — deliberately NOT a button) then
    // hint chips laid out with a running x so each can register its own click cells.
    let label = format!(" {} ", app.mode.label());
    let mut x = area.x + label.chars().count() as u16 + 2; // label + the "  " gap
    let mut spans: Vec<Span<'static>> = vec![
        Span::styled(label, Style::default().fg(mode_fg).bg(mode_bg).add_modifier(Modifier::BOLD)),
        Span::styled("  ", Style::default()),
    ];

    // Priority order (commands > warp > save/editor > open > search). Stop before a
    // chip would collide with the readout — the only truncation the bar has.
    for (key, lbl, target) in status_hints(app) {
        let w = (key.chars().count() + lbl.chars().count() + 6) as u16; // badge+label+gap
        if x + w > budget_right { break; }
        x = push_chip(app, &mut spans, x, area.y, &key, &lbl, target, key_bg, key_fg);
    }

    // Transient info trails the hints on the left, so the readout is never displaced.
    // A hovered clipped tab's full name (set by render_tab_bar this frame) shows here.
    if !app.pending_prefix.is_empty() {
        spans.push(Span::styled(
            format!(" {}- ", crate::config::render_chords(&app.pending_prefix)),
            Style::default().fg(app.tuning.palette.accent_bright).add_modifier(Modifier::BOLD),
        ));
    } else if let Some(msg) = &app.status_msg {
        spans.push(Span::styled(
            format!(" {msg} "),
            Style::default().fg(app.tuning.palette.accent_bright),
        ));
    } else if let Some(tip) = app.hover_tip.borrow().as_ref() {
        spans.push(Span::styled(
            format!(" {tip} "),
            Style::default().fg(app.tuning.palette.text_dim),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    // Position readout — ALWAYS right-aligned on top, so it can't be truncated
    // by the left hints or hidden by a status message.
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            readout,
            Style::default().fg(app.tuning.palette.accent_bright).add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Right),
        area,
    );
}

// ── Control bar (bottom row, always visible) ──────────────────────────────────

fn render_control_bar(frame: &mut Frame, app: &App, area: Rect) {
    match &app.mode {
        Mode::Bar => {
            // Show current query with mode prefix
            let palette = match app.palette.as_ref() {
                Some(p) => p,
                None => return,
            };
            let mode_label = match palette.bar_mode {
                BarMode::Command => "CMD",
                BarMode::Ask     => "ASK",
                BarMode::Shell   => "SH !",
            };
            let prompt = format!("[{}] › {}▎", mode_label, palette.query);
            let style = Style::default()
                .fg(app.tuning.palette.accent)
                .add_modifier(Modifier::BOLD);
            // In-bar quick keys, taught right where they work — only while the
            // query is empty, because that's the only time they fire.
            let legend = if palette.bar_mode == BarMode::Command && palette.query.is_empty() {
                let keys: Vec<String> = crate::palette::bar_quick_legend()
                    .iter()
                    .map(|(k, what)| format!("{k} {what}"))
                    .collect();
                format!("   {}", keys.join(" · "))
            } else {
                String::new()
            };
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(prompt.clone(), style),
                    Span::styled(legend, Style::default().fg(app.tuning.palette.text_faint)),
                ])),
                area,
            );
            // Set cursor at end of prompt
            let cx = area.x + prompt.chars().count() as u16 - 1; // before the ▎
            if cx < area.x + area.width {
                frame.set_cursor_position((cx, area.y));
            }
        }
        Mode::Prompt => {
            if let Some(p) = app.prompt.as_ref() {
                // Live search shows an `n/m` match counter (and a Tab hint).
                let extra = if p.kind == crate::app::PromptKind::Search {
                    match app.isearch_status() {
                        Some((cur, total)) if total > 0 => {
                            let pick = if total >= 2 { "  ⇥ jump" } else { "" };
                            format!("  {cur}/{total}{pick}")
                        }
                        _ if !p.input.is_empty() => "  (no match)".to_string(),
                        _ => String::new(),
                    }
                } else {
                    String::new()
                };
                let text = format!("{}{}{}", p.label, p.input, extra);
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        text.clone(),
                        Style::default().fg(app.tuning.palette.text).add_modifier(Modifier::BOLD),
                    )),
                    area,
                );
                // Cursor sits after the query itself, before the counter suffix.
                let cx = area.x + (p.label.chars().count() + p.input.chars().count()) as u16;
                if cx < area.x + area.width {
                    frame.set_cursor_position((cx, area.y));
                }
            }
        }
        // Idle (Edit/Terminal/Tab/Tree/Undo): nothing. The old idle row duplicated
        // the status bar's commands/open/search as a faint, redundant strip; the
        // status bar already carries the live, clickable chips. This row now exists
        // ONLY as the home for the command query and the minibuffer (the two arms
        // above), so it stays blank until one of them is active.
        _ => {}
    }
}

// ── Bar dropdown (grows upward from control bar) ──────────────────────────────

/// Returns the rect it drew (None when nothing rendered) so the cursor-anchored
/// shell overlay can yield to it instead of drawing on top.
/// Truncate to a column width with an ellipsis, so names/why-lines never hard-clip.
fn ellip(s: &str, w: usize) -> String {
    if w == 0 { return String::new(); }
    if s.chars().count() <= w { return s.to_string(); }
    let t: String = s.chars().take(w.saturating_sub(1)).collect();
    format!("{t}…")
}

/// The Commands list — the classic launcher rows: in-bar quick key, live binding
/// badge, label, dim description. `active` gates whether the selection highlights.
fn command_lines(app: &App, rows: &[crate::palette::PaletteRow], sel: usize, navigated: bool, active: bool, max_rows: usize, inner: Rect) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let body_max = max_rows.max(1);
    let scroll = if sel >= body_max { sel + 1 - body_max } else { 0 };
    for (idx, row) in rows.iter().enumerate().skip(scroll).take(body_max) {
        // Registered where the scroll offset is known, so a click can never
        // resolve to a different row than the one under the pointer.
        let target = crate::app::HitTarget::Row(crate::app::RowKind::Command, idx);
        app.hit(
            Rect { x: inner.x, y: inner.y + out.len() as u16, width: inner.width, height: 1 },
            target.clone(),
        );
        // Hover paints a row exactly like arrow-selection — same fill AND same label
        // color — so the mouse and keyboard agree on "this is the row that fires".
        let selected = (active && navigated && idx == sel) || app.is_hovered(&target);
        let bg = if selected { app.tuning.palette.select_row_bg } else { app.tuning.palette.surface };
        let has_sub = matches!(row.kind, ItemKind::Submenu(_));
        let quick = match &row.kind { ItemKind::Run(a) => crate::palette::bar_quick_key(a), _ => None };
        let binding = match &row.kind { ItemKind::Run(a) => app.keys.binding_for(a).unwrap_or_default(), _ => String::new() };
        let desc = if row.description.is_empty() { String::new() } else { format!(" — {}", row.description) };
        let type_mark = if has_sub { " ▸" } else { "" };
        let quick_span = match quick {
            Some(q) => Span::styled(format!(" {q} "), Style::default().fg(app.tuning.palette.on_accent).bg(app.tuning.palette.accent).add_modifier(Modifier::BOLD)),
            None => Span::styled("   ", Style::default().bg(bg)),
        };
        out.push(Line::from(vec![
            quick_span,
            Span::styled(format!(" {:<w$}", binding, w = app.tuning.binding_badge_width),
                Style::default().fg(app.tuning.palette.accent_bright).bg(bg).add_modifier(Modifier::BOLD)),
            Span::styled("  ", Style::default().bg(bg)),
            Span::styled(format!("{}{}", row.label, type_mark),
                if selected { Style::default().fg(app.tuning.palette.accent).bg(bg).add_modifier(Modifier::BOLD) }
                else { Style::default().fg(app.tuning.palette.text).bg(bg) }),
            Span::styled(desc, Style::default().fg(app.tuning.palette.text_faint).bg(bg)),
        ]));
    }
    out
}

/// One row per workspace: current marker ‹, verdict glyph (class color), id · name
/// (ellipsized), age right-aligned. Scrolls to keep the selection visible.
fn workspace_lines(app: &App, rows: &[crate::palette::PaletteRow], sel: usize, active: bool, width: u16, max_rows: usize, inner: Rect) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let body_max = max_rows.max(1);
    let scroll = if sel >= body_max { sel + 1 - body_max } else { 0 };
    for (idx, row) in rows.iter().enumerate().skip(scroll).take(body_max) {
        let target = crate::app::HitTarget::Row(crate::app::RowKind::Workspace, idx);
        app.hit(
            Rect { x: inner.x, y: inner.y + out.len() as u16, width: inner.width, height: 1 },
            target.clone(),
        );
        // Selected row: inverted teal (dark text on a teal bar) so it's clearly
        // visible — the old DarkGray highlight vanished against the dark ground.
        // Hover mirrors selection fully (fill + fg) so it can't render text on a
        // same-color band.
        let s = (active && idx == sel) || app.is_hovered(&target);
        let bg = if s { app.tuning.palette.info } else { app.tuning.palette.surface };
        let sel_fg = app.tuning.palette.on_accent;
        let (glyph, vcolor, id, cur) = match &row.kind {
            ItemKind::Surface(sr) => {
                ("●", verdict_color(app, sr.verdict), sr.tab_index + 1, sr.tab_index == app.active_tab)
            }
            _ => ("●", app.tuning.palette.text_dim, 0, false),
        };
        let _ = id;
        let marker = if cur { "‹" } else { " " };
        // Age no longer trails the row — it lives in the summary box for the
        // highlighted workspace. The selected row carries only the ↵ verb.
        let name_budget = (width as usize).saturating_sub(3 + 3);
        let mut spans = vec![
            Span::styled(marker.to_string(), Style::default().fg(if s { sel_fg } else { app.tuning.palette.accent_bright }).bg(bg).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{glyph} "), Style::default().fg(if s { sel_fg } else { vcolor }).bg(bg).add_modifier(Modifier::BOLD)),
            Span::styled(
                ellip(&row.label, name_budget),
                Style::default().fg(if s { sel_fg } else { app.tuning.palette.text }).bg(bg)
                    .add_modifier(if s { Modifier::BOLD } else { Modifier::empty() }),
            ),
        ];
        let right = if s { "↵ " } else { "  " };
        let lw: usize = spans.iter().map(|x| x.content.chars().count()).sum();
        let pad = (width as usize).saturating_sub(lw + right.chars().count());
        spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
        spans.push(Span::styled(right.to_string(), Style::default().fg(if s { sel_fg } else { app.tuning.palette.text_faint }).bg(bg).add_modifier(if s { Modifier::BOLD } else { Modifier::empty() })));
        out.push(Line::from(spans));
    }
    out
}

/// Word-wrap `text` to `width`, at most `max_lines` (the last ellipsized if the text
/// overruns). A long single word is hard-broken so it can't overflow the box.
fn wrap_summary(text: &str, width: usize, max_lines: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let word: String = if word.chars().count() > width { word.chars().take(width).collect() } else { word.to_string() };
        if cur.is_empty() {
            cur = word;
        } else if cur.chars().count() + 1 + word.chars().count() <= width {
            cur.push(' ');
            cur.push_str(&word);
        } else {
            lines.push(std::mem::take(&mut cur));
            cur = word;
            if lines.len() >= max_lines { break; }
        }
    }
    if !cur.is_empty() && lines.len() < max_lines { lines.push(cur); }
    if lines.len() > max_lines { lines.truncate(max_lines); }
    // If we ran out of room mid-text, ellipsize the last visible line.
    if lines.len() == max_lines {
        let more = text.split_whitespace().count()
            > lines.iter().map(|l| l.split_whitespace().count()).sum::<usize>();
        if more {
            if let Some(last) = lines.last_mut() {
                *last = ellip(&format!("{last} …"), width);
            }
        }
    }
    lines
}

/// The summary box for the highlighted workspace: a rule, "status: <state> · <age>"
/// in bold, then the generated summary WRAPPED across the box (not trailing off), and
/// a dim `s` hint. A teal left rail ties it to the teal-highlighted selection. The ↵
/// verb lives on the selected row, not here.
/// A live elapsed duration with seconds, so a running counter visibly ticks
/// (`45s`, `4m 12s`, `1h 03m`).
fn fmt_elapsed(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 { format!("{h}h {m:02}m") } else if m > 0 { format!("{m}m {s:02}s") } else { format!("{s}s") }
}

fn detail_lines(app: &App, row: Option<&crate::palette::PaletteRow>, width: u16) -> Vec<Line<'static>> {
    use crate::briefing::Verdict;
    let w = width as usize;
    let mut out = vec![Line::from(Span::styled(
        format!(" {} ", "─".repeat(w.saturating_sub(2))),
        Style::default().fg(app.tuning.palette.text_faint),
    ))];
    let Some(ItemKind::Surface(s)) = row.map(|r| &r.kind) else { return out };
    let vcolor = verdict_color(app, s.verdict);
    let vlabel = s.verdict.label();
    let teal = app.tuning.palette.info;
    let rail = || Span::styled(" ▌ ", Style::default().fg(teal));
    let content_w = w.saturating_sub(5); // " ▌ " + right padding
    // Status + age. A running job shows a live elapsed counter (with seconds, so it
    // visibly ticks); everything else shows the coarser age.
    let age = if s.age_secs == 0 {
        String::new()
    } else if s.verdict == Verdict::Running {
        format!(" · {}", fmt_elapsed(s.age_secs))
    } else {
        format!(" · {}", crate::briefing::fmt_secs(s.age_secs))
    };
    out.push(Line::from(vec![
        rail(),
        Span::styled("status: ", Style::default().fg(app.tuning.palette.text).add_modifier(Modifier::BOLD)),
        Span::styled(vlabel.to_string(), Style::default().fg(vcolor).add_modifier(Modifier::BOLD)),
        Span::styled(age, Style::default().fg(app.tuning.palette.text_faint)),
    ]));
    // The generated summary, wrapped to fill the box (up to 4 lines).
    let why = row.map(|r| r.description.clone()).unwrap_or_default();
    for line in wrap_summary(&why, content_w, 4) {
        out.push(Line::from(vec![
            rail(),
            Span::styled(line, Style::default().fg(app.tuning.palette.text_dim).add_modifier(Modifier::ITALIC)),
        ]));
    }
    out.push(Line::from(Span::styled(" s  summarize", Style::default().fg(app.tuning.palette.text_faint))));
    out
}

/// A tiny deterministic hash for star placement + twinkle phase — stable across
/// frames so stars never jump; only brightness moves.
fn star_hash(x: usize, y: usize) -> u64 {
    let mut h = (x as u64).wrapping_mul(73856093) ^ (y as u64).wrapping_mul(19349663);
    h ^= h >> 13;
    h = h.wrapping_mul(0x9E37_79B1);
    h ^ (h >> 16)
}

/// A calm night sky for the workspaces panel's empty space: a STILL, sparse field of
/// dim stars (no twinkle — the flicker was too stimulating), with an occasional slow
/// meteor drifting diagonally across and fading. Deterministic in position; the field
/// never moves, and only a meteor's brief, gentle pass animates.
fn starfield(app: &App, width: u16, height: u16) -> Vec<Line<'static>> {
    let (w, h) = (width as usize, height as usize);
    let teal = app.tuning.palette.info;
    // A still, dim scatter of stars — no motion. Quiet by design (the "no idle
    // frames" doctrine): a static field costs zero repaints and never distracts.
    let mut lines = Vec::with_capacity(h);
    for y in 0..h {
        let mut spans: Vec<Span> = Vec::new();
        let mut run = String::new();
        for x in 0..w {
            let seed = star_hash(x, y);
            if seed % 31 == 0 {
                if !run.is_empty() { spans.push(Span::raw(std::mem::take(&mut run))); }
                let glyph = match seed % 13 { 0 => "✦", 1 => "⋆", _ => "·" };
                let color = if seed % 101 == 0 { teal } else { app.tuning.palette.text_faint }; // still, dim
                spans.push(Span::styled(glyph.to_string(), Style::default().fg(color)));
            } else {
                run.push(' ');
            }
        }
        if !run.is_empty() { spans.push(Span::raw(run)); }
        lines.push(Line::from(spans));
    }
    lines
}

/// The Workspaces board — a separate teal box, reached from the command bar with ←.
/// Opens as tall as the command box (same height by default); its list + summary sit
/// at the top and the empty bottom fills with an ambient starfield.
fn render_workspaces_panel(frame: &mut Frame, app: &App, rect: Rect) {
    let active = app.palette.as_ref().map(|p| p.column == crate::palette::BarColumn::Workspaces).unwrap_or(false);
    let sel = app.palette.as_ref().map(|p| p.sel_ws).unwrap_or(0);
    let teal = app.tuning.palette.info;
    let mut bstyle = Style::default().fg(teal);
    if active { bstyle = bstyle.add_modifier(Modifier::BOLD); }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(bstyle)
        .title(Span::styled(" SPACES ", Style::default().fg(teal).add_modifier(Modifier::BOLD)));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    // The host-health line is pinned to the BOTTOM of the panel (rendered after the body,
    // below the sky); reserve a row for it here so the board flows above.
    let show_health = app.tuning.health_line == 1 && inner.height >= 2;
    let foot = if show_health { 1u16 } else { 0 };
    let body = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: inner.height.saturating_sub(foot),
    };

    let rows = app.bar_workspace_rows();
    let ih = body.height as usize;
    // Summary box for the highlighted workspace; the list gets what's left above it.
    let dlines = detail_lines(app, rows.get(sel), body.width);
    let summ_h = dlines.len().min(ih);
    let list_h = rows.len().min(ih.saturating_sub(summ_h));
    let list_rect = Rect { x: body.x, y: body.y, width: body.width, height: list_h as u16 };
    frame.render_widget(Paragraph::new(Text::from(workspace_lines(app, &rows, sel, active, body.width, list_h, list_rect))),
        list_rect);
    frame.render_widget(Paragraph::new(Text::from(dlines)),
        Rect { x: body.x, y: body.y + list_h as u16, width: body.width, height: summ_h as u16 });
    // The empty bottom is the sky.
    let used = list_h + summ_h;
    if ih > used {
        frame.render_widget(
            Paragraph::new(Text::from(starfield(app, body.width, (ih - used) as u16))),
            Rect { x: body.x, y: body.y + used as u16, width: body.width, height: (ih - used) as u16 },
        );
    }

    // Host-health line, pinned to the very bottom, colour-coded and justified.
    if show_health {
        render_health_line(frame, app, Rect { x: inner.x, y: inner.y + inner.height - 1, width: inner.width, height: 1 });
    }
}

/// The SPACES host-health line: each metric in a cool hue (teal), warming to amber then
/// red only when it's alarming, spread across the width in a justified layout.
fn render_health_line(frame: &mut Frame, app: &App, area: Rect) {
    use crate::health::HealthLevel;
    let metrics = app.health.metrics();
    if metrics.is_empty() || area.width < 4 {
        return;
    }
    let p = &app.tuning.palette;
    let colour = |lvl: HealthLevel| match lvl {
        HealthLevel::Ok => p.info,       // cool teal
        HealthLevel::Warn => p.warning,  // amber
        HealthLevel::Danger => p.danger, // red
    };
    let pad = 1usize;
    let inner = (area.width as usize).saturating_sub(pad * 2);
    // A narrow SPACES column can't hold all five metrics; keep the leading (most
    // important) ones that fit with at least single-space separators rather than
    // overflowing and clipping mid-word.
    let mut fit: Vec<&crate::health::HealthMetric> = Vec::new();
    let mut used = 0usize;
    for m in &metrics {
        let w = m.text.chars().count();
        let next = if fit.is_empty() { w } else { used + 1 + w };
        if !fit.is_empty() && next > inner {
            break;
        }
        used = next;
        fit.push(m);
    }
    let n = fit.len();
    let total: usize = fit.iter().map(|m| m.text.chars().count()).sum();
    let slack = inner.saturating_sub(total); // ≥ n-1 by construction, so gaps ≥ 1
    // Distribute the slack evenly between metrics (justify).
    let gap = |i: usize| -> usize {
        if n <= 1 {
            0
        } else {
            (slack / (n - 1) + usize::from(i < slack % (n - 1))).max(1)
        }
    };
    let mut spans = vec![Span::raw(" ".repeat(pad))];
    for (i, m) in fit.iter().enumerate() {
        let mut style = Style::default().fg(colour(m.level));
        if m.level == HealthLevel::Danger {
            style = style.add_modifier(Modifier::BOLD);
        }
        spans.push(Span::styled(m.text.clone(), style));
        if i < n - 1 {
            spans.push(Span::raw(" ".repeat(gap(i))));
        }
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The Commands launcher — the classic single-column dropdown, in its own bordered
/// box. `left_border` is dropped when the Workspaces panel sits to its left (that
/// panel's right border serves as the divider).
fn render_command_panel(frame: &mut Frame, app: &App, rect: Rect, left_border: bool, active: bool) {
    let palette = match app.palette.as_ref() { Some(p) => p, None => return };
    let borders = if left_border { Borders::TOP | Borders::LEFT | Borders::RIGHT } else { Borders::TOP | Borders::RIGHT };
    // The command box always keeps its accent (orange) border — a fully-boxed panel —
    // and just bolds when it holds focus; the selection highlight inside carries the
    // rest of the focus signal.
    let accent = app.tuning.palette.accent;
    let mut bstyle = Style::default().fg(accent);
    if active { bstyle = bstyle.add_modifier(Modifier::BOLD); }
    let block = Block::default()
        .borders(borders)
        .border_style(bstyle)
        .title(Span::styled(" COMMANDS ", Style::default().fg(accent).add_modifier(Modifier::BOLD)));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    let rows = app.bar_rows();
    let lines = command_lines(app, &rows, palette.selected, palette.navigated, active, inner.height as usize, inner);
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

fn render_bar_dropdown(
    frame: &mut Frame,
    app: &App,
    pane_area: Rect,
    bar_area: Rect,
) -> Option<Rect> {
    let palette = app.palette.as_ref()?;
    let show_ws = app.bar_show_workspaces();
    let cmd_rows = app.bar_rows();
    if cmd_rows.is_empty() && !show_ws {
        return None;
    }

    let max_height = ((pane_area.height * app.tuning.panel_max_height_pct / 100) as usize)
        .min(app.tuning.dropdown_max_rows as usize) as u16;
    // The command box height tracks the (filtering) command list.
    let cmd_h = (cmd_rows.len() as u16 + 1).min(max_height).max(2);

    if !show_ws {
        // No fleet to survey → the plain single-column launcher (previous behaviour).
        let full = Rect { x: bar_area.x, y: bar_area.y.saturating_sub(cmd_h), width: bar_area.width, height: cmd_h };
        clear_panel(frame, app, full);
        render_command_panel(frame, app, full, true, true);
        return Some(full);
    }

    // A separate, STATIC WORKSPACES box beside the command launcher. It opens as tall
    // as the command box's ceiling (same height by default on invocation) and stays
    // put while the command box shrinks as you type; its content sits at the top and
    // the empty bottom fills with the starfield. Both boxes are bottom-anchored; ←
    // focuses the panel, → the launcher.
    let ws_h = max_height.max(4);
    let ws_w = app.tuning.tree_width.clamp(20, bar_area.width.saturating_sub(30).max(20));
    let ws_rect = Rect { x: bar_area.x, y: bar_area.y.saturating_sub(ws_h), width: ws_w, height: ws_h };
    let cmd_rect = Rect {
        x: bar_area.x + ws_w,
        y: bar_area.y.saturating_sub(cmd_h),
        width: bar_area.width.saturating_sub(ws_w),
        height: cmd_h,
    };
    clear_panel(frame, app, ws_rect);
    clear_panel(frame, app, cmd_rect);
    render_workspaces_panel(frame, app, ws_rect);
    render_command_panel(frame, app, cmd_rect, true, palette.column == crate::palette::BarColumn::Commands);
    Some(Rect {
        x: bar_area.x,
        y: bar_area.y.saturating_sub(ws_h.max(cmd_h)),
        width: bar_area.width,
        height: ws_h.max(cmd_h),
    })
}

// ── Proactive notice line (W6 watch verdicts) ────────────────────────────────

fn render_notice(frame: &mut Frame, app: &App, pane_area: Rect) {
    let Some(n) = app.notices.first() else { return };
    if pane_area.height == 0 {
        return;
    }
    let row = Rect { x: pane_area.x, y: pane_area.bottom() - 1, width: pane_area.width, height: 1 };
    let (glyph, fg) = match n.kind {
        crate::app::NoticeKind::Failure => ("✗", app.tuning.palette.accent_bright),
        crate::app::NoticeKind::Blocked => ("⏸", app.tuning.palette.accent),
        crate::app::NoticeKind::Info => ("✓", app.tuning.palette.info),
    };
    let more = if app.notices.len() > 1 { format!("  (+{} more)", app.notices.len() - 1) } else { String::new() };
    let text = format!(" {glyph} {}{more}   Esc dismiss ", n.text);
    // The whole line expands the queue into a digest; the trailing "Esc dismiss"
    // affordance dismisses. Registered in that order so dismiss (drawn last,
    // hit-tested first) wins on the cells it labels. This is also the mouse's
    // way around P1.2: over a focused terminal, Esc goes to the shell — a click
    // needs no mode.
    // "Esc dismiss" trails the text inline, so its cells are counted, not assumed.
    let dismiss_at = (3 + n.text.chars().count() + more.chars().count() + 3) as u16;
    let dismiss_w = "Esc dismiss ".chars().count() as u16;
    app.hit(row, crate::app::HitTarget::Act(Action::ExpandNotices));
    if dismiss_at < row.width {
        app.hit(
            Rect {
                x: row.x + dismiss_at,
                y: row.y,
                width: dismiss_w.min(row.width - dismiss_at),
                height: 1,
            },
            crate::app::HitTarget::DismissNotice,
        );
    }
    clear_panel(frame, app, row);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(app.tuning.palette.on_accent).bg(fg).add_modifier(Modifier::BOLD),
        ))),
        row,
    );
}

// ── Left file-tree sidebar (@ / C-x d) ───────────────────────────────────────

fn render_file_tree(frame: &mut Frame, app: &App, area: Rect) {
    let accent = app.tuning.palette.accent;
    let focused = matches!(app.mode, Mode::Tree);
    let border = if focused { app.tuning.palette.accent_bright } else { app.tuning.palette.border };

    clear_panel(frame, app, area);
    // Header: the filter query (⌕) while filtering, else the root folder name.
    let (root_name, filter) = app
        .file_tree
        .as_ref()
        .map(|t| {
            let name = t
                .root
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| t.root.to_string_lossy().to_string());
            (name, t.filter.clone())
        })
        .unwrap_or_default();
    let title = if filter.is_empty() {
        format!(" Navigator · {root_name}/ ")
    } else {
        format!(" Navigator · ⌕ {filter} ")
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(Span::styled(
            title,
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let selected = app.file_tree.as_ref().map(|t| t.selected).unwrap_or(0);
    let max_show = inner.height as usize;
    if max_show == 0 {
        return;
    }
    let scroll = if selected >= max_show { selected + 1 - max_show } else { 0 };

    let width = inner.width as usize;
    let mut lines: Vec<Line> = Vec::new();
    for (idx, row) in app.tree_rows.iter().enumerate().skip(scroll).take(max_show) {
        let is_sel = idx == selected && focused;
        let hovered = app.is_hovered(&crate::app::HitTarget::Row(crate::app::RowKind::Tree, idx));
        // Selected row: a full-width accent band (unmistakable), like a chip. Hover
        // previews that band — and must adopt the SAME foreground as selection, or a
        // folder (accent text) vanishes against the accent band.
        let hot = is_sel || hovered;
        let bg = if hot { accent } else { app.tuning.palette.surface };
        let indent = "  ".repeat(row.depth);
        let glyph = if row.updir {
            "↑ "
        } else if row.is_dir {
            if row.expanded { "▾ " } else { "▸ " }
        } else {
            "  "
        };
        let label = if row.is_dir && !row.updir {
            format!("{}/", row.label)
        } else {
            row.label.clone()
        };
        // Foreground: readable on the band when selected; folders bold+accent,
        // files white, `../` dim — otherwise.
        let chip = app.tuning.palette.on_accent;
        let name_fg = if hot {
            chip
        } else if row.updir {
            app.tuning.palette.accent_bright // visible "go up" affordance
        } else if row.is_dir {
            accent
        } else {
            app.tuning.palette.text
        };
        let glyph_fg = if hot { chip } else { accent };
        let mut modifier = Modifier::empty();
        if row.is_dir && !row.updir {
            modifier |= Modifier::BOLD;
        }
        // Pad to the full inner width so the selection band spans the row.
        let used = indent.chars().count() + glyph.chars().count() + label.chars().count();
        let pad = " ".repeat(width.saturating_sub(used));
        lines.push(Line::from(vec![
            Span::styled(format!("{indent}{glyph}"), Style::default().fg(glyph_fg).bg(bg)),
            Span::styled(label, Style::default().fg(name_fg).bg(bg).add_modifier(modifier)),
            Span::styled(pad, Style::default().bg(bg)),
        ]));
        // The row's band spans the full inner width — so does its click target.
        let y = inner.y + (lines.len() as u16 - 1);
        app.hit(Rect { x: inner.x, y, width: inner.width, height: 1 },
                crate::app::HitTarget::Row(crate::app::RowKind::Tree, idx));
    }
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

// ── Shell-translate overlay (W3, anchored at the cursor — no eye-jump) ─────────

fn render_shell_overlay(frame: &mut Frame, app: &App, pane_area: Rect, avoid: Option<Rect>) {
    let query = app.palette.as_ref().map(|p| p.query.as_str()).unwrap_or("");
    let chipfg = app.tuning.palette.on_accent;
    let accent = app.tuning.palette.accent;

    // The input line begins EXACTLY where the cursor was (no label prefix), so
    // it reads as typing in place. A tiny chip sits just left of it: `!` for the
    // pure-shell mode, `›` for the unified composer (shell OR a picked command).
    let shell_mode = app
        .palette
        .as_ref()
        .map(|p| matches!(p.bar_mode, BarMode::Shell))
        .unwrap_or(true);
    let navigated = app.palette.as_ref().map(|p| p.navigated).unwrap_or(false);
    // Empty query: show a placeholder so the composer is unmistakably present.
    let input = if query.is_empty() {
        format!("{} run a command… ", if shell_mode { "!" } else { "›" })
    } else {
        format!("{} {query} ", if shell_mode { "!" } else { "›" })
    };
    let configured = crate::agent::AgentConfig::from_env().is_configured();
    let err = app.agent_answer.as_deref().filter(|a| a.starts_with('⚠'));
    let hint = if app.agent_pending {
        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let sp = frames[(app.frame_tick / app.tuning.spinner_speed_ticks % 10) as usize];
        format!(" {sp} translating…")
    } else if app.shell_ready {
        " ✓ Enter runs · edit to change · Esc cancel".to_string()
    } else if let Some(e) = err {
        format!(" {e} · Enter runs literally · Esc")
    } else if !configured {
        " Enter runs (set GEMINI_API_KEY to type English) · Esc".to_string()
    } else if shell_mode {
        " type English, Enter translates → command · Esc".to_string()
    } else if navigated {
        " Enter runs the highlighted command · no match → shell · Esc".to_string()
    } else {
        " type a command (English ok) · ! pure shell · Esc".to_string()
    };

    let width = ((input.chars().count().max(hint.chars().count())) as u16)
        .min(pane_area.width.max(4));

    // Anchor the INPUT row on the cursor row; hint goes below (or above at the
    // bottom edge). Clamp inside the pane.
    let (cx, cy) = app.cursor_screen.unwrap_or((pane_area.x, pane_area.y));
    let max_x = pane_area.x + pane_area.width.saturating_sub(width);
    let x = cx.min(max_x).max(pane_area.x);
    let hint_below = cy + 1 < pane_area.y + pane_area.height;
    let (input_y, hint_y) = if hint_below { (cy, cy + 1) } else { (cy, cy.saturating_sub(1)) };

    let input_rect = Rect { x, y: input_y, width, height: 1 };
    let hint_rect = Rect { x, y: hint_y, width, height: 1 };
    // The dropdown outranks the anchored composer: when they'd collide (cursor
    // near the bottom), skip the overlay entirely so the menu stays readable.
    if let Some(d) = avoid {
        if input_rect.intersects(d) || hint_rect.intersects(d) {
            return;
        }
    }
    clear_panel(frame, app, input_rect);
    clear_panel(frame, app, hint_rect);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            input,
            Style::default().fg(chipfg).bg(accent).add_modifier(Modifier::BOLD),
        ))),
        input_rect,
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hint,
            Style::default().fg(app.tuning.palette.text_faint).bg(app.tuning.palette.selection_bg),
        ))),
        hint_rect,
    );
    // Text cursor sits right after the query (input is "! {query} ").
    let curx = x + 2 + query.chars().count() as u16;
    if curx < x + width {
        frame.set_cursor_position((curx, input_y));
    }
}

// ── Ask panel (LLM answer, grows upward from control bar) ─────────────────────

fn render_ask_panel(frame: &mut Frame, app: &App, pane_area: Rect, bar_area: Rect) {
    let width = bar_area.width.saturating_sub(2).max(10) as usize;
    let sand = app.tuning.palette.accent_bright;
    let mut lines: Vec<Line> = Vec::new();

    // The conversation transcript.
    for (role, text) in &app.agent_history {
        let (tag, tag_style) = if role == "user" {
            ("you  › ", Style::default().fg(sand).add_modifier(Modifier::BOLD))
        } else {
            ("mars › ", Style::default().fg(app.tuning.palette.accent).add_modifier(Modifier::BOLD))
        };
        for (i, wrapped) in wrap_text(text, width.saturating_sub(7)).into_iter().enumerate() {
            let prefix = if i == 0 { tag } else { "       " };
            lines.push(Line::from(vec![
                Span::styled(prefix, tag_style),
                Span::styled(wrapped, Style::default().fg(app.tuning.palette.text)),
            ]));
        }
    }
    // The streamed reply-in-progress renders as a live assistant turn; the
    // final Answer replaces it (directive stripped, pushed into history).
    if let Some(partial) = app.agent_partial.as_ref().filter(|p| !p.is_empty()) {
        let tag_style =
            Style::default().fg(app.tuning.palette.accent).add_modifier(Modifier::BOLD);
        for (i, wrapped) in wrap_text(partial, width.saturating_sub(7)).into_iter().enumerate() {
            let prefix = if i == 0 { "mars › " } else { "       " };
            lines.push(Line::from(vec![
                Span::styled(prefix, tag_style),
                Span::styled(wrapped, Style::default().fg(app.tuning.palette.text)),
            ]));
        }
    }
    if app.agent_pending {
        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let speed = app.tuning.spinner_speed_ticks;
        let sp = frames[(app.frame_tick / speed % frames.len() as u64) as usize];
        lines.push(Line::from(Span::styled(
            format!(" {} thinking…", sp),
            Style::default().fg(sand).add_modifier(Modifier::BOLD),
        )));
    }
    if let Some(notice) = &app.agent_answer {
        for wrapped in wrap_text(notice, width) {
            lines.push(Line::from(Span::styled(
                wrapped,
                Style::default().fg(app.tuning.palette.accent_dark),
            )));
        }
    }
    // A pending selection-refactor takes the confirm slot (Enter replaces the
    // selection — or inserts at the cursor when nothing was selected — reversibly).
    if app.refactor_replacement.is_some() {
        let n = app.refactor_replacement.as_deref().map(|c| c.lines().count()).unwrap_or(0);
        let verb = match app.refactor_target {
            Some((_, s, e)) if s == e => "insert at the cursor",
            _ => "replace the selection",
        };
        lines.push(Line::from(Span::raw("")));
        lines.push(Line::from(Span::styled(
            format!(" ▶ Enter to {verb} ({n} lines) · C-l cancel "),
            Style::default().fg(app.tuning.palette.on_accent).bg(app.tuning.palette.success).add_modifier(Modifier::BOLD),
        )));
    } else if let Some(d) = &app.agent_directive {
        let label = match d {
            crate::agent::AgentDirective::Run(name) => format!(" ▶ Enter to run: {name} "),
            crate::agent::AgentDirective::Type(cmd) => {
                format!(" ▶ Enter to type into terminal: {cmd} ")
            }
            crate::agent::AgentDirective::Open(loc) => format!(" ▶ Enter to open: {loc} "),
            crate::agent::AgentDirective::Need(_) => String::new(), // auto-satisfied, never shown
        };
        lines.push(Line::from(Span::raw("")));
        lines.push(Line::from(Span::styled(
            label,
            Style::default()
                .fg(app.tuning.palette.on_accent)
                .bg(app.tuning.palette.success)
                .add_modifier(Modifier::BOLD),
        )));
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " Ask about what's on your screen — Enter sends · C-l new chat",
            Style::default().fg(app.tuning.palette.text_faint),
        )));
    }

    // Adaptive height: grow to the content, cap at ask_panel_max_pct — the
    // chat hugs the bottom of the screen and never buries the workspace;
    // older turns are one scroll (Up / wheel) away, not more panel.
    let max_h = ((pane_area.height as u32 * app.tuning.ask_panel_max_pct as u32 / 100)
        as u16)
        .max(3);
    let content_h = lines.len() as u16 + 1; // +1 for top border
    let panel_h = content_h.clamp(2, max_h);
    let visible = panel_h.saturating_sub(1) as usize;

    // Scroll: pin to the bottom, offset by ask_scroll (lines up from the end).
    let total = lines.len();
    if total > visible {
        let max_scroll = total - visible;
        let scroll = app.ask_scroll.min(max_scroll);
        let start = max_scroll - scroll;
        let mut view: Vec<Line> = lines.drain(start..start + visible).collect();
        if scroll > 0 {
            // Replace the last line with a "more below" marker.
            if let Some(last) = view.last_mut() {
                *last = Line::from(Span::styled(
                    format!(" ↓ {} more (Down to scroll) ", scroll),
                    Style::default().fg(app.tuning.palette.text_faint),
                ));
            }
        }
        if start > 0 {
            if let Some(first) = view.first_mut() {
                *first = Line::from(Span::styled(
                    format!(" ↑ {} more (Up to scroll) ", start),
                    Style::default().fg(app.tuning.palette.text_faint),
                ));
            }
        }
        lines = view;
    }

    let panel_y = bar_area.y.saturating_sub(panel_h);
    let rect = Rect {
        x: bar_area.x,
        y: panel_y,
        width: bar_area.width,
        height: panel_h,
    };

    clear_panel(frame, app, rect);
    let provider = crate::agent::AgentConfig::from_env().provider;
    let title = if provider == "none" {
        " ✦ ask ".to_string()
    } else {
        format!(" ✦ ask · {} ", provider)
    };
    let block = Block::default()
        .title(Span::styled(
            title,
            Style::default()
                .fg(app.tuning.palette.accent)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
        .border_style(Style::default().fg(app.tuning.palette.accent));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(Paragraph::new(Text::from(lines)), inner);
}

/// Word-wrap `text` to `width` columns, preserving explicit newlines.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.split('\n') {
        if para.trim().is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in para.split_whitespace() {
            if line.is_empty() {
                line = word.to_string();
            } else if line.chars().count() + 1 + word.chars().count() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    out
}
