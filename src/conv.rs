//! The conversation itself, rather than the terminal's rendering of it.
//!
//! An agent pane shows a Claude Code session, and until now everything downstream — the manager's
//! summaries, Rover's chat — read that pane's SCREEN: fifty lines, reflowed to whatever width the
//! phone had, with everything above the fold simply gone. Meanwhile the whole conversation sits on
//! disk as JSONL, with roles intact and nothing truncated.
//!
//! So this reads the file. The shape it keeps is deliberately small:
//!
//! - a **gist** — what the conversation is about and what is still open, written by the manager
//! - a **delta** — the raw exchanges that have appeared since the gist was last written
//! - a **cursor** — how far into the transcript the gist already accounts for
//!
//! Every reader gets `gist + delta`, so the offline manager and the conversational one hold the
//! same picture, and a run folds its delta into the gist and moves the cursor. That is what keeps
//! the cost flat: the gist stays one paragraph however long the conversation runs, and only the
//! new lines are ever re-read.
//!
//! The transcript is found BY ID, never by rebuilding Claude Code's directory naming from a path —
//! the id is unique, and a derived path is a guess that breaks the first time the rule changes.

use std::path::{Path, PathBuf};

/// What the last fold left behind for one pane.
#[derive(Default)]
pub struct Conv {
    /// Bytes already accounted for by the gist.
    pub offset: u64,
    /// Bytes last SHOWN to a reader. Becomes `offset` once the gist is seen to have been rewritten.
    pub shown: u64,
    /// When the gist file last changed — how a real fold is told from a run that never got there.
    pub gist_mtime: u64,
    /// Transcript this state belongs to. A resumed conversation gets a new file, and folding a
    /// new transcript's bytes at an old file's offset would silently skip its opening.
    pub chat: String,
}

/// The newest conversation Claude Code has recorded for work done in `dir`.
///
/// **The manager agent is already a Claude session.** `run.sh` cds into `~/.mars/manager` and runs
/// `claude` there, so its turns land in `~/.claude/projects/<encoded dir>/<session>.jsonl` in
/// exactly the format every agent pane's conversation uses. Nothing has to be built to watch it —
/// only found, which is what this does.
///
/// Newest by mtime rather than by name: the file name is a uuid and carries no order, and the
/// question is "what is it doing now", which is a property of writes.
/// Returns the id AND when it was last written — because a row that says "live" about a session
/// that last ran hours ago is a lie of exactly the kind this surface exists to stop telling.
pub fn newest_for_dir(dir: &Path) -> Option<(String, u64)> {
    let root = projects_root()?;
    // Claude Code's encoding: every `/` and `.` becomes `-`. `/Users/x/.mars/manager` →
    // `-Users-x--mars-manager`, which is why the double dash is not a typo.
    let encoded: String = dir
        .display()
        .to_string()
        .chars()
        .map(|c| if c == '/' || c == '.' { '-' } else { c })
        .collect();
    let mut best: Option<(std::time::SystemTime, String)> = None;
    for e in std::fs::read_dir(root.join(encoded)).ok()?.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(stem) = p.file_stem().and_then(|x| x.to_str()) else { continue };
        let Ok(m) = e.metadata().and_then(|m| m.modified()) else { continue };
        if best.as_ref().map(|(t, _)| m > *t).unwrap_or(true) {
            best = Some((m, stem.to_string()));
        }
    }
    best.map(|(t, id)| {
        let secs = t
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        (id, secs)
    })
}

fn projects_root() -> Option<PathBuf> {
    crate::sys::paths::home_dir().map(|h| h.join(".claude").join("projects"))
}

/// Locate a transcript by conversation id.
pub fn transcript_for(chat: &str) -> Option<PathBuf> {
    if chat.is_empty() {
        return None;
    }
    let root = projects_root()?;
    let want = format!("{chat}.jsonl");
    std::fs::read_dir(root).ok()?.flatten().find_map(|e| {
        let p = e.path().join(&want);
        p.is_file().then_some(p)
    })
}

/// How many bytes this conversation's transcript holds. `0` when there is no transcript.
///
/// The size, not the mtime. A pane asserts which conversation it holds when Mars typed
/// `--resume <id>` into it, and the only thing that distinguishes "the resume took" from "it did
/// not" is whether that transcript GREW afterwards. Clock-based tests all fail here, and one of
/// them was tried: comparing the mtime against the daemon's start looks right and is not, because
/// the dying agent from the previous daemon flushes its last lines a few seconds AFTER the new one
/// starts. Measured on this machine: daemon up at 10:00:10, the dead conversation's final write at
/// 10:00:16. Six seconds is enough to make a stopped thread look live.
///
/// A byte count has no such window. It either grew or it did not.
pub fn transcript_len(chat: &str) -> u64 {
    transcript_for(chat)
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .unwrap_or(0)
}

fn cursor_path(session_dir: &Path, pane: &str) -> PathBuf {
    session_dir.join("conv").join(format!("{pane}.cursor.json"))
}

/// The gist is MARKDOWN and belongs to the agent — the same shape it writes everywhere else, in a
/// file it owns. The cursor is JSON and belongs to Mars. Splitting them is what makes a failed run
/// safe: the cursor only advances once the gist file has actually changed, so a run that dies
/// after being shown a delta shows it again next time rather than losing it.
fn gist_path(session_dir: &Path, pane: &str) -> PathBuf {
    session_dir.join("conv").join(format!("{pane}.md"))
}

fn mtime(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn load_cursor(session_dir: &Path, pane: &str) -> Conv {
    let Ok(text) = std::fs::read_to_string(cursor_path(session_dir, pane)) else {
        return Conv::default();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Conv::default();
    };
    Conv {
        offset: v["offset"].as_u64().unwrap_or(0),
        shown: v["shown"].as_u64().unwrap_or(0),
        gist_mtime: v["gist_mtime"].as_u64().unwrap_or(0),
        chat: v["chat"].as_str().unwrap_or("").to_string(),
    }
}

fn save_cursor(session_dir: &Path, pane: &str, c: &Conv) {
    let p = cursor_path(session_dir, pane);
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(
        &p,
        serde_json::json!({
            "chat": c.chat, "offset": c.offset, "shown": c.shown, "gist_mtime": c.gist_mtime,
            "updated_ts": crate::worklog::now_secs(),
        })
        .to_string(),
    );
}

/// One readable line from a transcript record, or nothing if it carries no prose.
///
/// A transcript is mostly bookkeeping — modes, snapshots, titles, tool traffic. Only the two
/// message kinds are worth a reader's attention, and an assistant message is a list of blocks of
/// which only the text ones are prose.
fn readable(v: &serde_json::Value) -> Option<String> {
    let who = match v["type"].as_str()? {
        "user" => "captain",
        "assistant" => "agent",
        _ => return None,
    };
    let content = &v["message"]["content"];
    let text = if let Some(s) = content.as_str() {
        s.to_string()
    } else {
        content
            .as_array()?
            .iter()
            .filter(|b| b["type"].as_str() == Some("text"))
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n")
    };
    let text = text.trim();
    // Claude Code wraps some turns in machinery the reader never typed.
    if text.is_empty() || text.starts_with("<local-command") || text.starts_with("<command-") {
        return None;
    }
    Some(format!("{who}: {text}"))
}

/// The gist, plus everything said since the gist was last written.
///
/// Folding is detected, never assumed: if the gist file has changed since the cursor was written,
/// the agent has done its part and the cursor advances to whatever was last shown. So the delta a
/// reader sees is always "since the last real fold", and a run that failed halfway costs a repeat
/// rather than a hole.
///
/// `max_lines` bounds it for the same reason the screen tail is bounded — a pane left running all
/// day must not become an unbounded prompt — and says so when it truncates, because a reader told
/// "here is the conversation" and handed its last third will answer confidently about something
/// settled above.
pub fn gist_and_delta(
    session_dir: &Path,
    pane: &str,
    chat: &str,
    max_lines: usize,
) -> Option<(String, String)> {
    let path = transcript_for(chat)?;
    let mut st = load_cursor(session_dir, pane);
    let gp = gist_path(session_dir, pane);
    let gist = std::fs::read_to_string(&gp).unwrap_or_default();
    let gm = mtime(&gp);

    // A different transcript is a different conversation: read it from the beginning.
    if st.chat != chat {
        st = Conv { chat: chat.to_string(), ..Default::default() };
    } else if gm > st.gist_mtime && st.shown > st.offset {
        // The agent folded. Everything we showed it is now accounted for by the gist.
        st.offset = st.shown;
        st.gist_mtime = gm;
    }

    let bytes = std::fs::read(&path).ok()?;
    let end = bytes.len() as u64;
    let from = st.offset.min(end);
    let mut lines: Vec<String> = String::from_utf8_lossy(&bytes[from as usize..])
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| readable(&v))
        .collect();
    let dropped = lines.len().saturating_sub(max_lines);
    if dropped > 0 {
        lines.drain(..dropped);
        lines.insert(0, format!("[{dropped} earlier exchanges not shown — the gist covers them]"));
    }

    // Record what this reader was shown. The cursor moves only when the gist catches up.
    st.shown = end;
    st.gist_mtime = if gm > st.gist_mtime { gm } else { st.gist_mtime };
    save_cursor(session_dir, pane, &st);

    // Strip frontmatter from the agent's markdown — the reader wants the prose.
    let gist = crate::manager::split_front(&gist).map(|(_, b)| b.to_string()).unwrap_or(gist);
    Some((gist.trim().to_string(), lines.join("\n\n")))
}
