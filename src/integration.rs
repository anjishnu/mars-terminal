//! Which model integration this install uses, chosen once after installing (`mars setup`).
//!
//! Two paths. **Claude Code** runs every model feature through the `claude` CLI on the
//! user's own subscription, and lets the manager write mission briefings. **Groq** calls
//! Groq's API with a key Mars keeps in its own config. A machine that has chosen neither
//! keeps the older behaviour: whatever key is in the environment, paid providers first.
//!
//! The choice lives in `~/.mars/config.json` under `"agent"` and is read when a call is made,
//! not when a process starts. Read at startup, a choice made in `mars setup` never reached a
//! session that was already running, which is how a machine ends up with every summary
//! written by the keyword fallback.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Path {
    ClaudeCode,
    Groq,
}

impl Path {
    pub fn as_str(self) -> &'static str {
        match self {
            Path::ClaudeCode => "claude-code",
            Path::Groq => "groq",
        }
    }

    pub fn parse(s: &str) -> Option<Path> {
        match s.trim().to_ascii_lowercase().as_str() {
            "claude-code" | "claude" | "claude_code" | "claudecode" => Some(Path::ClaudeCode),
            "groq" => Some(Path::Groq),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Path::ClaudeCode => "Claude Code",
            Path::Groq => "Groq",
        }
    }
}

// Reread only when the file changes: the agent asks for its config on paths that run often.
static CACHE: Mutex<Option<(PathBuf, Option<(SystemTime, u64)>, serde_json::Value)>> = Mutex::new(None);

fn agent_section() -> serde_json::Value {
    let Some(path) = crate::config::config_json_path() else { return serde_json::Value::Null };
    // Length too: two saves inside one timestamp tick must not leave the first one cached.
    let mtime = std::fs::metadata(&path).ok().and_then(|m| Some((m.modified().ok()?, m.len())));
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((p, t, v)) = cache.as_ref() {
        if *p == path && *t == mtime {
            return v.clone();
        }
    }
    let v = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .map(|v| v["agent"].clone())
        .unwrap_or(serde_json::Value::Null);
    *cache = Some((path, mtime, v.clone()));
    v
}

/// The path chosen in `mars setup`, if any.
pub fn chosen() -> Option<Path> {
    agent_section()["path"].as_str().and_then(Path::parse)
}

/// The Groq key: the environment first, then the one `mars setup` kept. Kept in the config
/// rather than exported, so the shells in a session's panes never inherit it.
pub fn groq_key() -> Option<String> {
    std::env::var("GROQ_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .or_else(|| agent_section()["groq_api_key"].as_str().map(str::to_string))
        .filter(|k| !k.trim().is_empty())
}

/// Pure: the places a `claude` binary may be, in order: an explicit override, then PATH, then
/// where Claude Code's own installers put it. A daemon started by launchd or systemd often has
/// a PATH without `~/.local/bin`, so PATH alone would find it in a terminal and miss it there.
pub fn claude_candidates(override_bin: Option<&str>, path_var: &str, home: Option<&std::path::Path>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(b) = override_bin.filter(|b| !b.trim().is_empty()) {
        out.push(PathBuf::from(b));
        return out;
    }
    let names: &[&str] = if cfg!(windows) { &["claude.exe", "claude.cmd", "claude"] } else { &["claude"] };
    for dir in std::env::split_paths(path_var) {
        for n in names {
            out.push(dir.join(n));
        }
    }
    if let Some(h) = home {
        for n in names {
            out.push(h.join(".local").join("bin").join(n));
            out.push(h.join(".claude").join("local").join(n));
        }
    }
    out
}

/// The `claude` binary this machine would run, if there is one.
pub fn claude_bin() -> Option<PathBuf> {
    let over = std::env::var("MARS_CLAUDE_BIN").ok();
    let path_var = std::env::var("PATH").unwrap_or_default();
    let home = crate::sys::paths::home_dir();
    claude_candidates(over.as_deref(), &path_var, home.as_deref()).into_iter().find(|p| p.is_file())
}

/// Record the chosen path (and, for Groq, a key typed into `mars setup`). The rest of
/// `config.json` is kept as it was, and the file is made private because it may hold a key.
pub fn save(path: Path, groq_key: Option<&str>) -> anyhow::Result<PathBuf> {
    let file = crate::config::config_json_path().ok_or_else(|| anyhow::anyhow!("no home directory"))?;
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut v: serde_json::Value = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .filter(|v: &serde_json::Value| v.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    if !v["agent"].is_object() {
        v["agent"] = serde_json::json!({});
    }
    v["agent"]["path"] = serde_json::json!(path.as_str());
    if let Some(k) = groq_key.filter(|k| !k.trim().is_empty()) {
        v["agent"]["groq_api_key"] = serde_json::json!(k.trim());
    }
    std::fs::write(&file, serde_json::to_string_pretty(&v)?)?;
    crate::sys::fsperm::restrict_file(&file)?;
    Ok(file)
}

fn manager_switch() -> Option<PathBuf> {
    crate::manager::repo_dir().map(|r| r.join("agent.enabled"))
}

pub fn manager_on() -> bool {
    crate::manager::repo_dir().is_some_and(|r| crate::manager::agent_enabled(&r))
}

/// Turn the manager on or off: the same switch file the phone's menu writes.
pub fn set_manager(on: bool) -> anyhow::Result<()> {
    let p = manager_switch().ok_or_else(|| anyhow::anyhow!("no manager directory"))?;
    if on {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&p, "on\n")?;
    } else if p.exists() {
        std::fs::remove_file(&p)?;
    }
    Ok(())
}
