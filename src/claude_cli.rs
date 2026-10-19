//! The Claude Code path's transport: one `claude -p` per model call, on the user's own
//! subscription.
//!
//! Isolated on purpose. A plain `claude -p` is the coding agent: it loads the user's
//! CLAUDE.md, tools, MCP servers and settings, and saves a resumable session for every call.
//! Measured on a real install, that was ~30,000 tokens of context per call, and the global
//! CLAUDE.md was visible to the model. With the flags below the same call is ~600 tokens,
//! about twice as fast, leaves nothing on disk, and the model sees only what Mars sent.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

/// Pure: the arguments for one call. `streaming` asks for per-token events.
pub fn args(model: &str, system_file: &Path, streaming: bool) -> Vec<String> {
    let mut a: Vec<String> = vec!["-p".into(), "--model".into(), model.into()];
    if streaming {
        a.extend(["--output-format", "stream-json", "--include-partial-messages", "--verbose"].map(String::from));
    } else {
        a.extend(["--output-format", "json"].map(String::from));
    }
    a.extend(
        [
            "--tools", "",
            "--no-session-persistence",
            "--setting-sources", "",
            "--strict-mcp-config",
            "--disable-slash-commands",
        ]
        .map(String::from),
    );
    a.push("--system-prompt-file".into());
    a.push(system_file.to_string_lossy().into_owned());
    a
}

/// Variables the child must not inherit. An API key in the environment takes precedence over
/// the claude.ai login, so it would bill the key instead of the subscription (or fail when the
/// key is empty). The session markers make a `claude` started from inside a Claude Code pane
/// believe it is that session's child.
pub const SCRUB: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDECODE",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
    "CLAUDE_CODE_CHILD_SESSION",
];

fn text_of(content: &serde_json::Value) -> String {
    match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Pure: an OpenAI-style message list as (system prompt, one prompt). `claude -p` takes a single
/// turn, so earlier turns are written out as a transcript ahead of the last user message.
pub fn flatten(messages: &[serde_json::Value]) -> (String, String) {
    let mut system = Vec::new();
    let mut turns: Vec<(String, String)> = Vec::new();
    for m in messages {
        let text = text_of(&m["content"]);
        match m["role"].as_str().unwrap_or("user") {
            "system" => system.push(text),
            role => turns.push((role.to_string(), text)),
        }
    }
    let last = match turns.iter().rposition(|(r, _)| r == "user") {
        Some(i) => turns.remove(i).1,
        None => String::new(),
    };
    let prompt = if turns.is_empty() {
        last
    } else {
        let mut p = String::from("The conversation so far:\n\n");
        for (role, text) in &turns {
            let who = if role == "assistant" { "Assistant" } else { "User" };
            p.push_str(&format!("{who}: {text}\n\n"));
        }
        p.push_str("Reply to this, the user's latest message:\n\n");
        p.push_str(&last);
        p
    };
    (system.join("\n\n"), prompt)
}

/// One finished call: the reply and its token counts.
#[derive(Debug, PartialEq)]
pub struct Reply {
    pub text: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

fn tokens(usage: &serde_json::Value) -> (u64, u64) {
    let n = |k: &str| usage[k].as_u64().unwrap_or(0);
    (
        n("input_tokens") + n("cache_creation_input_tokens") + n("cache_read_input_tokens"),
        n("output_tokens"),
    )
}

/// Pure: the `result` record, from `--output-format json` or the last line of a stream. An
/// error record carries its reason in `result`, which is what the classifier needs to see.
pub fn parse_result(v: &serde_json::Value) -> anyhow::Result<Reply> {
    let text = v["result"].as_str().unwrap_or_default().to_string();
    if v["is_error"].as_bool().unwrap_or(false) || v["subtype"].as_str().is_some_and(|s| s != "success") {
        let why = if text.trim().is_empty() { v["subtype"].as_str().unwrap_or("error").to_string() } else { text };
        anyhow::bail!("claude: {}", explain(&why));
    }
    let (prompt_tokens, completion_tokens) = tokens(&v["usage"]);
    Ok(Reply { text, prompt_tokens, completion_tokens })
}

/// Pure: a stream event's visible text, if it is one.
pub fn stream_delta(v: &serde_json::Value) -> Option<String> {
    if v["type"] != "stream_event" || v["event"]["type"] != "content_block_delta" {
        return None;
    }
    let d = &v["event"]["delta"];
    (d["type"] == "text_delta").then(|| d["text"].as_str().unwrap_or_default().to_string())
}

/// Pure: Claude Code's message, worded so `agent::classify` sorts it. Signing out is an auth
/// failure, a used-up plan is exhaustion, and both say what to do.
pub fn explain(raw: &str) -> String {
    let low = raw.to_ascii_lowercase();
    if low.contains("not logged in") || low.contains("/login") || low.contains("invalid api key") || low.contains("oauth") {
        format!("{} (authentication: run `claude` once and sign in)", raw.trim())
    } else if low.contains("usage limit") || low.contains("limit reached") || low.contains("rate limit") || low.contains("quota") {
        format!("{} (usage limit reached on this Claude plan)", raw.trim())
    } else {
        raw.trim().to_string()
    }
}

// How many calls may run at once in this process, and how many have run in the last hour.
struct Gate {
    running: usize,
    recent: Vec<Instant>,
}

static GATE: Mutex<Gate> = Mutex::new(Gate { running: 0, recent: Vec::new() });
static FREED: Condvar = Condvar::new();

struct Slot;

impl Drop for Slot {
    fn drop(&mut self) {
        let mut g = GATE.lock().unwrap_or_else(|e| e.into_inner());
        g.running = g.running.saturating_sub(1);
        FREED.notify_one();
    }
}

fn take_slot(concurrency: usize, per_hour: u64, wait: Duration) -> anyhow::Result<Slot> {
    let deadline = Instant::now() + wait;
    let mut g = GATE.lock().unwrap_or_else(|e| e.into_inner());
    let hour = Duration::from_secs(3600);
    g.recent.retain(|t| t.elapsed() < hour);
    if per_hour > 0 && g.recent.len() as u64 >= per_hour {
        // Reported as exhaustion so callers fall back exactly as they do for a spent plan.
        anyhow::bail!("claude: {per_hour} calls this hour, the cap in tuning.json (claude_calls_per_hour) — quota");
    }
    while g.running >= concurrency.max(1) {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            anyhow::bail!("claude: timed out waiting for a free slot ({} already running)", g.running);
        }
        g = FREED.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
    }
    g.running += 1;
    g.recent.push(Instant::now());
    Ok(Slot)
}

/// Somewhere private to write the system prompt and run from. Outside the home directory, so
/// Claude Code finds no CLAUDE.md in any parent of its working directory.
fn work_dir() -> anyhow::Result<PathBuf> {
    let d = std::env::temp_dir().join(format!("mars-claude-{}", crate::sys::proc::uid_tag()));
    std::fs::create_dir_all(&d)?;
    crate::sys::fsperm::restrict_dir(&d)?;
    Ok(d)
}

static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Make one call. `cfg.url` holds the path of the `claude` binary; the sink, when given,
/// receives the reply as it streams.
pub fn chat(
    cfg: &crate::agent::AgentConfig,
    messages: &[serde_json::Value],
    mut sink: Option<&mut dyn FnMut(&str)>,
) -> anyhow::Result<(String, u64, u64)> {
    let t = crate::tuning::load();
    let _slot = take_slot(t.claude_concurrency as usize, t.claude_calls_per_hour, Duration::from_secs(t.claude_timeout_secs))?;
    let (system, prompt) = flatten(messages);
    let dir = work_dir()?;
    let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let sys_file = dir.join(format!("system-{}-{n}.txt", std::process::id()));
    std::fs::write(&sys_file, &system)?;
    let _ = crate::sys::fsperm::restrict_file(&sys_file);
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = Cleanup(sys_file.clone());

    let streaming = sink.is_some();
    let mut cmd = Command::new(&cfg.url);
    cmd.args(args(&cfg.model, &sys_file, streaming))
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for v in SCRUB {
        cmd.env_remove(v);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow::anyhow!("claude: could not start {} ({e}) — is Claude Code installed?", cfg.url))?;
    if let Some(mut stdin) = child.stdin.take() {
        // A prompt larger than the pipe buffer blocks until claude reads it, so write it from
        // a thread rather than before reading the reply.
        let p = prompt.into_bytes();
        std::thread::spawn(move || {
            let _ = stdin.write_all(&p);
        });
    }
    let mut stderr = child.stderr.take();
    let stderr_text = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(e) = stderr.as_mut() {
            let _ = e.take(16 * 1024).read_to_string(&mut s);
        }
        s
    });
    let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("claude: no output pipe"))?;

    // Lines arrive on a thread so the wait below can give up on a call that hangs.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(t.claude_timeout_secs.max(1));
    let mut result: Option<serde_json::Value> = None;
    let mut whole = String::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("claude: timed out after {}s (claude_timeout_secs in tuning.json)", t.claude_timeout_secs);
        }
        match rx.recv_timeout(left) {
            Ok(line) => {
                if !streaming {
                    whole.push_str(&line);
                    whole.push('\n');
                    continue;
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                if let Some(d) = stream_delta(&v) {
                    if let Some(s) = sink.as_mut() {
                        s(&d);
                    }
                } else if v["type"] == "result" {
                    result = Some(v);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let status = child.wait()?;
    if !streaming {
        result = serde_json::from_str(whole.trim()).ok();
    }
    match result {
        Some(v) => parse_result(&v).map(|r| (r.text, r.prompt_tokens, r.completion_tokens)),
        None => {
            let err = stderr_text.join().unwrap_or_default();
            let why = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("no result").trim().to_string();
            anyhow::bail!("claude: exited {} with {}", status.code().unwrap_or(-1), explain(&why))
        }
    }
}
