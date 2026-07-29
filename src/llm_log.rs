//! LLM call observability (debug mode).
//!
//! When `MARS_LLM_DEBUG=1` (or `mars --llm-debug`), every `agent::chat()` call
//! appends one JSON line to `~/.mars/logs/calls.jsonl` recording the task,
//! provider, model, real input/output token counts, latency, and the full
//! prompt + reply. `mars llm-stats` aggregates it into a per-task×model profile
//! ranked by token consumption — so you can see where the budget goes and
//! right-size the model (or trim the prompt) for each kind of call.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// Logging is off unless explicitly enabled (env, or the `--llm-debug` flag,
/// which sets the same env var early in `main`).
pub fn enabled() -> bool {
    matches!(
        std::env::var("MARS_LLM_DEBUG").as_deref(),
        Ok("1") | Ok("true") | Ok("yes") | Ok("on")
    )
}

/// Where the debug log lives. Durable by default (`~/.mars/logs/`) so a full day
/// of dogfooding survives reboots/`$TMPDIR` sweeps; `MARS_LLM_LOG_DIR` overrides
/// (tests point it at a temp dir so they never touch real captured data). Falls
/// back to `$TMPDIR/mars-llm` only when `$HOME` is unset.
pub fn log_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("MARS_LLM_LOG_DIR") {
        return PathBuf::from(d);
    }
    match crate::sys::paths::home_dir() {
        Some(h) => h.join(".mars").join("logs"),
        None => std::env::temp_dir().join("mars-llm"),
    }
}
/// One log file PER PROCESS. The daemon and `keyd` both write LLM records, and sharing one
/// file tore lines badly enough that the log meant for diagnosing a failure could not itself be
/// parsed. Readers glob `log_files()`; nothing else changes.
pub fn log_path() -> PathBuf {
    log_dir().join(format!("calls-{}.jsonl", std::process::id()))
}

/// Every call log in the directory, newest last — including `calls.jsonl` from before the
/// per-process split, so history stays readable.
pub fn log_files() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let legacy = log_dir().join("calls.jsonl");
    if legacy.exists() {
        out.push(legacy);
    }
    if let Ok(entries) = std::fs::read_dir(log_dir()) {
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("calls-") && n.ends_with(".jsonl"))
            })
            .collect();
        found.sort();
        out.extend(found);
    }
    out
}
/// Behavioral outcomes (accept/edit/reject of a suggestion) land here, keyed by
/// `call_id`, to be joined against `calls.jsonl` offline. Separate file so the
/// outcome — which arrives *after* the call returns — never mutates a call line.
pub fn outcomes_path() -> PathBuf {
    log_dir().join("outcomes.jsonl")
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A per-process session id, so calls (and session_start/end) can be grouped for
/// per-session and per-session-hour token stats. Stable for the life of the process.
pub fn session_id() -> &'static str {
    static SID: OnceLock<String> = OnceLock::new();
    SID.get_or_init(|| format!("{}-{}", std::process::id(), now_secs()))
}

/// Monotonic per-call id (unique within a process) used to correlate a call with
/// its later behavioral outcome.
pub fn next_call_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Append a JSON value as one line to the debug log (no-op unless enabled).
fn append(path: &PathBuf, line: &serde_json::Value) {
    if !enabled() {
        return;
    }
    let _ = std::fs::create_dir_all(log_dir());
    // ONE write of ONE buffer. `writeln!` formats incrementally and can issue several
    // syscalls, which is how half-records ended up in the log; an O_APPEND write of a single
    // buffer lands whole or not at all.
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let mut buf = line.to_string();
        buf.push('\n');
        let _ = f.write_all(buf.as_bytes());
    }
}

/// Emit a session boundary event so per-session / per-session-hour rates are
/// computable offline. `kind` is "session_start" or "session_end".
pub fn session_event(kind: &str) {
    append(
        &log_path(),
        &serde_json::json!({ "ts": now_secs(), "kind": kind, "session_id": session_id() }),
    );
}
/// A structured, non-call event (shift report shown/dismissed, …): one JSONL
/// line with arbitrary fields, same debug gate as everything else here.
pub fn event(kind: &str, mut fields: serde_json::Value) {
    if let Some(obj) = fields.as_object_mut() {
        obj.insert("ts".into(), serde_json::json!(now_secs()));
        obj.insert("kind".into(), serde_json::json!(kind));
        obj.insert("session_id".into(), serde_json::json!(session_id()));
    }
    append(&log_path(), &fields);
}

/// A background task that did NOT run, and why. The absence of a call record was the only
/// signal that auto-naming had stopped, and absence is not evidence anyone can act on — this
/// makes "skipped, because already attempted" a line you can read.
pub fn skipped(task: &str, reason: &str) {
    event("skipped", serde_json::json!({ "task": task, "reason": reason }));
}

pub fn session_start() {
    session_event("session_start");
}
pub fn session_end() {
    session_event("session_end");
}

/// RAII bookend: emits `session_start` on creation and `session_end` on drop, so
/// every `mars` invocation delimits a session in the log for per-session stats.
pub struct SessionGuard;
impl SessionGuard {
    pub fn start() -> Self {
        session_start();
        SessionGuard
    }
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        session_end();
    }
}

/// Record a behavioral outcome for a prior call (accept unedited / edited / reject).
pub fn record_outcome(call_id: u64, accepted_command: Option<&str>, edited: bool, rejected: bool) {
    append(
        &outcomes_path(),
        &serde_json::json!({
            "ts": now_secs(),
            "session_id": session_id(),
            "call_id": call_id,
            "accepted_command": accepted_command,
            "edited": edited,
            "rejected": rejected,
        }),
    );
}

/// One recorded call. Borrows so the hot path allocates nothing when disabled.
pub struct CallRecord<'a> {
    pub call_id: u64,
    pub task: &'a str,
    pub provider: &'a str,
    pub model: &'a str,
    /// Which memory/retrieval variant was active for this call ("n/a" when the
    /// path doesn't retrieve; e.g. "none", "history", "docs", "full").
    pub retrieval: &'a str,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub latency_ms: u64,
    pub ok: bool,
    pub error: Option<&'a str>,
    pub input: &'a [serde_json::Value],
    pub output: &'a str,
}

/// Append a call to the JSONL log (no-op unless debug is enabled). Best-effort:
/// a logging failure must never disturb the agent.
pub fn record(r: &CallRecord) {
    let line = serde_json::json!({
        "ts": now_secs(),
        "pid": std::process::id(),
        "session_id": session_id(),
        "call_id": r.call_id,
        "task": r.task,
        "provider": r.provider,
        "model": r.model,
        "retrieval": r.retrieval,
        "prompt_tokens": r.prompt_tokens,
        "completion_tokens": r.completion_tokens,
        "total_tokens": r.prompt_tokens + r.completion_tokens,
        "latency_ms": r.latency_ms,
        "ok": r.ok,
        "error": r.error,
        "input": r.input,
        "output": r.output,
    });
    append(&log_path(), &line);
}

#[derive(Default)]
struct Agg {
    n: u64,
    prompt: u64,
    completion: u64,
    ms: u64,
    errs: u64,
}
impl Agg {
    fn total_tokens(&self) -> u64 {
        self.prompt + self.completion
    }
    fn add(&mut self, pt: u64, ct: u64, ms: u64, ok: bool) {
        self.n += 1;
        self.prompt += pt;
        self.completion += ct;
        self.ms += ms;
        if !ok {
            self.errs += 1;
        }
    }
}

/// Days since 1970-01-01 → (year, month, day) via Howard Hinnant's civil algorithm,
/// so per-day buckets can be labelled without pulling in a date crate (UTC).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}
fn day_label(ts: u64) -> String {
    let (y, m, d) = civil_from_days((ts / 86400) as i64);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// `mars llm-stats [--raw|--json|--daily]` — profile the log so each call type can be
/// optimized. Rows are ranked by total token consumption (the biggest budget/latency
/// targets first). `--raw` dumps the JSONL; `--json` emits the aggregate + daily
/// series as JSON (scriptable); `--daily` prints a day-by-day token-trend chart.
pub fn stats(raw: bool, json: bool, daily: bool, since_secs: Option<u64>) -> anyhow::Result<()> {
    let cutoff = since_secs.map(|w| now_secs().saturating_sub(w));
    let path = log_path();
    // Read EVERY per-process log, not just this process's own — the daemon and `keyd` each
    // keep their own now, and a report drawn from one of them is a report of half the calls.
    let content: String = log_files()
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect::<Vec<_>>()
        .join("\n");
    let content = match if content.trim().is_empty() { Err(()) } else { Ok(content) } {
        Ok(c) => c,
        Err(_) => {
            if json {
                println!("{{\"calls\":0,\"rows\":[],\"daily\":[]}}");
            } else {
                println!(
                    "No LLM debug log yet at {}.\nEnable it with `mars --llm-debug` (or MARS_LLM_DEBUG=1), \
                     use the agent, then re-run `mars llm-stats`.",
                    path.display()
                );
            }
            return Ok(());
        }
    };
    if raw {
        print!("{content}");
        return Ok(());
    }

    use std::collections::BTreeMap;
    let mut by_key: BTreeMap<(String, String), Agg> = BTreeMap::new();
    let mut by_day: BTreeMap<String, Agg> = BTreeMap::new();
    let mut total = Agg::default();
    for line in content.lines() {
        let Ok(j) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if j.get("kind").is_some() {
            continue; // session_start / session_end boundary event, not a call
        }
        if let Some(cut) = cutoff {
            if j["ts"].as_u64().unwrap_or(0) < cut {
                continue; // outside the --since window
            }
        }
        let task = j["task"].as_str().unwrap_or("?").to_string();
        let model = j["model"].as_str().unwrap_or("?").to_string();
        let pt = j["prompt_tokens"].as_u64().unwrap_or(0);
        let ct = j["completion_tokens"].as_u64().unwrap_or(0);
        let ms = j["latency_ms"].as_u64().unwrap_or(0);
        let ok = j["ok"].as_bool().unwrap_or(true);
        by_key.entry((task, model)).or_default().add(pt, ct, ms, ok);
        if let Some(ts) = j["ts"].as_u64() {
            by_day.entry(day_label(ts)).or_default().add(pt, ct, ms, ok);
        }
        total.add(pt, ct, ms, ok);
    }
    if total.n == 0 {
        if json {
            println!("{{\"calls\":0,\"rows\":[],\"daily\":[]}}");
        } else {
            let scope = if cutoff.is_some() { " in the --since window" } else { "" };
            println!("No calls{scope} in the log at {}.", path.display());
        }
        return Ok(());
    }

    // Rank by total tokens desc — the heaviest call types (best optimization
    // targets) surface first.
    let mut rows: Vec<((String, String), Agg)> = by_key.into_iter().collect();
    rows.sort_by(|a, b| b.1.total_tokens().cmp(&a.1.total_tokens()));
    let grand = total.total_tokens().max(1);

    if json {
        let row_json: Vec<_> = rows.iter().map(|((task, model), a)| {
            let n = a.n.max(1);
            serde_json::json!({
                "task": task, "model": model, "n": a.n,
                "avg_in": a.prompt / n, "avg_out": a.completion / n,
                "total_tokens": a.total_tokens(), "pct_tokens": a.total_tokens() * 100 / grand,
                "avg_ms": a.ms / n, "errors": a.errs,
            })
        }).collect();
        let day_json: Vec<_> = by_day.iter().map(|(date, a)| {
            let n = a.n.max(1);
            serde_json::json!({
                "date": date, "calls": a.n, "prompt_tokens": a.prompt,
                "completion_tokens": a.completion, "total_tokens": a.total_tokens(),
                "avg_ms": a.ms / n, "errors": a.errs,
            })
        }).collect();
        let tn = total.n.max(1);
        let out = serde_json::json!({
            "log": path.display().to_string(),
            "calls": total.n,
            "total_tokens": total.total_tokens(),
            "avg_in": total.prompt / tn, "avg_out": total.completion / tn,
            "avg_ms": total.ms / tn, "errors": total.errs,
            "rows": row_json,
            "daily": day_json,
        });
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    println!(
        "{:<13} {:<22} {:>4} {:>8} {:>8} {:>9} {:>5} {:>7} {:>4}",
        "TASK", "MODEL", "N", "AVG_IN", "AVG_OUT", "TOT_TOK", "%TOK", "AVG_MS", "ERR"
    );
    println!("{:-<86}", "");
    for ((task, model), a) in &rows {
        let n = a.n.max(1);
        println!(
            "{:<13} {:<22} {:>4} {:>8} {:>8} {:>9} {:>4}% {:>7} {:>4}",
            task, model, a.n, a.prompt / n, a.completion / n,
            a.total_tokens(), a.total_tokens() * 100 / grand, a.ms / n, a.errs
        );
    }
    println!("{:-<86}", "");
    let n = total.n.max(1);
    println!(
        "{:<13} {:<22} {:>4} {:>8} {:>8} {:>9} {:>4}% {:>7} {:>4}",
        "TOTAL", "", total.n, total.prompt / n, total.completion / n,
        total.total_tokens(), 100, total.ms / n, total.errs
    );
    println!("\nlog: {}   ({} calls)", path.display(), total.n);

    if daily {
        // Day-by-day token trend — a bar per day, scaled to the busiest day.
        println!("\nday-by-day (total tokens):");
        let max = by_day.values().map(|a| a.total_tokens()).max().unwrap_or(1).max(1);
        for (date, a) in &by_day {
            let tt = a.total_tokens();
            let filled = (tt * 30 / max) as usize;
            let bar = "█".repeat(if tt > 0 { filled.max(1) } else { 0 });
            println!("  {}  {:>8} tok  {:>4} calls  {}", date, tt, a.n, bar);
        }
    } else {
        println!("tips: heaviest rows first — try a smaller model or a shorter prompt there;");
        println!("      `--raw` full I/O · `--daily` day-by-day · `--json` machine-readable · `--since 7d` a window.");
    }
    Ok(())
}
