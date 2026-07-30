//! `mars snapshot` — the deterministic half of the manager.
//!
//! Subscribes to every live session exactly as Rover does, takes one board frame each, and
//! maintains `~/.mars/manager` — a repo laid out session → workspaces → summary. See
//! `docs/layout.md`, which this module writes on first run.
//!
//! OWNERSHIP is the load-bearing rule, because an agent joins this repo later:
//!
//! * this module owns `sessions/**`, `index.json`, `index.md`, `timeline.md` — regenerated freely
//! * the AGENT owns `memory/**` — never written here, only read
//! * the HUMAN owns `AGENTS.md`, `docs/**`, `policy.md` — scaffolded once, never overwritten
//!
//! Nothing here uses an LLM. That is the point: the whole card pipeline — format, citations,
//! expiry, supersession, rendering on the phone — is proven against content that cannot be
//! wrong, so any bug found is a plumbing bug rather than a judgement one.

use anyhow::Result;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

/// One pane as the board reports it.
#[derive(Clone, Debug)]
pub struct PaneRow {
    pub id: String,
    pub pane_id: String,
    pub name: String,
    pub verdict: String,
    pub kind: String,
    pub why: String,
    pub age_secs: u64,
    pub blocked_prompt: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SessionSnap {
    pub name: String,
    pub health: String,
    pub panes: Vec<PaneRow>,
}

/// What a card is about. The pair `(pane, kind)` is a card's identity: one open card per pane per
/// kind, so a pane that stays blocked for six hours produces ONE card, not one per tick.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reflex {
    Blocked,
    Failed,
}

impl Reflex {
    fn slug(self) -> &'static str {
        match self {
            Reflex::Blocked => "blocked",
            Reflex::Failed => "failed",
        }
    }
    fn severity(self) -> &'static str {
        match self {
            Reflex::Blocked => "block",
            Reflex::Failed => "warn",
        }
    }
}

pub fn now_secs() -> u64 {
    crate::worklog::now_secs()
}

/// `2026-07-29T14:22:03Z` — the only timestamp format written anywhere here, so the timeline
/// sorts lexically and a human can read it without converting anything.
pub fn iso(ts: u64) -> String {
    let days = (ts / 86400) as i64;
    let (y, m, d) = civil_from_days(days);
    let rem = ts % 86400;
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// Days since the epoch → civil date (Howard Hinnant's algorithm). Local copy so this module
/// does not depend on a date crate for four lines of arithmetic.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// A card already on the feed, as parsed back off disk. Only the frontmatter fields the
/// deterministic half needs — the body is never re-read.
#[derive(Clone, Debug)]
pub struct OpenCard {
    pub id: String,
    pub file: PathBuf,
    pub session: String,
    pub pane: String,
    pub kind: String,
    pub severity: String,
    pub headline: String,
    /// Stable, meaningful name. `(session, title)` is a memo's identity — a memo often outlives
    /// the pane that prompted it, which is precisely why it is a memo and not a workspace row.
    pub title: String,
    /// 0–100, what should be READ first. Not the same as severity: a block they already know
    /// about ranks below a warning they have never seen.
    pub priority: u32,
    pub created: u64,
    pub expired: bool,
}

fn front_field(front: &str, key: &str) -> Option<String> {
    front.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
    })
}

/// Split a card file into (frontmatter, body). A file without a well-formed `---` block is not a
/// card; it is still a readable document, which is the whole reason for this format.
pub fn split_front(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    Some((&rest[..end], &rest[end + 5..]))
}

fn read_open_cards(feed: &Path) -> Vec<OpenCard> {
    let Ok(rd) = std::fs::read_dir(feed) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        // Any .md in the directory is a memo. The agent names files after their title, so an
        // allowlist of prefixes would silently drop everything it writes.
        let is_card = p.file_name().and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(".md") && !n.starts_with('.'));
        if !is_card {
            continue;
        }
        let stem = p.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let Ok(text) = std::fs::read_to_string(&p) else { continue };
        let Some((front, _)) = split_front(&text) else { continue };
        out.push(OpenCard {
            id: front_field(front, "id").unwrap_or_default(),
            file: p,
            session: front_field(front, "session").unwrap_or_default(),
            pane: front_field(front, "pane").unwrap_or_default(),
            kind: front_field(front, "kind").unwrap_or_default(),
            severity: front_field(front, "severity").unwrap_or_default(),
            headline: front_field(front, "headline").unwrap_or_default(),
            title: front_field(front, "title").unwrap_or_else(|| {
                // Pre-title memos fall back to their filename stem, so an older repo still sorts
                // and de-duplicates rather than collapsing every memo onto one empty identity.
                stem.clone()
            }),
            priority: front_field(front, "priority")
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(|| match front_field(front, "severity").as_deref() {
                    Some("block") => 85,
                    Some("warn") => 60,
                    Some("info") => 30,
                    _ => 10,
                })
                .min(100),
            created: front_field(front, "created_ts").and_then(|s| s.parse().ok()).unwrap_or(0),
            expired: front_field(front, "expired").as_deref() == Some("true"),
        });
    }
    out.sort_by(|a, b| a.created.cmp(&b.created));
    out
}

/// Pull `actions:` out of the frontmatter. Deliberately a small hand-parser rather than a YAML
/// dependency: the shape is fixed, and a card whose actions fail to parse should still render as
/// a readable document with no buttons — never vanish.
fn parse_actions(front: &str) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let mut in_actions = false;
    for line in front.lines() {
        if line.starts_with("actions:") {
            in_actions = true;
            continue;
        }
        if in_actions && !line.starts_with("  ") && !line.starts_with("- ") {
            break;
        }
        if !in_actions {
            continue;
        }
        let t = line.trim().trim_start_matches("- ").trim().trim_start_matches('{').trim_end_matches('}');
        let get = |k: &str| -> Option<String> {
            t.split(',').find_map(|kv| {
                let (kk, vv) = kv.split_once(':')?;
                (kk.trim() == k).then(|| vv.trim().trim_matches('"').to_string())
            })
        };
        if let (Some(id), Some(label), Some(keys)) = (get("id"), get("label"), get("keys")) {
            out.push(serde_json::json!({ "id": id, "label": label, "keys": keys.replace("\\r", "\r") }));
        }
    }
    out
}

/// Filesystem mtime in epoch seconds. Cards carry `created` in their frontmatter, but "when was
/// this file last touched" is the filesystem's job and is the honest answer for staleness — a
/// card that was expired in place has a newer mtime than its `created`.
fn mtime_secs(p: &Path) -> u64 {
    std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn human_age(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86399 => format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60),
        _ => format!("{}d{}h", secs / 86400, (secs % 86400) / 3600),
    }
}


/// Write only if the content actually differs.
///
/// Every daemon regenerates the SHARED index, so with three sessions running the file was rewritten
/// roughly every seven seconds regardless of whether anything changed — and a phone polling it
/// re-rendered each time. Nothing downstream can tell "this is new" from "this was rewritten"
/// except by comparing, so the comparison belongs here, once.
/// The path Mars may write for an artifact the AGENT owns.
///
/// Ownership was a convention in a doc, and a convention lost twice: the deterministic pass
/// overwrote `mission_briefing.md` every 60s, and then `workspaces/<pane>.md` every 60s, in both
/// cases erasing minutes of model work with a sentence of arithmetic. Cheap output must never be
/// able to clobber expensive output. Every deterministic write now goes through here, so the
/// suffix is applied by construction rather than remembered.
fn computed(path: PathBuf) -> PathBuf {
    let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    path.with_file_name(format!("{stem}.computed.md"))
}

fn write_if_changed(path: &Path, body: &str) -> Result<bool> {
    if std::fs::read_to_string(path).is_ok_and(|old| old == body) {
        return Ok(false);
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, body)?;
    Ok(true)
}

/// Write a reflex card. Markdown body, YAML-ish frontmatter — structure in the frontmatter,
/// meaning in the body, so a parse failure degrades to a readable document rather than to nothing.
/// A stable, filename-safe identifier from a workspace name.
fn slugify(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let t = out.trim_matches('-');
    if t.is_empty() { "workspace".into() } else { t.to_string() }
}

fn write_card(
    feed: &Path,
    id: &str,
    kind: Reflex,
    origin: &str,
    session: &str,
    pane: &PaneRow,
    ts: u64,
) -> Result<()> {
    let (headline, body, actions) = match kind {
        Reflex::Blocked => (
            format!("{} is waiting on a prompt · {}", pane.name, human_age(pane.age_secs)),
            format!(
                "The pane is sitting on a prompt and nothing in it advances until it is answered.\n\n\
                 ```\n{}\n```\n",
                pane.why.trim()
            ),
            "actions:\n  - {id: yes, label: \"Answer y\", keys: \"y\\r\"}\n  - {id: no, label: \"Answer n\", keys: \"n\\r\"}\n",
        ),
        Reflex::Failed => (
            format!("{} failed", pane.name),
            format!("{}\n", pane.why.trim()),
            "",
        ),
    };
    let card = format!(
        "---\n\
         id: {id}\n\
         v: 1\n\
         created: {created}\n\
         created_ts: {ts}\n\
         source: reflex\n\
         actor: mars@{origin}\n\
         severity: {sev}\n\
         origin: {origin}\n\
         session: \"{session}\"\n\
         pane: \"{pane_id}\"\n\
         kind: {kind_slug}\n\
         title: {title}\n\
         priority: {priority}\n\
         headline: \"{headline}\"\n\
         expired: false\n\
         {actions}\
         ---\n\
         {body}",
        created = iso(ts),
        sev = kind.severity(),
        pane_id = pane.pane_id,
        kind_slug = kind.slug(),
        // A reflex memo's title names the situation, not the moment, so re-detecting the same
        // block on a later tick recognises the existing memo instead of adding a second.
        title = format!("{}-{}", slugify(&pane.name), kind.slug()),
        priority = match kind {
            Reflex::Blocked => 85,
            Reflex::Failed => 65,
        },
        headline = headline.replace('"', "'"),
    );
    std::fs::write(feed.join(format!("{id}.md")), card)?;
    Ok(())
}

/// Mark a card expired in place. Cards are otherwise append-only; this flips one field so the
/// renderer can grey it without the file being rewritten or deleted.
fn expire_card(c: &OpenCard, ts: u64) -> Result<()> {
    let text = std::fs::read_to_string(&c.file)?;
    if text.contains("\nexpired: true\n") {
        return Ok(());
    }
    let flipped = text.replacen(
        "\nexpired: false\n",
        &format!("\nexpired: true\nupdated: {}\nupdated_ts: {ts}\n", iso(ts)),
        1,
    );
    std::fs::write(&c.file, flipped)?;
    Ok(())
}

/// The briefing — a deterministic workspace update, rewritten every run.
///
/// Deliberately NOT a model's job. Counting panes by verdict, naming what is blocked and for how
/// long, and saying "nothing needs you" when nothing does are all things arithmetic does better,
/// faster, and without a plan window.

/// A one-line status for a workspace. Deterministic, and deliberately TIME-FREE: the age travels
/// as its own `ageSecs` field for the renderer to format. Embedding it here made the summary churn
/// every tick ("idle · 1m" -> "idle · 2m"), rewriting the file and re-rendering the phone for no
/// new information — the exact failure the design rule against quoting live values describes.
fn workspace_summary(p: &PaneRow) -> String {
    let why = p.why.trim();
    let tail = if why.is_empty() { String::new() } else { format!(" · {why}") };
    match p.verdict.as_str() {
        "blocked" => format!("waiting on input{tail}"),
        "failed" => format!("failed{tail}"),
        "running" => format!("running{tail}"),
        "done" => "finished".into(),
        _ if !why.is_empty() => why.to_string(),
        _ => "idle".into(),
    }
}

/// Age, coarsened so prose does not churn. Exact ages belong in a field the renderer formats, not
/// in a sentence: "idle 1m" became "idle 2m" on the next tick, rewriting the file and re-rendering
/// the phone every 20 seconds. Buckets change on a human timescale instead.
fn coarse_age(secs: u64) -> String {
    match secs {
        0..=89 => "just now".into(),
        90..=3599 => format!("{}m", (secs + 30) / 60),
        3600..=86399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86400),
    }
}

/// Prose for the phone's briefing typewriter. Purpose-built and plain — a markdown table typed
/// out one character at a time reads as a bug, so the narrative and the document are separate
/// renderings of the same facts rather than one being derived from the other.
fn session_narrative(s: &SessionSnap) -> String {
    let blocked: Vec<&PaneRow> = s.panes.iter().filter(|p| p.verdict == "blocked").collect();
    let failed: Vec<&PaneRow> = s.panes.iter().filter(|p| p.verdict == "failed").collect();
    let running = s.panes.iter().filter(|p| p.verdict == "running").count();
    let done = s.panes.iter().filter(|p| p.verdict == "done").count();
    let names = |v: &[&PaneRow]| -> String {
        let n: Vec<&str> = v.iter().map(|p| p.name.as_str()).collect();
        match n.len() {
            0 => String::new(),
            1 => n[0].to_string(),
            2 => format!("{} and {}", n[0], n[1]),
            _ => format!("{} and {} others", n[0], n.len() - 1),
        }
    };
    let mut parts: Vec<String> = Vec::new();
    if !blocked.is_empty() {
        let oldest = blocked.iter().map(|p| p.age_secs).max().unwrap_or(0);
        parts.push(format!(
            "{} {} waiting on you — {}.",
            names(&blocked),
            if blocked.len() == 1 { "is" } else { "are" },
            coarse_age(oldest)
        ));
    }
    if !failed.is_empty() {
        parts.push(format!(
            "{} {} failed.",
            names(&failed),
            if failed.len() == 1 { "has" } else { "have" }
        ));
    }
    if running > 0 {
        parts.push(format!("{running} still running."));
    }
    if done > 0 {
        parts.push(format!("{done} finished clean."));
    }
    if parts.is_empty() {
        return "The board is quiet — nothing running, nothing waiting on you.".into();
    }
    parts.join(" ")
}

fn session_briefing(s: &SessionSnap, ts: u64) -> String {
    let mut needs: Vec<&PaneRow> = s
        .panes
        .iter()
        .filter(|p| p.verdict == "blocked" || p.verdict == "failed")
        .collect();
    needs.sort_by(|a, b| {
        let rank = |v: &str| if v == "blocked" { 0 } else { 1 };
        rank(&a.verdict).cmp(&rank(&b.verdict)).then(b.age_secs.cmp(&a.age_secs))
    });
    let running = s.panes.iter().filter(|p| p.verdict == "running").count();
    let idle = s.panes.len() - running - needs.len();

    let mut md = String::new();
    // No timestamp in the body: it is the only thing that changed between ticks, so it alone
    // made the file differ and the phone re-render. `meta.json` carries `updated_ts`.
    md.push_str(&format!("# {} · mission briefing\n\n", s.name));
    if needs.is_empty() {
        md.push_str("**Nothing needs you.**\n\n");
    } else {
        md.push_str(&format!(
            "**{} need{} you**\n\n",
            needs.len(),
            if needs.len() == 1 { "s" } else { "" }
        ));
        for p in &needs {
            let mark = if p.verdict == "blocked" { "⚠" } else { "✗" };
            md.push_str(&format!("- {mark} **{}** — {} · {}\n", p.name, p.verdict, coarse_age(p.age_secs)));
            if !p.why.trim().is_empty() {
                md.push_str(&format!("  \n  {}\n", p.why.trim()));
            }
        }
        md.push('\n');
    }
    md.push_str(&format!("{running} running · {idle} idle\n\n## workspaces\n\n"));
    if s.panes.is_empty() {
        md.push_str("_no panes_\n\n");
    } else {
        md.push_str("| workspace | state | age |\n|---|---|---|\n");
        for p in &s.panes {
            md.push_str(&format!("| {} | {} | {} |\n", p.name, p.verdict, coarse_age(p.age_secs)));
        }
        md.push('\n');
    }
    if !s.health.trim().is_empty() {
        md.push_str(&format!("_{}_\n", s.health.trim()));
    }
    md
}

/// Append to the human-readable timeline. One line per event, date-ordered because it is only
/// ever appended. This is the file to open when asking "what actually happened" — no tooling, no
/// parser, no query language.
fn append_timeline(repo: &Path, ts: u64, lines: &[String]) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }
    let path = repo.join("timeline.md");
    let fresh = !path.exists();
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&path)?;
    if fresh {
        f.write_all(b"# Timeline\n\nAppend-only, oldest first. Written by `mars snapshot`.\n\n")?;
    }
    let mut buf = String::new();
    for l in lines {
        buf.push_str(&format!("- `{}` {}\n", iso(ts), l));
    }
    f.write_all(buf.as_bytes())?;
    Ok(())
}

/// Regenerate the aggregate `index.json` from `sessions/**`. The tree is the truth; this is a
/// cache, rebuilt wholesale so it can never drift.
///
/// Bodies and actions are INLINED. Rover reads through a single-slot `fs.read`, so a screenful of
/// cards must be one round trip, not one per card.
/// Agent-written prose, if it is present, non-empty, and still describes the world.
///
/// Freshness is part of the preference, not an afterthought: prose older than `stale_secs` loses
/// to arithmetic, because an eloquent briefing about a board that has since moved is worse than a
/// blunt sentence about the board as it is. This is also the deterministic check that a dead
/// agent cannot go unnoticed — it stops being preferred the moment it stops writing.
/// Read a file another process may be writing right now.
///
/// Read it, then look at the timestamp again: if it moved while we were reading, or the file is
/// only a few milliseconds old, somebody is mid-write — wait a beat and take the second read.
/// The common case pays nothing. This is handled here rather than by asking the agent to write
/// atomically, because a rule in a markdown contract is a request, and this is a guarantee.
fn read_settled(path: &Path) -> Option<String> {
    let before = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let text = std::fs::read_to_string(path).ok()?;
    let after = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let fresh = after.elapsed().map(|d| d < std::time::Duration::from_millis(50)).unwrap_or(false);
    if before == after && !fresh {
        return Some(text);
    }
    std::thread::sleep(std::time::Duration::from_millis(40));
    std::fs::read_to_string(path).ok()
}

fn read_agent_prose(path: &Path, ts: u64, stale_secs: u64) -> Option<String> {
    let age = ts.saturating_sub(mtime_secs(path));
    if age > stale_secs {
        return None;
    }
    let text = read_settled(path)?;
    // The agent SIGNS what she writes. Authorship used to be inferred from how recently the file
    // was touched, and freshness is not authorship: an older daemon still writing this path
    // produced a recent mtime, so its deterministic markdown reached the phone stamped "agent" —
    // the provenance mark telling the exact lie it exists to prevent. An unsigned file is
    // somebody else's, and falls back to arithmetic.
    let (front, body) = split_front(&text)?;
    if front_field(front, "source").as_deref() != Some("agent") {
        return None;
    }
    let body = body.to_string();
    let body: String = body
        .lines()
        .skip_while(|l| l.trim().is_empty() || l.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");
    let body = body.trim();
    (!body.is_empty()).then(|| body.to_string())
}

/// The whole manager view, computed from the tree at the moment it is asked for.
///
/// Nothing is stored. `index.json` used to hold this, rewritten by every daemon on a 60s timer
/// and polled by the phone on a 4s one — two clocks, neither related to when the data changed.
/// Every serious bug in this module came from that: daemons on different builds overwrote each
/// other's schema, the phone read a copy forty minutes stale, and authorship had to be guessed
/// because the view was assembled by someone other than the writer.
///
/// Reading is cheap because it only opens what the phone needs — a briefing, a workspace summary
/// and a memo per workspace, a few dozen small files. The bulk (snapshots, inbox, receipts) is
/// the agent's raw material and is never opened here, so this cost is bounded by how many
/// workspaces exist rather than by how long the machine has been running.
pub fn view(repo: &Path, ts: u64, stale_secs: u64) -> serde_json::Value {
    let all = read_all_sessions();
    let computed: Vec<(String, String)> =
        all.iter().map(|s| (s.name.clone(), session_briefing(s, ts))).collect();
    let briefs: &[(String, String)] = &computed;
    let sessions: &[SessionSnap] = &all;
    // Scan the TREE, not just the sessions this process just wrote: each session's daemon owns
    // its own subtree, so an index built from one process's view would omit the others. The
    // directory is the truth; this is a cache of it.
    // Scan ~/.mars/sessions/*/meta.json: every session, at its CURRENT name, whichever daemon
    // wrote it. An index built from one process's view would omit the others.
    let mut dirs: Vec<(String, PathBuf)> = sessions_root()
        .and_then(|r| std::fs::read_dir(r).ok())
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .filter_map(|p| {
                    let meta: serde_json::Value =
                        serde_json::from_str(&std::fs::read_to_string(p.join("meta.json")).ok()?).ok()?;
                    let name = meta["name"].as_str()?.to_string();
                    // Same filter as read_all_sessions: this scan feeds `briefs` directly, so
                    // leaving it unfiltered let every dead session back into the view.
                    session_is_live(&name).then_some((name, p))
                })
                .collect()
        })
        .unwrap_or_default();
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut all: Vec<(String, String)> = dirs
        .iter()
        .map(|(n, dir)| {
            let b = briefs
                .iter()
                .find(|(bn, _)| bn == n)
                .map(|(_, b)| b.clone())
                .or_else(|| std::fs::read_to_string(dir.join("mission_briefing.md")).ok())
                .unwrap_or_default();
            (n.clone(), b)
        })
        .collect();
    if all.is_empty() {
        all = briefs.to_vec();
    }
    let briefs: &[(String, String)] = &all;
    let mut cards: Vec<serde_json::Value> = Vec::new();
    for (name, dir) in &dirs {
        let dir = dir.join("memos");
        for c in read_open_cards(&dir) {
            let text = std::fs::read_to_string(&c.file).unwrap_or_default();
            let (front, body) = split_front(&text).unwrap_or(("", ""));
            cards.push(serde_json::json!({
                "id": c.id,
                "path": c.file.to_string_lossy(),
                "title": c.title, "priority": c.priority,
                "severity": c.severity, "headline": c.headline, "session": c.session,
                "pane": c.pane, "kind": c.kind, "created_ts": c.created, "expired": c.expired,
                // Three different times, because they answer three different questions:
                // created = when the judgement was made, updated = when it last changed,
                // mtime = what the filesystem says, which is the one that cannot be forged.
                "updated_ts": front_field(front, "updated_ts").and_then(|s| s.parse::<u64>().ok()),
                "mtime": mtime_secs(&c.file),
                "body": body.trim(), "actions": parse_actions(front),
            }));
        }
    }
    // Ordered by PRIORITY — what should be read first — not by severity. A block the engineer
    // already knows about ranks below a warning they have never seen, and only the agent knows
    // the difference. Expired memos sink regardless; ties break most-recent-first.
    cards.sort_by(|a, b| {
        a["expired"].as_bool().unwrap_or(false).cmp(&b["expired"].as_bool().unwrap_or(false))
            .then(b["priority"].as_u64().unwrap_or(0).cmp(&a["priority"].as_u64().unwrap_or(0)))
            .then(b["created_ts"].as_u64().unwrap_or(0).cmp(&a["created_ts"].as_u64().unwrap_or(0)))
    });
    let (runs_ok, runs_total, timing) = run_tally(repo);
    let index = serde_json::json!({
        "generated": iso(ts), "generated_ts": ts,
        "sessions": briefs.iter().map(|(n, b)| {
            let snap = sessions.iter().find(|s| &s.name == n);
            let dir = dirs.iter().find(|(dn, _)| dn == n).map(|(_, d)| d.clone());
            // Arithmetic first — it is always available and is the floor nothing can fall below.
            let computed = snap.map(session_narrative).unwrap_or_else(|| {
                // A session another daemon owns: reuse the prose already in its briefing
                // rather than inventing a second source of truth.
                b.lines().find(|l| l.contains("need") || l.contains("Nothing needs"))
                    .map(|l| l.replace("**", "").trim().to_string())
                    .unwrap_or_default()
            });
            // …then prefer the agent, but only while its prose is FRESHER than the ground truth
            // it describes. An eloquent briefing about a board that has since moved is worse
            // than a blunt sentence about the board as it is.
            let agent = dir.as_ref().and_then(|d| read_agent_prose(&d.join("mission_briefing.md"), ts, stale_secs));
            let (narrative, narrative_source) = match agent {
                Some(a) => (a, "agent"),
                None => (computed, "computed"),
            };
            serde_json::json!({
                "name": n,
                "briefing": b,
                "narrative": narrative,
                "narrativeSource": narrative_source,
                "path": dir.as_ref()
                    .map(|d| d.join("mission_briefing.md").to_string_lossy().to_string())
                    .unwrap_or_default(),
                "workspaces": snap.map(|s| s.panes.iter().map(|p| {
                    // Per FIELD, not per document: a garbled workspace summary costs that one
                    // workspace its prose and nothing else.
                    let w = dir.as_ref()
                        .and_then(|d| read_agent_prose(&d.join("workspaces").join(format!("{}.md", p.pane_id)), ts, stale_secs));
                    let (summary, source) = match w {
                        Some(a) => (a, "agent"),
                        None => (workspace_summary(p), "computed"),
                    };
                    serde_json::json!({
                        "pane": p.pane_id, "name": p.name, "verdict": p.verdict,
                        "kind": p.kind, "ageSecs": p.age_secs,
                        "summary": summary, "summarySource": source,
                    })
                }).collect::<Vec<_>>()).unwrap_or_default(),
            })
        }).collect::<Vec<_>>(),
        "agentStaleSecs": stale_secs,
        "agentRuns": { "ok": runs_ok, "total": runs_total, "timing": timing },
        "agentEnabled": agent_enabled(repo),
        "memos": cards,
    });
    // Temp + rename: atomic on POSIX, so a reader never sees half an index even when two
    // daemons refresh it in the same instant. Last writer wins, and both are correct because
    // the index is derived from the tree.
    index
}


/// Read one board frame from a live session by subscribing exactly as Rover does.
fn board_of(session: &str) -> Option<SessionSnap> {
    let path = crate::session::socket_path(session).ok()?;
    let stream = crate::sys::control::connect(&path).ok()?;
    let mut w = stream.try_clone().ok()?;
    crate::session::write_frame(&mut w, &crate::session::ClientFrame::Subscribe).ok()?;
    let read = stream.try_clone().ok()?;
    read.set_read_timeout(Some(std::time::Duration::from_millis(400))).ok()?;
    let mut r = BufReader::new(read);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut buf = String::new();
    while std::time::Instant::now() < deadline {
        let _ = r.read_line(&mut buf);
        while let Some(nl) = buf.find('\n') {
            let line: String = buf.drain(..=nl).collect();
            if let Ok(crate::session::ServerFrame::Board { json }) =
                serde_json::from_str::<crate::session::ServerFrame>(line.trim())
            {
                return parse_board(&json);
            }
        }
    }
    None
}

fn parse_board(json: &str) -> Option<SessionSnap> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let panes = v["rows"]
        .as_array()?
        .iter()
        .map(|r| PaneRow {
            id: r["id"].as_str().unwrap_or_default().to_string(),
            pane_id: r["paneId"].as_str().unwrap_or_default().to_string(),
            name: r["name"].as_str().unwrap_or_default().to_string(),
            verdict: r["verdict"].as_str().unwrap_or("idle").to_string(),
            kind: r["kind"].as_str().unwrap_or("terminal").to_string(),
            why: r["why"].as_str().unwrap_or_default().to_string(),
            age_secs: r["ageSecs"].as_u64().unwrap_or(0),
            blocked_prompt: r["blocked"]["prompt"].as_str().map(|s| s.to_string()),
        })
        .collect();
    Some(SessionSnap {
        name: v["session"].as_str().unwrap_or_default().to_string(),
        health: v["health"].as_str().unwrap_or_default().to_string(),
        panes,
    })
}


// ── The agent inbox ──────────────────────────────────────────────────────────────────────
//
// Snapshots are ground truth: deterministic, append-only, never model-touched. The agent reads
// them in batches. `memory/cursor.json` maps a session id to the last snapshot FILENAME it
// consumed — filenames are ISO timestamps, so "unconsumed" is a lexicographic comparison and
// needs no parsing. Advancing the cursor is the agent's job, so a crashed run re-reads rather
// than skips.

/// Snapshot filenames under a session dir that the agent has not consumed, oldest first.
fn snapshots_after(sdir: &Path, after: Option<&str>) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(sdir.join("snapshots")) else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().to_string();
            if !n.ends_with(".json") {
                return None;
            }
            match after {
                Some(a) if n.as_str() <= a => None,
                _ => Some(n),
            }
        })
        .collect();
    names.sort();
    names
}

/// The one open batch, if there is one. Never more than a single file: a second open batch is
/// how a slow agent turns a busy period into a queue of disconnected wake-ups.
fn open_batch(repo: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(repo.join("inbox"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .map(|n| n.to_string_lossy().starts_with("batch-") && n.to_string_lossy().ends_with(".json"))
                .unwrap_or(false)
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

/// Compose (or extend) the batch the agent will work. Returns the batch path when there is
/// anything to do, `None` when every session is already consumed — the common, quiet case.
///
/// A session's entry carries the snapshot filenames rather than their contents: the agent has a
/// filesystem and the batch stays small enough to re-read cheaply on every merge.
pub fn compose_batch(repo: &Path, sessions: &[SessionSnap], ts: u64) -> Result<Option<PathBuf>> {
    let cursor: serde_json::Value = std::fs::read_to_string(repo.join("memory/cursor.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));

    let mut entries: Vec<serde_json::Value> = Vec::new();
    for s in sessions {
        let Some(sdir) = existing_session_dir(&s.name) else { continue };
        let id = sdir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let after = cursor.get(&id).and_then(|v| v.as_str());
        let pending = snapshots_after(&sdir, after);
        if pending.is_empty() {
            continue;
        }
        entries.push(serde_json::json!({
            "id": id,
            "name": s.name,
            "dir": sdir.display().to_string(),
            "after": after,
            "snapshots": pending,
            "blocked": s.panes.iter().filter(|p| p.verdict == "blocked")
                .map(|p| p.pane_id.clone()).collect::<Vec<_>>(),
        }));
    }
    if entries.is_empty() {
        return Ok(None);
    }

    let inbox = repo.join("inbox");
    std::fs::create_dir_all(&inbox)?;
    // Merge into the open batch when there is one, so related work stays one run and one
    // context. `opened_ts` is preserved — the agent should see how long the story has been
    // accumulating, not just when we last touched it.
    let (path, opened_ts) = match open_batch(repo) {
        Some(p) => {
            let prior: serde_json::Value = std::fs::read_to_string(&p)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            let o = prior.get("opened_ts").and_then(|v| v.as_u64()).unwrap_or(ts);
            (p, o)
        }
        None => (inbox.join(format!("batch-{}.json", iso(ts).replace(':', "-"))), ts),
    };
    let body = serde_json::json!({
        "v": 1,
        "opened": iso(opened_ts), "opened_ts": opened_ts,
        "updated": iso(ts), "updated_ts": ts,
        "sessions": entries,
    });
    std::fs::write(&path, serde_json::to_string_pretty(&body)?)?;
    Ok(Some(path))
}

/// Whether to wake the agent, and why. Two gates: a floor so a busy board cannot spiral the way
/// the LLM summaries did, and one exception — a workspace that has newly entered `blocked` wakes
/// it immediately, because that is the state where the engineer is the bottleneck and latency is
/// pure waste. A block already seen at the last nudge is not new and does not re-fire.
pub fn nudge_reason(repo: &Path, sessions: &[SessionSnap], ts: u64, floor_secs: u64) -> Option<&'static str> {
    let state: serde_json::Value = std::fs::read_to_string(repo.join("memory/agent.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let seen: Vec<String> = state["blocked"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let fresh_block = sessions.iter().any(|s| {
        s.panes.iter().any(|p| {
            p.verdict == "blocked" && !seen.contains(&format!("{}:{}", s.name, p.pane_id))
        })
    });
    if fresh_block {
        return Some("blocked");
    }
    let last = state["last_nudge_ts"].as_u64().unwrap_or(0);
    (ts.saturating_sub(last) >= floor_secs).then_some("material-change")
}

/// Record that the agent was woken. Stores the blocked set alongside, so the next tick can tell
/// a *new* block from one it has already been told about.
pub fn mark_nudged(repo: &Path, sessions: &[SessionSnap], ts: u64) -> Result<()> {
    let blocked: Vec<String> = sessions
        .iter()
        .flat_map(|s| {
            s.panes
                .iter()
                .filter(|p| p.verdict == "blocked")
                .map(move |p| format!("{}:{}", s.name, p.pane_id))
        })
        .collect();
    std::fs::create_dir_all(repo.join("memory"))?;
    std::fs::write(
        repo.join("memory/agent.json"),
        serde_json::to_string_pretty(&serde_json::json!({
            "last_nudge_ts": ts, "last_nudge": iso(ts), "blocked": blocked,
        }))?,
    )?;
    Ok(())
}

/// Score the runs the agent has finished, and record any that produced nothing.
///
/// A finished run is a batch file sitting in `inbox/done/`. For each one we ask the only question
/// that matters: did the sessions it named end up with a briefing written *after* the batch was
/// opened? A run that consumed its batch and wrote nothing is the dangerous failure — it looks
/// exactly like a quiet board from the outside, and without this it would never be noticed.
///
/// Appends one line per newly-scored run to `memory/runs.jsonl` and returns the rolling tally.
fn score_runs(repo: &Path, ts: u64) {
    let done = repo.join("inbox/done");
    let log = repo.join("memory/runs.jsonl");
    let seen: std::collections::HashSet<String> = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v["batch"].as_str().map(String::from))
        .collect();

    let mut lines = String::new();
    if let Ok(rd) = std::fs::read_dir(&done) {
        let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for path in entries {
            let name = match path.file_name().map(|n| n.to_string_lossy().to_string()) {
                Some(n) if n.ends_with(".json") && !seen.contains(&n) => n,
                _ => continue,
            };
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let Ok(batch) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
            let opened = batch["opened_ts"].as_u64().unwrap_or(0);
            let delivered = batch["delivered_ts"].as_u64().unwrap_or(0);
            // NOT the mtime of the file in done/. Moving it is a rename, and rename preserves
            // mtime — so that timestamp is when WE last wrote the batch (stamping delivered_ts),
            // which made every duration come out as zero. The receipt is the last thing written
            // in a run and is a real write, so its mtime is the honest finish time.
            let finished = mtime_secs(&repo.join("runs").join(&name)).max(mtime_secs(&path));
            let duration = (delivered > 0 && finished >= delivered).then(|| finished - delivered);

            let sessions = batch["sessions"].as_array().cloned().unwrap_or_default();

            // The agent's own account of the run. We verify the ACCOUNT against the filesystem
            // rather than checking a fixed list of files exists — see `docs/receipts.md`. A rule
            // of "produce a file per workspace" reliably produces a file per workspace, including
            // for the workspaces that did not change, which is the padding we are trying to stop.
            let receipt: serde_json::Value = std::fs::read_to_string(repo.join("runs").join(&name))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok())
                .unwrap_or_else(|| serde_json::json!({}));
            let claimed: Vec<String> = receipt["wrote"].as_array().map(|a| {
                a.iter().filter_map(|v| v.as_str().map(String::from)).collect()
            }).unwrap_or_default();

            // Phase breakdown, free: every artifact the agent writes carries an mtime, and the
            // receipt already names them. Statting those reconstructs the whole turn without the
            // agent reporting anything — so "why was that slow" is answerable rather than a
            // single opaque number.
            //
            //   ramp  delivered -> first write   process start, auth, reading the snapshots
            //   write first     -> last write    producing the documents
            //   wrap  last      -> batch moved   cursor, receipt, tidying up
            let stamps: Vec<u64> = Some(&claimed).map(|a| a.iter()
                .map(|p| mtime_secs(Path::new(p)))
                .filter(|&t| t > 0)
                .collect()).unwrap_or_default();
            let first_write = stamps.iter().copied().min();
            let last_write = stamps.iter().copied().max();
            let span = |a: Option<u64>, b: Option<u64>| match (a, b) {
                (Some(x), Some(y)) if y >= x => Some(y - x),
                _ => None,
            };
            let ramp = span(Some(delivered).filter(|&d| d > 0), first_write);
            let write = span(first_write, last_write);
            let wrap = span(last_write, Some(finished).filter(|&f| f > 0));

            let mut faults: Vec<serde_json::Value> = Vec::new();
            // 1. Every file it SAYS it wrote must exist and post-date the batch. This is the
            //    check that catches a run which reported success and did nothing.
            for f in &claimed {
                if mtime_secs(Path::new(f)) < opened {
                    faults.push(serde_json::json!({ "kind": "claimed-not-written", "path": f }));
                }
            }
            // 2. Every session in the batch must be accounted for — written about, or skipped
            //    WITH A REASON. Silence is the only thing not allowed; deciding there is nothing
            //    to say is a valid outcome and is recorded as one.
            for sess in &sessions {
                let sname = sess["name"].as_str().unwrap_or_default();
                let dir = sess["dir"].as_str().unwrap_or_default();
                let touched = claimed.iter().any(|f| f.starts_with(dir));
                let skipped = receipt["skipped"].as_array().is_some_and(|a| {
                    a.iter().any(|k| k["session"].as_str() == Some(sname)
                        && k["why"].as_str().is_some_and(|w| !w.trim().is_empty()))
                });
                if !touched && !skipped {
                    faults.push(serde_json::json!({ "kind": "unaccounted", "session": sname }));
                }
            }
            // 3. The cursor may not advance past what the batch actually offered — that would
            //    silently mark snapshots as read that nobody read.
            for sess in &sessions {
                let sid = sess["id"].as_str().unwrap_or_default();
                let newest = sess["snapshots"].as_array()
                    .and_then(|a| a.last()).and_then(|v| v.as_str()).unwrap_or_default();
                if let Some(at) = receipt["cursor"].get(sid).and_then(|v| v.as_str()) {
                    if !newest.is_empty() && at > newest {
                        faults.push(serde_json::json!({
                            "kind": "cursor-overrun", "session": sid, "at": at, "offered": newest,
                        }));
                    }
                }
            }
            let no_receipt = receipt.get("wrote").is_none();
            if no_receipt {
                faults.push(serde_json::json!({ "kind": "no-receipt" }));
            }
            lines.push_str(&format!(
                "{}\n",
                serde_json::json!({
                    "batch": name, "scored": iso(ts), "scored_ts": ts, "opened_ts": opened,
                    "delivered_ts": delivered, "finished_ts": finished,
                    "duration_secs": duration,
                    "ramp_secs": ramp, "write_secs": write, "wrap_secs": wrap,
                    "files_written": stamps.len(),
                    "sessions": sessions.len(), "claimed": claimed.len(),
                    "skipped": receipt["skipped"].as_array().map(|a| a.len()).unwrap_or(0),
                    "faults": faults, "ok": faults.is_empty(),
                })
            ));
        }
    }
    if !lines.is_empty() {
        let _ = std::fs::create_dir_all(repo.join("memory"));
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&log) {
            let _ = f.write_all(lines.as_bytes());
        }
    }
}

/// Rolling tally over the recent tail — a bad week should not be hidden by a good year. A pure
/// read: scoring is a write and happens on the tick, so asking for the view changes nothing.
fn run_tally(repo: &Path) -> (u64, u64, serde_json::Value) {
    let all: Vec<serde_json::Value> = std::fs::read_to_string(repo.join("memory/runs.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let tail: Vec<&serde_json::Value> = all.iter().rev().take(20).collect();
    let total = tail.len() as u64;
    let ok = tail.iter().filter(|v| v["ok"].as_bool().unwrap_or(false)).count() as u64;
    // p50 and p90 rather than a mean: run times are long-tailed, and the tail is the thing worth
    // knowing about.
    let pct = |key: &str, p: f64| -> Option<u64> {
        let mut xs: Vec<u64> = tail.iter().filter_map(|v| v[key].as_u64()).collect();
        if xs.is_empty() { return None; }
        xs.sort_unstable();
        Some(xs[(((xs.len() - 1) as f64) * p).round() as usize])
    };
    let last = tail.first();
    (ok, total, serde_json::json!({
        "lastSecs": last.and_then(|v| v["duration_secs"].as_u64()),
        "p50Secs": pct("duration_secs", 0.5),
        "p90Secs": pct("duration_secs", 0.9),
        "phases": {
            "rampP50": pct("ramp_secs", 0.5),
            "writeP50": pct("write_secs", 0.5),
            "wrapP50": pct("wrap_secs", 0.5),
        },
        "lastPhases": last.map(|v| serde_json::json!({
            "ramp": v["ramp_secs"], "write": v["write_secs"], "wrap": v["wrap_secs"],
            "files": v["files_written"],
        })),
        "samples": tail.iter().filter(|v| v["duration_secs"].as_u64().is_some()).count(),
    }))
}

/// Is the agent switched on?
///
/// Absent means off, so nothing runs an agent by merely existing. Present-and-empty means on, so
/// `touch` does the obvious thing from a shell. And because the CONTENTS decide it when there are
/// any, the phone can flip it through the `fs.write` it already has — no delete verb, no new
/// protocol frame, and the same file is the switch from either side.
pub fn agent_enabled(repo: &Path) -> bool {
    match std::fs::read_to_string(repo.join("agent.enabled")) {
        Err(_) => false,
        Ok(body) => !matches!(body.trim(), "0" | "off" | "false" | "no"),
    }
}

/// The file the phone writes to flip the switch.
pub fn agent_switch_path() -> Option<PathBuf> {
    Some(repo_dir()?.join("agent.enabled"))
}

/// Record that a turn was actually DELIVERED to the agent — called only once the bytes are in the
/// pane, never merely because we decided to send them. The first attempt failed exactly here: the
/// pane had just been created, the prompt went to a shell that had not finished starting, and
/// marking the run anyway would have parked the agent for a further twenty minutes.
pub fn mark_run(board_json: &str, ts: u64) {
    let Some(repo) = repo_dir() else { return };
    // Stamp the batch we just handed over. `opened_ts` is when the work ARRIVED, which can be
    // several floors before anyone was woken — so it cannot be a start time. This can.
    if let Some(p) = open_batch(&repo) {
        if let Ok(t) = std::fs::read_to_string(&p) {
            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&t) {
                v["delivered_ts"] = serde_json::json!(ts);
                v["delivered"] = serde_json::json!(iso(ts));
                let _ = std::fs::write(&p, v.to_string());
            }
        }
    }
    if let Some(snap) = parse_board(board_json) {
        let _ = mark_nudged(&repo, &[snap], ts);
    }
    let _ = std::fs::write(last_run_path(&repo), format!("{}\n", iso(ts)));
}

/// The file whose mtime gates the agent's cadence. Deleting it forces a run on the next tick —
/// that is its second job, and the reason the gate is a file rather than an in-memory instant:
/// `rm ~/.mars/manager/memory/last_run` is the whole iteration loop when working on the agent.
fn last_run_path(repo: &Path) -> PathBuf {
    repo.join("memory/last_run")
}

/// How long a lock may go unrefreshed before another daemon may take it.
///
/// Derived from how often the lock is REFRESHED — every manager tick, 60s by default — not from
/// the agent's own cadence. Deriving it from the agent floor (20 min) meant a killed daemon parked
/// the agent for an hour: its last refresh was seconds old, so every survivor politely backed off
/// from a lock whose owner no longer existed. Three missed refreshes is dead.
const AGENT_LOCK_STALE_SECS: u64 = 180;


/// Only one daemon may drive the agent. Every session runs its own tick, so without this each
/// would spawn its own `claude` in its own hidden pane and they would all write the same files.
/// The lock is claimable when stale so a killed daemon does not park the manager forever.
fn claim_agent_lock(repo: &Path, owner: &str, ts: u64, stale_secs: u64) -> bool {
    let path = repo.join("agent.lock");
    let held: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let holder = held["owner"].as_str().unwrap_or_default();
    let at = held["ts"].as_u64().unwrap_or(0);
    let ours = holder == owner;
    if !ours && !holder.is_empty() && ts.saturating_sub(at) < stale_secs {
        return false;
    }
    if let Some(d) = path.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({ "owner": owner, "ts": ts, "at": iso(ts) }))
            .unwrap_or_default(),
    );
    true
}

/// Decide whether to wake the agent, and compose what to type if so.
///
/// Returns the prompt line. `None` means "nothing to do" — which is the common case and is a
/// success: a quiet board costs no tokens at all. The batch is left open when we decline, so
/// work is never dropped, only deferred into a later, larger turn.
pub fn agent_tick(
    board_json: &str,
    ts: u64,
    floor_secs: u64,
    owner: &str,
) -> Option<String> {
    let repo = repo_dir()?;
    // The switch. Absent → the agent never runs, whatever the cadence says. Opt-in rather than
    // opt-out because waking it spawns a real `claude` process that spends real tokens: a fresh
    // install, a test run, or an isolated runtime must never do that by simply existing.
    //   touch ~/.mars/manager/agent.enabled     # on
    //   rm    ~/.mars/manager/agent.enabled     # off
    if !agent_enabled(&repo) {
        return None;
    }
    let snap = parse_board(board_json)?;
    let sessions = [snap];

    if !claim_agent_lock(&repo, owner, ts, AGENT_LOCK_STALE_SECS) {
        return None;
    }
    // The flag file is the cadence gate. Absent → due now, which is what makes deleting it the
    // testing lever. A newly-blocked workspace still bypasses the floor entirely.
    let last = std::fs::metadata(last_run_path(&repo))
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    let floor_passed = match last {
        None => true,
        Some(t) => ts.saturating_sub(t) >= floor_secs,
    };
    let reason = nudge_reason(&repo, &sessions, ts, floor_secs)?;
    if reason != "blocked" && !floor_passed {
        return None;
    }
    // Only now do we touch the inbox: no pending snapshots means no turn, whatever the clock says.
    // Compose the batch, but return only WHY we are waking. The turn's instruction lives in
    // prompt.md and is read by the shell at delivery — the one thing we most want to iterate on
    // should not need a recompile, and there is only ever one open batch to find.
    compose_batch(&repo, &sessions, ts).ok().flatten()?;
    Some(reason.to_string())
}

/// Write a file only if it is absent. `AGENTS.md`, `docs/**`, `policy.md` and the memory seeds
/// belong to the human and the agent — scaffolded once, then never touched again, so an edit is
/// never silently reverted by the next tick.
/// Our shipped docs carry `<!-- mars-doc-version: N -->`. We replace an on-disk copy whose marker
/// is older than ours — the agent's contract has to be able to change — but the moment a human
/// edits the marker away, the file is theirs and we never touch it again. "We maintain our own
/// docs until you make them yours."
fn doc_superseded(path: &Path, ours: &str) -> bool {
    fn version(text: &str) -> Option<u32> {
        let at = text.find("mars-doc-version:")?;
        text[at + 17..].trim_start().split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
    }
    let Some(mine) = version(ours) else { return false };
    match std::fs::read_to_string(path).ok().as_deref().and_then(version) {
        Some(theirs) => theirs < mine,
        None => false,
    }
}

fn seed(path: &Path, body: &str) -> Result<()> {
    if path.exists() && !doc_superseded(path, body) {
        return Ok(());
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(path, body)?;
    Ok(())
}

/// The comprehensive guide any coding agent reads on entry. A hub in `AGENTS.md` with spokes in
/// `docs/`, so the cached prefix stays small and detail is loaded only when it is needed.
fn scaffold_docs(repo: &Path) -> Result<()> {
    seed(&repo.join("AGENTS.md"), include_str!("manager_docs/AGENTS.md"))?;
    seed(&repo.join("docs/layout.md"), include_str!("manager_docs/layout.md"))?;
    seed(&repo.join("docs/cards.md"), include_str!("manager_docs/cards.md"))?;
    seed(&repo.join("docs/memos.md"), include_str!("manager_docs/memos.md"))?;
    seed(&repo.join("docs/briefing.md"), include_str!("manager_docs/briefing.md"))?;
    seed(&repo.join("docs/workspaces.md"), include_str!("manager_docs/workspaces.md"))?;
    seed(&repo.join("docs/receipts.md"), include_str!("manager_docs/receipts.md"))?;
    seed(&repo.join("prompt.md"), include_str!("manager_docs/prompt.md"))?;
    let runner = repo.join("run.sh");
    seed(&runner, include_str!("manager_docs/run.sh"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(md) = std::fs::metadata(&runner) {
            let mut perm = md.permissions();
            if perm.mode() & 0o111 == 0 {
                perm.set_mode(0o755);
                let _ = std::fs::set_permissions(&runner, perm);
            }
        }
    }
    seed(&repo.join("docs/memory.md"), include_str!("manager_docs/memory.md"))?;
    seed(&repo.join("docs/tools.md"), include_str!("manager_docs/tools.md"))?;
    seed(&repo.join("policy.md"), include_str!("manager_docs/policy.md"))?;
    seed(
        &repo.join("memory/beliefs.md"),
        "# Beliefs\n\nRead this first. Revised, never appended. Under 200 lines.\n\n\
         ## Where things are\n\n\
         - [projects.md](projects.md) — projects and workstreams, with their workspaces nested.\n\n\
         ## Cross-cutting\n\n\
         _Beliefs belonging to no single project. Nothing yet._\n\n\
         See [docs/memory.md](../docs/memory.md).\n",
    )?;
    seed(
        &repo.join("memory/projects.md"),
        "# Projects\n\nOne section per project. `Purpose:` is durable; `State:` and `Next:` are\n\
         today. Workspaces nest underneath the project they serve.\n\n\
         _Nothing observed yet._\n",
    )?;
    seed(&repo.join("memory/cursor.json"), "{}\n")?;
    seed(
        &repo.join(".gitignore"),
        "inbox/\nsessions/*/snapshots/\n*.tmp\n",
    )?;
    Ok(())
}

/// The whole deterministic pass. Pure once `sessions` is in hand, so `--selfcheck` drives it
/// directly with synthetic boards and never needs a live daemon.
pub fn emit(
    repo: &Path,
    origin: &str,
    sessions: &[SessionSnap],
    ts: u64,
    stale_secs: u64,
    output: &serde_json::Value,
) -> Result<Vec<String>> {
    scaffold_docs(repo)?;
    score_runs(repo, ts);
    let mut events: Vec<String> = Vec::new();
    let mut briefs: Vec<(String, String)> = Vec::new();

    for s in sessions {
        // Session artifacts live under ~/.mars/sessions/<id>/, NOT under the manager repo keyed by
        // name. A rename now rewrites meta.json instead of forking a new directory.
        let Some(sdir) =
            session_dir_fp(&s.name, origin, ts, &fingerprint_material(s), &fingerprint_detail(s))
        else {
            continue;
        };
        let cards = sdir.join("memos");
        let snaps = sdir.join("snapshots");
        std::fs::create_dir_all(&cards)?;
        std::fs::create_dir_all(&snaps)?;

        // 1. The stimulus for this session — what an agent reads, or a human with `jq`.
        let stim = serde_json::json!({
            "at": iso(ts), "at_ts": ts, "origin": origin, "session": s.name,
            "health": s.health,
            "goals": goals_json(),
            "workspaces": s.panes.iter().map(|p| {
                // tail = where it landed, delta = what changed, signals = what to act on.
                let o = &output[&p.pane_id];
                let tail: Vec<String> = o["tail"].as_array().map(|a| a.iter()
                    .filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
                let delta: Vec<String> = o["delta"].as_array().map(|a| a.iter()
                    .filter_map(|v| v.as_str().map(String::from)).collect()).unwrap_or_default();
                let sig = signals(&tail, &delta);
                serde_json::json!({
                    "id": p.id, "paneId": p.pane_id, "name": p.name, "verdict": p.verdict,
                    "kind": p.kind, "why": p.why, "ageSecs": p.age_secs,
                    "blockedPrompt": p.blocked_prompt,
                    "output": { "tail": tail, "delta": delta, "signals": sig },
                })
            }).collect::<Vec<_>>(),
        });
        std::fs::write(
            snaps.join(format!("{}.json", iso(ts).replace(':', "-"))),
            serde_json::to_string_pretty(&stim)?,
        )?;

        // 2. Reflex cards. One open card per (workspace, kind) — a pane blocked for six hours is
        //    one card, not one per tick.
        let open = read_open_cards(&cards);
        let mut live: Vec<(String, &'static str)> = Vec::new();
        for p in &s.panes {
            let kind = match p.verdict.as_str() {
                "blocked" => Some(Reflex::Blocked),
                "failed" => Some(Reflex::Failed),
                _ => None,
            };
            let Some(kind) = kind else { continue };
            live.push((p.pane_id.clone(), kind.slug()));
            if open.iter().any(|c| !c.expired && c.pane == p.pane_id && c.kind == kind.slug()) {
                continue;
            }
            let id = format!("card-{}-{}-{}-{ts}", s.name, p.pane_id, kind.slug());
            write_card(&cards, &id, kind, origin, &s.name, p, ts)?;
            events.push(format!(
                "`{}` **{}** {} — card `{id}`",
                s.name,
                p.name,
                if kind == Reflex::Blocked { "blocked on a prompt" } else { "failed" }
            ));
        }

        // 3. Expire what no longer holds. Declarative staleness: nothing re-examined the card,
        //    the condition simply stopped being true.
        for c in &open {
            if c.expired || c.kind.is_empty() {
                continue;
            }
            if !live.iter().any(|(pid, k)| *pid == c.pane && *k == c.kind) {
                expire_card(c, ts)?;
                events.push(format!("`{}` resolved — card `{}` ({})", s.name, c.id, c.headline));
            }
        }

        // 4. session → workspaces → summary.
        let brief = session_briefing(s, ts);
        // NOT mission_briefing.md — that file belongs to the agent now, and rewriting it every
        // tick would make its work vanish every 60 seconds. The deterministic sentence lives
        // beside it as the visible floor, and reaches the phone through the index either way.
        write_if_changed(&computed(sdir.join("mission_briefing.md")), &brief)?;
        // …and one document per workspace, so a single workspace can be read (or later enriched
        // by an agent) without touching the session summary.
        let wdir = sdir.join("workspaces");
        std::fs::create_dir_all(&wdir)?;
        for p in &s.panes {
            // NOT <pane>.md — that belongs to the agent, exactly as mission_briefing.md does.
            // Writing it here erased her summaries within a tick, which is why every workspace
            // on the phone read "computed" however well the agent had described it.
            write_if_changed(
                &computed(wdir.join(format!("{}.md", p.pane_id))),
                &format!(
                    "# {} · {}\n\n{}\n\n- state: {}\n- kind: {}\n",
                    p.name, s.name, workspace_summary(p), p.verdict, p.kind
                ),
            )?;
        }
        append_timeline(&sdir, ts, &events)?;
        briefs.push((s.name.clone(), brief));
    }

    append_timeline(repo, ts, &events)?;
    Ok(events)
}

#[cfg(feature = "ssh")]
fn host_name() -> String {
    crate::sys::proc::hostname().unwrap_or_else(|| "local".into())
}
#[cfg(not(feature = "ssh"))]
fn host_name() -> String {
    "local".into()
}



/// The manager's own root: the guide, the agent's memory, and the aggregate index Rover reads.
/// Session artifacts do NOT live here — see `sessions_root`.
pub fn repo_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("MARS_MANAGER_DIR") {
        return Some(PathBuf::from(d));
    }
    if let Some(rt) = std::env::var_os(crate::session::RUNTIME_DIR_ENV) {
        return Some(PathBuf::from(rt).join("manager"));
    }
    crate::sys::paths::home_dir().map(|h| h.join(".mars").join("manager"))
}

/// `~/.mars/sessions/` — one directory per session, and the eventual home for everything
/// session-scoped (worklog, briefings, goals), which today is interleaved in shared files keyed by
/// name. Keeping the same isolation hook as the rest so a test run never writes into a real repo.
pub fn sessions_root() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("MARS_SESSIONS_DIR") {
        return Some(PathBuf::from(d));
    }
    if let Some(rt) = std::env::var_os(crate::session::RUNTIME_DIR_ENV) {
        return Some(PathBuf::from(rt).join("sessions"));
    }
    crate::sys::paths::home_dir().map(|h| h.join(".mars").join("sessions"))
}

/// Find a session's directory without minting one — used by the idle fast path, which must not
/// create anything just to discover there is nothing to do.
pub fn existing_session_dir_pub(name: &str) -> Option<PathBuf> { existing_session_dir(name) }

fn existing_session_dir(name: &str) -> Option<PathBuf> {
    let root = sessions_root()?;
    for e in std::fs::read_dir(root).ok()?.flatten() {
        // `continue`, not `?`: one directory without a readable meta.json must not abort the
        // search. As a `?` it did, so the idle fast path never found its own session and every
        // tick fell through to a full regeneration.
        let Ok(txt) = std::fs::read_to_string(e.path().join("meta.json")) else { continue };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { continue };
        if v["name"].as_str() == Some(name) {
            return Some(e.path());
        }
    }
    None
}

/// Resolve (or mint) the directory for a session, keyed by an id the session KEEPS.
///
/// The id is immutable once minted and the current name lives in `meta.json`, so a rename rewrites
/// one field instead of moving a directory. Keying by name is what turned four renames of one
/// daemon into 118 directories.
///
/// Lookup is still by name today — the daemon has no persisted session id yet — but the *storage*
/// is already id-keyed, which is the half that has to be right first.
pub fn session_dir(name: &str, instance_id: &str, ts: u64) -> Option<PathBuf> {
    session_dir_fp(name, instance_id, ts, "", "")
}

pub fn session_dir_fp(
    name: &str,
    instance_id: &str,
    ts: u64,
    fp: &str,
    detail: &str,
) -> Option<PathBuf> {
    let root = sessions_root()?;
    std::fs::create_dir_all(&root).ok()?;
    // An existing session with this name keeps its directory.
    if let Ok(rd) = std::fs::read_dir(&root) {
        for e in rd.flatten() {
            let meta = e.path().join("meta.json");
            let Ok(txt) = std::fs::read_to_string(&meta) else { continue };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { continue };
            if v["name"].as_str() == Some(name) {
                write_meta(&e.path(), v["id"].as_str().unwrap_or(name), name, instance_id, ts, fp, detail);
                return Some(e.path());
            }
        }
    }
    // Mint one. Start from the name for legibility, then disambiguate.
    let base: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let mut id = base.clone();
    let mut n = 2;
    while root.join(&id).exists() {
        id = format!("{base}-{n}");
        n += 1;
    }
    let dir = root.join(&id);
    std::fs::create_dir_all(&dir).ok()?;
    write_meta(&dir, &id, name, instance_id, ts, fp, detail);
    Some(dir)
}

/// `meta.json` — the indirection that makes a rename cheap. The id is the directory; the name is
/// data.
fn write_meta(
    dir: &Path, id: &str, name: &str, instance_id: &str, ts: u64, fp: &str, detail: &str,
) {
    let existing: Option<serde_json::Value> = std::fs::read_to_string(dir.join("meta.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok());
    let created = existing
        .as_ref()
        .and_then(|v| v["created_ts"].as_u64())
        .unwrap_or(ts);
    let meta = serde_json::json!({
        "id": id, "name": name, "instance_id": instance_id,
        "created": iso(created), "created_ts": created,
        "updated": iso(ts), "updated_ts": ts,
        // What the idle fast path compares against.
        "fingerprint": fp,
        "detail": detail,
    });
    if let Ok(txt) = serde_json::to_string_pretty(&meta) {
        let _ = std::fs::write(dir.join("meta.json"), txt);
    }
}


/// Two fingerprints, because two kinds of change deserve two answers.
///
/// MATERIAL — which workspaces exist and what state they are in. A verdict flipping to failed or
/// blocked is news; it should reach the phone at once.
///
/// DETAIL — the descriptive text a workspace carries. For a pane running an agent this changes
/// every time it prints, which is many times a second. Regenerating on it would rewrite the tree
/// continuously and churn the phone for nothing, so it is rate-limited instead.
///
/// Neither includes ages or timestamps: those change on every board push and would make every tick
/// look like news.
fn fingerprint_material(s: &SessionSnap) -> String {
    let mut parts: Vec<String> = s
        .panes
        .iter()
        .map(|p| {
            format!(
                "{}|{}|{}|{}|{}",
                p.pane_id, p.name, p.verdict, p.kind,
                p.blocked_prompt.as_deref().unwrap_or("")
            )
        })
        .collect();
    parts.sort();
    format!("{}::{}", s.name, parts.join("~"))
}

fn fingerprint_detail(s: &SessionSnap) -> String {
    let mut parts: Vec<String> = s
        .panes
        .iter()
        .map(|p| format!("{}|{}", p.pane_id, p.why.trim()))
        .collect();
    parts.sort();
    parts.join("~")
}

/// Write ONE session's subtree, from the daemon's own state. Called on a timer by the session
/// daemon, so cards exist before anyone looks — the point of an ambient layer is that nobody has
/// to ask for it.
///
/// The daemon that owns a session is the only process that writes `sessions/<name>/`, so the
/// "one writer per path" rule is enforced by process boundaries rather than by convention.
/// Takes no repo path ON PURPOSE. It derives one from `repo_dir()`, so a caller cannot pass the
/// wrong root — a hardcoded `~/.mars/manager` at the daemon's call site is exactly how an isolated
/// test run wrote into the user's live repo.
/// Every live session as the tree records it, for an index rebuild that is not tied to one
/// session's board frame.
/// Is this session backed by a running daemon? A directory that outlives its daemon is history,
/// not a session — worth keeping on disk, not worth describing as the present.
///
/// MARS_VIEW_ALL_SESSIONS exists for the headless checks, which build session trees with no
/// daemons behind them and would otherwise see an empty view.
fn session_is_live(name: &str) -> bool {
    std::env::var("MARS_VIEW_ALL_SESSIONS").is_ok()
        || crate::session::socket_path(name).map(|p| p.exists()).unwrap_or(false)
}

fn read_all_sessions() -> Vec<SessionSnap> {
    let Some(root) = sessions_root() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let dir = e.path();
        let Ok(text) = std::fs::read_to_string(dir.join("meta.json")) else { continue };
        let Ok(meta) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let Some(name) = meta["name"].as_str() else { continue };
        // A directory outliving its daemon is not a session. Without this the view reports every
        // session that ever existed, and the agent writes a briefing about five boards when one
        // is live — history worth keeping on disk, but not worth describing as the present.
        //
        // MARS_VIEW_ALL_SESSIONS exists for the headless checks, which build session trees with
        // no daemons behind them and would otherwise see an empty view.
        if !session_is_live(name) {
            continue;
        }
        // Reconstruct the board from the NEWEST snapshot only — one file per session, not the
        // directory. Snapshots are ground truth and the latest one is the current board, so the
        // query stays bounded by how many sessions exist rather than by how long they have run.
        let newest = std::fs::read_dir(dir.join("snapshots")).ok().and_then(|rd| {
            let mut names: Vec<PathBuf> = rd.flatten().map(|e| e.path())
                .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json")).collect();
            names.sort();
            names.pop()
        });
        let (health, panes) = newest
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .map(|v| {
                let panes = v["workspaces"].as_array().map(|a| a.iter().map(|w| PaneRow {
                    id: w["id"].as_str().unwrap_or_default().to_string(),
                    pane_id: w["paneId"].as_str().unwrap_or_default().to_string(),
                    name: w["name"].as_str().unwrap_or_default().to_string(),
                    verdict: w["verdict"].as_str().unwrap_or("idle").to_string(),
                    kind: w["kind"].as_str().unwrap_or("terminal").to_string(),
                    why: w["why"].as_str().unwrap_or_default().to_string(),
                    age_secs: w["ageSecs"].as_u64().unwrap_or(0),
                    blocked_prompt: w["blockedPrompt"].as_str().map(|s| s.to_string()),
                }).collect::<Vec<_>>()).unwrap_or_default();
                (v["health"].as_str().unwrap_or_default().to_string(), panes)
            })
            .unwrap_or_default();
        out.push(SessionSnap { name: name.to_string(), health, panes });
    }
    out
}

pub fn tick_session(
    origin: &str,
    board_json: &str,
    ts: u64,
    keep: usize,
    detail_min_secs: u64,
    stale_secs: u64,
    output: &serde_json::Value,
) -> Result<()> {
    let Some(repo) = repo_dir() else { return Ok(()) };
    let repo = repo.as_path();
    // Bookkeeping first, on every tick and before any early return: a finished agent run must be
    // scored whether or not the board moved, and the read path must stay free of writes.
    score_runs(repo, ts);
    let Some(snap) = parse_board(board_json) else { return Ok(()) };
    if snap.name.is_empty() {
        return Ok(());
    }
    // An idle session should cost ONE read and zero writes, however often the timer fires. The
    // rate was never the problem — regenerating unconditionally was. Nothing here is news unless
    // a workspace appeared, vanished, changed state, or changed what it says.
    let (mat, det) = (fingerprint_material(&snap), fingerprint_detail(&snap));
    if let Some(dir) = existing_session_dir(&snap.name) {
        if let Some(m) = std::fs::read_to_string(dir.join("meta.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        {
            let material_same = m["fingerprint"].as_str() == Some(mat.as_str());
            let detail_same = m["detail"].as_str() == Some(det.as_str());
            let since = ts.saturating_sub(m["updated_ts"].as_u64().unwrap_or(0));
            // Nothing material, and either the text is the same or it changed too recently to be
            // worth rewriting the tree and re-rendering a phone for.
            if material_same && (detail_same || since < detail_min_secs) {
                // The BOARD is unchanged — so write no snapshot, no reflex memo, no tree. But the
                // index still has to be reassembled, because the agent writes on her own clock and
                // her prose is the thing most likely to have changed since the last tick. Gating
                // assembly on board movement meant a briefing written over an idle board never
                // reached the phone at all, and score_runs never ran to notice the turn happened.
                // write_index has its own unchanged-check, so on a truly quiet system this still
                // costs a read and no write.
                // Docs must still reach an idle system; nothing else is owed here, because
                // no view is stored to go stale.
                scaffold_docs(repo)?;
                return Ok(());
            }
        }
    }
    scaffold_docs(repo)?;
    emit(repo, origin, std::slice::from_ref(&snap), ts, stale_secs, output)?;
    if let Some(d) = existing_session_dir(&snap.name) {
        prune_snapshots(&d.join("snapshots"), keep)?;
    }
    Ok(())
}

/// Keep the newest `keep` stimuli per session. Written every tick forever otherwise, and a
/// directory that only grows is a directory someone eventually has to explain.
fn prune_snapshots(dir: &Path, keep: usize) -> Result<()> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Ok(()) };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    if files.len() <= keep {
        return Ok(());
    }
    files.sort(); // ISO names sort chronologically
    for p in files.iter().take(files.len() - keep) {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

/// CLI: `mars snapshot [--repo DIR]`.
pub fn snapshot_main(repo: Option<String>) -> Result<()> {
    let repo = repo
        .map(PathBuf::from)
        .or_else(repo_dir)
        .ok_or_else(|| anyhow::anyhow!("no --repo and no home directory"))?;
    // `hostname()` lives behind the ssh feature; the memory-free SKU still needs an origin, and
    // an explicit override is useful anyway when one machine hosts several logical workspaces.
    let origin = std::env::var("MARS_ORIGIN").ok().filter(|s| !s.is_empty()).unwrap_or_else(host_name);
    let sessions: Vec<SessionSnap> = crate::session::list_sessions()?
        .into_iter()
        .filter(|(_, alive, _)| *alive)
        .filter_map(|(name, _, _)| board_of(&name))
        .collect();
    let ts = now_secs();
    let events = emit(&repo, &origin, &sessions, ts, 2700, &serde_json::Value::Null)?;
    println!(
        "snapshot {} · {} session(s) · {} event(s) → {}",
        iso(ts),
        sessions.len(),
        events.len(),
        repo.display()
    );
    for e in &events {
        println!("  {}", e.replace("**", ""));
    }
    Ok(())
}

// ── `mars manager` — the agent loop, detached from the daemon ────────────────────────────
//
// The daemon decides WHEN a turn should happen; these decide nothing and just do it. Keeping the
// two welded together meant every experiment on the agent needed a daemon on the right binary, a
// board that happened to change, an elapsed floor, a free lock and an idle pane — five gates
// between an edit and an observation. Policy stays in `agent_tick`; the mechanism lives here.

/// Snapshot every live session right now, whatever the fingerprint says.
///
/// The tick deliberately writes nothing when nothing changed, which is correct in production and
/// useless when you want something to feed the agent on demand.
pub fn force_snapshot(ts: u64) -> Result<usize> {
    let Some(repo) = repo_dir() else { return Ok(0) };
    let mut n = 0;
    for name in live_session_names() {
        let Some(snap) = board_of(&name) else { continue };
        emit(&repo, &host_name(), std::slice::from_ref(&snap), ts, 2700, &serde_json::Value::Null)?;
        n += 1;
    }
    Ok(n)
}

fn live_session_names() -> Vec<String> {
    let Some(root) = sessions_root() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    rd.flatten()
        .filter_map(|e| {
            let meta: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(e.path().join("meta.json")).ok()?).ok()?;
            let name = meta["name"].as_str()?.to_string();
            session_is_live(&name).then_some(name)
        })
        .collect()
}

/// Run exactly one turn, synchronously, and report what happened. No daemon, no cadence, no lock.
pub fn run_once(ts: u64, force: bool) -> Result<String> {
    let Some(repo) = repo_dir() else { anyhow::bail!("no manager repo") };
    scaffold_docs(&repo)?;
    if force {
        let n = force_snapshot(ts)?;
        println!("snapshotted {n} live session(s)");
    }
    let sessions: Vec<SessionSnap> = live_session_names().iter().filter_map(|n| board_of(n)).collect();
    let Some(batch) = compose_batch(&repo, &sessions, ts)? else {
        return Ok("nothing to do — no unconsumed snapshots (try --force)".into());
    };
    let name = batch.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    // Stamp BEFORE running. The agent moves the batch into done/ as its last act, so writing
    // afterwards both misses the file and recreates a phantom open batch in inbox/.
    if let Ok(t) = std::fs::read_to_string(&batch) {
        if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&t) {
            v["delivered_ts"] = serde_json::json!(ts);
            let _ = std::fs::write(&batch, v.to_string());
        }
    }
    println!("running batch {name} …");
    let started = std::time::Instant::now();
    let out = std::process::Command::new("sh")
        .arg(repo.join("run.sh"))
        .current_dir(&repo)
        .output()?;
    let secs = started.elapsed().as_secs();
    print!("{}", String::from_utf8_lossy(&out.stdout));
    if !out.stderr.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
    }
    score_runs(&repo, ts + secs + 1);
    Ok(format!("turn finished in {secs}s (exit {})", out.status))
}

/// Why the agent is or is not about to run. Every gate, in one place, instead of five files.
pub fn status_report(ts: u64) -> Result<String> {
    let Some(repo) = repo_dir() else { anyhow::bail!("no manager repo") };
    let live = live_session_names();
    let pending: usize = live
        .iter()
        .filter_map(|n| existing_session_dir(n))
        .map(|d| snapshots_after(&d, None).len())
        .sum();
    let last = std::fs::metadata(last_run_path(&repo))
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    let (ok, total, timing) = run_tally(&repo);
    let mut s = String::new();
    s.push_str(&format!("repo          {}\n", repo.display()));
    s.push_str(&format!("agent         {}\n", if agent_enabled(&repo) { "ON" } else { "OFF — touch agent.enabled" }));
    s.push_str(&format!("live sessions {}\n", if live.is_empty() { "(none)".into() } else { live.join(", ") }));
    s.push_str(&format!("snapshots     {pending} on disk\n"));
    s.push_str(&format!("open batch    {}\n", open_batch(&repo).map(|p| p.file_name().unwrap_or_default().to_string_lossy().to_string()).unwrap_or_else(|| "(none)".into())));
    s.push_str(&format!("last run      {}\n", last.map(|t| format!("{} ago", human_age(ts.saturating_sub(t)))).unwrap_or_else(|| "never (floor open)".into())));
    s.push_str(&format!("lock          {}\n", if repo.join("agent.lock").exists() { "held" } else { "free" }));
    s.push_str(&format!("runs          {ok}/{total} clean · {timing}\n"));
    Ok(s)
}

// ── Pane output: what the agent was starved of ───────────────────────────────────────────
//
// A snapshot used to carry a pane's name, verdict and age, and nothing else — so a briefing could
// only restate the status LED, and the deterministic sentence did that better for free. These
// give the model something a model is actually good at: reading output and deciding what matters.

/// Strip ANSI so the agent reads text rather than escape codes. Rows arrive formatted because the
/// same log feeds the phone's scrollback, which does want them.
pub fn plain(row: &[u8]) -> String {
    let mut out = String::with_capacity(row.len());
    let mut i = 0;
    while i < row.len() {
        if row[i] == 0x1b {
            i += 1;
            if i < row.len() && row[i] == b'[' {
                i += 1;
                while i < row.len() && !row[i].is_ascii_alphabetic() {
                    i += 1;
                }
            }
            i += 1;
            continue;
        }
        out.push(row[i] as char);
        i += 1;
    }
    out.trim_end().to_string()
}

/// What is worth acting on, pulled out of the text by pattern rather than by judgement.
///
/// Everything here is arithmetic: a shell prompt, an exit code, a `[y/N]`, a compiler error, a
/// test tally. Pushing it below the model is the same move that made the deterministic floor
/// work, one level up — and it makes citations precise, because a claim can point at an extracted
/// signal instead of a line number in a blob.
/// The number immediately preceding `word`, e.g. `3` in "3 failed".
///
/// Must be ADJACENT, and the last such match wins. Splitting on the word and taking the trailing
/// digits of whatever came before matched the wrong thing on the common line
/// `test result: FAILED. 118 passed; 3 failed` — the first " failed" there is part of the verdict,
/// with no number in front of it at all.
fn count_before(line: &str, word: &str) -> Option<u64> {
    let mut found = None;
    let mut from = 0;
    while let Some(at) = line[from..].find(word) {
        let end = from + at;
        let digits: String = line[..end].chars().rev().take_while(|c| c.is_ascii_digit())
            .collect::<Vec<_>>().into_iter().rev().collect();
        if let Ok(n) = digits.parse() {
            found = Some(n);
        }
        from = end + word.len();
    }
    found
}

pub fn signals(tail: &[String], delta: &[String]) -> serde_json::Value {
    let all: Vec<&String> = delta.iter().chain(tail.iter()).collect();
    let mut errors: Vec<String> = Vec::new();
    let mut prompt: Option<String> = None;
    let mut passed: Option<u64> = None;
    let mut failed: Option<u64> = None;
    let mut exit: Option<i64> = None;

    for line in &all {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }
        let low = l.to_ascii_lowercase();
        if errors.len() < 5
            && (low.starts_with("error") || low.contains("error[") || low.starts_with("panicked at")
                || low.starts_with("fatal:") || low.contains("failed to compile"))
        {
            errors.push(l.chars().take(160).collect());
        }
        // A question the engineer has to answer is the single most actionable thing on a board.
        if prompt.is_none() && (l.contains("[y/N]") || l.contains("[Y/n]") || l.contains("(y/n)")) {
            prompt = Some(l.chars().take(160).collect());
        }
        if let Some(n) = count_before(&low, " passed") {
            passed = Some(n);
        }
        if let Some(n) = count_before(&low, " failed") {
            failed = Some(n);
        }
        if let Some(rest) = low.strip_prefix("exit code ").or_else(|| low.strip_prefix("exit status ")) {
            exit = rest.trim().parse().ok();
        }
    }

    let mut out = serde_json::Map::new();
    if !errors.is_empty() {
        out.insert("errors".into(), serde_json::json!(errors));
    }
    if let Some(p) = prompt {
        out.insert("prompt".into(), serde_json::json!(p));
    }
    if let Some(e) = exit {
        out.insert("exit".into(), serde_json::json!(e));
    }
    if passed.is_some() || failed.is_some() {
        out.insert("counts".into(), serde_json::json!({ "passed": passed, "failed": failed }));
    }
    serde_json::Value::Object(out)
}

/// Declared intent. `goals.json` and `mission.json` have existed unread this whole time, which is
/// why memo priority has been a guess: importance is relative to a goal, and the agent had none.
pub fn goals_json() -> serde_json::Value {
    let Some(home) = dirs_home() else { return serde_json::Value::Null };
    let read = |n: &str| -> Option<serde_json::Value> {
        serde_json::from_str(&std::fs::read_to_string(home.join(".mars").join(n)).ok()?).ok()
    };
    let mut out = serde_json::Map::new();
    if let Some(g) = read("goals.json") {
        out.insert("goals".into(), g);
    }
    if let Some(m) = read("mission.json") {
        out.insert("mission".into(), m);
    }
    if out.is_empty() { serde_json::Value::Null } else { serde_json::Value::Object(out) }
}

fn dirs_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}
