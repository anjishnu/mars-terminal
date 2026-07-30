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
        let is_card = p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
            n.starts_with("card-") && n.ends_with(".md")
        });
        if !is_card {
            continue;
        }
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

/// Write a reflex card. Markdown body, YAML-ish frontmatter — structure in the frontmatter,
/// meaning in the body, so a parse failure degrades to a readable document rather than to nothing.
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
         headline: \"{headline}\"\n\
         expired: false\n\
         {actions}\
         ---\n\
         {body}",
        created = iso(ts),
        sev = kind.severity(),
        pane_id = pane.pane_id,
        kind_slug = kind.slug(),
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

/// A one-line status for a workspace. Deterministic: verdict, age, and the board's own
/// description. This is what replaces a per-pane LLM summary call — counting and naming are not
/// judgement, and a model adds latency and cost to produce the same sentence.
fn workspace_summary(p: &PaneRow) -> String {
    let why = p.why.trim();
    match p.verdict.as_str() {
        "blocked" => format!("waiting on input · {}{}", human_age(p.age_secs),
                             if why.is_empty() { String::new() } else { format!(" · {why}") }),
        "failed" => format!("failed · {}{}", human_age(p.age_secs),
                            if why.is_empty() { String::new() } else { format!(" · {why}") }),
        "running" => format!("running · {}{}", human_age(p.age_secs),
                             if why.is_empty() { String::new() } else { format!(" · {why}") }),
        "done" => format!("finished · {}", human_age(p.age_secs)),
        _ if !why.is_empty() => format!("{why} · idle {}", human_age(p.age_secs)),
        _ => format!("idle · {}", human_age(p.age_secs)),
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
            "{} {} waiting on you — {} now.",
            names(&blocked),
            if blocked.len() == 1 { "is" } else { "are" },
            human_age(oldest)
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
    md.push_str(&format!("# {} · mission briefing\n\n_{}_\n\n", s.name, iso(ts)));
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
            md.push_str(&format!("- {mark} **{}** — {} · {}\n", p.name, p.verdict, human_age(p.age_secs)));
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
            md.push_str(&format!("| {} | {} | {} |\n", p.name, p.verdict, human_age(p.age_secs)));
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
fn write_index(
    repo: &Path,
    briefs: &[(String, String)],
    sessions: &[SessionSnap],
    ts: u64,
) -> Result<()> {
    // Scan the TREE, not just the sessions this process just wrote: each session's daemon owns
    // its own subtree, so an index built from one process's view would omit the others. The
    // directory is the truth; this is a cache of it.
    let mut names: Vec<String> = std::fs::read_dir(repo.join("sessions"))
        .map(|rd| rd.flatten().filter(|e| e.path().is_dir())
            .filter_map(|e| e.file_name().to_str().map(String::from)).collect())
        .unwrap_or_default();
    names.sort();
    let mut all: Vec<(String, String)> = names
        .into_iter()
        .map(|n| {
            let b = briefs.iter().find(|(bn, _)| *bn == n).map(|(_, b)| b.clone()).or_else(|| {
                std::fs::read_to_string(repo.join("sessions").join(&n).join("mission_briefing.md")).ok()
            });
            (n, b.unwrap_or_default())
        })
        .collect();
    if all.is_empty() {
        all = briefs.to_vec();
    }
    let briefs: &[(String, String)] = &all;
    let mut cards: Vec<serde_json::Value> = Vec::new();
    for (name, _) in briefs {
        let dir = repo.join("sessions").join(name).join("cards");
        for c in read_open_cards(&dir) {
            let text = std::fs::read_to_string(&c.file).unwrap_or_default();
            let (front, body) = split_front(&text).unwrap_or(("", ""));
            cards.push(serde_json::json!({
                "id": c.id,
                "path": format!("sessions/{}/cards/{}", name,
                                c.file.file_name().and_then(|n| n.to_str()).unwrap_or_default()),
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
    let rank = |s: &str| match s { "block" => 0, "warn" => 1, "info" => 2, _ => 3 };
    cards.sort_by(|a, b| {
        let (sa, sb) = (a["severity"].as_str().unwrap_or(""), b["severity"].as_str().unwrap_or(""));
        a["expired"].as_bool().unwrap_or(false).cmp(&b["expired"].as_bool().unwrap_or(false))
            .then(rank(sa).cmp(&rank(sb)))
            .then(b["created_ts"].as_u64().unwrap_or(0).cmp(&a["created_ts"].as_u64().unwrap_or(0)))
    });
    let index = serde_json::json!({
        "generated": iso(ts), "generated_ts": ts,
        "sessions": briefs.iter().map(|(n, b)| {
            let snap = sessions.iter().find(|s| &s.name == n);
            serde_json::json!({
                "name": n,
                "briefing": b,
                "narrative": snap.map(session_narrative).unwrap_or_else(|| {
                    // A session another daemon owns: reuse the prose already in its briefing
                    // rather than inventing a second source of truth.
                    b.lines().find(|l| l.contains("need") || l.contains("Nothing needs"))
                        .map(|l| l.replace("**", "").trim().to_string())
                        .unwrap_or_default()
                }),
                "path": format!("sessions/{n}/mission_briefing.md"),
                "workspaces": snap.map(|s| s.panes.iter().map(|p| serde_json::json!({
                    "pane": p.pane_id, "name": p.name, "verdict": p.verdict,
                    "kind": p.kind, "ageSecs": p.age_secs, "summary": workspace_summary(p),
                })).collect::<Vec<_>>()).unwrap_or_default(),
            })
        }).collect::<Vec<_>>(),
        "cards": cards,
    });
    // Temp + rename: atomic on POSIX, so a reader never sees half an index even when two
    // daemons refresh it in the same instant. Last writer wins, and both are correct because
    // the index is derived from the tree.
    let tmp = repo.join("index.json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&index)?)?;
    std::fs::rename(&tmp, repo.join("index.json"))?;

    // …and the same thing as a document, because a human should be able to read the status of
    // everything without a JSON viewer. This is the file to open first.
    let mut md = format!("# Status · {}\n\n", iso(ts));
    let live: Vec<&serde_json::Value> =
        index["cards"].as_array().unwrap().iter().filter(|c| !c["expired"].as_bool().unwrap_or(false)).collect();
    if live.is_empty() {
        md.push_str("Nothing needs you.\n\n");
    } else {
        md.push_str("## needs you\n\n");
        for c in &live {
            md.push_str(&format!(
                "- `{}` **{}** — {}\n",
                c["session"].as_str().unwrap_or(""),
                c["headline"].as_str().unwrap_or(""),
                c["severity"].as_str().unwrap_or("")
            ));
        }
        md.push('\n');
    }
    md.push_str("## sessions\n\n");
    for (n, _) in briefs {
        md.push_str(&format!("- [{n}](sessions/{n}/mission_briefing.md)\n"));
    }
    md.push_str("\n## memory\n\n- [beliefs](memory/beliefs.md)\n- [projects](memory/projects.md)\n\n");
    md.push_str("_Generated by `mars snapshot`. See [AGENTS.md](AGENTS.md)._\n");
    std::fs::write(repo.join("index.md"), md)?;
    Ok(())
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


/// Write a file only if it is absent. `AGENTS.md`, `docs/**`, `policy.md` and the memory seeds
/// belong to the human and the agent — scaffolded once, then never touched again, so an edit is
/// never silently reverted by the next tick.
fn seed(path: &Path, body: &str) -> Result<()> {
    if path.exists() {
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
    seed(&repo.join("docs/memory.md"), include_str!("manager_docs/memory.md"))?;
    seed(&repo.join("docs/tools.md"), include_str!("manager_docs/tools.md"))?;
    seed(&repo.join("policy.md"), include_str!("manager_docs/policy.md"))?;
    seed(
        &repo.join("memory/beliefs.md"),
        "# Beliefs\n\nWorking memory. Rewritten, not appended. Keep under 200 lines.\n\n\
         Nothing believed yet.\n",
    )?;
    seed(
        &repo.join("memory/projects.md"),
        "# Projects\n\nWhat each project IS — stable context the snapshot cannot know.\n\n\
         _Add a section per project._\n",
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
pub fn emit(repo: &Path, origin: &str, sessions: &[SessionSnap], ts: u64) -> Result<Vec<String>> {
    scaffold_docs(repo)?;
    let mut events: Vec<String> = Vec::new();
    let mut briefs: Vec<(String, String)> = Vec::new();

    for s in sessions {
        let sdir = repo.join("sessions").join(&s.name);
        let cards = sdir.join("cards");
        let snaps = sdir.join("snapshots");
        std::fs::create_dir_all(&cards)?;
        std::fs::create_dir_all(&snaps)?;

        // 1. The stimulus for this session — what an agent reads, or a human with `jq`.
        let stim = serde_json::json!({
            "at": iso(ts), "at_ts": ts, "origin": origin, "session": s.name,
            "health": s.health,
            "workspaces": s.panes.iter().map(|p| serde_json::json!({
                "id": p.id, "paneId": p.pane_id, "name": p.name, "verdict": p.verdict,
                "kind": p.kind, "why": p.why, "ageSecs": p.age_secs,
                "blockedPrompt": p.blocked_prompt,
            })).collect::<Vec<_>>(),
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
        std::fs::write(sdir.join("mission_briefing.md"), &brief)?;
        // …and one document per workspace, so a single workspace can be read (or later enriched
        // by an agent) without touching the session summary.
        let wdir = sdir.join("workspaces");
        std::fs::create_dir_all(&wdir)?;
        for p in &s.panes {
            std::fs::write(
                wdir.join(format!("{}.md", p.pane_id)),
                format!(
                    "# {} · {}\n\n_{}_\n\n{}\n\n- state: {}\n- age: {}\n- kind: {}\n",
                    p.name, s.name, iso(ts), workspace_summary(p),
                    p.verdict, human_age(p.age_secs), p.kind
                ),
            )?;
        }
        append_timeline(&sdir, ts, &events)?;
        briefs.push((s.name.clone(), brief));
    }

    write_index(repo, &briefs, sessions, ts)?;
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



/// Where the manager repo lives.
///
/// `MARS_MANAGER_DIR` overrides everything. Otherwise, and this is the important part: when
/// `MARS_RUNTIME_DIR` is set the repo goes UNDER it, because that variable is how Mars isolates a
/// test run — and a run that isolates its sockets but writes cards into the user's real repo is
/// worse than one that isolates nothing. `--selfcheck` did exactly that and filled a real
/// `~/.mars/manager` with a dozen throwaway sessions.
pub fn repo_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("MARS_MANAGER_DIR") {
        return Some(PathBuf::from(d));
    }
    if let Some(rt) = std::env::var_os(crate::session::RUNTIME_DIR_ENV) {
        return Some(PathBuf::from(rt).join("manager"));
    }
    crate::sys::paths::home_dir().map(|h| h.join(".mars").join("manager"))
}

/// Write ONE session's subtree, from the daemon's own state. Called on a timer by the session
/// daemon, so cards exist before anyone looks — the point of an ambient layer is that nobody has
/// to ask for it.
///
/// The daemon that owns a session is the only process that writes `sessions/<name>/`, so the
/// "one writer per path" rule is enforced by process boundaries rather than by convention.
pub fn tick_session(repo: &Path, origin: &str, board_json: &str, ts: u64, keep: usize) -> Result<()> {
    let Some(snap) = parse_board(board_json) else { return Ok(()) };
    if snap.name.is_empty() {
        return Ok(());
    }
    scaffold_docs(repo)?;
    emit(repo, origin, std::slice::from_ref(&snap), ts)?;
    prune_snapshots(&repo.join("sessions").join(&snap.name).join("snapshots"), keep)?;
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
    let events = emit(&repo, &origin, &sessions, ts)?;
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
