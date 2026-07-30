//! `mars snapshot` — the deterministic half of the manager.
//!
//! Subscribes to every live session exactly as Rover does, takes one board frame each, and writes
//! four things into a plain directory:
//!
//! * `snapshots/<ts>.json` — the machine-readable stimulus (what an agent would read later)
//! * `feed/card-*.md`      — REFLEX cards: markdown + YAML frontmatter, no model involved
//! * `feed/briefing.md`    — a workspace briefing, regenerated each run, also deterministic
//! * `timeline.md`         — an append-only, date-ordered, human-readable event log
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
fn expire_card(c: &OpenCard) -> Result<()> {
    let text = std::fs::read_to_string(&c.file)?;
    if text.contains("\nexpired: true\n") {
        return Ok(());
    }
    std::fs::write(&c.file, text.replacen("\nexpired: false\n", "\nexpired: true\n", 1))?;
    Ok(())
}

/// The briefing — a deterministic workspace update, rewritten every run.
///
/// Deliberately NOT a model's job. Counting panes by verdict, naming what is blocked and for how
/// long, and saying "nothing needs you" when nothing does are all things arithmetic does better,
/// faster, and without a plan window.
fn write_briefing(feed: &Path, sessions: &[SessionSnap], ts: u64) -> Result<()> {
    let mut needs: Vec<&PaneRow> = Vec::new();
    let mut running = 0usize;
    let mut idle = 0usize;
    for s in sessions {
        for p in &s.panes {
            match p.verdict.as_str() {
                "blocked" | "failed" => needs.push(p),
                "running" => running += 1,
                _ => idle += 1,
            }
        }
    }
    needs.sort_by(|a, b| {
        let rank = |v: &str| if v == "blocked" { 0 } else { 1 };
        rank(&a.verdict).cmp(&rank(&b.verdict)).then(b.age_secs.cmp(&a.age_secs))
    });

    let mut md = String::new();
    md.push_str("---\nid: briefing\nv: 1\nkind: briefing\nseverity: info\n");
    md.push_str(&format!("created: {}\ncreated_ts: {ts}\nsource: reflex\nexpired: false\n---\n", iso(ts)));
    md.push_str(&format!("# Workspaces · {}\n\n", iso(ts)));

    if needs.is_empty() {
        md.push_str("**Nothing needs you.**\n\n");
    } else {
        md.push_str(&format!("**{} need{} you**\n\n", needs.len(), if needs.len() == 1 { "s" } else { "" }));
        for p in &needs {
            let mark = if p.verdict == "blocked" { "⚠" } else { "✗" };
            md.push_str(&format!(
                "- {mark} **{}** — {} · {}\n",
                p.name,
                p.verdict,
                human_age(p.age_secs)
            ));
            if !p.why.trim().is_empty() {
                md.push_str(&format!("  \n  {}\n", p.why.trim()));
            }
        }
        md.push('\n');
    }
    md.push_str(&format!("{running} running · {idle} idle\n\n"));
    for s in sessions {
        md.push_str(&format!("## {}\n\n", s.name));
        if s.panes.is_empty() {
            md.push_str("_no panes_\n\n");
            continue;
        }
        md.push_str("| pane | state | age |\n|---|---|---|\n");
        for p in &s.panes {
            md.push_str(&format!("| {} | {} | {} |\n", p.name, p.verdict, human_age(p.age_secs)));
        }
        md.push('\n');
        if !s.health.trim().is_empty() {
            md.push_str(&format!("_{}_\n\n", s.health.trim()));
        }
    }
    std::fs::write(feed.join("briefing.md"), md)?;
    Ok(())
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

/// Regenerate `feed/index.json` from the directory. The directory is the truth; the index is a
/// cache, so it is rebuilt rather than mutated and can never drift.
fn write_index(feed: &Path, ts: u64) -> Result<()> {
    // Bodies are INLINED. Rover reads files through a single-slot `fs.read`, so N reads for one
    // screen would race each other; one self-contained file is both simpler and correct. The
    // index is a generated cache, so duplicating a few hundred bytes per card costs nothing.
    let mut cards: Vec<serde_json::Value> = read_open_cards(feed)
        .into_iter()
        .map(|c| {
            let body = std::fs::read_to_string(&c.file)
                .ok()
                .and_then(|t| split_front(&t).map(|(_, b)| b.trim().to_string()))
                .unwrap_or_default();
            let actions = std::fs::read_to_string(&c.file)
                .ok()
                .and_then(|t| split_front(&t).map(|(f, _)| parse_actions(f)))
                .unwrap_or_default();
            serde_json::json!({
                "id": c.id, "file": c.file.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
                "severity": c.severity, "headline": c.headline, "session": c.session,
                "pane": c.pane, "kind": c.kind, "created_ts": c.created, "expired": c.expired,
                "body": body, "actions": actions,
            })
        })
        .collect();
    // Most severe first, then newest — the order Rover pins in.
    let rank = |s: &str| match s { "block" => 0, "warn" => 1, "info" => 2, _ => 3 };
    cards.sort_by(|a, b| {
        let (sa, sb) = (a["severity"].as_str().unwrap_or(""), b["severity"].as_str().unwrap_or(""));
        a["expired"].as_bool().unwrap_or(false).cmp(&b["expired"].as_bool().unwrap_or(false))
            .then(rank(sa).cmp(&rank(sb)))
            .then(b["created_ts"].as_u64().unwrap_or(0).cmp(&a["created_ts"].as_u64().unwrap_or(0)))
    });
    let briefing = std::fs::read_to_string(feed.join("briefing.md"))
        .ok()
        .and_then(|t| split_front(&t).map(|(_, b)| b.trim().to_string()));
    let index = serde_json::json!({
        "generated": iso(ts), "generated_ts": ts,
        "briefing": briefing,
        "cards": cards,
    });
    std::fs::write(feed.join("index.json"), serde_json::to_string_pretty(&index)?)?;
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

/// The whole deterministic pass. Pure once `sessions` is in hand, so `--selfcheck` drives it
/// directly with synthetic boards and never needs a live daemon.
pub fn emit(repo: &Path, origin: &str, sessions: &[SessionSnap], ts: u64) -> Result<Vec<String>> {
    let feed = repo.join("feed");
    let snaps = repo.join("snapshots");
    std::fs::create_dir_all(&feed)?;
    std::fs::create_dir_all(&snaps)?;

    // 1. The stimulus, for whatever reads it later (an agent, or a human with `jq`).
    let stim = serde_json::json!({
        "at": iso(ts), "at_ts": ts, "origin": origin,
        "sessions": sessions.iter().map(|s| serde_json::json!({
            "name": s.name, "health": s.health,
            "panes": s.panes.iter().map(|p| serde_json::json!({
                "id": p.id, "paneId": p.pane_id, "name": p.name, "verdict": p.verdict,
                "kind": p.kind, "why": p.why, "ageSecs": p.age_secs,
                "blockedPrompt": p.blocked_prompt,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });
    std::fs::write(
        snaps.join(format!("{}.json", iso(ts).replace(':', "-"))),
        serde_json::to_string_pretty(&stim)?,
    )?;

    // 2. Reflex cards, one open card per (pane, kind) — a pane blocked for six hours is one card.
    let open = read_open_cards(&feed);
    let mut events: Vec<String> = Vec::new();
    let mut live: Vec<(String, String, &'static str)> = Vec::new();

    for s in sessions {
        for p in &s.panes {
            let kind = match p.verdict.as_str() {
                "blocked" => Some(Reflex::Blocked),
                "failed" => Some(Reflex::Failed),
                _ => None,
            };
            let Some(kind) = kind else { continue };
            live.push((s.name.clone(), p.pane_id.clone(), kind.slug()));
            let already = open.iter().any(|c| {
                !c.expired && c.session == s.name && c.pane == p.pane_id && c.kind == kind.slug()
            });
            if already {
                continue;
            }
            let id = format!("card-{}-{}-{}-{ts}", s.name, p.pane_id, kind.slug());
            write_card(&feed, &id, kind, origin, &s.name, p, ts)?;
            events.push(format!(
                "**{}** {} in `{}` — card `{id}`",
                p.name,
                if kind == Reflex::Blocked { "blocked on a prompt" } else { "failed" },
                s.name
            ));
        }
    }

    // 3. Expire cards whose condition no longer holds. Declarative staleness: a card retires
    //    because the world moved, not because anything re-examined it.
    for c in &open {
        if c.expired || c.kind.is_empty() {
            continue;
        }
        let still = live.iter().any(|(sn, pid, k)| *sn == c.session && *pid == c.pane && *k == c.kind);
        if !still {
            expire_card(c)?;
            events.push(format!("resolved — card `{}` ({})", c.id, c.headline));
        }
    }

    // 4. The briefing and the index, both regenerated wholesale.
    write_briefing(&feed, sessions, ts)?;
    write_index(&feed, ts)?;
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

/// CLI: `mars snapshot [--repo DIR]`.
pub fn snapshot_main(repo: Option<String>) -> Result<()> {
    let repo = repo
        .map(PathBuf::from)
        .or_else(|| crate::sys::paths::home_dir().map(|h| h.join("manager")))
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
