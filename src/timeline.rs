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

/// A repository change, as small as it can be while still being checkable.
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub before: Option<String>,
    pub after: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    User { text: String },
    Assistant { text: String },
    Tool {
        id: String,
        name: String,
        summary: String,
        status: ToolStatus,
        detail: Option<String>,
        /// What an edit replaced, and with what. Carried only for the tools that CHANGE the repo,
        /// because those are the rows a supervisor actually audits — "it edited three files" is
        /// worth nothing without "and here is what it put there". Everything else keeps `detail`,
        /// which is the tool's own output.
        change: Option<Change>,
        /// Seconds between the call and its result. A supervisor's second question about any
        /// command is how long it took, and the transcript timestamps both ends, so answering it
        /// costs a subtraction.
        secs: Option<u64>,
    },
    /// The model thinking aloud. Common enough that treating it as unknown would bury the
    /// timeline; muted rather than hidden, because on a phone it is usually skipped and
    /// occasionally the most informative thing there.
    Reasoning { text: String },
    /// The agent's plan, as a list rather than as a paragraph about a list. This is the single
    /// most useful thing on the screen when the question is "does it understand the job", and it
    /// arrives as a tool call that would otherwise render as `TodoWrite · {…}`.
    Todo { items: Vec<(String, String)> },
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

/// How much of a MESSAGE survives, as opposed to a tool's output.
///
/// These shared a cap at 2048, which meant the conversation view could not show the end of a long
/// reply: the agent finished, the pane went still, and the last thing on screen trailed off into
/// an ellipsis mid-sentence with no way to reach the rest. Scrolling did not help, because the
/// text had been cut before it left this machine. A tool's output genuinely wants clipping — it is
/// a log, and the reader wants the shape of it. A message is the thing they came to read.
const PROSE_CHARS: usize = 24_000;

/// How many of the newest messages keep their full length.
///
/// This payload is re-sent every few seconds while a phone is watching, so a whole transcript at
/// full length is not a phone-sized thing to send. Reading happens at the END, so that is where
/// the budget goes: recent messages arrive whole, older ones keep the clip they always had, and
/// what you are actually reading is never the truncated one.
const FULL_PROSE_ROWS: usize = 8;
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
        Some(s) => one_line(s, SUMMARY_CHARS),
        None => match input {
            Value::Null => String::new(),
            other => one_line(&other.to_string(), SUMMARY_CHARS),
        },
    }
}

/// A summary is one line, whatever the input was.
///
/// Commands are routinely multi-line — a heredoc, a shell loop — and truncating at the first
/// newline showed `cd ~/Code/replyguy-web` for a call whose actual work was the forty lines below
/// it. Collapsing runs of whitespace keeps the whole command in view up to the cap, so the summary
/// says what the call DID rather than where it started.
fn one_line(s: &str, max: usize) -> String {
    let collapsed = s.split_whitespace().collect::<Vec<_>>().join(" ");
    clean(&collapsed, max)
}

/// Seconds out of an ISO-8601 stamp, without a date library.
///
/// Only ever subtracted from another stamp minutes away, so days and leap seconds do not arise —
/// and a stamp we cannot read yields `None`, which renders as no duration rather than as a wrong
/// one.
fn epoch_secs(iso: &str) -> Option<u64> {
    // 2026-08-12T20:50:21.106Z
    let (date, rest) = iso.split_once('T')?;
    let mut d = date.split('-');
    let (y, mo, da): (u64, u64, u64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let mut t = rest.trim_end_matches('Z').split(':');
    let (h, mi): (u64, u64) = (t.next()?.parse().ok()?, t.next()?.parse().ok()?);
    let se: u64 = t.next()?.split('.').next()?.parse().ok()?;
    // Days since an arbitrary epoch — only differences are ever used.
    let days = y * 365 + y / 4 + mo * 31 + da;
    Some(((days * 24 + h) * 60 + mi) * 60 + se)
}

/// Bounded, because a written file can be a megabyte and this crosses to a phone.
const CHANGE_CHARS: usize = 1200;

/// The plan out of a `TodoWrite`, if that is what this is.
///
/// Shapes vary by version — `todos` or `items`, `content` or `text` — so each is tried rather than
/// assumed, and anything unrecognised falls through to being an ordinary tool row. A plan we
/// cannot read is not worth a broken one.
fn todo_items(name: &str, input: &Value) -> Option<Vec<(String, String)>> {
    if name != "TodoWrite" {
        return None;
    }
    let arr = input.get("todos").or_else(|| input.get("items"))?.as_array()?;
    let items: Vec<(String, String)> = arr
        .iter()
        .filter_map(|t| {
            let text = t.get("content").or_else(|| t.get("text")).or_else(|| t.get("activeForm"))?.as_str()?;
            let status = t.get("status").and_then(|s| s.as_str()).unwrap_or("pending");
            Some((clean(text, 160), clean(status, 20)))
        })
        .collect();
    (!items.is_empty()).then_some(items)
}

/// What this call changed, for the tools that change things.
///
/// `Edit` carries both halves and is a real diff. `Write` has no before — a new file, or a whole
/// replacement — and saying so with `None` is more honest than inventing an empty string, which a
/// reader would take to mean "the file was empty".
fn change_of(name: &str, input: &Value) -> Option<Change> {
    let get = |k: &str| input.get(k).and_then(|v| v.as_str());
    match name {
        "Edit" => Some(Change {
            before: get("old_string").map(|s| clean(s, CHANGE_CHARS)),
            after: clean(get("new_string")?, CHANGE_CHARS),
        }),
        "Write" => Some(Change { before: None, after: clean(get("content")?, CHANGE_CHARS) }),
        _ => None,
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
    // `<system-reminder>` is the one that actually cost us. Claude Code files harness injections —
    // session-name hints, tool nudges, memory recalls — as ordinary `user` messages, so they reach
    // here indistinguishable from something the person typed. Measured on a live pane: three of the
    // six rows in the conversation window were reminders and a slash command, which is why the
    // "what is this agent doing" summary read as noise. A window is a budget, and machinery in it
    // is not merely untidy — it is spent instead of the last real exchange.
    //
    // A SLASH COMMAND IS ALSO NOT PROSE. `/compact`, `/clear` and friends are recorded as user
    // messages but they are addressed to the harness, not to the agent; showing one as the human's
    // latest word says the conversation stopped there when it did not.
    t.starts_with("<local-command")
        || t.starts_with("<command-")
        || t.starts_with("<system-reminder")
        || t.starts_with("Caveat: The messages below")
        || is_slash_command(t)
}

/// A bare slash command — `/compact`, `/clear`, `/model sonnet`. Deliberately narrow: only a
/// leading `/` followed by a word, so a message that merely BEGINS with a path (`/Users/... is
/// wrong`) or asks about a command mid-sentence is left alone. Prose that opens with a slash and
/// then keeps going is prose.
fn is_slash_command(t: &str) -> bool {
    let Some(rest) = t.strip_prefix('/') else { return false };
    let mut lines = rest.lines();
    let Some(first) = lines.next() else { return false };
    if lines.next().is_some() {
        return false; // multi-line: somebody is talking, not invoking
    }
    let mut words = first.split_whitespace();
    let Some(verb) = words.next() else { return false };
    verb.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') && words.count() <= 2
}

/// Parse a whole transcript body into rows. Separate from the file read so the selfcheck can drive
/// it with a fixture — the mapping is the part that breaks, and it should be testable without a
/// 188 MB file or a live agent.
pub fn rows_from_str(body: &str, limit: usize) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    // Where each tool call landed, so its result can resolve it in place. A result whose call we
    // never saw is ignored rather than invented — with a tail read, the call may simply be above
    // the window.
    let mut pending: Vec<(String, usize, Option<u64>)> = Vec::new();

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
        let at = v["timestamp"].as_str().and_then(epoch_secs);

        match kind {
            "user" => {
                if let Some(text) = content.as_str() {
                    if !is_machinery(text) {
                        let t = clean(text, PROSE_CHARS);
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
                            if let Some((_, idx, started)) = pending.iter().find(|(pid, _, _)| pid == id) {
                                let took = started.zip(at).map(|(a, b)| b.saturating_sub(a));
                                if let Some(Row::Tool { status, detail, secs, .. }) = rows.get_mut(*idx) {
                                    *status = if failed { ToolStatus::Failed } else { ToolStatus::Ok };
                                    *secs = took;
                                    let d = clean(&text, DETAIL_CHARS);
                                    if !d.is_empty() {
                                        *detail = Some(d);
                                    }
                                }
                            }
                        }
                        "text" => {
                            let t = clean(block["text"].as_str().unwrap_or(""), PROSE_CHARS);
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
                            let t = clean(block["text"].as_str().unwrap_or(""), PROSE_CHARS);
                            if !t.is_empty() {
                                rows.push(Row::Assistant { text: t });
                            }
                        }
                        "tool_use" => {
                            let id = block["id"].as_str().unwrap_or_default().to_string();
                            let name = clean(block["name"].as_str().unwrap_or("tool"), 60);
                            let summary = tool_summary(&name, &block["input"]);
                            let change = change_of(&name, &block["input"]);
                            if let Some(items) = todo_items(&name, &block["input"]) {
                                rows.push(Row::Todo { items });
                                continue;
                            }
                            pending.push((id.clone(), rows.len(), at));
                            rows.push(Row::Tool {
                                id,
                                name,
                                summary,
                                status: ToolStatus::Running,
                                detail: None,
                                change,
                                secs: None,
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
    // The newest messages keep their full length; everything above them goes back to the clip.
    // Done after the window is chosen, so "newest" means newest in what is actually being sent.
    let mut kept = 0usize;
    for row in rows.iter_mut().rev() {
        let text = match row {
            Row::Assistant { text } | Row::User { text } => text,
            _ => continue,
        };
        kept += 1;
        if kept > FULL_PROSE_ROWS && text.chars().count() > DETAIL_CHARS {
            *text = clean(text, DETAIL_CHARS);
        }
    }
    rows
}

/// The last `limit` rows of a conversation, by id.
///
/// Reads only the tail of the file: transcripts grow without bound and this is polled while a phone
/// is watching. The first line of the window is usually a fragment, and `rows_from_str` drops
/// unparseable lines, so the cost of the cheap read is at most one lost row at the top of a window
/// the reader is scrolling anyway.
///
/// `None` means ONE thing: no file on this machine carries that id. Every other failure happens
/// after the file has been found, and answering those with `None` too made the caller report "no
/// transcript found for this conversation yet" — the single claim we know to be false, because the
/// file is right there and could not be read. A transcript nobody has started and one this process
/// cannot open look identical to a reader and deserve different answers, so the second becomes a
/// row that says so.
pub fn rows_for(chat: &str, limit: usize) -> Option<Vec<Row>> {
    Some(rows_for_path(&crate::conv::transcript_for(chat)?, limit))
}

/// The rows of a transcript already located, which is where the reading actually happens.
///
/// Separate from `rows_for` so the failure above can be exercised at all: `rows_for` finds its file
/// by searching `~/.claude/projects`, and a check that has to plant a file in the developer's real
/// Claude Code directory to prove anything is a check nobody will keep.
pub(crate) fn rows_for_path(path: &std::path::Path, limit: usize) -> Vec<Row> {
    match read_tail(path) {
        Ok(bytes) => rows_from_str(&String::from_utf8_lossy(&bytes), limit),
        Err(e) => vec![Row::Error { message: format!("this conversation could not be read: {e}") }],
    }
}

/// The last `TAIL_BYTES` of a file, or the whole of one shorter than that.
///
/// Split out so the failures above are one `?`-chain rather than five `.ok()?`s that all mean
/// different things and collapse to the same answer.
fn read_tail(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let from = std::fs::metadata(path)?.len().saturating_sub(TAIL_BYTES);
    if from == 0 {
        return std::fs::read(path);
    }
    let mut f = std::fs::File::open(path)?;
    f.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::new();
    f.take(TAIL_BYTES + 1).read_to_end(&mut buf)?;
    Ok(buf)
}

/// Rows as the phone receives them. A flat `kind` rather than a nested enum, because the client
/// switches on one field and a wire format that mirrors Rust's enum encoding would make that
/// awkward for no gain.
pub fn rows_json(rows: &[Row]) -> Vec<Value> {
    rows.iter()
        .map(|r| match r {
            Row::User { text } => serde_json::json!({ "kind": "user", "text": text }),
            Row::Assistant { text } => serde_json::json!({ "kind": "assistant", "text": text }),
            Row::Todo { items } => serde_json::json!({
                "kind": "todo",
                "todos": items.iter().map(|(t, st)| serde_json::json!({ "text": t, "status": st })).collect::<Vec<_>>(),
            }),
            Row::Tool { id, name, summary, status, detail, change, secs } => serde_json::json!({
                "kind": "tool", "id": id, "name": name, "summary": summary,
                "status": status.as_str(), "detail": detail, "secs": secs,
                "change": change.as_ref().map(|c| serde_json::json!({ "before": c.before, "after": c.after })),
            }),
            Row::Reasoning { text } => serde_json::json!({ "kind": "reasoning", "text": text }),
            Row::Error { message } => serde_json::json!({ "kind": "error", "text": message }),
            Row::Unknown { label } => serde_json::json!({ "kind": "unknown", "text": label }),
        })
        .collect()
}

/// A conversation the captain could bind to a pane: what it is called, and when it was last
/// touched. The title is Claude Code's own — the name the captain set with `/rename` if there is
/// one, else the generated `aiTitle` — which is a far better thing to choose from than a uuid, and
/// reading one bounded window for it costs nothing even on a 200 MB transcript.
///
/// Returns the list AND whether it is scoped to the directory that was asked about, because the
/// fallback below silently changes what the list MEANS. "Conversations from this workspace" and
/// "every conversation on this machine" are different offers, and a reader shown the second while
/// expecting the first will bind a stranger's conversation to their pane and have no way to know.
/// The caller says which one it handed over; only the picker can say it out loud.
pub fn candidates(cwd: Option<&str>, limit: usize) -> (Vec<serde_json::Value>, bool) {
    let Some(home) = crate::sys::paths::home_dir() else { return (Vec::new(), cwd.is_none()) };
    let root = home.join(".claude").join("projects");
    let want = cwd.map(crate::conv::project_slug);

    let mut found: Vec<(u64, std::path::PathBuf, String)> = Vec::new();
    for dir in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let dname = dir.file_name().to_string_lossy().to_string();
        // Same directory first. When nothing matches — a conversation started elsewhere and
        // `cd`-ed, a slug we cannot reproduce — fall back to everything rather than to an empty
        // list, because a wrong-looking list is still a choosable list and an empty one is a dead
        // end wearing an explanation.
        if let Some(w) = &want {
            if &dname != w {
                continue;
            }
        }
        for f in std::fs::read_dir(dir.path()).into_iter().flatten().flatten() {
            let path = f.path();
            if path.extension().and_then(|x| x.to_str()) != Some("jsonl") {
                continue;
            }
            let mtime = std::fs::metadata(&path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            found.push((mtime, path, dname.clone()));
        }
    }
    // WIDENED, AND SAID SO. The fallback is still right — a wrong-looking list is choosable and an
    // empty one is a dead end wearing an explanation — but it is no longer silent, because the
    // reader is the only one who can tell a stranger's conversation from theirs.
    if found.is_empty() && want.is_some() {
        return (candidates(None, limit).0, false);
    }

    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.truncate(limit);
    let list: Vec<serde_json::Value> = found
        .into_iter()
        .filter_map(|(mtime, path, dname)| {
            let id = path.file_stem()?.to_string_lossy().to_string();
            if !crate::session::valid_chat_id(&id) {
                return None;
            }
            serde_json::json!({
                "chat": id, "title": title_of(&path), "dir": dname, "mtime": mtime,
            })
            .into()
        })
        .collect();
    (list, want.is_some())
}

/// WHICH conversation a pane is showing: the project it belongs to, and its name.
///
/// The timeline could always render the right rows and still not say whose they were, and a
/// transcript of the wrong conversation reads as "my pane stopped updating" rather than as "you
/// are looking at something else". Nothing on this path ever checks that a bound conversation
/// belongs to the pane holding it, so the reader is the last line of defence — and they cannot be
/// that while the surface stays silent about what it picked.
///
/// The directory comes from the transcript's own location rather than from anything the caller
/// believes, which is the point: it is the one fact that can DISAGREE with the pane.
pub fn identity_of(chat: &str) -> Option<(String, String)> {
    let path = crate::conv::transcript_for(chat)?;
    let dir = path.parent()?.file_name()?.to_string_lossy().to_string();
    Some((dir, title_of(&path)))
}

/// The conversation's own title, from the LAST 64 KB.
///
/// Two faults lived here and either one alone hid a rename completely.
///
/// The window was the file's HEAD. A transcript is an APPEND-ONLY log, so its head holds the name
/// the conversation was born with and can never hold any later one: on a 200 MB transcript a
/// `/rename` was invisible forever, and the picker went on offering a name the captain had already
/// replaced. The comment claimed to take "the freshest opinion in that window", which was true and
/// useless — the freshest opinion in the oldest window is still the oldest opinion.
///
/// And `aiTitle` was allowed to win. It is the model's generated summary, rewritten as the
/// conversation drifts; `/rename` writes `customTitle` and `agentName` and deliberately leaves
/// `aiTitle` alone. Last-record-wins therefore handed the host's guess the final say over the
/// captain's answer, which inverts the one rule this surface exists to honour.
///
/// So: read the tail, and let a name a PERSON set outrank one the host guessed regardless of which
/// came last. The tail is the same single `read` the head cost, and a file shorter than the window
/// is read whole.
pub(crate) fn title_of(path: &std::path::Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    const WINDOW: u64 = 64 * 1024;
    let mut buf = Vec::new();
    if let Ok(mut f) = std::fs::File::open(path) {
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let _ = f.seek(SeekFrom::Start(len.saturating_sub(WINDOW)));
        let _ = f.take(WINDOW).read_to_end(&mut buf);
    }
    // Seeking to a byte offset lands mid-line unless we are at the start of the file; that first
    // partial line fails to parse and is skipped, which is all the trimming it needs.
    let tail = String::from_utf8_lossy(&buf);
    let (mut title, mut set_by_hand) = (String::new(), false);
    for line in tail.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        for (key, by_hand) in [("customTitle", true), ("agentName", true), ("aiTitle", false)] {
            if let Some(t) = v.get(key).and_then(|x| x.as_str()).filter(|t| !t.is_empty()) {
                if by_hand || !set_by_hand {
                    title = clean(t, 80);
                    set_by_hand |= by_hand;
                }
            }
        }
    }
    title
}

