use crate::pane::PaneId;

/// Percentage of the split given to the first child (top/left). Clamped so a
/// pane can never fully vanish.
const RATIO_MIN: u16 = 15;
const RATIO_MAX: u16 = 85;
const RATIO_DEFAULT: u16 = 50;

#[derive(Debug, Clone)]
pub enum PaneLayout {
    Single(PaneId),
    HSplit {
        top: Box<PaneLayout>,
        bottom: Box<PaneLayout>,
        /// Percent of the height given to `top`.
        ratio: u16,
    },
    VSplit {
        left: Box<PaneLayout>,
        right: Box<PaneLayout>,
        /// Percent of the width given to `left`.
        ratio: u16,
    },
}

impl PaneLayout {
    pub fn pane_ids(&self) -> Vec<PaneId> {
        match self {
            PaneLayout::Single(id) => vec![*id],
            PaneLayout::HSplit { top, bottom, .. } => {
                let mut v = top.pane_ids();
                v.extend(bottom.pane_ids());
                v
            }
            PaneLayout::VSplit { left, right, .. } => {
                let mut v = left.pane_ids();
                v.extend(right.pane_ids());
                v
            }
        }
    }

    pub fn count(&self) -> usize {
        self.pane_ids().len()
    }

    fn contains(&self, id: PaneId) -> bool {
        self.pane_ids().contains(&id)
    }

    /// Split the pane with `focused` into top/bottom, inserting `new_id` at bottom.
    pub fn hsplit(&mut self, focused: PaneId, new_id: PaneId) -> bool {
        match self {
            PaneLayout::Single(id) if *id == focused => {
                let old = *id;
                *self = PaneLayout::HSplit {
                    top: Box::new(PaneLayout::Single(old)),
                    bottom: Box::new(PaneLayout::Single(new_id)),
                    ratio: RATIO_DEFAULT,
                };
                true
            }
            PaneLayout::HSplit { top, bottom, .. } => {
                top.hsplit(focused, new_id) || bottom.hsplit(focused, new_id)
            }
            PaneLayout::VSplit { left, right, .. } => {
                left.hsplit(focused, new_id) || right.hsplit(focused, new_id)
            }
            _ => false,
        }
    }

    /// Split the pane with `focused` into left/right, inserting `new_id` at right.
    pub fn vsplit(&mut self, focused: PaneId, new_id: PaneId) -> bool {
        match self {
            PaneLayout::Single(id) if *id == focused => {
                let old = *id;
                *self = PaneLayout::VSplit {
                    left: Box::new(PaneLayout::Single(old)),
                    right: Box::new(PaneLayout::Single(new_id)),
                    ratio: RATIO_DEFAULT,
                };
                true
            }
            PaneLayout::HSplit { top, bottom, .. } => {
                top.vsplit(focused, new_id) || bottom.vsplit(focused, new_id)
            }
            PaneLayout::VSplit { left, right, .. } => {
                left.vsplit(focused, new_id) || right.vsplit(focused, new_id)
            }
            _ => false,
        }
    }

    /// Grow/shrink the split boundary nearest the focused pane. `delta` is a
    /// percentage-point nudge; positive grows the side the focused pane is on.
    /// Returns true if any boundary moved.
    pub fn resize(&mut self, focused: PaneId, delta: i16) -> bool {
        match self {
            PaneLayout::Single(_) => false,
            PaneLayout::HSplit { top, bottom, ratio } => {
                // Deepest-first: adjust the innermost split around the focus.
                if top.resize(focused, delta) || bottom.resize(focused, delta) {
                    return true;
                }
                let focus_first = top.contains(focused);
                if focus_first || bottom.contains(focused) {
                    let signed = if focus_first { delta } else { -delta };
                    *ratio = (*ratio as i16 + signed).clamp(RATIO_MIN as i16, RATIO_MAX as i16) as u16;
                    return true;
                }
                false
            }
            PaneLayout::VSplit { left, right, ratio } => {
                if left.resize(focused, delta) || right.resize(focused, delta) {
                    return true;
                }
                let focus_first = left.contains(focused);
                if focus_first || right.contains(focused) {
                    let signed = if focus_first { delta } else { -delta };
                    *ratio = (*ratio as i16 + signed).clamp(RATIO_MIN as i16, RATIO_MAX as i16) as u16;
                    return true;
                }
                false
            }
        }
    }

    /// Set one split's ratio outright, addressed by the path from the root (0 =
    /// first child, 1 = second). A dragged border must move *that* boundary — the
    /// keyboard's `resize` picks the innermost split around the focus, which is a
    /// different split whenever the pane you grabbed the edge of sits inside a
    /// nested one. Absolute rather than incremental so the divider tracks the
    /// pointer exactly instead of accumulating rounding error.
    pub fn set_ratio(&mut self, path: &[u8], ratio: u16) -> bool {
        let r = ratio.clamp(RATIO_MIN, RATIO_MAX);
        match (self, path.split_first()) {
            (PaneLayout::HSplit { ratio: cur, .. }, None)
            | (PaneLayout::VSplit { ratio: cur, .. }, None) => {
                *cur = r;
                true
            }
            (PaneLayout::HSplit { top, bottom, .. }, Some((&step, rest)))
            | (PaneLayout::VSplit { left: top, right: bottom, .. }, Some((&step, rest))) => {
                if step == 0 { top.set_ratio(rest, r) } else { bottom.set_ratio(rest, r) }
            }
            _ => false,
        }
    }

    /// Remove `id`, promoting its sibling. Returns false if this is the only pane.
    pub fn remove(&mut self, id: PaneId) -> bool {
        match self {
            PaneLayout::Single(_) => false,
            PaneLayout::HSplit { top, bottom, .. } => {
                if matches!(top.as_ref(), PaneLayout::Single(x) if *x == id) {
                    *self = (**bottom).clone();
                    return true;
                }
                if matches!(bottom.as_ref(), PaneLayout::Single(x) if *x == id) {
                    *self = (**top).clone();
                    return true;
                }
                top.remove(id) || bottom.remove(id)
            }
            PaneLayout::VSplit { left, right, .. } => {
                if matches!(left.as_ref(), PaneLayout::Single(x) if *x == id) {
                    *self = (**right).clone();
                    return true;
                }
                if matches!(right.as_ref(), PaneLayout::Single(x) if *x == id) {
                    *self = (**left).clone();
                    return true;
                }
                left.remove(id) || right.remove(id)
            }
        }
    }

    pub fn next_pane(&self, current: PaneId) -> PaneId {
        let ids = self.pane_ids();
        if ids.len() <= 1 {
            return current;
        }
        let pos = ids.iter().position(|&id| id == current).unwrap_or(0);
        ids[(pos + 1) % ids.len()]
    }

    pub fn prev_pane(&self, current: PaneId) -> PaneId {
        let ids = self.pane_ids();
        if ids.len() <= 1 {
            return current;
        }
        let pos = ids.iter().position(|&id| id == current).unwrap_or(0);
        ids[if pos == 0 { ids.len() - 1 } else { pos - 1 }]
    }
}
