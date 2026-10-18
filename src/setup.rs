//! `mars setup`: choose the model integration (Claude Code or Groq), test it, save it.
//!
//! The old command printed how to get a key and stopped, so a machine could have Claude Code
//! signed in and still run every summary through the keyword fallback, and nothing said so.
//! This one starts from what the machine already has, proves the choice with one tiny call
//! before saving anything, and reports both model paths in `--check`.

use crate::agent::{self, AgentConfig};
use crate::integration::{self, Path};
use anyhow::{anyhow, Result};
use std::io::Write;

const GROQ_KEYS_URL: &str = "https://console.groq.com/keys";
const CLAUDE_CODE_URL: &str = "https://docs.claude.com/en/docs/claude-code/setup";

/// Read a secret without echoing it. Piped stdin is read as one line, so setup can be scripted
/// (`mars setup --path groq < key-file`) without the secret reaching argv or shell history.
pub fn read_secret(prompt: &str) -> Result<String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    if !crate::sys::tty::is_stdin_tty() {
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        println!();
        return Ok(line.trim().to_string());
    }
    use crossterm::event::{read, Event, KeyCode, KeyEventKind, KeyModifiers};
    crossterm::terminal::enable_raw_mode()?;
    let mut s = String::new();
    let outcome = loop {
        match read() {
            Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Esc => break Err(anyhow!("cancelled — nothing was changed")),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(anyhow!("cancelled — nothing was changed"))
                }
                KeyCode::Backspace => {
                    s.pop();
                }
                KeyCode::Char(c) => s.push(c),
                _ => {}
            },
            Ok(Event::Paste(p)) => s.push_str(&p),
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    println!();
    outcome?;
    let t = s.trim().to_string();
    if !t.is_empty() {
        println!("  received {} characters", t.chars().count());
    }
    Ok(t)
}

fn ask_line(prompt: &str) -> Result<String> {
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(line.trim().to_string())
}

fn tilde(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().into_owned();
    match crate::sys::paths::home_dir().map(|h| h.to_string_lossy().into_owned()) {
        Some(h) if s.starts_with(&h) => format!("~{}", &s[h.len()..]),
        _ => s,
    }
}

/// Pure: the one-line state of the model, for `--check`, the status after setup and the
/// in-session notice. `None` means it works.
pub fn model_problem(cfg: &AgentConfig, chosen: Option<Path>) -> Option<String> {
    if cfg.is_configured() {
        return None;
    }
    Some(match chosen {
        Some(Path::ClaudeCode) => "Claude Code is the chosen path, but there is no `claude` on this machine".into(),
        Some(Path::Groq) => "Groq is the chosen path, but there is no Groq key".into(),
        None => "no model chosen, and no key in Mars's environment".into(),
    })
}

/// What the model status line says when it works.
fn model_ok(cfg: &AgentConfig, chosen: Option<Path>) -> String {
    match (chosen, cfg.provider) {
        (Some(Path::ClaudeCode), _) => format!("Claude Code ({} for summaries, sonnet for Ask) via {}", cfg.model, tilde(std::path::Path::new(&cfg.url))),
        (Some(Path::Groq), _) => "Groq".to_string(),
        (None, p) => format!("{p}, from an environment key (no path chosen — `mars setup` to pick one)"),
    }
}

/// `mars setup --check`: both model paths, one line each. Exit status 1 when the model does not
/// work, so a script (or the installer) can tell. The manager is reported, not required: on the
/// Groq path there is no manager, and on Claude Code leaving it off is a choice.
pub fn check() -> Result<bool> {
    let chosen = integration::chosen();
    let cfg = AgentConfig::from_env();
    let mut fine = true;
    match model_problem(&cfg, chosen) {
        None => println!("model    ✓ {}", model_ok(&cfg, chosen)),
        Some(p) => {
            fine = false;
            println!("model    ✗ {p} — summaries and briefings are keyword guesses     fix: mars setup");
        }
    }
    if integration::manager_on() {
        let note = if integration::claude_bin().is_some() { "" } else { " (but there is no `claude` to run it)" };
        println!("manager  ✓ on{note}");
    } else {
        let fix = if integration::claude_bin().is_some() { "fix: mars setup --manager on" } else { "needs Claude Code" };
        println!("manager  ✗ off — mission briefings are computed only     {fix}");
    }
    Ok(fine)
}

/// One tiny call on exactly this config, with no rotation to anything else: a bad Groq key
/// must fail here rather than quietly succeed on some other key in the environment.
fn test(cfg: &AgentConfig) -> Result<std::time::Duration> {
    let start = std::time::Instant::now();
    agent::probe(cfg)?;
    Ok(start.elapsed())
}

fn claude_config() -> Result<AgentConfig> {
    let cfg = AgentConfig::for_path(Path::ClaudeCode);
    if cfg.url.is_empty() {
        anyhow::bail!("there is no `claude` on this machine. Install Claude Code ({CLAUDE_CODE_URL}), sign in once with `claude`, then run `mars setup` again");
    }
    Ok(cfg)
}

fn groq_config(typed: Option<String>) -> Result<(AgentConfig, Option<String>)> {
    let mut cfg = AgentConfig::for_path(Path::Groq);
    let mut fresh = None;
    if let Some(k) = typed.filter(|k| !k.is_empty()) {
        cfg.key = k.clone();
        fresh = Some(k);
    }
    if cfg.key.is_empty() {
        println!("  Groq's free tier is fast and generous. Create a key at {GROQ_KEYS_URL}");
        let k = read_secret("  Groq key (not shown, Esc cancels): ")?;
        if k.is_empty() {
            anyhow::bail!("no key given — nothing was changed");
        }
        cfg.key = k.clone();
        fresh = Some(k);
    }
    Ok((cfg, fresh))
}

/// Test, then save. Nothing is written unless the call came back.
fn adopt(path: Path, cfg: &AgentConfig, fresh_key: Option<&str>) -> Result<()> {
    print!("  testing {} … ", path.label());
    let _ = std::io::stdout().flush();
    match test(cfg) {
        Ok(took) => println!("✓ answered in {:.1}s", took.as_secs_f64()),
        Err(e) => {
            println!("✗");
            anyhow::bail!("{} did not answer: {e}. Nothing was changed.", path.label());
        }
    }
    let file = integration::save(path, fresh_key)?;
    let stored = if fresh_key.is_some() { "key stored, readable only by you" } else { "no key stored" };
    println!("  saved to {} ({stored})", tilde(&file));
    let live = crate::session::list_sessions()
        .unwrap_or_default()
        .into_iter()
        .filter(|(_, alive, _)| *alive)
        .map(|(n, _, _)| n)
        .collect::<Vec<_>>();
    if !live.is_empty() {
        println!("  running sessions use it from their next call: {}", live.join(", "));
        println!("  (a session started before this version of mars needs one `mars reboot <name>`)");
    }
    Ok(())
}

fn offer_manager() -> Result<()> {
    if integration::manager_on() {
        println!("  manager: already on");
        return Ok(());
    }
    println!();
    println!("  The manager can also write mission briefings for the phone and desk: one Claude Code");
    println!("  (Sonnet) turn when there is new work, at most every 20 minutes, on your subscription.");
    let a = ask_line("  Turn the manager on? [y/N] ")?;
    if matches!(a.to_ascii_lowercase().as_str(), "y" | "yes") {
        integration::set_manager(true)?;
        let at = crate::manager::repo_dir().map(|r| tilde(&r.join("agent.enabled"))).unwrap_or_default();
        println!("  ✓ manager on ({at})");
    } else {
        println!("  manager stays off — turn it on later with: mars setup --manager on");
    }
    Ok(())
}

fn interactive() -> Result<()> {
    let claude = integration::claude_bin();
    let env_groq = std::env::var("GROQ_API_KEY").is_ok_and(|k| !k.trim().is_empty());
    let saved_groq = !env_groq && integration::groq_key().is_some();
    let current = integration::chosen();
    println!();
    println!("  Mars uses a model for summaries, the welcome-back briefing, session missions, Ask,");
    println!("  English→shell and tab names. Choose how it reaches one:");
    println!();
    let c_note = match &claude {
        Some(p) => format!("✓ found ({}) · your own subscription, no key", tilde(p)),
        None => format!("not installed · {CLAUDE_CODE_URL}"),
    };
    let g_note = if env_groq {
        "✓ key in your environment".to_string()
    } else if saved_groq {
        "✓ key saved by an earlier setup".to_string()
    } else {
        format!("free key from {GROQ_KEYS_URL}")
    };
    println!("    1  Claude Code   {c_note}");
    println!("                     every feature, plus mission briefings from the manager");
    println!("    2  Groq          {g_note}");
    println!("                     fast free tier; summaries, briefings, Ask (no manager)");
    println!();
    if let Some(p) = current {
        println!("  Currently: {}", p.label());
    }
    let default = if claude.is_some() { 1 } else { 2 };
    let pick = ask_line(&format!("  Path [1/2, Enter = {default}]: "))?;
    let choice = match pick.as_str() {
        "" => default,
        "1" => 1,
        "2" => 2,
        other => match Path::parse(other) {
            Some(Path::ClaudeCode) => 1,
            Some(Path::Groq) => 2,
            None => anyhow::bail!("'{other}' is not one of the choices — nothing was changed"),
        },
    };
    if choice == 1 {
        let cfg = claude_config()?;
        adopt(Path::ClaudeCode, &cfg, None)?;
        offer_manager()?;
    } else {
        let (cfg, fresh) = groq_config(None)?;
        adopt(Path::Groq, &cfg, fresh.as_deref())?;
    }
    println!();
    println!("  You'll see it in:  mars ls summaries · the welcome-back briefing · Ask (?) in a session");
    println!("  Check anytime:     mars setup --check");
    Ok(())
}

/// `mars setup [--check] [--path claude-code|groq] [--manager on|off]`. A failure is one line
/// that says what to do, never a backtrace.
pub fn setup_main(args: &[String]) -> Result<()> {
    if let Err(e) = run(args) {
        eprintln!("  ✗ {e}");
        std::process::exit(1);
    }
    Ok(())
}

fn run(args: &[String]) -> Result<()> {
    let flag = |name: &str| args.iter().position(|a| a == name).map(|i| args.get(i + 1).cloned());
    if args.iter().any(|a| a == "--check") {
        if !check()? {
            std::process::exit(1);
        }
        return Ok(());
    }
    let path = flag("--path");
    let manager = flag("--manager");
    if path.is_none() && manager.is_none() {
        if crate::sys::tty::is_stdin_tty() {
            interactive()?;
        } else {
            println!("{}", agent::SETUP_HELP);
            println!();
            check()?;
        }
        return Ok(());
    }
    if let Some(p) = path {
        let p = p.ok_or_else(|| anyhow!("usage: mars setup --path claude-code|groq"))?;
        match Path::parse(&p).ok_or_else(|| anyhow!("unknown path '{p}' — use claude-code or groq"))? {
            Path::ClaudeCode => adopt(Path::ClaudeCode, &claude_config()?, None)?,
            Path::Groq => {
                // Piped stdin is the key, so the key never lands in argv or shell history.
                let piped = if crate::sys::tty::is_stdin_tty() { None } else { Some(read_secret("")?) };
                let (cfg, fresh) = groq_config(piped)?;
                adopt(Path::Groq, &cfg, fresh.as_deref())?;
            }
        }
    }
    if let Some(m) = manager {
        match m.as_deref() {
            Some("on") => {
                if integration::claude_bin().is_none() {
                    anyhow::bail!("the manager runs Claude Code, and there is no `claude` on this machine ({CLAUDE_CODE_URL})");
                }
                integration::set_manager(true)?;
                println!("  ✓ manager on — it writes mission briefings when there is new work");
            }
            Some("off") => {
                integration::set_manager(false)?;
                println!("  manager off");
            }
            _ => anyhow::bail!("usage: mars setup --manager on|off"),
        }
    }
    Ok(())
}
