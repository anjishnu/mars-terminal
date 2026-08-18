//! The fleet registry: which hosts you've been on, for `mars ls`.
//!
//! Extracted from `broker.rs` because it is *portable* state (JSON files under
//! `~/.mars`), consumed by `mars ls` on every platform — while the recorder
//! (`mars ssh` / the keyd status push) is the Unix-only ssh capability. A build
//! without the `ssh` feature still lists whatever fleet the file holds.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::worklog::now_secs;

/// One host you've connected to — the home machine's view of the fleet.
/// `session` / `last_status` are refreshed by the broker's status push (every
/// brokered agent call self-reports host + session). `cwd` is **unpopulated**:
/// every call site passes `None`, so nothing records a remote's working
/// directory and `mars ls` shows `—` in its `DIR` column for a host. The field
/// is kept for the day `mars ssh` learns to report one.
#[derive(Serialize, Deserialize, Clone)]
pub struct FleetEntry {
    pub host: String,
    pub cwd: Option<String>,
    pub session: Option<String>,
    pub last_status: Option<String>,
    /// Unix seconds of the last interaction.
    pub as_of: u64,
}

/// `~/.mars/fleet.json`, in the same owner-only dir as the broker socket.
fn fleet_path() -> Result<PathBuf> {
    let home = crate::sys::paths::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    let dir = home.join(".mars");
    std::fs::create_dir_all(&dir)?;
    crate::sys::fsperm::restrict_dir(&dir)?;
    Ok(dir.join("fleet.json"))
}

/// Load the fleet, most-recent first. Empty on any error (the cache is best-effort).
pub fn fleet_load() -> Vec<FleetEntry> {
    let mut v: Vec<FleetEntry> = fleet_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    v.sort_by(|a, b| b.as_of.cmp(&a.as_of));
    v
}

/// Record (upsert) a host interaction. Best-effort; never fails a connection.
pub fn fleet_record(host: &str, cwd: Option<String>) {
    let mut v = fleet_load();
    match v.iter_mut().find(|e| e.host == host) {
        Some(e) => {
            e.as_of = now_secs();
            if cwd.is_some() {
                e.cwd = cwd;
            }
        }
        None => v.push(FleetEntry {
            host: host.to_string(),
            cwd,
            session: None,
            last_status: None,
            as_of: now_secs(),
        }),
    }
    fleet_save(v);
}

/// The status-push half of the fleet cache: every brokered agent call from a
/// remote refreshes the home view of that host, so `mars ls` shows current
/// activity instead of only "when you last ssh'd there".
pub fn fleet_status(host: &str, session: Option<String>, status: &str) {
    let mut v = fleet_load();
    match v.iter_mut().find(|e| e.host == host) {
        Some(e) => {
            e.as_of = now_secs();
            e.last_status = Some(status.to_string());
            if session.is_some() {
                e.session = session;
            }
        }
        None => v.push(FleetEntry {
            host: host.to_string(),
            cwd: None,
            session,
            last_status: Some(status.to_string()),
            as_of: now_secs(),
        }),
    }
    fleet_save(v);
}

fn fleet_save(mut v: Vec<FleetEntry>) {
    v.sort_by(|a, b| b.as_of.cmp(&a.as_of));
    v.truncate(50);
    if let Ok(p) = fleet_path() {
        if let Ok(s) = serde_json::to_string_pretty(&v) {
            let _ = std::fs::write(p, s);
        }
    }
}

/// Resolve a `mars ls` follow-up (an ordinal like "2", or a host name / unique
/// prefix) to a host from the numbered list. `None` = skip / no match.
pub fn resolve_target(hosts: &[String], input: &str) -> Option<String> {
    let t = input.trim();
    if t.is_empty() || t == "q" {
        return None;
    }
    if let Ok(n) = t.parse::<usize>() {
        return hosts.get(n.checked_sub(1)?).cloned();
    }
    if let Some(h) = hosts.iter().find(|h| h.as_str() == t) {
        return Some(h.clone());
    }
    let pre: Vec<&String> = hosts.iter().filter(|h| h.starts_with(t)).collect();
    if pre.len() == 1 {
        Some(pre[0].clone())
    } else {
        None
    }
}
