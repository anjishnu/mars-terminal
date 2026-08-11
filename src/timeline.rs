//! The agent conversation as ROWS — what happened, in order, with structure.
//!
//! [`crate::conv`] reads the same transcript for a different reader: it produces prose for a model
//! to fold into a gist. This produces typed rows for a person to skim on a phone, where a wall of
//! terminal output is the wrong shape and scrolling it with a thumb is worse.
//!
//! The two share a source and nothing else, which is why this is a sibling module rather than a
//! mode of that one: the gist wants meaning and drops detail, a timeline wants the sequence and
//! keeps it.
//!
//! **Nothing here fails on unfamiliar input.** A record shape we do not know becomes
//! [`Row::Unknown`] carrying its own type name, never a parse error and never a dropped line. The
//! transcript belongs to another program that changes on its own schedule; a reader that breaks
//! when Claude Code adds a block type is a reader that breaks on somebody else's release day. The
//! phone can always fall through to the terminal, which is the honest view of anything we cannot
//! name.

use serde_json::Value;

/// How far a tool call got. `Running` is not a guess — it means no result has appeared yet, which
/// on a live transcript is the common and interesting case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Running,
    Ok,
    Failed,
}

impl ToolStatus {
    fn as_str(self) -> &'static str {
        match self {
            ToolStatus::Running => "running",
            ToolStatus::Ok => "ok",
            ToolStatus::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    User { text: String },
    Assistant { text: String },
    Tool { id: String, name: String, summary: String, status: ToolStatus, detail: Option<String> },
    /// The model thinking aloud. Common enough that treating it as unknown would bury the
    /// timeline; muted rather than hidden, because on a phone it is usually skipped and
    /// occasionally the most informative thing there.
    Reasoning { text: String },
    Error { message: String },
    /// A record we do not model, named by its own `type`. Rendered as a muted line rather than
    /// hidden — a gap the reader can see is honest; a silently dropped row is not.
    Unknown { label: String },
}

/// One line of a summary, and a detail that fits on a phone.
///
/// These bounds are not tidiness. The transcript is written by a program that reads whatever is on
/// the machine, so a tool input can carry a megabyte of file content or a terminal escape sequence,
/// and both of those cross to a phone from here.
const SUMMARY_CHARS: usize = 120;
const DETAIL_CHARS: usize = 2048;
/// How much of the file's tail to read. The live transcript of a long session on this machine is
/// 188 MB; reading it whole to show the last twenty rows would stall the bridge every poll.
const TAIL_BYTES: u64 = 512 * 1024;

/// Strip anything that would scramble a phone's layout. Newline and tab survive in detail text
/// because they carry structure; every other control character is removed rather than escaped,
/// since nobody reading a tool summary wants to see `\u{1b}`.
fn clean(s: &str, max: usize) -> String {
    let mut out: String = s
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect();
    out = out.trim().to_string();
    if out.chars().count() > max {
        out = out.chars().take(max.saturating_sub(1)).collect::<String>();
        out.push('…');
    }
    out
}

/// The one thing worth saying about a tool call, in a phone-width line.
///
/// Per tool, because "Bash" alone is useless and the whole input is too much: what a reader wants
/// is the command, the path, the pattern — the argument that identifies THIS call among the twenty
/// like it. Unknown tools fall back to compact JSON, which is ugly and still better than nothing.
fn tool_summary(name: &str, input: &Value) -> String {
    let pick = |k: &str| input.get(k).and_then(|v| v.as_str());
    let salient = match name {
        "Bash" | "BashOutput" => pick("command"),
        "Read" | "Write" | "NotebookEdit" => pick("file_path"),
        "Edit" => pick("file_path"),
        "Grep" => pick("pattern"),
        "Glob" => pick("pattern"),
        "WebFetch" => pick("url"),
        "WebSearch" => pick("query"),
        "Task" | "Agent" => pick("description"),
        "TodoWrite" => None,
        _ => None,
    };
    match salient {
        Some(s) => clean(s, SUMMARY_CHARS),
        None => match input {
            Value::Null => String::new(),
            other => clean(&other.to_string(), SUMMARY_CHARS),
        },
    }
}

/// Text out of a `tool_result`'s content, which is a string on some records and a block list on
/// others. Both shapes are real; neither is documented to us.
fn result_text(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// Claude Code wraps some turns in machinery the human never typed.
fn is_machinery(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with("<local-command") || t.starts_with("<command-") || t.starts_with("Caveat: The messages below")
}

/// Parse a whole transcript body into rows. Separate from the file read so the selfcheck can drive
/// it with a fixture — the mapping is the part that breaks, and it should be testable without a
/// 188 MB file or a live agent.
pub fn rows_from_str(body: &str, limit: usize) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    // Where each tool call landed, so its result can resolve it in place. A result whose call we
    // never saw is ignored rather than invented — with a tail read, the call may simply be above
    // the window.
    let mut pending: Vec<(String, usize)> = Vec::new();

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            // A truncated final line is normal on a file being appended to as we read it.
            continue;
        };
        let kind = v["type"].as_str().unwrap_or("");
        let content = &v["message"]["content"];

        match kind {
            "user" => {
                if let Some(text) = content.as_str() {
                    if !is_machinery(text) {
                        let t = clean(text, DETAIL_CHARS);
                        if !t.is_empty() {
                            rows.push(Row::User { text: t });
                        }
                    }
                    continue;
                }
                for block in content.as_array().into_iter().flatten() {
                    match block["type"].as_str().unwrap_or("") {
                        "tool_result" => {
                            let id = block["tool_use_id"].as_str().unwrap_or_default();
                            let failed = block["is_error"].as_bool().unwrap_or(false);
                            let text = result_text(&block["content"]);
                            if let Some((_, idx)) = pending.iter().find(|(pid, _)| pid == id) {
                                if let Some(Row::Tool { status, detail, .. }) = rows.get_mut(*idx) {
                                    *status = if failed { ToolStatus::Failed } else { ToolStatus::Ok };
                                    let d = clean(&text, DETAIL_CHARS);
                                    if !d.is_empty() {
                                        *detail = Some(d);
                                    }
                                }
                            }
                        }
                        "text" => {
                            let t = clean(block["text"].as_str().unwrap_or(""), DETAIL_CHARS);
                            if !t.is_empty() && !is_machinery(&t) {
                                rows.push(Row::User { text: t });
                            }
                        }
                        _ => {}
                    }
                }
            }
            "assistant" => {
                for block in content.as_array().into_iter().flatten() {
                    match block["type"].as_str().unwrap_or("") {
                        "text" => {
                            let t = clean(block["text"].as_str().unwrap_or(""), DETAIL_CHARS);
                            if !t.is_empty() {
                                rows.push(Row::Assistant { text: t });
                            }
                        }
                        "tool_use" => {
                            let id = block["id"].as_str().unwrap_or_default().to_string();
                            let name = clean(block["name"].as_str().unwrap_or("tool"), 60);
                            let summary = tool_summary(&name, &block["input"]);
                            pending.push((id.clone(), rows.len()));
                            rows.push(Row::Tool {
                                id,
                                name,
                                summary,
                                status: ToolStatus::Running,
                                detail: None,
                            });
                        }
                        "thinking" => {
                            let t = clean(block["thinking"].as_str().unwrap_or(""), DETAIL_CHARS);
                            if !t.is_empty() {
                                rows.push(Row::Reasoning { text: t });
                            }
                        }
                        // Anything else the model emits: named, not dropped.
                        other if !other.is_empty() => {
                            rows.push(Row::Unknown { label: other.to_string() })
                        }
                        _ => {}
                    }
                }
            }
            // A record with no `message` is bookkeeping, not conversation — measured on a live
            // transcript: `relocated`, `agent-name`, `mode`, `permission-mode`, `last-prompt`,
            // `custom-title`, `ai-title`, `file-history-delta`, `attachment`. Showing those as
            // unknown rows would have made the timeline roughly half noise, which is how a
            // "never drop a line" rule turns into a screen nobody reads. The rule survives where
            // it earns its keep: something that IS a message and that we cannot map still shows.
            other if v.get("message").is_some() => rows.push(Row::Unknown { label: clean(other, 40) }),
            _ => {}
        }
    }

    if limit > 0 && rows.len() > limit {
        rows.drain(..rows.len() - limit);
    }
    rows
}

/// The last `limit` rows of a conversation, by id.
///
/// Reads only the tail of the file: transcripts grow without bound and this is polled while a phone
/// is watching. The first line of the window is usually a fragment, and `rows_from_str` drops
/// unparseable lines, so the cost of the cheap read is at most one lost row at the top of a window
/// the reader is scrolling anyway.
pub fn rows_for(chat: &str, limit: usize) -> Option<Vec<Row>> {
    let path = crate::conv::transcript_for(chat)?;
    let len = std::fs::metadata(&path).ok()?.len();
    let from = len.saturating_sub(TAIL_BYTES);
    let bytes = if from == 0 {
        std::fs::read(&path).ok()?
    } else {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&path).ok()?;
        f.seek(SeekFrom::Start(from)).ok()?;
        let mut buf = Vec::new();
        f.take(TAIL_BYTES + 1).read_to_end(&mut buf).ok()?;
        buf
    };
    Some(rows_from_str(&String::from_utf8_lossy(&bytes), limit))
}

/// Rows as the phone receives them. A flat `kind` rather than a nested enum, because the client
/// switches on one field and a wire format that mirrors Rust's enum encoding would make that
/// awkward for no gain.
pub fn rows_json(rows: &[Row]) -> Vec<Value> {
    rows.iter()
        .map(|r| match r {
            Row::User { text } => serde_json::json!({ "kind": "user", "text": text }),
            Row::Assistant { text } => serde_json::json!({ "kind": "assistant", "text": text }),
            Row::Tool { id, name, summary, status, detail } => serde_json::json!({
                "kind": "tool", "id": id, "name": name, "summary": summary,
                "status": status.as_str(), "detail": detail,
            }),
            Row::Reasoning { text } => serde_json::json!({ "kind": "reasoning", "text": text }),
            Row::Error { message } => serde_json::json!({ "kind": "error", "text": message }),
            Row::Unknown { label } => serde_json::json!({ "kind": "unknown", "text": label }),
        })
        .collect()
}
