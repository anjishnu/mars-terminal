use ropey::Rope;
use std::path::PathBuf;

pub type BufferId = usize;

pub struct Buffer {
    pub id: BufferId,
    pub name: String,
    pub path: Option<PathBuf>,
    pub rope: Rope,
    pub modified: bool,
    /// Monotonic edit counter — bumped on every content change. The syntax
    /// highlighter keys its cache on this, so a stale (pre-edit) highlight is
    /// shown until a fresh one for the current `rev` lands.
    pub rev: u64,
    undo_stack: Vec<Rope>,
    redo_stack: Vec<Rope>,
}

impl Buffer {
    pub fn new_scratch(id: BufferId) -> Self {
        Buffer {
            id,
            name: format!("*scratch*"),
            path: None,
            rope: Rope::new(),
            modified: false,
            rev: 0,
            undo_stack: vec![],
            redo_stack: vec![],
        }
    }

    pub fn from_file(id: BufferId, path: PathBuf) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(&path)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Ok(Buffer {
            id,
            name,
            path: Some(path),
            rope: Rope::from_str(&content),
            modified: false,
            rev: 0,
            undo_stack: vec![],
            redo_stack: vec![],
        })
    }

    pub fn save(&mut self) -> anyhow::Result<()> {
        if let Some(path) = &self.path {
            std::fs::write(path, self.rope.to_string())?;
            self.modified = false;
        }
        Ok(())
    }

    pub fn save_as(&mut self, path: PathBuf) -> anyhow::Result<()> {
        std::fs::write(&path, self.rope.to_string())?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        self.name = name;
        self.path = Some(path);
        self.modified = false;
        Ok(())
    }

    /// Record a content edit: flag the buffer dirty and bump the revision the
    /// syntax highlighter caches against. Call after any rope mutation.
    pub fn mark_edited(&mut self) {
        self.modified = true;
        self.rev += 1;
    }

    /// Snapshot the current state so the next `undo()` can return to it.
    /// Call before any user-visible edit batch (entering insert mode, dd, etc.)
    pub fn checkpoint(&mut self) {
        self.undo_stack.push(self.rope.clone());
        self.redo_stack.clear();
    }

    pub fn undo(&mut self) -> bool {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.rope.clone());
            self.rope = prev;
            self.modified = true;
            self.rev += 1;
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.rope.clone());
            self.rope = next;
            self.modified = true;
            self.rev += 1;
            true
        } else {
            false
        }
    }

    /// (steps you can undo, steps you can redo) — for the time-travel indicator.
    pub fn undo_depth(&self) -> (usize, usize) {
        (self.undo_stack.len(), self.redo_stack.len())
    }

    pub fn line_count(&self) -> usize {
        let n = self.rope.len_lines();
        if n == 0 {
            return 1;
        }
        let len = self.rope.len_chars();
        if len > 0 && self.rope.char(len - 1) == '\n' {
            n.saturating_sub(1).max(1)
        } else {
            n
        }
    }

    pub fn line_str(&self, row: usize) -> String {
        if row >= self.rope.len_lines() {
            return String::new();
        }
        let s = self.rope.line(row).to_string();
        s.trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_string()
    }

    pub fn line_len(&self, row: usize) -> usize {
        self.line_str(row).chars().count()
    }

    /// Rope char index for a logical (row, col) position.
    pub fn char_at(&self, row: usize, col: usize) -> usize {
        if self.rope.len_chars() == 0 {
            return 0;
        }
        let line_count = self.rope.len_lines();
        if row >= line_count {
            return self.rope.len_chars();
        }
        let line_start = self.rope.line_to_char(row);
        let safe_col = col.min(self.line_len(row));
        line_start + safe_col
    }
}
