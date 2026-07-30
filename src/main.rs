mod agent;
mod app;
mod banner;
mod briefing;
// The ssh/keyd broker is optional as one unit. Without the feature it is replaced
// by an inert stub, so callers never learn the capability is missing.
#[cfg(feature = "ssh")]
mod broker;
#[cfg(feature = "ssh")]
mod ssh;
#[cfg(not(feature = "ssh"))]
#[path = "broker_stub.rs"]
mod broker;
mod buffer;
mod config;
mod fleet;
mod health;
mod layout;
mod llm_log;
mod manager;
mod mode;
mod osc133;
mod palette;
mod pane;
// The deletion-proof seam: without the `memory` feature the whole retrieval
// subsystem is replaced by a neutral stub and the terminal works unchanged.
#[cfg(feature = "memory")]
mod retrieval;
#[cfg(not(feature = "memory"))]
#[path = "retrieval_stub.rs"]
mod retrieval;
mod project;
mod session;
mod sys;
mod tab;
mod terminal;
mod persona;
mod prompts;
mod tiers;
mod themes;
// Syntax highlighting is optional; without the `syntax` feature an inert stub takes
// over (renders plain, never errors) and syntect isn't compiled in.
#[cfg(feature = "syntax")]
mod syntax;
#[cfg(not(feature = "syntax"))]
#[path = "syntax_stub.rs"]
mod syntax;
// The Rover bridge (`mars serve` / `mars qr`) is optional; without `web` it isn't
// compiled at all (no stub needed — its CLI arms print a rebuild hint instead).
#[cfg(feature = "web")]
mod serve;
mod worklog;
mod tuning;
mod ui;

use anyhow::Result;
use app::{App, InputEvent};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, supports_keyboard_enhancement, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{env, io};

const HELP: &str = "\
mars — mission control for your terminal

USAGE
  mars                           new session, opens a terminal (like tmux)
  mars FILE                      new session, editing FILE
  mars -s [FILE]                 quick standalone edit, no daemon (scripts)
  mars <COMMAND> [ARGS]

  By default every `mars` is a session: detachable, survives disconnects,
  auto-named (a number first, then an AI label). See below to manage them.

SESSIONS  (work survives closed windows and disconnects)
  mars new <name> [FILE]         start an explicitly-named session
                                 (aliases: session, --session)
  mars attach [name]             reattach — most recent session if unnamed
                                 (aliases: a, resume, --resume)
  mars ls                        list sessions and their attach state
                                 (aliases: list, --list)
  mars rename <old> <new>        rename a running session
  mars kill <name>               end + delete a session (autosaves first)
  mars killall                   the reset button: end every session (autosaved)
                                 and mars process, stop the Rover bridge, shut down
                                 ssh tunnel state + the key broker, sweep stale
                                 sockets. Memory files are kept; no new session is
                                 started.
                                 (aliases: cleanup, --killall, --cleanup)

  Inside a session:  quitting (C-x C-c) just DETACHES — the session lives on;
                     \"Kill session\" in the menu (or mars kill) ends it for good
  Closing the terminal window just detaches — nothing is lost.
  Reattach greets you with a \"while away\" line if anything happened;
  C-x g opens the full Away Digest (timeline + durations).

AGENT  (BETA — an assistant, not an authority; review what it proposes)
  mars setup                     how to get a free API key and turn on the agent
  mars ask \"<question>\"          one-shot answer from the LLM agent
  Keys (paid-first): ANTHROPIC_API_KEY, OPENAI_API_KEY, GROQ_API_KEY,
                     GEMINI_API_KEY, or MARS_LLM_KEY + MARS_LLM_URL (any
                     OpenAI-compatible endpoint, e.g. local Ollama).
                     MARS_LLM_MODEL overrides the model for any provider.
  Enterprise:        AWS_BEARER_TOKEN_BEDROCK (+ AWS_REGION) for Bedrock;
                     AZURE_OPENAI_API_KEY + AZURE_OPENAI_ENDPOINT
                     (+ MARS_AZURE_DEPLOYMENT) for Azure OpenAI / Foundry.

LLM DEBUG  (calibrate prompts / right-size models per call)
  mars --llm-debug <cmd>         log every LLM call (prompt, model, tokens,
                                 latency) to ~/.mars/logs/ (or MARS_LLM_DEBUG=1;
                                 export it so session daemons inherit it all day)
  mars llm-stats [--raw|--json|--daily|--since 7d]
                                 profile the log: per task×model ranked by token
                                 use; --daily trend, --json scriptable, --since window
  mars theme [list|<name>]       list color themes, or switch (writes config) [beta]
  mars config                    show the global config (~/.mars/config.json)
  mars translate \"<english>\"     headless: English → one shell command (logs it)
  --memory none|history|docs|full  retrieval variant: history = your own commands
                                 for translate; docs = Mars's own docs for ask

REMOTE  (BETA — the agent works on every box; the key never leaves home)
  mars ssh <host> [ssh args]     land in a mars session on the remote (attach
                                 the most recent, else create \"main\") with the
                                 auth socket forwarded — the remote agent asks
                                 home, no key on the box. Detach returns here.
                                 Auto-starts the key broker; plain `ssh` still
                                 gives a bare shell.
                                 Windows-home → Unix-remote is supported; install
                                 mars on the remote first. Windows remotes pending.
  mars keyd                      (optional) start the broker explicitly, in a
                                 shell where your API key is set

INSIDE THE EDITOR
  Ctrl+Space   mission control (search)    !   run a shell command
  ?            ask the agent               C-t space warp (tabs/panes)
  C-x C-f      Navigator (browse & jump)       C-x C-s save   C-g cancel anything

MORE
  mars help                      this text          (aliases: -h, --help)
  mars version                   version            (aliases: -V, --version)
  mars reset                     restore default keybindings + tuning (backs up old)
  mars --selfcheck               run the built-in test suite

  Config: ~/.config/mars/keys.json (bindings), tuning.json (behavior knobs)
  Session logs: ~/.local/state/mars/<name>.log";

/// Apply the global MARS config (`~/.mars/config.json`) if present — like a shell
/// rc. Currently supports an `env` object whose entries are exported into the process
/// environment (so a spawned session daemon inherits them too), but ONLY when the
/// variable isn't already set — the real environment and explicit flags always win.
/// A malformed file warns and is ignored; a bad config must never block startup.
/// Path to the global MARS config file: `~/.mars/config.json`, alongside the rest of
/// MARS's state (worklog, briefings, logs). `None` only when `$HOME` is unset.
fn config_path() -> Option<std::path::PathBuf> {
    crate::sys::paths::home_dir().map(|h| h.join(".mars").join("config.json"))
}

fn apply_config_from(path: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(path) else { return };
    let val: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => {
            eprintln!("mars: ignoring {} — not valid JSON", path.display());
            return;
        }
    };
    if let Some(env) = val.get("env").and_then(|e| e.as_object()) {
        for (k, v) in env {
            if std::env::var_os(k).is_some() {
                continue; // the real environment wins over the file
            }
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Bool(b) => (if *b { "1" } else { "0" }).to_string(),
                serde_json::Value::Number(n) => n.to_string(),
                _ => continue, // ignore arrays/objects/null — env values are scalars
            };
            std::env::set_var(k, s);
        }
    }
}

fn main() -> Result<()> {
    // A previously killed client may have left this TTY in raw mode — repair
    // it before printing anything (and before crossterm snapshots "original").
    session::sanitize_tty();

    // The global MARS config (~/.mars/config.json) can export env overrides — e.g.
    // {"env": {"MARS_LLM_DEBUG": "1"}} to turn on LLM call logging. Applied before
    // anything reads the environment (so the daemon inherits it); the real env wins.
    if let Some(p) = config_path() {
        apply_config_from(&p);
    }

    // `--llm-debug` is a global flag (any position): turn on LLM call logging and
    // strip it out so it isn't mistaken for a filename/unknown command. It sets
    // the same env var MARS_LLM_DEBUG so the session daemon inherits it too.
    let raw_args: Vec<String> = env::args().skip(1).collect();
    if raw_args.iter().any(|a| a == "--llm-debug") {
        std::env::set_var("MARS_LLM_DEBUG", "1");
    }
    // `--memory <mode>` (none|history|docs|full) selects the retrieval variant for
    // this run (sets MARS_MEMORY, read by src/retrieval.rs) — the eval ablation knob.
    if let Some(i) = raw_args.iter().position(|a| a == "--memory") {
        if let Some(mode) = raw_args.get(i + 1) {
            std::env::set_var("MARS_MEMORY", mode);
        }
    }
    let mut args = raw_args
        .into_iter()
        .filter(|a| a != "--llm-debug")
        .scan(false, |skip, a| {
            // drop `--memory` and its value so they aren't parsed as commands/files
            if *skip {
                *skip = false;
                return Some(None);
            }
            if a == "--memory" {
                *skip = true;
                return Some(None);
            }
            Some(Some(a))
        })
        .flatten();
    let first = args.next();

    // Bookend this invocation as a session in the debug log (no-op when logging
    // is off). Held for the whole process; session_end fires on any exit path.
    let _llm_session = llm_log::SessionGuard::start();

    match first.as_deref() {
        Some("help") | Some("--help") | Some("-h") => {
            println!("{HELP}");
            return Ok(());
        }
        Some("version") | Some("--version") | Some("-V") => {
            banner::print_banner();
            println!("\n  mars {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Some("--broker-handoff-version") => {
            #[cfg(feature = "ssh")]
            {
                println!("{}", broker::BROKER_HANDOFF_PROTOCOL);
                return Ok(());
            }
            #[cfg(not(feature = "ssh"))]
            anyhow::bail!("this Mars build has no SSH broker support");
        }
        // Headless self-check (no TTY needed) — render, bar, PTY, and sessions.
        Some("--selfcheck") => return selfcheck(),
        // Deterministic manager pass: stimulus + reflex cards + briefing + timeline. No model.
        Some("snapshot") => {
            let flags: Vec<String> = args.collect();
            let repo = flags.iter().position(|a| a == "--repo").and_then(|i| flags.get(i + 1)).cloned();
            return manager::snapshot_main(repo);
        }
        // LLM observability: profile the debug log to right-size models per call.
        Some("llm-stats") => {
            let flags: Vec<String> = args.collect();
            let has = |f: &str| flags.iter().any(|a| a == f);
            // `--since 7d` / `12h` / `30m` / bare `7` (days) — a trailing window.
            let since = flags.iter().position(|a| a == "--since").and_then(|i| flags.get(i + 1)).and_then(|s| {
                let (num, mult) = match s.chars().last() {
                    Some('d') => (&s[..s.len() - 1], 86400),
                    Some('h') => (&s[..s.len() - 1], 3600),
                    Some('m') => (&s[..s.len() - 1], 60),
                    Some(c) if c.is_ascii_digit() => (s.as_str(), 86400),
                    _ => return None,
                };
                num.parse::<u64>().ok().map(|n| n * mult)
            });
            return llm_log::stats(has("--raw"), has("--json"), has("--daily"), since);
        }
        // Show the global config file (~/.mars/config.json): path + contents.
        Some("config") => {
            let Some(p) = config::config_json_path() else {
                anyhow::bail!("no home directory");
            };
            println!("{}", p.display());
            match std::fs::read_to_string(&p) {
                Ok(s) => print!("\n{s}"),
                Err(_) => println!("\n(no config file yet — create it to set env overrides or a theme)\nexample: {{ \"theme\": \"eclipse\", \"env\": {{ \"MARS_LLM_DEBUG\": \"1\" }} }}"),
            }
            return Ok(());
        }
        // Setup instructions: how to get and configure a free LLM API key.
        Some("setup") | Some("keys") => {
            println!("{}", agent::SETUP_HELP);
            let cfg = agent::AgentConfig::from_env();
            println!();
            if cfg.is_configured() {
                println!("Status: ✓ a key is configured (provider: {}).", cfg.provider);
            } else {
                println!("Status: ✗ no key detected yet — agent features are off until you add one.");
            }
            return Ok(());
        }
        // Color themes: list, or switch (writes ~/.mars/config.json "theme").
        Some("theme") => {
            let active = config::selected_theme().unwrap_or_else(|| "mission-control".into());
            return match args.next().as_deref() {
                None | Some("list") => {
                    println!("Active theme: {active}");
                    if let Some(p) = config::config_json_path() {
                        println!("  ({})\n", p.display());
                    }
                    println!("Available:");
                    for t in themes::list() {
                        let mark = if t.name == active { " [active]" } else { "" };
                        let dot = if t.dark { "●" } else { "○" };
                        let who = if t.user { " (user)" } else { "" };
                        println!("  {:<16} {dot} {}{who}{mark}", t.name, t.about);
                    }
                    Ok(())
                }
                Some(name) => {
                    if !themes::exists(name) {
                        let names: Vec<String> = themes::list().into_iter().map(|t| t.name).collect();
                        anyhow::bail!("unknown theme '{name}'. Available: {}", names.join(", "));
                    }
                    config::set_theme(name)?;
                    println!("Theme set to '{name}'. New sessions use it; restart a running session to repaint.");
                    Ok(())
                }
            };
        }
        // Headless ask — verify the agent provider end-to-end from the shell.
        Some("ask") | Some("--ask") => {
            let question: String = args.collect::<Vec<_>>().join(" ");
            return ask_cli(question);
        }
        // Headless shell-translation primitive — the eval harness drives this in
        // batch (with --llm-debug --memory <mode>) to measure Axis A.
        Some("translate") => {
            let request: String = args.collect::<Vec<_>>().join(" ");
            return translate_cli(request);
        }
        // Session daemon (internal — spawned by `new`).
        Some("--server") => {
            let name = args.next().ok_or_else(|| anyhow::anyhow!("--server needs a name"))?;
            // stderr is the session log file (set up by `mars new`).
            eprintln!("[mars] session '{name}' starting (pid {})", std::process::id());
            let result = session::server_main(&name, args.next());
            match &result {
                Ok(_) => eprintln!("[mars] session '{name}' ended cleanly"),
                Err(e) => eprintln!("[mars] session '{name}' died: {e}"),
            }
            return result;
        }
        // Create-or-attach a named session.
        Some("new") | Some("session") | Some("--session") => {
            let name = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("usage: mars new <name> [file]"))?;
            return session::session_main(&name, args.next());
        }
        // Reattach: named, or the most recently active session.
        Some("attach") | Some("a") | Some("resume") | Some("--resume") => {
            return session::resume_main(args.next());
        }
        Some("ls") | Some("list") | Some("--list") => {
            let interactive = !args.any(|a| a == "--no-prompt");
            return session::list_main(interactive);
        }
        // The key-never-leaves-home broker: run once on your machine.
        Some("keyd") => return broker::keyd_main(),
        // SSH to a host with the auth socket forwarded — the agent works there
        // with no key on the box.
        Some("ssh") => {
            let host = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("usage: mars ssh <host> [ssh args…]   (needs: mars keyd)"))?;
            return broker::ssh_main(host, args.collect());
        }
        Some("kill") | Some("--kill") => {
            let name = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("usage: mars kill <name>   (see: mars ls)"))?;
            return session::kill_main(&name);
        }
        // The Rover bridge — a phone/web client over WebSocket. `qr` prints a pairing
        // QR; `serve` runs the WS ⇄ session-socket pump. Built only with `--features web`.
        #[cfg(feature = "web")]
        Some("serve") => {
            // `mars serve [session]` runs the bridge (or reprints the QR if one's already up);
            // `mars serve --reset [session]` rotates the pairing QR and disconnects phones.
            let rest: Vec<String> = args.collect();
            let reset = rest.iter().any(|a| a == "--reset" || a == "reset");
            let session = rest.into_iter().find(|a| !a.starts_with('-') && a != "reset");
            return if reset { serve::reset_main(session) } else { serve::serve_main(session) };
        }
        #[cfg(feature = "web")]
        Some("qr") => return serve::qr_main(args.next()),
        #[cfg(not(feature = "web"))]
        Some("serve") | Some("qr") => {
            eprintln!(
                "this build has no Rover bridge; rebuild with:  cargo build --features web"
            );
            std::process::exit(2);
        }
        // The reset button: end everything, sweep stale sockets, start nothing. `cleanup` is
        // the same sweep under a name that reads as maintenance rather than violence.
        Some(verb @ ("killall" | "--killall" | "cleanup" | "--cleanup")) => {
            if let Ok(session) = std::env::var("MARS_SESSION") {
                let verb = verb.trim_start_matches('-');
                anyhow::bail!(
                    "you're inside Mars session '{session}' — {verb} would cut its own branch.\n  \
                     Detach first (C-x C-c), then run: mars {verb}"
                );
            }
            return session::killall_main(true);
        }
        Some("rename") | Some("--rename") => {
            let (old, new) = (args.next(), args.next());
            let (Some(old), Some(new)) = (old, new) else {
                anyhow::bail!("usage: mars rename <old> <new>   (see: mars ls)");
            };
            return session::rename_main(&old, &new);
        }
        // Factory reset: restore default keybindings + tuning (backs up the old files).
        Some("reset") | Some("--reset") => {
            let path = config::reset_keys()?;
            tuning::reset();
            println!(
                "Restored default keybindings and tuning.\n  {}\n  (your previous files were \
                 backed up alongside as *.bak)",
                path.display()
            );
            return Ok(());
        }
        // Quick standalone edit — no daemon (scripts, throwaway).
        Some("-s") | Some("--standalone") => {} // handled below
        // Unknown flags are errors, not filenames.
        Some(s) if s.starts_with('-') => {
            eprintln!("unknown option: {s}\n");
            eprintln!("{HELP}");
            std::process::exit(2);
        }
        _ => {}
    }

    // Default: sessions-by-default (like tmux). `mars [file]` spins up an
    // auto-numbered session (terminal if no file, editor if a file); the AI
    // renames it later. `-s`/`--standalone` opts out for a quick no-daemon edit.
    let standalone = matches!(first.as_deref(), Some("-s") | Some("--standalone"));
    let file = if standalone { args.next() } else { first };
    if !standalone {
        // Already inside a Mars session's terminal? Route the open to the running
        // daemon (as a new tab) instead of nesting a whole second Mars.
        if let Ok(session) = std::env::var("MARS_SESSION") {
            match &file {
                Some(f) => match session::open_in_session(&session, f) {
                    Ok(()) => {
                        println!("opened '{f}' in Mars session '{session}' (new tab)");
                        return Ok(());
                    }
                    // Session gone/stale (e.g. renamed) → fall through, start fresh.
                    Err(_) => {}
                },
                None => {
                    eprintln!(
                        "You're already inside Mars session '{session}'.\n  \
                         mars <file>       open a file here (new tab)\n  \
                         mars new <name>   start a separate session"
                    );
                    return Ok(());
                }
            }
        }
        let name = session::next_auto_name()?;
        return session::session_main(&name, file);
    }

    // ── Standalone mode (no daemon; `mars -s [file]`) ────────────────────────
    session::install_panic_restore();
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;

    // Kitty keyboard protocol where supported (kitty/WezTerm/Ghostty/iTerm2 3.5+):
    // unlocks chords legacy encoding can't express (C-{, C-}, C--, C-|).
    let enhanced = supports_keyboard_enhancement().unwrap_or(false);
    if enhanced {
        execute!(
            stdout,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;

    // TTY-reader thread → the source-agnostic input channel.
    let (tx, rx) = std::sync::mpsc::channel::<InputEvent>();
    std::thread::spawn(move || loop {
        match crossterm::event::read() {
            Ok(Event::Key(k)) => { if tx.send(InputEvent::Key(k)).is_err() { break; } }
            Ok(Event::Mouse(m)) => { if tx.send(InputEvent::Mouse(m)).is_err() { break; } }
            Ok(Event::Paste(s)) => { if tx.send(InputEvent::Paste(s)).is_err() { break; } }
            Ok(_) => {} // Resize handled by ratatui autoresize on the real TTY
            Err(_) => break,
        }
    });

    let had_file = file.is_some();
    let mut app = App::new(file)?;
    if !had_file {
        app.open_terminal(); // no file → open a shell, not a scratch buffer
    }
    // First-run nudge: unconfigured → a single dismissible notice pointing at setup.
    // The editor/multiplexer work regardless; this never blocks.
    if !agent::AgentConfig::from_env().is_configured() {
        app.notice_no_key();
    }
    let result = app.run(&mut terminal, &rx);

    disable_raw_mode()?;
    if enhanced {
        let _ = execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags);
    }
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;

    result
}

/// Headless one-shot question through the real agent path (provider detection,
/// registry context, RUN: parsing) — the live verification `--selfcheck` can't do.
/// Headless shell-translation: `mars translate "<nl>"` → prints ONE command and
/// logs the call (with the active memory variant). Used by the Python eval harness.
fn translate_cli(request: String) -> Result<()> {
    if request.trim().is_empty() {
        anyhow::bail!("usage: mars translate \"<english request>\"  [--memory none|history|docs|full]");
    }
    let cfg = agent::AgentConfig::from_env();
    if !cfg.is_configured() {
        eprintln!("{}\n", agent::SETUP_HELP);
        anyhow::bail!("no API key configured");
    }
    let (command, _call_id) = agent::translate_once(&cfg, &request, "")?;
    println!("{command}");
    Ok(())
}

fn ask_cli(question: String) -> Result<()> {
    if question.trim().is_empty() {
        anyhow::bail!("usage: mars --ask \"<question>\"");
    }
    let cfg = agent::AgentConfig::from_env();
    if !cfg.is_configured() {
        eprintln!("{}\n", agent::SETUP_HELP);
        anyhow::bail!("no API key configured");
    }
    println!("provider: {}   model: {}", cfg.provider, cfg.model);
    let (tx, rx) = std::sync::mpsc::channel();
    agent::ask(
        cfg,
        question,
        palette::registry_context(),
        String::new(), // no live screen in headless mode
        Vec::new(),
        tx,
    );
    loop {
        match rx.recv_timeout(std::time::Duration::from_secs(60))? {
            agent::AgentEvent::NameFailed { .. } => {}
            agent::AgentEvent::Answer { text, directive } => {
                println!("{}", text);
                match directive {
                    Some(agent::AgentDirective::Run(name)) => println!("[would run: {name}]"),
                    Some(agent::AgentDirective::Type(cmd)) => {
                        println!("[would type into terminal: {cmd}]")
                    }
                    Some(agent::AgentDirective::Open(loc)) => println!("[would open: {loc}]"),
                    Some(agent::AgentDirective::Need(_)) => {}
                    None => {}
                }
                return Ok(());
            }
            // Streaming progress; headless output stays the final text only,
            // so scripts and the eval harness see an unchanged format.
            agent::AgentEvent::AnswerStart | agent::AgentEvent::AnswerDelta { .. } => continue,
            agent::AgentEvent::AutoName { .. }
            | agent::AgentEvent::SessionName { .. }
            | agent::AgentEvent::WatchSummary { .. }
            | agent::AgentEvent::SurfaceSummary { .. }
            | agent::AgentEvent::Mission { .. }
            | agent::AgentEvent::BgDone
            | agent::AgentEvent::ShiftDelta { .. }
            | agent::AgentEvent::ShiftDone
            | agent::AgentEvent::Goals { .. }
            | agent::AgentEvent::RoverSummary { .. }
            | agent::AgentEvent::RoverBrief { .. }
            | agent::AgentEvent::ShellTranslation { .. } => return Ok(()),
            agent::AgentEvent::Error(e) => anyhow::bail!("agent error: {}", e),
        }
    }
}

/// Headless verification of the core paths, runnable without a real terminal.
fn selfcheck() -> Result<()> {
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use ratatui::backend::TestBackend;


    // Hermetic: an inherited agent key would flip no-key code paths (e.g. the
    // shell composer translates instead of running). Clear them so the suite is
    // deterministic regardless of the caller's environment.
    for key in [
        "GEMINI_API_KEY", "GOOGLE_API_KEY", "GROQ_API_KEY",
        "ANTHROPIC_API_KEY", "OPENAI_API_KEY",
        "MARS_LLM_KEY", "MARS_LLM_URL", "ARES_LLM_KEY", "ARES_LLM_URL",
        "MARS_AUTH_SOCK", "MARS_LLM_DEBUG",
        // Developing Mars from inside a Mars pane is the normal workflow, and the
        // session env it exports is not inert: MARS_OPEN_TERMINAL makes a daemon
        // open a terminal pane at startup (moving the layout the daemon block
        // drives), and MARS_SESSION/_ID send detect_broker_sock() to the PARENT
        // session for a route instead of this process's own.
        "MARS_SESSION", "MARS_SESSION_ID", "MARS_OPEN_TERMINAL",
    ] {
        std::env::remove_var(key);
    }
    // Hermetic, part 2: keyless watch fires now produce deterministic verdicts
    // that land in the work journal — point the WHOLE suite at a scratch file so
    // no block can pollute the user's real ~/.mars/worklog.jsonl. Blocks that
    // need their own seeded journal set/reset MARS_WORKLOG back to this default.
    let worklog_default =
        std::env::temp_dir().join(format!("mars-selfcheck-worklog-{}", std::process::id()));
    let _ = std::fs::remove_file(&worklog_default);
    std::env::set_var("MARS_WORKLOG", &worklog_default);

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn kc(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }
    fn typ(app: &mut App, s: &str) -> Result<()> {
        for c in s.chars() { app.handle_key(k(KeyCode::Char(c)))?; }
        Ok(())
    }
    fn screen_text(term: &Terminal<TestBackend>) -> String {
        term.backend().buffer().content().iter().map(|c| c.symbol()).collect()
    }
    // The PTY probes drive whatever real shell `sys::shell` picked; scripted
    // commands must speak its dialect (PowerShell on Windows, POSIX elsewhere).
    fn shell_is_powershell() -> bool {
        let s = crate::sys::shell::default_shell().to_lowercase();
        s.contains("pwsh") || s.contains("powershell")
    }
    // Poll a condition instead of napping a fixed interval — a cold shell
    // (PowerShell especially) can take seconds to prompt; the deadline only
    // bounds the failure case.
    fn wait_until(mut cond: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !cond() {
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        true
    }

    // Never touch the user's real clipboard from tests (also makes the
    // C-c → C-v round-trip deterministic via the kill-ring fallback).
    std::env::set_var("MARS_NO_SYSTEM_CLIPBOARD", "1");
    // Isolate config: fresh defaults in a temp dir, immune to the user's real
    // remaps/tuning — and this exercises the default-file writers. Keep the dir
    // name SHORT: the session runtime (with its Unix socket, ~104-char SUN_LEN
    // limit on macOS) nests under it, so a long prefix overflows the socket path
    // (invisible on Windows, which uses loopback TCP instead of a Unix socket).
    let cfg_dir = std::env::temp_dir().join(format!("msc{}", std::process::id()));
    std::fs::create_dir_all(&cfg_dir)?;
    std::env::set_var("XDG_CONFIG_HOME", &cfg_dir);
    std::env::set_var(session::RUNTIME_DIR_ENV, cfg_dir.join("runtime"));
    // Isolate agent auth the same way. A test suite must not read the developer's real keys:
    // the broker's socket sits at a FIXED path no isolation covers (with `mars keyd` running —
    // which a session now auto-starts — it won over every provider), and provider keys arrive
    // from the ambient environment. Both made the suite's behaviour depend on the machine:
    // every test that runs a command through the `!` bar took the translate branch instead.
    // Tests that want an agent set their own key after this point.
    std::env::set_var("MARS_NO_BROKER", "1");
    for v in ["MARS_LLM_KEY", "MARS_LLM_URL", "MARS_LLM_MODEL",
              "ARES_LLM_KEY", "ARES_LLM_URL", "ARES_LLM_MODEL", "MARS_AUTH_SOCK",
              "GROQ_API_KEY", "ANTHROPIC_API_KEY", "OPENAI_API_KEY", "GEMINI_API_KEY",
              "GOOGLE_API_KEY", "AWS_BEARER_TOKEN_BEDROCK", "AZURE_OPENAI_API_KEY",
              "AZURE_OPENAI_ENDPOINT", "MARS_BEDROCK_REGION", "AWS_REGION",
              "AWS_DEFAULT_REGION", "MARS_AZURE_DEPLOYMENT", "MARS_AZURE_API_VERSION"] {
        std::env::remove_var(v);
    }

    let mut app = App::new(None)?;
    let mut term = Terminal::new(TestBackend::new(120, 40))?;

    // 1. Renders, starts non-modal (EDIT), and shows the MARS splash.
    term.draw(|f| ui::render(f, &mut app))?;
    let t1 = screen_text(&term);
    assert!(t1.contains("EDIT"), "status bar missing EDIT mode");
    assert!(t1.contains("scratch"), "scratch buffer not shown");
    assert!(t1.contains("control for your terminal"), "MARS splash missing on first render");
    println!("[selfcheck] render + splash ............ PASS");

    // 2. Typing dismisses the splash and inserts text (no command side-effects).
    typ(&mut app, "hello world")?;
    term.draw(|f| ui::render(f, &mut app))?;
    let t2 = screen_text(&term);
    assert!(t2.contains("hello world"), "typing did not insert");
    assert!(!t2.contains("control for your terminal"), "splash did not dismiss on keypress");
    assert!(app.mode == mode::Mode::Edit, "typing changed mode");
    println!("[selfcheck] non-modal insert ........... PASS");

    let mut event_app = App::new(None)?;
    event_app.apply_input(InputEvent::Key(KeyEvent::new_with_kind(
        KeyCode::Char('a'),
        KeyModifiers::NONE,
        KeyEventKind::Press,
    )))?;
    event_app.apply_input(InputEvent::Key(KeyEvent::new_with_kind(
        KeyCode::Char('a'),
        KeyModifiers::NONE,
        KeyEventKind::Release,
    )))?;
    event_app.apply_input(InputEvent::Key(KeyEvent::new_with_kind(
        KeyCode::Char('b'),
        KeyModifiers::NONE,
        KeyEventKind::Repeat,
    )))?;
    assert_eq!(
        event_app.focused_buf().rope.to_string(),
        "ab",
        "key release duplicated input or key repeat was dropped"
    );
    println!("[selfcheck] key press/release kinds .... PASS");

    // 2b. Idle-render gating (the SSH no-op-flush fix): after a draw, an idle
    //     tick must NOT request a redraw; a background agent event and an active
    //     spinner MUST. Zero idle flushes = a quiet link.
    {
        let mut app = App::new(None)?;
        app.needs_redraw = false; // pretend we just drew
        app.tick();
        assert!(!app.needs_redraw, "idle tick asked for a redraw (would flush over SSH every tick)");
        app.agent_tx.send(agent::AgentEvent::BgDone)?; // a background event landed
        app.tick();
        assert!(app.needs_redraw, "an agent event did not request a redraw");
        app.needs_redraw = false;
        app.agent_pending = true; // spinner animates → redraw each tick
        app.tick();
        assert!(app.needs_redraw, "the thinking spinner did not request a redraw");
    }
    println!("[selfcheck] idle render gating ........ PASS");

    // 3. Kill-ring round-trip: C-k kills the line, C-y yanks it back.
    app.handle_key(k(KeyCode::Enter))?;
    typ(&mut app, "KILLME")?;
    app.handle_key(kc(KeyCode::Char('a')))?; // C-a: line start
    app.handle_key(kc(KeyCode::Char('k')))?; // C-k: kill line
    assert_eq!(app.kill_ring.last().map(String::as_str), Some("KILLME"), "kill-ring empty");
    term.draw(|f| ui::render(f, &mut app))?;
    assert!(!screen_text(&term).contains("KILLME"), "C-k did not remove text");
    app.handle_key(kc(KeyCode::Char('y')))?; // C-y: yank
    term.draw(|f| ui::render(f, &mut app))?;
    assert!(screen_text(&term).contains("KILLME"), "C-y did not restore text");
    println!("[selfcheck] kill/yank (C-k/C-y) ........ PASS");

    // 4. Chord normalization: ALT|SHIFT+'<' (what terminals send for M-<)
    //    must equal the parsed "M-<" binding; C-_ must undo (C-/ alias).
    let raw = KeyEvent::new(KeyCode::Char('<'), KeyModifiers::ALT | KeyModifiers::SHIFT);
    assert_eq!(
        config::chord_of(&raw),
        config::parse_key("M-<").unwrap(),
        "chord_of failed to normalize ALT|SHIFT+'<'"
    );
    typ(&mut app, "abc")?;
    app.handle_key(kc(KeyCode::Char('_')))?; // C-_ → Undo
    term.draw(|f| ui::render(f, &mut app))?;
    assert!(!screen_text(&term).contains("abc"), "C-_ did not undo");
    println!("[selfcheck] chord normalize + C-_ ...... PASS");

    // 5. M-< (as the real ALT|SHIFT event) goes to top; Shift+Right selects;
    //    typing REPLACES the selection (Mac contract).
    app.handle_key(raw)?; // M-< → GoTop
    assert_eq!((app.focused_pane().cursor_row, app.focused_pane().cursor_col), (0, 0));
    for _ in 0..5 {
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT))?;
    }
    typ(&mut app, "X")?;
    term.draw(|f| ui::render(f, &mut app))?;
    let t5 = screen_text(&term);
    assert!(t5.contains("X world"), "typing did not replace the selection");
    assert!(!t5.contains("hello world"), "selection text survived replacement");
    println!("[selfcheck] select + replace-on-type ... PASS");

    // 6. Live isearch: typing jumps to the match, C-g restores the origin.
    let origin = (app.focused_pane().cursor_row, app.focused_pane().cursor_col);
    app.handle_key(kc(KeyCode::Char('s')))?; // C-s → isearch
    assert!(app.mode == mode::Mode::Prompt, "C-s did not open isearch");
    typ(&mut app, "world")?;
    assert_eq!(
        (app.focused_pane().cursor_row, app.focused_pane().cursor_col),
        (0, 2),
        "isearch did not jump to the live match"
    );
    assert!(!app.search_hl.is_empty(), "isearch matches not highlighted");
    app.handle_key(kc(KeyCode::Char('g')))?; // C-g → cancel, restore origin
    assert_eq!(
        (app.focused_pane().cursor_row, app.focused_pane().cursor_col),
        origin,
        "C-g did not restore the origin"
    );
    assert!(app.search_hl.is_empty(), "highlights not cleared after cancel");
    println!("[selfcheck] live isearch (C-s/C-g) ..... PASS");

    // 6a. Search-as-teleport: match counter, Tab-label jump, land-on-any-key.
    {
        let mut app = App::new(None)?;
        typ(&mut app, "aa bb aa cc aa")?; // three "aa" matches at cols 0, 6, 12
        app.handle_key(kc(KeyCode::Char('a')))?; // C-a → line start
        app.handle_key(kc(KeyCode::Char('s')))?; // C-s → isearch
        typ(&mut app, "aa")?;
        assert_eq!(app.isearch_status(), Some((1, 3)), "counter should read 1/3 at first match");
        // Tab labels the matches; the 2nd label ('s') jumps to the 2nd match (col 6).
        app.handle_key(k(KeyCode::Tab))?;
        assert!(app.search_pick && app.search_labels.len() == 3, "Tab did not label matches");
        assert_eq!(app.search_labels[1].2, 's', "second label should be 's'");
        app.handle_key(k(KeyCode::Char('s')))?; // pick label 's' → 2nd match
        assert_eq!(app.focused_pane().cursor_col, 6, "label jump did not land on the 2nd match");
        assert!(matches!(app.mode, mode::Mode::Edit), "label pick did not accept the search");

        // Land-on-any-key: type target, then a motion key commits + applies.
        app.handle_key(kc(KeyCode::Char('s')))?; // isearch again
        typ(&mut app, "cc")?; // jumps to "cc" (col 9)
        assert_eq!(app.focused_pane().cursor_col, 9, "isearch did not land on cc");
        app.handle_key(kc(KeyCode::Char('a')))?; // C-a while searching → commit + line-start
        assert!(matches!(app.mode, mode::Mode::Edit), "land-on-key did not exit search");
        assert_eq!(app.focused_pane().cursor_col, 0, "C-a did not apply after committing search");
    }
    println!("[selfcheck] search-as-teleport ......... PASS");

    // 6c. Selection-aware refactor: code-block extraction + reversible apply.
    {
        assert_eq!(
            app::extract_code_block("here you go:\n```rust\nfn a() {}\n```\ndone"),
            Some("fn a() {}".to_string()),
            "code block not extracted"
        );
        assert_eq!(app::extract_code_block("no fences here"), None);

        let mut app = App::new(None)?;
        typ(&mut app, "old_code")?;
        app.handle_key(kc(KeyCode::Char('a')))?; // C-a → line start
        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::SHIFT))?; // select the line
        assert!(app.selection_range().is_some(), "selection not set");
        // Simulate an accepted refactor and apply it as one reversible edit.
        app.refactor_target = app.selection_range();
        app.refactor_replacement = Some("new_code".to_string());
        app.apply_refactor();
        term.draw(|f| ui::render(f, &mut app))?;
        let t = screen_text(&term);
        assert!(t.contains("new_code") && !t.contains("old_code"), "refactor not applied");
        app.handle_key(kc(KeyCode::Char('_')))?; // C-_ → Undo (one chunk)
        term.draw(|f| ui::render(f, &mut app))?;
        assert!(screen_text(&term).contains("old_code"), "one undo did not revert the refactor");
    }
    println!("[selfcheck] selection refactor (undo) .. PASS");

    // 6b. Fast motion: ⌘-token stops (code-aware), matching-bracket, symbol jump.
    {
        let mut app = App::new(None)?;
        typ(&mut app, "foo.bar(baz)")?;
        app.handle_key(kc(KeyCode::Char('a')))?; // C-a → line start (col 0)
        let col = |a: &App| a.focused_pane().cursor_col;
        app.move_token_forward(); assert_eq!(col(&app), 3, "token→ should stop at '.'");
        app.move_token_forward(); assert_eq!(col(&app), 4, "token→ should stop at 'bar'");
        app.move_token_forward(); assert_eq!(col(&app), 7, "token→ should stop at '('");
        app.move_token_backward(); assert_eq!(col(&app), 4, "token← should return to 'bar'");
        app.move_token_forward(); // back onto '(' at col 7
        app.match_bracket(); assert_eq!(col(&app), 11, "match_bracket → ')'");
        app.match_bracket(); assert_eq!(col(&app), 7, "match_bracket → '('");

        let mut app = App::new(None)?;
        typ(&mut app, "fn one() {")?; app.handle_key(k(KeyCode::Enter))?;
        typ(&mut app, "    body")?;   app.handle_key(k(KeyCode::Enter))?;
        typ(&mut app, "fn two() {")?; // cursor on row 2
        app.jump_symbol(false); assert_eq!(app.focused_pane().cursor_row, 0, "symbol← to fn one");
        app.jump_symbol(true);  assert_eq!(app.focused_pane().cursor_row, 2, "symbol→ to fn two");
    }
    println!("[selfcheck] token/bracket/symbol jumps . PASS");

    // 6d. Undo MVP: a run of typed chars is ONE undo step (typing was previously
    //     invisible to undo); a motion breaks the run into separate steps.
    {
        let mut app = App::new(None)?;
        typ(&mut app, "hello")?;
        term.draw(|f| ui::render(f, &mut app))?;
        assert!(screen_text(&term).contains("hello"), "typing did not appear");
        app.handle_key(kc(KeyCode::Char('_')))?; // C-_ → Undo (whole run)
        term.draw(|f| ui::render(f, &mut app))?;
        assert!(!screen_text(&term).contains("hello"), "undo did not reverse a typed run");

        let mut app = App::new(None)?;
        typ(&mut app, "AAA")?;
        app.handle_key(kc(KeyCode::Char('a')))?; // C-a (line start) breaks the run
        typ(&mut app, "BBB")?; // inserts before AAA → "BBBAAA", a second run
        app.handle_key(kc(KeyCode::Char('_')))?; // undo removes only the BBB run
        term.draw(|f| ui::render(f, &mut app))?;
        let t = screen_text(&term);
        assert!(t.contains("AAA") && !t.contains("BBB"), "undo did not stop at the run boundary");
    }
    println!("[selfcheck] undo (coalesced runs) ..... PASS");

    // 6d2. Undo time-travel mode: enter, ← steps back, Home undoes all, End
    //      redoes all, Esc exits.
    {
        let mut app = App::new(None)?;
        typ(&mut app, "one")?;
        app.handle_key(kc(KeyCode::Char('a')))?; // C-a breaks the run
        typ(&mut app, "two")?; // two undo steps
        let text = |a: &App| -> String {
            match a.focused_pane().content {
                pane::PaneContent::Editor(id) => a.buffers[&id].rope.to_string(),
                _ => String::new(),
            }
        };
        assert!(text(&app).contains("one"), "setup: text not present before undo");
        app.run_action(palette::Action::UndoMode);
        assert!(matches!(app.mode, mode::Mode::Undo), "UndoMode did not enter undo mode");
        app.handle_key(k(KeyCode::Home))?; // undo everything
        assert!(text(&app).trim().is_empty(), "Home did not undo to the start: {:?}", text(&app));
        app.handle_key(k(KeyCode::End))?; // redo everything
        assert!(text(&app).contains("one"), "End did not redo forward");
        app.handle_key(k(KeyCode::Esc))?;
        assert!(matches!(app.mode, mode::Mode::Edit), "Esc did not exit undo mode");
    }
    println!("[selfcheck] undo time-travel mode ..... PASS");

    // 6e. Horizontal motion wraps across lines: → at line end → next line start;
    //     ← at line start → previous line end.
    {
        let mut app = App::new(None)?;
        typ(&mut app, "ab")?;
        app.handle_key(k(KeyCode::Enter))?;
        typ(&mut app, "cd")?; // buffer "ab\ncd"
        app.handle_key(k(KeyCode::Up))?;   // row 0
        app.handle_key(k(KeyCode::End))?;  // (0, 2) end of "ab"
        let pos = |a: &App| (a.focused_pane().cursor_row, a.focused_pane().cursor_col);
        assert_eq!(pos(&app), (0, 2), "End did not reach line end");
        app.handle_key(k(KeyCode::Right))?;
        assert_eq!(pos(&app), (1, 0), "→ at line end did not wrap to the next line");
        app.handle_key(k(KeyCode::Left))?;
        assert_eq!(pos(&app), (0, 2), "← at line start did not wrap to the previous line");
    }
    println!("[selfcheck] cross-line arrow wrap ..... PASS");

    // 6f. Auto-indent: Enter carries the previous line's leading whitespace.
    {
        let mut app = App::new(None)?;
        typ(&mut app, "    x")?; // 4-space indent
        app.handle_key(k(KeyCode::Enter))?;
        assert_eq!(app.focused_pane().cursor_col, 4, "Enter did not auto-indent to match");
    }
    println!("[selfcheck] auto-indent on newline .... PASS");

    let text_of = |a: &App| -> String {
        match a.focused_pane().content {
            pane::PaneContent::Editor(id) => a.buffers[&id].rope.to_string(),
            _ => String::new(),
        }
    };

    // 6g. Tab indents / Shift-Tab dedents the selected lines (one undo step).
    {
        let mut app = App::new(None)?;
        typ(&mut app, "aa")?; app.handle_key(k(KeyCode::Enter))?; typ(&mut app, "bb")?;
        app.handle_key(kc(KeyCode::Char('x')))?; app.handle_key(k(KeyCode::Char('h')))?; // C-x h select all
        app.handle_key(k(KeyCode::Tab))?;
        assert_eq!(text_of(&app), "    aa\n    bb", "Tab did not indent the block");
        app.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))?;
        assert_eq!(text_of(&app), "aa\nbb", "Shift-Tab did not dedent the block");
    }
    println!("[selfcheck] indent/dedent selection ... PASS");

    // 6h. Query-replace: from/to prompts, then y replaces one, ! replaces the rest.
    {
        let mut app = App::new(None)?;
        typ(&mut app, "foo bar foo")?;
        app.run_action(palette::Action::QueryReplace);
        assert!(app.mode == mode::Mode::Prompt, "query-replace did not prompt");
        typ(&mut app, "foo")?; app.handle_key(k(KeyCode::Enter))?; // from
        typ(&mut app, "XYZ")?; app.handle_key(k(KeyCode::Enter))?; // to → begins stepping
        assert!(app.mode == mode::Mode::Prompt, "query-replace stepping prompt missing");
        app.handle_key(k(KeyCode::Char('y')))?; // replace first
        app.handle_key(k(KeyCode::Char('!')))?; // replace the rest
        assert_eq!(text_of(&app), "XYZ bar XYZ", "query-replace did not replace all matches");
        assert!(app.mode == mode::Mode::Edit, "query-replace did not finish");
    }
    println!("[selfcheck] query-replace (y/n/!/q) ... PASS");

    // 7. which-key: a pending prefix pops the continuation panel after a beat;
    //    C-x C-s on a pathless buffer opens Save-As (no ghost `:w` advice).
    app.handle_key(kc(KeyCode::Char('x')))?;
    assert_eq!(app.pending_prefix.len(), 1, "C-x did not arm a prefix");
    app.frame_tick += 30; // simulate the hesitation
    term.draw(|f| ui::render(f, &mut app))?;
    assert!(screen_text(&term).contains("C-x -"), "which-key panel missing");
    app.handle_key(kc(KeyCode::Char('s')))?; // completes C-x C-s → Save
    assert!(app.mode == mode::Mode::Prompt, "pathless save did not prompt Save-As");
    term.draw(|f| ui::render(f, &mut app))?;
    assert!(screen_text(&term).contains("Save as:"), "Save-As prompt not shown");
    app.handle_key(kc(KeyCode::Char('g')))?; // cancel
    println!("[selfcheck] which-key + Save-As ........ PASS");

    // 8. Dirty-quit guard: C-x C-c with modified buffers must confirm.
    app.handle_key(kc(KeyCode::Char('x')))?;
    app.handle_key(kc(KeyCode::Char('c')))?;
    assert!(!app.should_quit, "quit discarded unsaved work without asking");
    assert!(app.mode == mode::Mode::Prompt, "no quit confirmation prompt");
    app.handle_key(k(KeyCode::Char('q')))?; // quit anyway
    assert!(app.should_quit, "confirmed quit did not quit");
    app.should_quit = false;
    println!("[selfcheck] dirty-quit guard ........... PASS");

    // ── Fresh app: command bar surfaces ──────────────────────────────────────
    let mut app = App::new(None)?;

    // 9. Ctrl+Space opens the bar; fuzzy finds Split; the row shows the REAL
    //    binding (C-x 2), not a dead hotkey badge.
    app.handle_key(kc(KeyCode::Char(' ')))?;
    term.draw(|f| ui::render(f, &mut app))?;
    assert!(screen_text(&term).contains("CMD"), "Ctrl+Space did not open the bar");
    typ(&mut app, "split")?;
    term.draw(|f| ui::render(f, &mut app))?;
    let t9 = screen_text(&term);
    assert!(t9.contains("Split"), "fuzzy search lost Split");
    // Capability-tiered binding wins the badge: SplitVertical → "C-x 3" (universal),
    // never the kitty-only "C--" a legacy terminal can't send (honesty invariant).
    assert!(t9.contains("C-x 3"), "dropdown row missing its live (universal) keybinding");
    assert!(!t9.contains("C--"), "dropdown taught a kitty-only chord (honesty breach)");
    app.handle_key(k(KeyCode::Esc))?;
    println!("[selfcheck] bar fuzzy + live binding ... PASS");

    // 10. Graduation nudge: the 3rd bar-run of an action hints its binding.
    for _ in 0..3 {
        app.handle_key(kc(KeyCode::Char(' ')))?;
        typ(&mut app, "undo")?;
        app.handle_key(k(KeyCode::Enter))?;
    }
    assert!(
        app.status_msg.as_deref().unwrap_or("").contains("next time"),
        "no graduation nudge after 3 bar uses"
    );
    println!("[selfcheck] graduation nudge ........... PASS");

    // 11. `!` shell mode: the command reaches a real PTY in a pane.
    app.handle_key(kc(KeyCode::Char(' ')))?;
    app.handle_key(k(KeyCode::Char('!')))?;
    term.draw(|f| ui::render(f, &mut app))?;
    assert!(screen_text(&term).contains("SH !"), "! did not enter shell mode");
    typ(&mut app, "echo ares_shell_ok")?;
    app.handle_key(k(KeyCode::Enter))?;
    assert!(app.mode == mode::Mode::Terminal, "shell command did not attach terminal");
    let tid = match app.focused_pane().content {
        pane::PaneContent::Terminal(id) => id,
        _ => panic!("focused pane is not a terminal"),
    };
    assert!(
        wait_until(|| {
            app.tick();
            app.terms[&tid].screen().contents().contains("ares_shell_ok")
        }),
        "shell command output not found in PTY"
    );
    println!("[selfcheck] bar `!` → shell ............ PASS");

    // 11b. Rover reflow + takeover: while Rover owns a pane, the render keeps it at the
    // phone's size (a wide TUI reflows narrow); the desk reclaims it on interaction.
    let term_pane = *app
        .panes
        .iter()
        .find(|(_, p)| match &p.content {
            pane::PaneContent::Terminal(t) => *t == tid,
            _ => false,
        })
        .map(|(id, _)| id)
        .expect("terminal pane id");
    app.mobile_reflow = Some((term_pane, 24, 48));
    term.draw(|f| ui::render(f, &mut app))?;
    assert_eq!(app.terms[&tid].screen().size(), (24, 48), "render did not honour Rover takeover");
    app.mobile_reflow = None; // mars becomes active
    term.draw(|f| ui::render(f, &mut app))?;
    assert_ne!(app.terms[&tid].screen().size(), (24, 48), "mars did not reclaim the pane size");
    println!("[selfcheck] rover takeover / reclaim .. PASS");

    // 12. Ctrl+Space works INSIDE the terminal, and closing returns to it.
    app.handle_key(kc(KeyCode::Char(' ')))?;
    assert!(app.mode == mode::Mode::Bar, "Ctrl+Space dead inside terminal");
    app.handle_key(k(KeyCode::Esc))?;
    assert!(app.mode == mode::Mode::Terminal, "bar did not return to terminal");
    app.handle_key(kc(KeyCode::Char('g')))?; // detach
    assert!(app.mode == mode::Mode::Edit, "C-g did not detach");
    println!("[selfcheck] bar from terminal .......... PASS");

    // 13. Ask mode with no key gives a friendly notice (or fires if a key is set).
    app.handle_key(kc(KeyCode::Char(' ')))?;
    app.handle_key(k(KeyCode::Tab))?; // → ASK
    typ(&mut app, "hi")?;
    app.handle_key(k(KeyCode::Enter))?;
    if agent::AgentConfig::from_env().is_configured() {
        assert!(app.agent_pending, "expected a pending request with key set");
        println!("[selfcheck] ask agent (key set) ........ PASS");
    } else {
        let ans = app.agent_answer.clone().unwrap_or_default();
        assert!(ans.contains("mars setup"), "expected no-key setup nudge, got: {ans:?}");
        println!("[selfcheck] ask agent (no key) ......... PASS");
    }
    app.handle_key(k(KeyCode::Esc))?;

    // 14. kill_buffer with two panes on one buffer must not leave a dangling id.
    let mut app = App::new(None)?;
    typ(&mut app, "one")?;
    app.handle_key(kc(KeyCode::Char('x')))?;
    app.handle_key(k(KeyCode::Char('2')))?; // C-x 2 → split (same buffer)
    app.new_scratch(); // a second buffer to land on
    app.handle_key(kc(KeyCode::Char('x')))?;
    app.handle_key(k(KeyCode::Char('k')))?; // C-x k → kill buffer
    app.handle_key(kc(KeyCode::Char('x')))?;
    app.handle_key(k(KeyCode::Char('o')))?; // C-x o → other pane (would panic before)
    let _ = app.focused_buf(); // must not panic
    assert_eq!(app.buffers.len(), 1, "kill_buffer left extra buffers");
    println!("[selfcheck] kill_buffer retarget ....... PASS");

    // 15. A real terminal PTY spawns and echoes.
    let (tx, rx) = std::sync::mpsc::channel();
    let startup_probe = std::time::Duration::from_millis(
        tuning::Tuning::default().terminal_startup_probe_ms,
    );
    let mut sh = terminal::spawn(0, 24, 80, 1000, 1 << 20, None, None, None, startup_probe, tx)?;
    let screen_has_line = |sh: &terminal::Term, needle: &str| {
        sh.screen()
            .contents()
            .lines()
            .any(|line| line.trim() == needle)
    };
    sh.send_bytes(b"echo ares_pty_ok\r");
    assert!(
        wait_until(|| {
            sh.flush_startup_input();
            screen_has_line(&sh, "ares_pty_ok")
        }),
        "terminal output not found: {:?}",
        sh.screen().contents()
    );
    while rx.try_recv().is_ok() {}
    println!("[selfcheck] terminal PTY echo .......... PASS");

    // 15a. Terminal mouse-copy: the selection extractor pulls the selected cells
    //      as text (the core of drag-to-copy in a terminal pane).
    {
        // Both dialects print the bare marker on its own row (the echoed command
        // line never trim-equals it, so only real output can match below).
        if shell_is_powershell() {
            sh.send_bytes(b"echo COPYME123\r");
        } else {
            sh.send_bytes(b"printf 'COPYME123\\n'\r");
        }
        let row_of = |sh: &terminal::Term| -> Option<u16> {
            let screen = sh.screen();
            let (rows, cols) = screen.size();
            (0..rows).find(|&r| {
                let mut line = String::new();
                for c in 0..cols {
                    line.push_str(&screen.cell(r, c).map(|x| x.contents()).unwrap_or_default());
                }
                line.trim() == "COPYME123"
            })
        };
        assert!(wait_until(|| row_of(&sh).is_some()), "marker output row not found on screen");
        while rx.try_recv().is_ok() {}
        let screen = sh.screen();
        let (_, cols) = screen.size();
        let r = row_of(&sh).expect("marker row vanished");
        let text = app::selection_text_from_screen(&screen, (r, 0), (r, 8), cols - 1);
        assert_eq!(text, "COPYME123", "terminal selection extraction wrong: {text:?}");
    }
    println!("[selfcheck] terminal mouse-copy ....... PASS");

    // 15b. The pane transcript: every line that scrolls off is captured, numbered, exactly
    //      once, in order. Numbered output makes each of the failures this replaced
    //      unambiguous — a duplicate, a gap, or a reversed page all break the same assert.
    {
        // Formatted rows carry SGR bytes, so compare through a real ANSI interpreter rather
        // than on the raw bytes (see AGENTS.md).
        let plain = |row: &[u8]| -> String {
            let mut p = vt100::Parser::new(1, 400, 0);
            p.process(row);
            p.screen().contents().trim().to_string()
        };
        if shell_is_powershell() {
            sh.send_bytes(b"1..300 | % { 'LINELOG_{0:d3}' -f $_ }\r");
        } else {
            sh.send_bytes(b"seq 1 300 | awk '{printf \"LINELOG_%03d\\n\", $1}'\r");
        }
        assert!(
            wait_until(|| screen_has_line(&sh, "LINELOG_300")),
            "transcript fixture never ran: {:?}",
            sh.screen().contents()
        );
        while rx.try_recv().is_ok() {}

        let (from, first, total, rows) = sh.lines(0, u64::MAX);
        assert_eq!(from, first, "full read should start at the retained floor");
        assert_eq!(total - first, rows.len() as u64, "total disagrees with what was returned");
        let seen: Vec<usize> = rows
            .iter()
            .filter_map(|r| plain(r).strip_prefix("LINELOG_").and_then(|n| n.parse().ok()))
            .collect();
        assert!(seen.len() >= 250, "transcript captured only {} of 300 lines", seen.len());
        assert!(
            seen.windows(2).all(|w| w[1] == w[0] + 1),
            "transcript duplicated, gapped or reordered: {:?}",
            seen.windows(2).find(|w| w[1] != w[0] + 1)
        );

        // A window means the same slice a full read would have handed back at those ids.
        let mid = first + (total - first) / 2;
        let (wfrom, _, _, wrows) = sh.lines(mid, mid + 10);
        assert_eq!(wfrom, mid, "window start not echoed");
        assert_eq!(wrows.len(), 10, "window returned {} rows, wanted 10", wrows.len());
        assert_eq!(
            plain(&wrows[0]),
            plain(&rows[(mid - first) as usize]),
            "window {mid} disagrees with the full read at the same id"
        );

        // An empty range is how a client asks "how much is there?" before asking for any.
        let (_, bf, bt, brows) = sh.lines(0, 0);
        assert!(brows.is_empty(), "empty range returned rows");
        assert_eq!((bf, bt), (first, total), "bounds probe disagrees with the full read");
        // Past the end is empty and clamped, never a panic or a wrapped window.
        let (ofrom, _, _, orows) = sh.lines(total + 500, total + 600);
        assert!(orows.is_empty() && ofrom == total, "out-of-range window not clamped");
    }
    println!("[selfcheck] pane transcript ............ PASS");

    // 15d. Overwriting in place is not history. A line redrawn over itself must reach the
    //      transcript once, in its final form — this is what stopped a repainting program
    //      from stacking near-identical copies of the same command.
    {
        let plain = |row: &[u8]| -> String {
            let mut p = vt100::Parser::new(1, 400, 0);
            p.process(row);
            p.screen().contents().trim().to_string()
        };
        let before = sh.lines(0, 0).2;
        if shell_is_powershell() {
            sh.send_bytes(b"Write-Host \"ZAPMEE`rKEEPME\"\r");
        } else {
            sh.send_bytes(b"printf 'ZAPMEE\\rKEEPME\\n'\r");
        }
        assert!(wait_until(|| screen_has_line(&sh, "KEEPME")), "repaint fixture never ran");
        // Push it off the screen so it has to travel through the transcript to be seen.
        if shell_is_powershell() {
            sh.send_bytes(b"1..40 | % { 'PUSH' }\r");
        } else {
            sh.send_bytes(b"seq 1 40 | awk '{print \"PUSH\"}'\r");
        }
        assert!(wait_until(|| sh.lines(0, 0).2 > before + 40), "transcript did not advance");
        while rx.try_recv().is_ok() {}
        let (_, _, _, rows) = sh.lines(before, u64::MAX);
        let text: Vec<String> = rows.iter().map(|r| plain(r)).collect();
        assert_eq!(
            text.iter().filter(|l| l.as_str() == "KEEPME").count(),
            1,
            "overwritten line reached the transcript {} times",
            text.iter().filter(|l| l.as_str() == "KEEPME").count()
        );
        // Trim-equality, not `contains`: the shell echoes the command line itself, and that
        // line legitimately holds the literal `ZAPMEE` — a transcript records what was typed.
        // What must not survive is a ROW that still reads as the overwritten text.
        assert!(
            !text.iter().any(|l| l == "ZAPMEE"),
            "overwritten row survived into the transcript: {text:?}"
        );
    }
    println!("[selfcheck] transcript repaint .......... PASS");

    // 15b. Nested open: `mars <file>` from inside a session routes here and opens
    //      the file in a NEW tab, switched-to (instead of nesting a second Mars).
    {
        let mut app = App::new(None)?;
        let tabs_before = app.tabs.len();
        let f = std::env::temp_dir().join(format!("mars-nest-{}.txt", std::process::id()));
        std::fs::write(&f, "nested_open_content")?;
        app.open_file_in_new_tab(f.to_str().unwrap());
        assert_eq!(app.tabs.len(), tabs_before + 1, "open did not add a tab");
        assert_eq!(app.active_tab, app.tabs.len() - 1, "did not switch to the new tab");
        assert!(app.mode == mode::Mode::Edit, "not in edit mode after nested open");
        let shows = match app.focused_pane().content {
            pane::PaneContent::Editor(id) => app.buffers[&id].rope.to_string().contains("nested_open_content"),
            _ => false,
        };
        assert!(shows, "new tab's focused pane is not the opened file");
        let _ = std::fs::remove_file(&f);
    }
    println!("[selfcheck] nested open (new tab) ..... PASS");

    // 15b. Scrollback: history survives past the viewport and the view can
    //      scroll back through it, then snap to live.
    if shell_is_powershell() {
        sh.send_bytes(b"1..100\r"); // pwsh's seq: a range prints one line each
    } else {
        sh.send_bytes(b"seq 1 100\r");
    }
    // "99" is proof the OUTPUT arrived — the echoed command line contains "100"
    // in both dialects, so waiting on that could fire before the run.
    assert!(
        wait_until(|| sh.screen().contents().contains("99")),
        "seq output missing"
    );
    while rx.try_recv().is_ok() {}
    let live = sh.screen().contents();
    assert!(live.contains("100"), "seq output missing");
    assert!(!live.contains("\n3\n"), "expected early lines scrolled out of view");
    sh.scroll_view(80); // back into history
    assert!(sh.view_offset() > 0, "view offset did not move");
    let back = sh.screen().contents();
    assert_ne!(live, back, "scrollback view identical to live view");
    assert!(back.contains("\n3\n") || back.contains("seq 1 100"),
        "history not visible after scrolling back");
    sh.scroll_to_live();
    assert_eq!(sh.view_offset(), 0, "snap-back failed");
    assert_eq!(sh.screen().contents(), live, "live view changed after snap-back");
    // W5: history_tail pages the scrollback (below the viewport) and restores the
    // live view — must reach early lines the live screen scrolled past.
    let tail = sh.history_tail(60);
    assert!(tail.contains("100"), "history_tail missing recent output");
    assert!(tail.lines().count() > 24, "history_tail did not exceed one screen");
    assert_eq!(sh.view_offset(), 0, "history_tail left the view scrolled back");
    println!("[selfcheck] terminal scrollback ........ PASS");

    // 15e. Dead-shell lifecycle: exit → Exited event → Enter recycles the pane.
    let mut app = App::new(None)?;
    app.handle_key(kc(KeyCode::Char(' ')))?;
    app.handle_key(k(KeyCode::Char('!')))?;
    typ(&mut app, "exit")?;
    app.handle_key(k(KeyCode::Enter))?; // attached; shell exits immediately
    let dead = wait_until(|| {
        app.tick(); // drains TermEvent::Exited
        app.terms.values().any(|t| t.exited)
    });
    assert!(dead, "shell exit not detected");
    term.draw(|f| ui::render(f, &mut app))?;
    assert!(
        screen_text(&term).contains("process exited"),
        "dead-shell notice not rendered"
    );
    app.handle_key(k(KeyCode::Enter))?; // dismiss
    assert!(app.terms.is_empty(), "exited terminal not cleaned up");
    assert!(
        matches!(app.focused_pane().content, pane::PaneContent::Editor(_)),
        "pane not recycled to an editor"
    );
    assert!(app.mode == mode::Mode::Edit, "mode not restored after dismissal");
    println!("[selfcheck] dead-shell lifecycle ....... PASS");

    // 15d. Autosave: a modified path-backed buffer hits disk on the timer.
    let autosave_file = cfg_dir.join("autosave_probe.txt");
    std::fs::write(&autosave_file, "original")?;
    let mut app = App::new(Some(autosave_file.to_string_lossy().to_string()))?;
    app.tuning.autosave_secs = 1;
    app.handle_key(kc(KeyCode::Char('e')))?; // line end
    typ(&mut app, " EDITED")?;
    let ticks = (1000 / app.tuning.poll_interval_ms).max(1) + 2;
    for _ in 0..ticks {
        app.tick();
    }
    assert!(
        std::fs::read_to_string(&autosave_file)?.contains("EDITED"),
        "autosave did not write the buffer to disk"
    );
    println!("[selfcheck] autosave (crash safety) .... PASS");

    // ── Movement audit ───────────────────────────────────────────────────────
    fn alt(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::ALT)
    }

    // 16. Tabs: C-t t = new tab (travel mode); M-{ / M-} switch (sent as
    //     ALT|SHIFT, normalized); M-1 jumps directly.
    let mut app = App::new(None)?;
    app.handle_key(kc(KeyCode::Char('t')))?; // C-t → travel mode
    assert!(app.mode == mode::Mode::Tab, "C-t did not enter travel mode");
    app.handle_key(k(KeyCode::Char('t')))?; // t → new tab, exits mode
    assert_eq!(app.tabs.len(), 2, "C-t t did not open a new tab");
    assert_eq!(app.active_tab, 1);
    assert!(app.mode == mode::Mode::Edit, "new tab did not exit travel mode");
    app.handle_key(KeyEvent::new(KeyCode::Char('{'), KeyModifiers::ALT | KeyModifiers::SHIFT))?;
    assert_eq!(app.active_tab, 0, "M-{{ did not switch to the previous tab");
    app.handle_key(KeyEvent::new(KeyCode::Char('}'), KeyModifiers::ALT | KeyModifiers::SHIFT))?;
    assert_eq!(app.active_tab, 1, "M-}} did not switch to the next tab");
    app.handle_key(alt(KeyCode::Char('1')))?;
    assert_eq!(app.active_tab, 0, "M-1 did not jump to tab 1");
    println!("[selfcheck] tab movement ............... PASS");

    // 17. Panes: C-\ splits; Ctrl-arrows focus directionally (Alt-arrows now move
    //     by token/page); M-o cycles; C-x x moves.
    let mut app = App::new(None)?;
    app.handle_key(kc(KeyCode::Char('\\')))?; // C-\ → split right
    assert_eq!(app.tab().layout.count(), 2, "C-\\ did not split");
    let right = app.focused_pane_id();
    term.draw(|f| ui::render(f, &mut app))?; // populate pane geometry
    app.handle_key(kc(KeyCode::Left))?; // C-← → focus left pane
    let left = app.focused_pane_id();
    assert_ne!(left, right, "C-Left did not move focus");
    app.handle_key(kc(KeyCode::Right))?; // C-→ → back right
    assert_eq!(app.focused_pane_id(), right, "C-Right did not move focus back");
    app.handle_key(alt(KeyCode::Char('o')))?; // M-o → cycle
    assert_eq!(app.focused_pane_id(), left, "M-o did not cycle panes");
    // Move (swap): give the focused pane its own buffer, then C-x x.
    let buf1 = app.new_scratch();
    app.panes.get_mut(&left).unwrap().content = pane::PaneContent::Editor(buf1);
    app.handle_key(kc(KeyCode::Char('x')))?;
    app.handle_key(k(KeyCode::Char('x')))?; // C-x x → swap with next pane
    assert_eq!(app.focused_pane_id(), right, "swap did not follow the moved content");
    assert!(
        matches!(app.focused_pane().content, pane::PaneContent::Editor(id) if id == buf1),
        "pane content did not move on swap"
    );
    println!("[selfcheck] pane movement + swap ....... PASS");

    // 18. M-g goes to a line.
    let mut app = App::new(None)?;
    typ(&mut app, "l1")?;
    app.handle_key(k(KeyCode::Enter))?;
    typ(&mut app, "l2")?;
    app.handle_key(k(KeyCode::Enter))?;
    typ(&mut app, "l3")?;
    app.handle_key(alt(KeyCode::Char('g')))?; // M-g → goto-line prompt
    assert!(app.mode == mode::Mode::Prompt, "M-g did not prompt");
    typ(&mut app, "1")?;
    app.handle_key(k(KeyCode::Enter))?;
    assert_eq!(app.focused_pane().cursor_row, 0, "goto-line did not jump");
    println!("[selfcheck] goto line (M-g) ............ PASS");

    // 19. Paste: bracketed-paste text inserts; C-v is bound to Paste.
    app.paste_text("PASTED");
    term.draw(|f| ui::render(f, &mut app))?;
    assert!(screen_text(&term).contains("PASTED"), "paste_text did not insert");
    let cv = config::parse_key("C-v").unwrap();
    assert_eq!(
        app.keys.lookup(&[cv]),
        Some(palette::Action::Paste),
        "C-v is not bound to Paste"
    );
    println!("[selfcheck] paste (C-v + bracketed) .... PASS");

    // 20. Ctrl+C / Ctrl+V round-trip: select → C-c → move → C-v duplicates;
    //     C-c with no selection copies the whole line.
    let mut app = App::new(None)?;
    typ(&mut app, "hello world")?;
    app.handle_key(kc(KeyCode::Char('a')))?; // line start
    for _ in 0..5 {
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT))?;
    }
    app.handle_key(kc(KeyCode::Char('c')))?; // C-c → copy selection
    assert_eq!(app.kill_ring.last().map(String::as_str), Some("hello"), "C-c did not copy");
    // Every copy queues an OSC 52 escape for the real terminal — the only
    // clipboard channel that survives the daemon/ssh hop.
    {
        use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
        let osc = app.take_osc().expect("copy did not queue an OSC 52 escape");
        let b64 = osc
            .strip_prefix("\x1b]52;c;")
            .and_then(|s| s.strip_suffix('\x07'))
            .expect("OSC 52 escape malformed");
        assert_eq!(B64.decode(b64).unwrap(), b"hello", "OSC 52 payload mismatch");
        assert!(app.take_osc().is_none(), "take_osc did not drain");
    }
    app.handle_key(kc(KeyCode::Char('e')))?; // line end
    app.handle_key(kc(KeyCode::Char('v')))?; // C-v → paste
    term.draw(|f| ui::render(f, &mut app))?;
    assert!(screen_text(&term).contains("hello worldhello"), "C-v did not paste the copy");
    app.handle_key(kc(KeyCode::Char('c')))?; // no selection → copy line
    assert_eq!(
        app.kill_ring.last().map(String::as_str),
        Some("hello worldhello"),
        "C-c without selection did not copy the line"
    );
    println!("[selfcheck] Ctrl+C / Ctrl+V ............ PASS");

    // 21. C-t travel mode: cheat panel renders; navigation stays, creation exits.
    let mut app = App::new(None)?;
    app.handle_key(kc(KeyCode::Char('t')))?; // enter travel mode
    term.draw(|f| ui::render(f, &mut app))?;
    let t21 = screen_text(&term);
    assert!(t21.contains("WARP"), "space-warp cheat panel (WARP box) missing");
    assert!(t21.contains("split right"), "cheat panel missing split hint");
    app.handle_key(k(KeyCode::Char('-')))?; // split below, exits
    assert_eq!(app.tab().layout.count(), 2, "travel '-' did not split");
    assert!(app.mode == mode::Mode::Edit, "split did not exit travel mode");
    app.handle_key(kc(KeyCode::Char('t')))?;
    app.handle_key(k(KeyCode::Char('o')))?; // next pane — stays in mode
    assert!(app.mode == mode::Mode::Tab, "navigation should stay in travel mode");
    app.handle_key(k(KeyCode::Char('|')))?; // split right, exits
    assert_eq!(app.tab().layout.count(), 3, "travel '|' did not split");
    app.handle_key(kc(KeyCode::Char('t')))?;
    app.handle_key(k(KeyCode::Esc))?; // Esc leaves
    assert!(app.mode == mode::Mode::Edit, "Esc did not leave travel mode");
    println!("[selfcheck] C-t travel mode ............ PASS");

    // 22. Modern-terminal chords are bound (fire under the kitty protocol).
    for (seq, action) in [
        ("C-{", palette::Action::PrevTab),
        ("C-}", palette::Action::NextTab),
        ("C--", palette::Action::SplitHorizontal),
        ("C-|", palette::Action::SplitVertical),
        ("M--", palette::Action::SplitHorizontal),
    ] {
        let chord = config::parse_key(seq).unwrap();
        assert_eq!(app.keys.lookup(&[chord]), Some(action), "{seq} not bound");
    }
    println!("[selfcheck] modern chords (kitty) ...... PASS");

    // 23. Pane movement without Meta: C-o cycles; Ctrl+arrows move directionally.
    let mut app = App::new(None)?;
    app.handle_key(kc(KeyCode::Char('\\')))?; // split right
    let right = app.focused_pane_id();
    term.draw(|f| ui::render(f, &mut app))?;
    app.handle_key(kc(KeyCode::Left))?; // Ctrl+← → left pane
    let left = app.focused_pane_id();
    assert_ne!(left, right, "Ctrl+Left did not move focus");
    app.handle_key(kc(KeyCode::Char('o')))?; // C-o → cycle
    assert_eq!(app.focused_pane_id(), right, "C-o did not cycle panes");
    println!("[selfcheck] pane nav sans Meta ......... PASS");

    // 24. Terminal chrome: navigation chords work INSIDE a terminal pane.
    let mut app = App::new(None)?;
    app.handle_key(kc(KeyCode::Char(' ')))?;
    app.handle_key(k(KeyCode::Char('!')))?;
    typ(&mut app, "true")?;
    app.handle_key(k(KeyCode::Enter))?; // now attached to a shell pane
    assert!(app.mode == mode::Mode::Terminal);
    app.handle_key(kc(KeyCode::Char('t')))?; // C-t → travel mode, even here
    assert!(app.mode == mode::Mode::Tab, "C-t dead inside terminal");
    app.handle_key(k(KeyCode::Char('t')))?; // new tab (editor) — exits travel
    assert_eq!(app.tabs.len(), 2);
    assert!(app.mode == mode::Mode::Edit);
    app.handle_key(KeyEvent::new(KeyCode::Char('{'), KeyModifiers::ALT | KeyModifiers::SHIFT))?;
    assert_eq!(app.active_tab, 0, "M-{{ did not switch tab from editor");
    // Back on the terminal tab: M-} from INSIDE the terminal switches tabs.
    app.handle_key(k(KeyCode::Enter))?; // re-attach (terminal pane focused)
    assert!(app.mode == mode::Mode::Terminal);
    app.handle_key(KeyEvent::new(KeyCode::Char('}'), KeyModifiers::ALT | KeyModifiers::SHIFT))?;
    assert_eq!(app.active_tab, 1, "M-}} dead inside terminal");
    println!("[selfcheck] terminal chrome layer ...... PASS");

    // 25. Cmd (super) bindings parse and are bound.
    let cmd_c = config::parse_key("cmd-c").unwrap();
    assert!(cmd_c.modifiers.contains(KeyModifiers::SUPER), "cmd- prefix not SUPER");
    assert_eq!(app.keys.lookup(&[cmd_c]), Some(palette::Action::CopyRegion));
    assert_eq!(
        app.keys.lookup(&[config::parse_key("cmd-v").unwrap()]),
        Some(palette::Action::Paste)
    );
    println!("[selfcheck] cmd-c / cmd-v bindings ..... PASS");

    // 26. Tuning: defaults written with descriptions; user overrides apply.
    let tuning_path = cfg_dir.join("mars").join("tuning.json");
    let written = std::fs::read_to_string(&tuning_path)?;
    assert!(written.contains("description"), "tuning defaults lack descriptions");
    assert!(written.contains("which_key_delay_ms"), "tuning defaults missing knobs");
    assert!(written.contains("terminal_startup_probe_ms"), "tuning defaults missing shell probe knob");
    std::fs::write(
        &tuning_path,
        r#"{ "max_panes": { "value": 2, "description": "test override" } }"#,
    )?;
    let mut app = App::new(None)?;
    assert_eq!(app.tuning.max_panes, 2, "tuning override not applied");
    app.handle_key(kc(KeyCode::Char('\\')))?; // 2nd pane — at the cap
    app.handle_key(kc(KeyCode::Char('\\')))?; // refused
    assert_eq!(app.tab().layout.count(), 2, "max_panes override not enforced");
    assert!(
        app.status_msg.as_deref().unwrap_or("").contains("Max 2"),
        "cap message should reflect the tuned value"
    );
    std::fs::remove_file(&tuning_path)?; // restore defaults for any later checks
    println!("[selfcheck] tuning knobs + override .... PASS");

    // 26a2. The top-right status counter (beacon) is REMOVED — it silted up with
    //       finished-Done counts that never clear. Status renders as a single colored
    //       bubble (●) in a consistent position — no per-status glyphs (⏸/✗/✓).
    {
        let mut app = App::new(None)?;
        app.handle_key(k(KeyCode::Char('x')))?; // dismiss the splash
        app.open_terminal();
        let tid = *app.terms.keys().next().expect("open_terminal makes a terminal");
        app.watches.entry(tid).or_default().verdict =
            Some("blocked: overwrite runs/best.pt? [y/N]".into());
        let mut term = Terminal::new(TestBackend::new(100, 20))?;
        term.draw(|f| ui::render(f, &mut app))?;
        let t = screen_text(&term);
        assert!(t.contains("●"), "the tab label must show a status bubble: {t}");
        assert!(!t.contains("⏸") && !t.contains("✗") && !t.contains("✓"),
            "status must be bubbles only — no per-status glyphs: {t}");
        println!("[selfcheck] status bubbles ............ PASS");
    }

    // 26a3. The surface-status seam: pane/tab verdict is the ONE source the tab
    //       labels (and later the board + pane borders) render, so no two views of
    //       the monitor can disagree. Per-tab status colors the WHOLE label.
    {
        let mut app = App::new(None)?;
        app.handle_key(k(KeyCode::Char('x')))?; // dismiss splash
        app.open_terminal(); // focused pane → a terminal surface
        let tid = *app.terms.keys().next().expect("open_terminal makes a terminal");
        // Alive terminal, no watch → idle (Context); the tab reads idle, not a lie.
        assert_eq!(app.tab_status(app.tab()), briefing::Verdict::Context,
            "an idle terminal pane must read Context");
        // Running means producing output NOW, not "ever produced output". A watch
        // whose run started but whose last output is stale (a shell idling at a
        // prompt) must read Context — NOT a green Running lie. frame_tick is far
        // past a stale last_output_tick, so the quiet window has elapsed.
        app.frame_tick = 100_000;
        app.watches.entry(tid).or_default().run_started_tick = 1;
        app.watches.entry(tid).or_default().last_output_tick = 1; // long ago
        assert_eq!(app.pane_verdict(app.focused_pane_id()), briefing::Verdict::Context,
            "an idle-at-prompt terminal (stale output) must read Context, not Running");
        // …but fresh output (last_output_tick ≈ now) is a real Running.
        app.watches.entry(tid).or_default().last_output_tick = app.frame_tick;
        assert_eq!(app.pane_verdict(app.focused_pane_id()), briefing::Verdict::Running,
            "a terminal producing output now must read Running");
        app.watches.remove(&tid); // reset for the blocked-verdict check below
        // A blocked watch verdict propagates pane → tab (worst-wins aggregate)…
        app.watches.entry(tid).or_default().verdict =
            Some("blocked: overwrite runs/best.pt? [y/N]".into());
        assert_eq!(app.pane_verdict(app.focused_pane_id()), briefing::Verdict::Blocked);
        assert_eq!(app.tab_status(app.tab()), briefing::Verdict::Blocked);
        // …and the tab bar renders a status bubble for it.
        let mut term = Terminal::new(TestBackend::new(80, 20))?;
        term.draw(|f| ui::render(f, &mut app))?;
        assert!(screen_text(&term).contains("●"),
            "a blocked tab must render a status bubble in the tab bar");
        println!("[selfcheck] surface status seam ...... PASS");
    }

    // 26b. Default gutter is a slim pointer (no numbers); the knob restores
    //      the number column; the line/col lives in the status bar.
    let mut app = App::new(None)?;
    app.handle_key(k(KeyCode::Char('G')))?; // dismiss splash, type
    typ(&mut app, "UT")?;
    term.draw(|f| ui::render(f, &mut app))?;
    let t26 = screen_text(&term);
    assert!(t26.contains("GUT"), "typed text missing");
    assert!(!t26.contains("   1│"), "number gutter rendered despite line_numbers=false");
    assert!(t26.contains("▸"), "pointer gutter marker missing on the cursor line");
    assert!(t26.contains("Ln 1, Col 4"), "status bar missing line/col readout");
    app.tuning.line_numbers = true;
    term.draw(|f| ui::render(f, &mut app))?;
    assert!(screen_text(&term).contains("   1│"), "line_numbers knob did not restore numbers");
    println!("[selfcheck] pointer gutter + Ln/Col ... PASS");

    // 26b2. Project index: skip-list + cap.
    let proj = cfg_dir.join(format!("proj-{}", std::process::id()));
    {
        use std::io::Write as _;
        std::fs::create_dir_all(proj.join("src"))?;
        std::fs::create_dir_all(proj.join("target"))?; // must be skipped
        for f in ["src/main.rs", "src/session.rs", "README.md"] {
            let p = proj.join(f);
            std::fs::create_dir_all(p.parent().unwrap())?;
            std::fs::File::create(&p)?.write_all(b"x")?;
        }
        std::fs::File::create(proj.join("target/junk.rs"))?.write_all(b"x")?;
        let idx = project::Index::build(proj.clone(), 10_000, &["target".to_string()]);
        assert!(idx.files.iter().any(|f| f == "src/session.rs"), "index missing a real file");
        assert!(!idx.files.iter().any(|f| f.contains("target")), "index did not skip target/");
        assert!(project::Index::build(proj.clone(), 2, &["target".to_string()]).files.len() <= 2,
            "index did not honor the cap");
    }
    println!("[selfcheck] project index (skip/cap) .. PASS");

    // 26b3. Left file tree: `@` opens it, browse shows folders (not target/),
    //       expand reveals children, type-to-filter shortlists, Enter opens.
    let mut app = App::new(None)?;
    // Root the tree at the temp project (browse reads the real filesystem).
    app.set_project_index_for_test(
        proj.clone(),
        vec!["src/main.rs".into(), "src/session.rs".into(), "README.md".into()],
    );
    app.handle_key(kc(KeyCode::Char(' ')))?; // command bar
    app.handle_key(k(KeyCode::Char('@')))?;  // → open the tree
    assert!(matches!(app.mode, mode::Mode::Tree) && app.tree_open, "@ did not open the tree");
    assert!(app.tree_rows.iter().any(|r| r.label == "src" && r.is_dir), "tree missing src/ folder");
    assert!(!app.tree_rows.iter().any(|r| r.label == "target"), "tree showed the ignored dir");
    // Move to src/ (row after the ../ row) and expand it.
    app.handle_key(k(KeyCode::Down))?;   // ../ → src
    app.handle_key(k(KeyCode::Enter))?;  // expand
    assert!(app.tree_rows.iter().any(|r| r.label == "session.rs" && r.depth == 1),
        "expanding src/ did not reveal its children");
    term.draw(|f| ui::render(f, &mut app))?;
    assert!(screen_text(&term).contains("src"), "tree sidebar did not render");
    // Type-to-filter → fuzzy shortlist over the index.
    typ(&mut app, "sesn")?;
    assert_eq!(app.tree_rows.first().map(|r| r.label.as_str()), Some("src/session.rs"),
        "filter did not shortlist session.rs on top");
    // → PREVIEWS the top file: shows it but stays in the tree (reversible).
    let bufs_before = app.buffers.len();
    app.handle_key(k(KeyCode::Right))?;
    assert!(matches!(app.mode, mode::Mode::Tree), "→ on a file left the tree (should preview)");
    // A second preview of the same file must not duplicate the buffer.
    app.handle_key(k(KeyCode::Right))?;
    assert_eq!(app.buffers.len(), bufs_before + 1, "preview duplicated the buffer");
    // Enter COMMITS: opens the top match in a NEW TAB → focus returns to the editor.
    let tabs_before = app.tabs.len();
    app.handle_key(k(KeyCode::Enter))?;
    assert!(matches!(app.mode, mode::Mode::Edit), "Enter on a tree file did not focus the editor");
    assert_eq!(app.tabs.len(), tabs_before + 1, "navigator open should create a new tab");
    assert!(app.tree_open, "sidebar should stay open after opening a file");
    println!("[selfcheck] file tree (preview/open) .. PASS");

    // 26b4. C-x d toggles the tree; `../` re-roots to the parent directory.
    let mut app = App::new(None)?;
    app.set_project_index_for_test(proj.clone(), vec!["README.md".into()]);
    app.handle_key(kc(KeyCode::Char('x')))?;
    app.handle_key(k(KeyCode::Char('d')))?; // C-x d
    assert!(app.tree_open && matches!(app.mode, mode::Mode::Tree), "C-x d did not open the tree");
    let root_before = app.file_tree.as_ref().map(|t| t.root.clone()).unwrap();
    app.handle_key(k(KeyCode::Enter))?; // selected row 0 is ../ → re-root up
    let root_after = app.file_tree.as_ref().map(|t| t.root.clone()).unwrap();
    assert_eq!(root_after, root_before.parent().unwrap(), "../ did not re-root to the parent");
    app.handle_key(k(KeyCode::Esc))?; // Esc closes the focused tree
    assert!(!app.tree_open && matches!(app.mode, mode::Mode::Edit), "Esc did not close the tree");
    // Closing forgets navigation state: reopening starts back at the project root.
    app.handle_key(kc(KeyCode::Char('x')))?;
    app.handle_key(k(KeyCode::Char('d')))?; // C-x d → reopen
    let reopened_root = app.file_tree.as_ref().map(|t| t.root.clone()).unwrap();
    assert_eq!(reopened_root, root_before, "tree did not reset to the project root on reopen");
    assert!(app.tree_open && matches!(app.mode, mode::Mode::Tree), "C-x d did not reopen the tree");
    println!("[selfcheck] file tree (reset/../) ..... PASS");

    // 26b5. Ctrl+Space on a folder re-roots INTO it (descend) — the mirror of `../`,
    //       so you can drill back down after going up. On a file it opens the bar.
    {
        let mut app = App::new(None)?;
        app.set_project_index_for_test(proj.clone(), vec!["src/main.rs".into(), "README.md".into()]);
        app.handle_key(kc(KeyCode::Char('x')))?;
        app.handle_key(k(KeyCode::Char('d')))?; // open the tree
        let src_idx = app.tree_rows.iter().position(|r| r.label == "src" && r.is_dir).expect("src row missing");
        for _ in 0..src_idx { app.handle_key(k(KeyCode::Down))?; } // land on src/
        let before = app.file_tree.as_ref().map(|t| t.root.clone()).unwrap();
        app.handle_key(kc(KeyCode::Char(' ')))?; // Ctrl+Space on the folder → root into it
        let after = app.file_tree.as_ref().map(|t| t.root.clone()).unwrap();
        assert_eq!(after, before.join("src"), "Ctrl+Space did not re-root into the folder");
        assert!(matches!(app.mode, mode::Mode::Tree), "descend should stay in the navigator, not open the bar");
        println!("[selfcheck] navigator descend (C-Space) . PASS");
    }

    // 26c. Conversation transcript: history renders bottom-pinned inside the
    //      ask_panel_max_pct cap (~30% of the workspace), scrolls, and C-l
    //      clears.
    let mut app = App::new(None)?;
    app.agent_history.push(("user".into(), "first question".into()));
    let long: String = (1..=30).map(|i| format!("L{i}")).collect::<Vec<_>>().join("\n");
    app.agent_history.push(("assistant".into(), long));
    app.handle_key(kc(KeyCode::Char(' ')))?;
    app.handle_key(k(KeyCode::Tab))?; // → ASK
    term.draw(|f| ui::render(f, &mut app))?;
    let t27 = screen_text(&term);
    // Bottom-pinned tail, bounded panel: the newest lines show, the middle of
    // the answer does not (it would under the old 60% cap), and the marker
    // teaches the way up.
    assert!(t27.contains("L30") && t27.contains("L23"),
        "panel lost the tail of a long answer");
    assert!(!t27.contains("L15"), "ask panel escaped the ask_panel_max_pct cap");
    assert!(t27.contains("(Up to scroll)"), "scroll-up marker missing");
    // Scroll up to reach the start of the conversation.
    for _ in 0..25 {
        app.handle_key(k(KeyCode::Up))?;
    }
    term.draw(|f| ui::render(f, &mut app))?;
    let t27b = screen_text(&term);
    assert!(t27b.contains("first question"), "scroll-up did not reach the first turn");
    assert!(t27b.contains("more"), "scroll indicator missing");
    app.handle_key(kc(KeyCode::Char('l')))?; // new chat
    assert!(app.agent_history.is_empty(), "C-l did not clear the conversation");
    app.handle_key(k(KeyCode::Esc))?;
    println!("[selfcheck] ask transcript + scroll .... PASS");

    // 26d. History really reaches the provider; directives parse. (Pinned to
    //      the no-persona-file state → the default voice is the last system
    //      message, before history — regardless of this machine's ~/.mars.)
    std::env::set_var("MARS_PERSONA", std::env::temp_dir().join("mars-no-such-persona.md"));
    let msgs = agent::build_ask_messages(
        "reg", "screen",
        &[("user".into(), "q1".into()), ("assistant".into(), "a1".into())],
        "q2",
    );
    std::env::remove_var("MARS_PERSONA");
    assert_eq!(msgs.len(), 5, "system + persona + 2 history + question expected");
    assert!(msgs[0]["content"].as_str().unwrap_or("").contains("screen"));
    assert!(msgs[1]["content"].as_str().unwrap_or("").contains("VOICE"), "persona not the last system message");
    assert!(msgs[2]["content"].as_str().unwrap_or("").contains("q1"));
    let (d1, dir1) = agent::parse_directive("use ls.\nTYPE: ls -la");
    assert_eq!(d1, "use ls.");
    assert_eq!(dir1, Some(agent::AgentDirective::Type("ls -la".into())));
    let (_, dir2) = agent::parse_directive("split it\nRUN: SplitVertical");
    assert_eq!(dir2, Some(agent::AgentDirective::Run("SplitVertical".into())));
    assert_eq!(agent::parse_directive("plain answer").1, None);
    println!("[selfcheck] agent history + directives . PASS");

    // 26e. TYPE directive types into the terminal pane on Enter.
    let mut app = App::new(None)?;
    app.handle_key(kc(KeyCode::Char(' ')))?;
    app.handle_key(k(KeyCode::Char('!')))?;
    typ(&mut app, "true")?;
    app.handle_key(k(KeyCode::Enter))?; // attached shell pane
    app.handle_key(kc(KeyCode::Char(' ')))?; // Ctrl+Space → inline shell composer
    app.handle_key(kc(KeyCode::Char(' ')))?; // again → full command bar
    app.handle_key(k(KeyCode::Tab))?; // → ASK
    app.agent_directive = Some(agent::AgentDirective::Type("echo mars_type_ok".into()));
    app.handle_key(k(KeyCode::Enter))?; // confirm-fire
    assert!(app.mode == mode::Mode::Terminal, "TYPE did not land in the terminal");
    let typed = wait_until(|| {
        app.tick();
        match app.focused_pane().content {
            pane::PaneContent::Terminal(tid) => {
                app.terms[&tid].screen().contents().contains("mars_type_ok")
            }
            _ => false,
        }
    });
    assert!(typed, "TYPE'd command never reached the PTY");
    println!("[selfcheck] TYPE → terminal ............ PASS");

    // 26f. Renames: tab via travel `r`; pane via action; auto-name plumbing.
    let mut app = App::new(None)?;
    app.handle_key(kc(KeyCode::Char('t')))?; // travel mode
    app.handle_key(k(KeyCode::Char('r')))?; // rename tab → prompt (prefilled "1")
    assert!(app.mode == mode::Mode::Prompt, "travel r did not prompt");
    app.handle_key(k(KeyCode::Backspace))?; // clear the "1"
    typ(&mut app, "build")?;
    app.handle_key(k(KeyCode::Enter))?;
    assert_eq!(app.tab().name, "build", "tab rename failed");
    // A generated name must not even be OFFERED over a user-chosen (non-numeric) name.
    let tid0 = app.tab().id;
    app.agent_tx.send(agent::AgentEvent::AutoName { tab_id: tid0, name: "x".into() })?;
    app.tick();
    assert_eq!(app.tab().name, "build", "generated name overrode a manual rename");
    assert!(app.tab_name_suggestion.is_empty(), "suggested a name over a user's own");
    app.run_action(palette::Action::RenamePane);
    typ(&mut app, "logs")?;
    app.handle_key(k(KeyCode::Enter))?;
    term.draw(|f| ui::render(f, &mut app))?;
    assert!(screen_text(&term).contains(" logs "), "pane title not rendered");
    // A default-named tab gets a SUGGESTION, not a rename. Applying it automatically is what
    // moved sessions out from under the things that address them by name; see 29a5.
    let mut app = App::new(None)?;
    let tid1 = app.tab().id;
    app.agent_tx.send(agent::AgentEvent::AutoName { tab_id: tid1, name: "auto-label".into() })?;
    app.tick();
    assert_eq!(app.tab().name, "1", "a generated name was applied without being accepted");
    assert_eq!(app.tab_name_suggestion.get(&tid1).map(String::as_str), Some("auto-label"));
    app.run_action(palette::Action::AcceptTabName);
    assert_eq!(app.tab().name, "auto-label", "accepting the suggestion did not rename");
    println!("[selfcheck] renames + auto-name ........ PASS");

    // ── Phase 1 agentic workflows ────────────────────────────────────────────

    // 26g. Directive parser: OPEN added; lenient to backticks + trailing lines.
    use agent::AgentDirective;
    assert_eq!(
        agent::parse_directive("Line 42 is the bug.\nOPEN: src/main.rs:42").1,
        Some(AgentDirective::Open("src/main.rs:42".into()))
    );
    assert_eq!(
        agent::parse_directive("try this\n`TYPE: git status`").1,
        Some(AgentDirective::Type("git status".into())),
        "parser should tolerate backtick-wrapped directives"
    );
    // Directive on the 2nd-to-last line (model added a sign-off).
    let (disp, dir) = agent::parse_directive("Here's the fix.\nRUN: SplitVertical\nHope that helps!");
    assert_eq!(dir, Some(AgentDirective::Run("SplitVertical".into())));
    assert!(!disp.contains("SplitVertical"), "directive line should be stripped from display");
    assert_eq!(agent::parse_directive("just prose").1, None);
    // Rate-limit retry hint parsing (rounds up).
    assert_eq!(agent::retry_secs("quota exceeded. Please retry in 14.89197552s."), Some(15));
    assert_eq!(agent::retry_secs("no hint here"), None);
    println!("[selfcheck] directive parse (OPEN/lenient) PASS");

    // 26h. OPEN lands the cursor at the cited line in the named file.
    let probe = cfg_dir.join("open_probe.txt");
    std::fs::write(&probe, "a\nb\nc\nd\ne\nf\n")?;
    let mut app = App::new(None)?;
    app.handle_key(kc(KeyCode::Char(' ')))?; // bar
    app.handle_key(k(KeyCode::Tab))?; // → ASK
    app.agent_directive = Some(AgentDirective::Open(format!("{}:4", probe.to_string_lossy())));
    app.handle_key(k(KeyCode::Enter))?; // fire the directive
    assert_eq!(app.focused_pane().cursor_row, 3, "OPEN did not land on line 4");
    assert!(app.focused_buf().name.contains("open_probe"), "OPEN did not open the file");
    println!("[selfcheck] OPEN directive lands ....... PASS");

    // 26i. W1/W2: ExplainThis and ExplainFailure open Ask pre-filled + submit.
    let mut app = App::new(None)?;
    typ(&mut app, "some code")?;
    app.run_action(palette::Action::ExplainThis);
    assert!(app.mode == mode::Mode::Bar, "ExplainThis did not open the bar");
    assert!(
        matches!(app.palette.as_ref().map(|p| &p.bar_mode), Some(palette::BarMode::Ask)),
        "ExplainThis is not in Ask mode"
    );
    // Pre-filled with a grounded question…
    assert!(
        app.palette.as_ref().map(|p| p.query.contains("Explain")).unwrap_or(false),
        "ExplainThis did not pre-fill a question"
    );
    // …and it submitted (no key in this env → the no-key notice proves the attempt).
    assert!(
        app.agent_answer.as_deref().unwrap_or("").contains("mars setup"),
        "ExplainThis did not auto-submit"
    );
    println!("[selfcheck] explain-this / failure .... PASS");

    // 26j. Pane resize changes the split ratio; zoom collapses then restores.
    let mut app = App::new(None)?;
    app.handle_key(kc(KeyCode::Char('\\')))?; // C-\ split right (2 panes)
    term.draw(|f| ui::render(f, &mut app))?;
    let two_rects = app.pane_rects.clone();
    assert_eq!(two_rects.len(), 2, "expected 2 panes");
    // Enter travel mode once; resize + zoom all stay in it (navigation stays).
    app.handle_key(kc(KeyCode::Char('t')))?; // travel mode
    app.handle_key(k(KeyCode::Char('>')))?; // grow focused pane
    app.handle_key(k(KeyCode::Char('>')))?;
    term.draw(|f| ui::render(f, &mut app))?;
    let grown = app.pane_rects.clone();
    let focused = app.focused_pane_id();
    let w_before = two_rects.iter().find(|(id, _)| *id == focused).unwrap().1.width;
    let w_after = grown.iter().find(|(id, _)| *id == focused).unwrap().1.width;
    assert!(w_after > w_before, "resize did not grow the focused pane");
    // Zoom: only the focused pane renders full-area; toggling restores 2.
    app.handle_key(k(KeyCode::Char('z')))?; // zoom (still in travel)
    term.draw(|f| ui::render(f, &mut app))?;
    assert_eq!(app.pane_rects.len(), 1, "zoom did not collapse to one pane");
    app.handle_key(k(KeyCode::Char('z')))?; // unzoom
    term.draw(|f| ui::render(f, &mut app))?;
    assert_eq!(app.pane_rects.len(), 2, "unzoom did not restore both panes");
    app.handle_key(k(KeyCode::Esc))?; // leave travel
    println!("[selfcheck] pane resize + zoom ........ PASS");

    // 26k. Terminal Ctrl+Space → the UNIFIED composer in one keystroke: Command
    //      mode (↑/↓ command menu) with the red inline overlay; `!` forces pure
    //      shell; with no agent key, Enter runs the typed command → terminal.
    let mut app = App::new(None)?;
    app.handle_key(kc(KeyCode::Char(' ')))?;
    app.handle_key(k(KeyCode::Char('!')))?; // open a terminal via bar `!`…
    typ(&mut app, "true")?;
    app.handle_key(k(KeyCode::Enter))?; // …now attached to a terminal pane
    assert!(app.mode == mode::Mode::Terminal, "not in a terminal");
    app.handle_key(kc(KeyCode::Char(' ')))?; // Ctrl+Space → the unified composer
    assert!(
        matches!(app.palette.as_ref().map(|p| &p.bar_mode), Some(palette::BarMode::Command)),
        "Ctrl+Space in terminal did not open the unified (Command) composer"
    );
    // REGISTRY-FIRST (2026-07 ruling, reversing the earlier shell-first one):
    // typing pre-selects the top match and Enter fires it — no arrowing.
    typ(&mut app, "split")?;
    assert!(
        app.palette.as_ref().map(|p| p.navigated).unwrap_or(false),
        "typing did not pre-select the top match"
    );
    let panes_before = app.tab().layout.pane_ids().len();
    app.handle_key(k(KeyCode::Enter))?;
    assert!(
        app.tab().layout.pane_ids().len() > panes_before,
        "Enter did not fire the pre-selected top match"
    );
    // Only a query NOTHING matches falls through to the shell (no key set →
    // runs literally in the pane).
    app.handle_key(kc(KeyCode::Char(' ')))?;
    typ(&mut app, "qqq")?;
    let no_match = app
        .palette
        .as_ref()
        .map(|p| p.visible_items(&app.frecency).is_empty())
        .unwrap_or(false);
    assert!(no_match, "'qqq' unexpectedly matched a registry row");
    app.handle_key(k(KeyCode::Enter))?;
    assert!(
        app.mode == mode::Mode::Terminal && app.palette.is_none(),
        "no-match Enter did not fall through to the shell"
    );
    // `!` still forces pure-shell mode.
    app.handle_key(kc(KeyCode::Char(' ')))?;
    app.handle_key(k(KeyCode::Char('!')))?;
    assert!(
        matches!(app.palette.as_ref().map(|p| &p.bar_mode), Some(palette::BarMode::Shell)),
        "`!` did not force pure-shell mode"
    );
    typ(&mut app, "echo composer_ok")?;
    app.handle_key(k(KeyCode::Enter))?; // no key → runs the command directly
    assert!(app.mode == mode::Mode::Terminal, "shell composer Enter did not run the command");
    println!("[selfcheck] terminal composer (unified) . PASS");

    // 26k2. In-bar quick keys: the bar_quick_key/legend tables must not drift
    //       from what the keys actually do; the terminal composer's unengaged
    //       empty-query Enter is a no-op (never fire a row the user can't see
    //       is selected — editor bars are menu-first and DO highlight row one);
    //       and an editor no-match query falls through to a natural-language ask.
    {
        let mut app = App::new(None)?;
        app.handle_key(kc(KeyCode::Char(' ')))?;
        app.handle_key(k(KeyCode::Char('!')))?;
        typ(&mut app, "true")?;
        app.handle_key(k(KeyCode::Enter))?; // attached to a terminal pane
        app.handle_key(kc(KeyCode::Char(' ')))?; // unified composer, unengaged
        app.handle_key(k(KeyCode::Enter))?; // empty query, nothing highlighted
        assert!(
            app.palette.is_some() && matches!(app.mode, mode::Mode::Bar),
            "empty-query Enter should be a no-op, not fire a row"
        );
        app.handle_key(k(KeyCode::Char('?')))?;
        assert!(
            matches!(app.palette.as_ref().map(|p| &p.bar_mode), Some(palette::BarMode::Ask)),
            "`?` did not switch to ask mode"
        );
        app.handle_key(kc(KeyCode::Char('g')))?;
        app.handle_key(kc(KeyCode::Char(' ')))?;
        app.handle_key(k(KeyCode::Char('@')))?;
        assert!(app.tree_open, "`@` did not open the navigator");
        assert_eq!(palette::bar_quick_key(&palette::Action::ToggleFileTree), Some('@'));
        assert_eq!(palette::bar_quick_key(&palette::Action::AskAgent), Some('?'));
        assert!(
            palette::bar_quick_legend().iter().any(|(key, _)| *key == "!"),
            "quick-key legend lost `!` shell"
        );

        let mut app = App::new(None)?;
        app.handle_key(kc(KeyCode::Char(' ')))?;
        typ(&mut app, "qqq")?;
        app.handle_key(k(KeyCode::Enter))?;
        assert!(
            matches!(app.palette.as_ref().map(|p| &p.bar_mode), Some(palette::BarMode::Ask)),
            "editor no-match Enter did not fall through to an ask"
        );
        assert!(
            app.agent_answer.as_deref().unwrap_or("").starts_with('⚠'),
            "hermetic ask fallback should surface the no-key notice"
        );
    }
    println!("[selfcheck] bar quick keys + fallbacks . PASS");

    // 26k3. Cursor-point generation: with no selection an editor ask targets an
    //       empty range at point, so a reply's code block INSERTS there — as one
    //       reversible undo step ("write a limerick about potatoes").
    {
        let mut app = App::new(None)?;
        typ(&mut app, "ab")?;
        let buf_id = match app.focused_pane().content {
            pane::PaneContent::Editor(id) => id,
            _ => panic!("scratch pane is not an editor"),
        };
        // The capture: a configured ask from an editor with no selection marks
        // the cursor as an empty target range (the request thread itself fails
        // fast against a closed port and is irrelevant here).
        std::env::set_var("MARS_LLM_KEY", "selfcheck");
        std::env::set_var("MARS_LLM_URL", "http://127.0.0.1:9/v1/chat/completions");
        app.handle_key(kc(KeyCode::Char(' ')))?;
        app.handle_key(k(KeyCode::Char('?')))?;
        typ(&mut app, "write a limerick about potatoes")?;
        app.handle_key(k(KeyCode::Enter))?;
        assert_eq!(
            app.refactor_target,
            Some((buf_id, 2, 2)),
            "no-selection ask did not target an empty range at the cursor"
        );
        std::env::remove_var("MARS_LLM_KEY");
        std::env::remove_var("MARS_LLM_URL");
        // The apply: an empty target range inserts (removes nothing), one undo
        // step reverts, and the confirm chip verb says "insert".
        app.refactor_target = Some((buf_id, 1, 1));
        app.refactor_replacement = Some("XY".into());
        term.draw(|f| ui::render(f, &mut app))?;
        assert!(
            screen_text(&term).contains("insert at the cursor"),
            "confirm chip did not say insert for an empty target range"
        );
        app.apply_refactor();
        let text = app.buffers.get(&buf_id).map(|b| b.rope.to_string()).unwrap_or_default();
        assert_eq!(text, "aXYb", "empty-range refactor did not insert at point");
        app.run_action(palette::Action::Undo);
        let text = app.buffers.get(&buf_id).map(|b| b.rope.to_string()).unwrap_or_default();
        assert_eq!(text, "ab", "cursor insertion was not one reversible undo step");
    }
    println!("[selfcheck] cursor-point generation .... PASS");

    // 26k4. The cursor-anchored composer yields to the dropdown: cursor at the
    //       top → both render; cursor pushed to the bottom rows the dropdown
    //       covers → the overlay is hidden, the menu stays readable.
    {
        let mut app = App::new(None)?;
        app.handle_key(kc(KeyCode::Char(' ')))?;
        app.handle_key(k(KeyCode::Char('!')))?;
        typ(&mut app, "true")?;
        app.handle_key(k(KeyCode::Enter))?; // fresh shell → cursor near the top
        term.draw(|f| ui::render(f, &mut app))?; // sizes the PTY to the pane
        let tid = match app.focused_pane().content {
            pane::PaneContent::Terminal(id) => id,
            _ => panic!("focused pane is not a terminal"),
        };
        app.handle_key(kc(KeyCode::Char(' ')))?; // unified composer
        term.draw(|f| ui::render(f, &mut app))?;
        let t = screen_text(&term);
        assert!(
            t.contains("run a command…"),
            "overlay missing though it does not overlap the dropdown"
        );
        // The in-bar quick keys are taught on the bar line (empty query only).
        assert!(
            t.contains("! shell") && t.contains("? ask") && t.contains("@ files"),
            "quick-key legend missing from the empty-query bar line"
        );
        app.handle_key(kc(KeyCode::Char('g')))?; // back to the terminal
        typ(&mut app, if shell_is_powershell() { "1..200" } else { "seq 1 200" })?;
        app.handle_key(k(KeyCode::Enter))?;
        let pushed = wait_until(|| {
            app.tick();
            app.terms.get(&tid).map(|t| t.screen().cursor_position().0 >= 25).unwrap_or(false)
        });
        assert!(pushed, "seq did not push the terminal cursor into the dropdown rows");
        app.handle_key(kc(KeyCode::Char(' ')))?;
        term.draw(|f| ui::render(f, &mut app))?;
        let t = screen_text(&term);
        assert!(
            !t.contains("run a command…"),
            "overlay drew on top of the dropdown instead of yielding"
        );
        assert!(t.contains("Navigator"), "dropdown missing while the overlay yielded");
        app.handle_key(kc(KeyCode::Char('g')))?;
    }
    println!("[selfcheck] overlay yields to dropdown . PASS");

    // 26k5. The ask/chat panel is bounded to the bottom ask_panel_max_pct of
    //       the workspace: a long transcript shows only its tail, older turns
    //       are reachable by scrolling (Up key and mouse wheel), and the
    //       "↑ more" marker teaches that.
    {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut app = App::new(None)?;
        app.handle_key(kc(KeyCode::Char(' ')))?;
        app.handle_key(k(KeyCode::Char('?')))?; // ask mode
        for i in 0..40 {
            app.agent_history.push(("user".into(), format!("question number {i}")));
            app.agent_history.push(("assistant".into(), format!("answer number {i}")));
        }
        term.draw(|f| ui::render(f, &mut app))?;
        let t = screen_text(&term);
        assert!(t.contains("answer number 39"), "panel not pinned to the newest turn");
        assert!(!t.contains("question number 0"), "80-turn transcript rendered unbounded");
        assert!(t.contains("more (Up to scroll)"), "scroll-up marker missing");
        // ≤ 30% of a ~37-row workspace is ~11 rows — far below the ~22 the old
        // 60% cap allowed. Count rendered turn prefixes to pin the bound.
        let turns = t.matches("you  ›").count() + t.matches("mars ›").count();
        assert!(
            (2..=13).contains(&turns),
            "ask panel height escaped the 30% cap ({turns} turns visible)"
        );
        // Wheel = the Up/Down keys; scrolling up reveals older turns.
        let wheel = |up: bool| MouseEvent {
            kind: if up { MouseEventKind::ScrollUp } else { MouseEventKind::ScrollDown },
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_mouse(wheel(true));
        assert_eq!(app.ask_scroll, app.tuning.wheel_scroll_lines, "wheel did not scroll the ask panel");
        for _ in 0..20 { app.handle_mouse(wheel(true)); }
        term.draw(|f| ui::render(f, &mut app))?;
        let t = screen_text(&term);
        assert!(
            t.contains("more (Down to scroll)"),
            "scrolled-up panel lost its way back down"
        );
        app.handle_mouse(wheel(false));
        assert!(app.ask_scroll < 21 * app.tuning.wheel_scroll_lines, "wheel down did not scroll back");
    }
    println!("[selfcheck] ask panel bounded+scrolls .. PASS");

    // 26l. W6 watch: watching a pane + a verdict event → a failure notice that
    //      renders and is dismissed with Esc.
    let mut app = App::new(None)?;
    app.handle_key(kc(KeyCode::Char(' ')))?;
    app.handle_key(k(KeyCode::Char('!')))?;
    typ(&mut app, "true")?;
    app.handle_key(k(KeyCode::Enter))?; // attached to a terminal pane
    let tid = match app.focused_pane().content {
        pane::PaneContent::Terminal(id) => id,
        _ => panic!("focused pane is not a terminal"),
    };
    app.run_action(palette::Action::WatchPane);
    assert!(app.watches.get(&tid).map(|w| w.watched).unwrap_or(false), "pane not marked watched");
    // Simulate the background summary landing (the hermetic auto-name pattern).
    app.agent_tx.send(agent::AgentEvent::WatchSummary { term_id: tid, verdict: "failed: linker error".into() })?;
    app.tick();
    assert_eq!(app.notices.len(), 1, "verdict did not queue a notice");
    assert!(matches!(app.notices[0].kind, app::NoticeKind::Failure), "verdict not classified as failure");
    term.draw(|f| ui::render(f, &mut app))?;
    assert!(screen_text(&term).contains("linker error"), "notice not rendered");
    app.mode = mode::Mode::Edit; // Esc is a shell key in terminal mode; dismiss from edit
    app.handle_key(k(KeyCode::Esc))?;
    assert!(app.notices.is_empty(), "Esc did not dismiss the notice");
    // A failed background call must not wedge the gate: BgDone always clears it.
    app.bg_busy = true;
    app.agent_tx.send(agent::AgentEvent::BgDone)?;
    app.tick();
    assert!(!app.bg_busy, "BgDone did not release the bg_busy gate");
    println!("[selfcheck] watch pane + notice (W6) ... PASS");

    // 26l2. The quiet-timer actually fires: an old last_output_tick + a zero
    //       threshold trips maybe_fire_watches (no key → sets `triggered`, no LLM).
    {
        let mut app = App::new(None)?;
        app.tuning.watch_quiet_secs = 0;
        app.watches.insert(4242, app::WatchState { watched: true, last_output_tick: 0, ..Default::default() });
        app.tick();
        assert!(app.watches.get(&4242).map(|w| w.triggered).unwrap_or(false),
            "quiet timer did not fire maybe_fire_watches");
    }
    println!("[selfcheck] watch quiet-timer fires .... PASS");

    // 26m. Away Digest (W7+): quiet when idle; a watched task finishing while
    //      detached yields ONE duration-anchored headline (the W6 notice it
    //      subsumes is deduped), and the digest view renders sections — all
    //      deterministic, no API key (broker-ready: only verdict TEXT is LLM-made).
    {
        let mut app = App::new(None)?;
        app.tuning.mission_briefing = 1; // this block pins the CLASSIC notice mode
        app.on_detach();
        app.on_attach();
        assert!(app.notices.is_empty(), "briefing appeared when nothing changed");
        // A watched run finishes while detached: tick processes the verdict
        // (W6 notice + away-log event), then reattach builds the headline.
        app.on_detach();
        app.watches.insert(7, app::WatchState { watched: true, run_started_tick: 1, ..Default::default() });
        for _ in 0..20 { app.frame_tick += 1; } // time passes while away
        app.agent_tx.send(agent::AgentEvent::WatchSummary { term_id: 7, verdict: "failed: tests red".into() })?;
        app.tick();
        app.on_attach();
        assert_eq!(app.notices.len(), 1, "expected exactly one briefing (W6 dupe not subsumed?)");
        let n = &app.notices[0];
        assert!(n.text.contains("while away") && n.text.contains("tests red"),
            "headline missing duration/verdict: {}", n.text);
        assert!(matches!(n.kind, app::NoticeKind::Failure), "failing briefing not a Failure");
        // The digest view: sectioned, with the run duration, rendered with no key.
        app.run_action(palette::Action::AwayDigest);
        let d = app.agent_history.last().map(|(_, t)| t.clone()).unwrap_or_default();
        assert!(d.contains("needs you") && d.contains("tests red") && d.contains("ran "),
            "digest sections/duration missing:\n{d}");
    }
    println!("[selfcheck] away digest (W7+) ......... PASS");

    // 26n. W4/W5: NEED: parses; the first NEED re-asks (not surfaced), a second
    //      (depth capped) is surfaced normally.
    {
        assert_eq!(
            agent::parse_directive("looking…\nNEED: scrollback").1,
            Some(agent::AgentDirective::Need(agent::NeedKind::Scrollback)),
            "NEED: scrollback did not parse"
        );
        assert_eq!(
            agent::parse_directive("NEED: tab api").1,
            Some(agent::AgentDirective::Need(agent::NeedKind::Tab("api".into()))),
            "NEED: tab did not parse"
        );
        let mut app = App::new(None)?;
        let base = app.agent_history.len();
        let need = || agent::AgentEvent::Answer {
            text: "need more".into(),
            directive: Some(agent::AgentDirective::Need(agent::NeedKind::Scrollback)),
        };
        app.agent_tx.send(need())?;
        app.tick(); // depth 0→1, re-asks (no key → no-op), NOT surfaced
        assert_eq!(app.agent_history.len(), base, "first NEED should not reach the transcript");
        app.agent_tx.send(need())?;
        app.tick(); // depth capped → surfaced as a normal answer
        assert_eq!(app.agent_history.len(), base + 1, "capped NEED should surface");
    }
    println!("[selfcheck] NEED: expansion (W4/W5) ... PASS");

    // 27. Session daemon: detach → state + shells survive → reattach; takeover;
    //     version handshake; quit removes the socket. Fully headless.
    {
        use std::io::{BufRead, BufReader};
        use crate::sys::control::Stream as UnixStream;

        let sname = format!("selfcheck-{}", std::process::id());
        let spath = session::socket_path(&sname)?;
        let sname2 = sname.clone();
        let server = std::thread::spawn(move || session::server_main(&sname2, None));

        // Wait for the daemon socket.
        let mut up = false;
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(30));
            if crate::sys::control::connect(&spath).is_ok() { up = true; break; }
        }
        assert!(up, "session server did not come up");

        // A persistent test client: one writer + one reader per connection
        // (mirrors the real client_main — never re-clone/drop per frame).
        // Output bytes are fed through a real ANSI parser (vt100 — the same
        // crate that renders terminal panes) so incremental cell diffs
        // (cursor repositions interleaved between changed characters) are
        // interpreted correctly instead of pattern-matched as raw bytes.
        struct TestClient {
            writer: UnixStream,
            reader: BufReader<UnixStream>,
            screen: vt100::Parser,
        }
        impl TestClient {
            fn connect(path: &std::path::Path, version: &str) -> Result<Self> {
                Self::connect_with_broker(path, version, None, None)
            }
            fn connect_with_broker(
                path: &std::path::Path,
                version: &str,
                broker_sock: Option<&str>,
                broker_capability: Option<&str>,
            ) -> Result<Self> {
                use anyhow::Context as _;
                let stream = crate::sys::control::connect(path).context("testclient: connect")?;
                let reader = BufReader::new(stream.try_clone().context("testclient: clone")?);
                let mut me = TestClient { writer: stream, reader, screen: vt100::Parser::new(30, 100, 0) };
                session::write_frame(&mut me.writer, &session::ClientFrame::Hello {
                    cols: 100, rows: 30, version: version.to_string(),
                    broker_sock: broker_sock.map(str::to_string),
                    broker_capability: broker_capability.map(str::to_string),
                })
                .context("testclient: hello")?;
                Ok(me)
            }
            fn key(&mut self, key: KeyEvent) -> Result<()> {
                session::write_frame(&mut self.writer, &session::ClientFrame::Key(key))
                    .map_err(Into::into)
            }
            fn text(&mut self, s: &str) -> Result<()> {
                for c in s.chars() {
                    self.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))?;
                }
                Ok(())
            }
            /// Read Output frames until `needle` appears in the interpreted
            /// screen contents (or an Exit arrives), within `secs`.
            fn read_until(&mut self, needle: &str, secs: u64) -> Result<(bool, Option<String>)> {
                self.read_until_matching(secs, |contents| contents.contains(needle))
            }
            fn read_until_line(&mut self, needle: &str, secs: u64) -> Result<(bool, Option<String>)> {
                self.read_until_matching(secs, |contents| {
                    contents.lines().any(|line| {
                        line.trim_matches(|c: char| c.is_whitespace() || c == '│') == needle
                    })
                })
            }
            fn read_until_matching(
                &mut self,
                secs: u64,
                found: impl Fn(&str) -> bool,
            ) -> Result<(bool, Option<String>)> {
                use anyhow::Context as _;
                use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
                self.reader
                    .get_ref()
                    .set_read_timeout(Some(std::time::Duration::from_millis(200)))
                    .context("testclient: set_read_timeout")?;
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
                while std::time::Instant::now() < deadline {
                    let mut line = String::new();
                    match self.reader.read_line(&mut line) {
                        Ok(0) => break,
                        Ok(_) => match serde_json::from_str::<session::ServerFrame>(line.trim()) {
                            Ok(session::ServerFrame::Output { b64 }) => {
                                if let Ok(bytes) = B64.decode(b64) {
                                    self.screen.process(&bytes);
                                }
                                if found(&self.screen.screen().contents()) {
                                    return Ok((true, None));
                                }
                            }
                            Ok(session::ServerFrame::Exit { message }) => {
                                return Ok((found(&self.screen.screen().contents()), Some(message)));
                            }
                            Ok(session::ServerFrame::Status { .. }) => {}
                            Ok(session::ServerFrame::BrokerRoute { .. }) => {}
                            Ok(session::ServerFrame::Board { .. }) => {}
                            Ok(session::ServerFrame::Briefing { .. }) => {}
                            Ok(session::ServerFrame::PaneScreen { .. }) => {}
                            Ok(session::ServerFrame::PaneOutput { .. }) => {}
                            Ok(session::ServerFrame::PaneHistory { .. }) => {}
                            Ok(session::ServerFrame::PaneLines { .. }) => {}
                            Err(_) => {}
                        },
                        Err(_) => {} // timeout tick — keep waiting until deadline
                    }
                }
                Ok((found(&self.screen.screen().contents()), None))
            }
        }

        // c1 attaches, types a marker, sees it rendered.
        let mut c1 = TestClient::connect(&spath, session::SESSION_PROTOCOL_VERSION)?;
        c1.text("sessionmarker")?;
        let (found, _) = c1.read_until("sessionmarker", 5)?;
        assert!(found, "marker not rendered to first client");

        // Version handshake: bogus client is refused, c1 unaffected.
        let mut c_bad = TestClient::connect(&spath, "0.0.0-bogus")?;
        let (_, exit) = c_bad.read_until("\u{0}never\u{0}", 3)?;
        assert!(
            exit.map(|m| m.contains("version mismatch")).unwrap_or(false),
            "version mismatch not refused"
        );
        let mut c_old = TestClient::connect(&spath, env!("CARGO_PKG_VERSION"))?;
        let (_, exit) = c_old.read_until("\u{0}never\u{0}", 3)?;
        assert!(
            exit.map(|m| m.contains("version mismatch")).unwrap_or(false),
            "pre-handoff session protocol was not refused"
        );
        assert!(session::client_exit_is_error("version mismatch: old server"));
        assert!(!session::client_exit_is_error("detached: another client attached"));

        // Takeover + reattach: c2 attaches → c1 is dropped, c2 gets a full
        // redraw that still contains the marker (state survived).
        let mut c2 = TestClient::connect_with_broker(
            &spath,
            session::SESSION_PROTOCOL_VERSION,
            Some("/tmp/mars-auth-cap-route-one.sock"),
            Some("11111111111111111111111111111111"),
        )?;
        let (_, c1_exit) = c1.read_until("\u{0}never\u{0}", 3)?;
        assert!(c1_exit.is_some(), "old client not notified on takeover");
        let (found2, _) = c2.read_until("sessionmarker", 5)?;
        assert!(found2, "state lost across reattach");
        #[cfg(windows)]
        std::thread::sleep(std::time::Duration::from_millis(2_100));
        c1.text("stale_writer")?;
        c2.text("fresh_writer")?;
        let (fresh, _) = c2.read_until("fresh_writer", 5)?;
        assert!(fresh, "active client input was not applied after takeover");
        assert!(
            !c2.screen.screen().contents().contains("stale_writer"),
            "detached client injected input after takeover"
        );
        #[cfg(feature = "ssh")]
        {
            assert_eq!(
                broker::detect_broker_sock().as_deref(),
                Some("/tmp/mars-auth-cap-route-one.sock"),
                "session daemon did not accept the attached client's broker route"
            );
            assert_eq!(
                broker::broker_capability_for("/tmp/mars-auth-cap-route-one.sock").as_deref(),
                Some("11111111111111111111111111111111"),
                "session daemon lost the attached client's broker capability"
            );
        }

        // Shell pane survives a hard disconnect: start one, run a command,
        // drop the client entirely, reconnect, and find the output.
        c2.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL))?;
        c2.key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE))?;
        c2.text("echo daemon_pty_ok")?;
        c2.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;
        let pty_started = std::time::Instant::now();
        let (pty_ok, pty_exit) = c2.read_until_line("daemon_pty_ok", 15)?;
        assert!(
            pty_ok,
            "shell output not rendered in session after {:?} (exit {pty_exit:?}): {:?}",
            pty_started.elapsed(),
            c2.screen.screen().contents()
        );
        drop(c2); // hard disconnect — no Detach, just gone
        std::thread::sleep(std::time::Duration::from_millis(150));
        let mut c3 = TestClient::connect_with_broker(
            &spath,
            session::SESSION_PROTOCOL_VERSION,
            Some("/tmp/mars-auth-cap-route-two.sock"),
            Some("22222222222222222222222222222222"),
        )?;
        // Reattach now always greets with the briefing overlay (iteration mode) —
        // dismiss it like a real user before reading the workspace underneath.
        c3.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))?;
        let (pty_survived, _) = c3.read_until_line("daemon_pty_ok", 5)?;
        assert!(pty_survived, "PTY did not survive the disconnect");
        #[cfg(feature = "ssh")]
        let session_instance_id = {
            assert_eq!(
                broker::detect_broker_sock().as_deref(),
                Some("/tmp/mars-auth-cap-route-two.sock"),
                "reattach did not replace the stale broker route"
            );
            assert_eq!(
                broker::broker_capability_for("/tmp/mars-auth-cap-route-two.sock").as_deref(),
                Some("22222222222222222222222222222222"),
                "reattach did not replace the stale broker capability"
            );
            let (nested_sock, nested_capability, nested_instance_id) =
                session::query_broker_route(&sname, None)?;
            assert_eq!(
                nested_sock.as_deref(),
                Some("/tmp/mars-auth-cap-route-two.sock"),
                "persistent PTYs cannot query the reattached broker route"
            );
            assert_eq!(
                nested_capability.as_deref(),
                Some("22222222222222222222222222222222"),
                "persistent PTYs cannot query the reattached broker capability"
            );
            nested_instance_id
        };

        // `mars ls` sees it, including the attached state (c3 is attached).
        assert!(
            session::list_sessions()?
                .iter()
                .any(|(n, alive, attached)| n == &sname && *alive && *attached),
            "ls missing the live+attached session"
        );

        // Live rename: the socket moves, the attached client keeps working.
        let renamed = format!("{sname}-renamed");
        let rpath = session::socket_path(&renamed)?;
        {
            let ctl = crate::sys::control::connect(&spath)?;
            let mut w = ctl.try_clone()?;
            session::write_frame(&mut w, &session::ClientFrame::Rename { to: renamed.clone() })?;
        }
        let mut moved = false;
        for _ in 0..40 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            if rpath.exists() && !spath.exists() { moved = true; break; }
        }
        assert!(moved, "session rename did not move the socket");
        assert!(
            session::list_sessions()?.iter().any(|(n, alive, _)| n == &renamed && *alive),
            "renamed session missing from ls"
        );
        #[cfg(feature = "ssh")]
        {
            let (nested_sock, nested_capability, renamed_instance_id) =
                session::query_broker_route(&sname, Some(&session_instance_id))?;
            assert_eq!(renamed_instance_id, session_instance_id);
            assert_eq!(
                nested_sock.as_deref(),
                Some("/tmp/mars-auth-cap-route-two.sock"),
                "renamed PTY lost the current broker route"
            );
            assert_eq!(
                nested_capability.as_deref(),
                Some("22222222222222222222222222222222"),
                "renamed PTY lost the current broker capability"
            );
        }
        // c3 (attached before the rename) still drives the session.
        c3.text("post-rename")?;
        let (still_alive, _) = c3.read_until("post-rename", 5)?;
        assert!(still_alive, "attached client broke across the rename");

        // Quit = detach: C-x C-c leaves the client but the session lives on
        // (no dirty guard — nothing is lost). Only `kill` ends it.
        c3.key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL))?; // detach PTY
        c3.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL))?;
        c3.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))?;
        let (_, quit_exit) = c3.read_until("\u{0}never\u{0}", 5)?;
        assert!(
            quit_exit.map(|m| m.contains("detached")).unwrap_or(false),
            "quit did not detach"
        );
        assert!(rpath.exists(), "quit killed the session instead of detaching");
        session::kill_main(&renamed)?;
        server.join().expect("server thread panicked")?;
        assert!(!rpath.exists(), "socket not removed after kill");
        println!("[selfcheck] session daemon ............ PASS");

        // 27a2. Mobile bridge: a Subscribe side-channel receives the structured
        //       board JSON WITHOUT taking over the attached desktop client (the
        //       non-takeover glance), and verdicts map to the contract strings.
        {
            // Read the next Board frame's parsed JSON payload, within `secs`.
            // The reader must carry a read timeout so read_line returns to check
            // the deadline between pushes.
            fn next_board(
                reader: &mut BufReader<UnixStream>,
                secs: u64,
            ) -> Option<serde_json::Value> {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
                while std::time::Instant::now() < deadline {
                    let mut line = String::new();
                    match reader.read_line(&mut line) {
                        Ok(0) => return None,
                        Ok(_) => {
                            if let Ok(session::ServerFrame::Board { json }) =
                                serde_json::from_str::<session::ServerFrame>(line.trim())
                            {
                                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
                                    return Some(v);
                                }
                            }
                        }
                        Err(_) => {} // timeout tick — keep waiting
                    }
                }
                None
            }

            let bname = format!("selfcheck-board-{}", std::process::id());
            let bpath = session::socket_path(&bname)?;
            let bname2 = bname.clone();
            let bserver = std::thread::spawn(move || session::server_main(&bname2, None));
            let mut up = false;
            for _ in 0..100 {
                std::thread::sleep(std::time::Duration::from_millis(30));
                if crate::sys::control::connect(&bpath).is_ok() { up = true; break; }
            }
            assert!(up, "board-test server did not come up");

            // The owning desktop client attaches and drives the session (scratch
            // editor tab → one workspace row).
            let mut owner = TestClient::connect(&bpath, session::SESSION_PROTOCOL_VERSION)?;
            owner.text("ownermarker")?;
            let (seen, _) = owner.read_until("ownermarker", 5)?;
            assert!(seen, "owner client not driving the board session");

            // A phone subscribes over the same socket — a read-only glance.
            let sub = crate::sys::control::connect(&bpath)?;
            let mut sub_w = sub.try_clone()?;
            session::write_frame(&mut sub_w, &session::ClientFrame::Subscribe)?;
            let read_half = sub.try_clone()?;
            read_half.set_read_timeout(Some(std::time::Duration::from_millis(200)))?;
            let mut sub_r = BufReader::new(read_half);

            // Task 1: a structured board frame arrives, with the right shape and
            // verdict strings.
            let board = next_board(&mut sub_r, 5).expect("no board frame from subscription");
            assert_eq!(
                board["session"], serde_json::json!(bname),
                "board session name mismatch: {board}"
            );
            let rows = board["rows"].as_array().expect("board rows not an array");
            assert!(!rows.is_empty(), "board carried no workspace rows: {board}");
            let allowed = ["failed", "blocked", "done", "running", "idle"];
            for row in rows {
                let v = row["verdict"].as_str().unwrap_or("");
                assert!(allowed.contains(&v), "board verdict '{v}' is not a contract state");
            }

            // Task 2: the glance is NON-TAKEOVER — the owning client is still
            // attached and still drives the session.
            owner.text("stillhere")?;
            let (still, ex) = owner.read_until("stillhere", 5)?;
            assert!(still, "subscribe kicked the owning client (must be non-takeover): {ex:?}");
            assert!(
                session::list_sessions()?
                    .iter()
                    .any(|(n, alive, attached)| n == &bname && *alive && *attached),
                "subscription must not clear the attached-client flag"
            );

            // Verdict mapping for a live, non-idle state: launch a shell command
            // via the unified composer and confirm the pane surfaces on the board
            // as verdict "running" — a `Verdict::Running → "running"` round trip
            // over the wire. (A nonzero exit maps to "failed", but only after the
            // pane's `watch_quiet_secs` window elapses — `pane_verdict` reports
            // Running until the auto-watch goes quiet — so it's the slow path, not
            // asserted here.)
            owner.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL))?; // open bar
            owner.key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE))?;    // shell composer
            owner.text("echo boardrun")?;
            owner.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;        // run it
            let mut saw_running = false;
            let mut last_board = serde_json::Value::Null;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            while std::time::Instant::now() < deadline {
                if let Some(b) = next_board(&mut sub_r, 2) {
                    if b["rows"]
                        .as_array()
                        .map(|rs| rs.iter().any(|r| r["verdict"] == "running"))
                        .unwrap_or(false)
                    {
                        saw_running = true;
                        break;
                    }
                    last_board = b;
                }
            }
            assert!(
                saw_running,
                "a live shell pane never surfaced as verdict 'running'; last board: {last_board}"
            );

            // Task 3: the transcript over the WIRE. `Term::lines` is covered above; this is the
            // frame path the phone actually uses — ClientFrame::PaneLines → SrvEvent →
            // ServerFrame::PaneLines — including that a window comes back at the ids asked for.
            {
                use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
                owner.key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL))?;
                owner.key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE))?;
                if shell_is_powershell() {
                    owner.text("1..200 | % { 'WIRE_{0:d3}' -f $_ }")?;
                } else {
                    owner.text("seq 1 200 | awk '{printf \"WIRE_%03d\\n\", $1}'")?;
                }
                owner.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))?;

                // A full transcript is a ~30 KB frame, and the 200 ms read timeout above tears
                // it mid-line — so accumulate across timeouts and only parse a COMPLETE line.
                // Board pushes keep arriving on the same stream, so this cannot assume the next
                // line is the reply.
                let _ = sub.try_clone()?.set_read_timeout(Some(std::time::Duration::from_millis(400)));
                let mut got: Option<(u64, u64, u64, Vec<String>)> = None;
                let mut buf = String::new();
                let mut next_ask = std::time::Instant::now();
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(25);
                while std::time::Instant::now() < deadline && got.is_none() {
                    if std::time::Instant::now() >= next_ask {
                        // Pane ids are not dense from 1 — sweep a small range and take whichever
                        // one answers with a transcript.
                        for pane in 0..=8usize {
                            session::write_frame(
                                &mut sub_w,
                                &session::ClientFrame::PaneLines { pane, from: 0, to: u64::MAX },
                            )?;
                        }
                        next_ask = std::time::Instant::now() + std::time::Duration::from_secs(2);
                    }
                    let _ = sub_r.read_line(&mut buf);
                    while let Some(nl) = buf.find('\n') {
                        let line: String = buf.drain(..=nl).collect();
                        if let Ok(session::ServerFrame::PaneLines { pane: _, from, first, total, rows }) =
                            serde_json::from_str::<session::ServerFrame>(line.trim())
                        {
                            if rows.len() >= 150 {
                                got = Some((from, first, total, rows));
                                break;
                            }
                        }
                    }
                }
                let (from, first, total, rows) = got.expect("no PaneLines frame carried a transcript");
                assert_eq!(from, first, "a full read must start at the retained floor");
                assert_eq!(total - first, rows.len() as u64, "total disagrees with the rows sent");
                let seen: Vec<usize> = rows
                    .iter()
                    .filter_map(|r| {
                        let bytes = B64.decode(r).ok()?;
                        let mut p = vt100::Parser::new(1, 400, 0);
                        p.process(&bytes);
                        let t = p.screen().contents();
                        t.trim().strip_prefix("WIRE_")?.parse().ok()
                    })
                    .collect();
                assert!(seen.len() >= 150, "wire transcript carried only {} lines", seen.len());
                assert!(
                    seen.windows(2).all(|w| w[1] == w[0] + 1),
                    "wire transcript duplicated, gapped or reordered: {:?}",
                    seen.windows(2).find(|w| w[1] != w[0] + 1)
                );
            }

            // Task 4: a session's IDENTITY survives a rename, and can be found by it.
            //
            // The bug this closes: `mars serve` captured a socket PATH at startup, the socket is
            // named after the session, so a rename (or a restart under a new name) left the bridge
            // forwarding an empty board forever — which reads as a broken phone app.
            {
                let (name0, id0, _) = session::identify(&bpath).expect("session did not identify");
                assert_eq!(name0, bname, "identify reported the wrong name");
                assert!(!id0.is_empty(), "session reported no instance id");

                // Found by id, at its current name.
                let (found_name, found_path) =
                    session::socket_for_instance(&id0).expect("instance not findable by id");
                assert_eq!(found_name, bname);
                assert_eq!(found_path, bpath);

                // Rename it, then find it again by the SAME id at its NEW name and path.
                // Short on purpose: the socket path (isolated temp dir + name) must stay under
                // macOS's ~104-char SUN_LEN, or the renamed socket cannot be connected to at all.
                let renamed = "rn0".to_string();
                session::rename_main(&bname, &renamed)?;
                let (new_name, new_path) = session::socket_for_instance(&id0)
                    .expect("instance id did not survive a rename — the whole point of it");
                assert_eq!(new_name, renamed, "id resolved to a stale name");
                assert_ne!(new_path, bpath, "socket path did not move with the rename");
                let (_, id_after, _) = session::identify(&new_path).expect("renamed session mute");
                assert_eq!(id_after, id0, "instance id changed across a rename");

                // A dead id resolves to nothing, so a bridge can refuse instead of serving air.
                assert!(session::socket_for_instance("deadbeef-0").is_none(),
                        "an unknown instance id must not resolve");
                assert!(session::socket_for_instance("").is_none(), "empty id must not resolve");

                // Put the name back so the teardown below still finds it.
                session::rename_main(&renamed, &bname)?;
            }
            drop(sub);
            session::kill_main(&bname)?;
            bserver.join().expect("board-test server panicked")?;
            assert!(!bpath.exists(), "board-test socket not removed after kill");
            println!("[selfcheck] mobile board subscribe ... PASS");
        }

        // 27b. Session management: Status reports detached; `kill` ends a
        //      session cleanly from outside.
        let kname = format!("selfcheck-kill-{}", std::process::id());
        let kpath = session::socket_path(&kname)?;
        let kname2 = kname.clone();
        let kserver = std::thread::spawn(move || session::server_main(&kname2, None));
        let mut up = false;
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(30));
            if crate::sys::control::connect(&kpath).is_ok() { up = true; break; }
        }
        assert!(up, "kill-test server did not come up");
        assert!(
            session::list_sessions()?
                .iter()
                .any(|(n, alive, attached)| n == &kname && *alive && !*attached),
            "fresh session should be alive and detached"
        );
        session::kill_main(&kname)?;
        kserver.join().expect("kill-test server panicked")?;
        assert!(!kpath.exists(), "socket not removed after kill");
        println!("[selfcheck] session status + kill ..... PASS");

        // 27b2. Quit = detach; kill is the deleting verb. In a session, Quit
        //       requests a detach and never ends the daemon; KillSession is the
        //       confirm-gated ender; `mars killall` sweeps every live daemon
        //       (under the suite's isolated runtime dir, never the user's).
        {
            let mut app = App::new(None)?;
            app.session_name = Some("some-session".into());
            app.run_action(palette::Action::Quit);
            assert!(app.detach_requested, "in-session Quit did not request a detach");
            assert!(!app.should_quit, "in-session Quit ended the session");
            app.detach_requested = false;
            app.run_action(palette::Action::KillSession);
            assert!(app.should_quit, "KillSession did not end a clean session");
            assert!(
                palette::Action::KillSession.is_destructive(),
                "KillSession must be confirm-gated for agent directives"
            );

            let names: Vec<String> =
                (0..2).map(|i| format!("selfcheck-ka{i}-{}", std::process::id())).collect();
            let mut servers = Vec::new();
            for n in &names {
                let n2 = n.clone();
                servers.push(std::thread::spawn(move || session::server_main(&n2, None)));
            }
            for n in &names {
                let p = session::socket_path(n)?;
                let mut up = false;
                for _ in 0..100 {
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    if crate::sys::control::connect(&p).is_ok() { up = true; break; }
                }
                assert!(up, "killall-test server '{n}' did not come up");
            }
            session::killall_main(false)?;
            for s in servers { s.join().expect("killall-test server panicked")?; }
            for n in &names {
                assert!(!session::socket_path(n)?.exists(), "killall left the socket for '{n}'");
            }
        }
        println!("[selfcheck] quit=detach + killall ..... PASS");
    }

    #[cfg(windows)]
    {
        use std::io::{BufRead as _, Write as _};
        use std::time::Duration;

        let force_sweep = crate::sys::proc::kill_all_mars_script(4242);
        assert!(
            force_sweep.contains("-Filter \"Name = 'mars.exe'\"")
                && force_sweep.contains("ProcessId -ne 4242")
                && !force_sweep.contains("CommandLine -like"),
            "Windows killall must target every other mars.exe by exact executable name"
        );

        let dir = std::env::temp_dir()
            .join(format!("mars-control-auth-sc-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let addr = dir.join("impostor.sock");
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let token = "0123456789abcdef0123456789abcdef";
        std::fs::write(&addr, format!("2 {port} {token}\n"))?;
        let impostor = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().expect("impostor accept");
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .expect("impostor timeout");
                let mut nonce = String::new();
                std::io::BufReader::new(stream.try_clone().expect("impostor clone"))
                    .read_line(&mut nonce)
                    .expect("impostor nonce");
                assert_eq!(nonce.trim_end().len(), 32);
                assert_ne!(nonce.trim_end(), token, "client disclosed the shared token");
                writeln!(stream, "{}", "0".repeat(64)).expect("impostor proof");
            }
        });
        assert!(
            crate::sys::control::connect(&addr).is_err(),
            "client accepted a server that could not prove the rendezvous token"
        );
        assert_eq!(
            crate::sys::control::probe(&addr),
            crate::sys::control::Probe::Dead,
            "authentication failure was not distinguished from an OS permission error"
        );
        impostor.join().expect("impostor thread");
        let _ = std::fs::remove_dir_all(&dir);
        println!("[selfcheck] control mutual auth ........ PASS");
    }

    // 27c. Auto session name is a lowest-free number; session AI-name applies
    //      only while numeric (explicit names win).
    assert!(
        session::next_auto_name()?.parse::<u32>().is_ok(),
        "auto session name should be numeric"
    );
    {
        let mut app = App::new(None)?;
        app.session_name = Some("0".into()); // numeric → a generated name may be SUGGESTED
        app.agent_tx.send(agent::AgentEvent::SessionName { name: "mars-dev".into() })?;
        app.tick();
        assert!(app.rename_session_to.is_none(), "session renamed without being accepted");
        assert_eq!(app.session_name_suggestion.as_deref(), Some("mars-dev"), "no suggestion offered");
        app.run_action(palette::Action::AcceptSessionName);
        assert_eq!(app.rename_session_to.as_deref(), Some("mars-dev"), "accept did not rename");
        let mut app = App::new(None)?;
        app.session_name = Some("work".into()); // explicit → AI name ignored
        app.agent_tx.send(agent::AgentEvent::SessionName { name: "auto".into() })?;
        app.tick();
        assert!(app.rename_session_to.is_none(), "explicit session name overridden by AI");
        let mut app = App::new(None)?;
        app.session_name = Some("0".into());
        app.agent_tx.send(agent::AgentEvent::SessionName { name: "CON".into() })?;
        app.tick();
        assert!(app.rename_session_to.is_none(), "reserved generated session name accepted");
        assert!(
            app.status_msg.as_deref().unwrap_or_default().contains("reserved by Windows"),
            "invalid generated session name was not surfaced"
        );
    }
    println!("[selfcheck] session auto-naming ....... PASS");

    for name in ["0", "mars-dev", "release 0.4", "alpha.beta"] {
        assert!(session::validate_session_name(name).is_ok(), "valid session name rejected: {name}");
    }
    for name in [
        "", ".", "..", "../escape", r"folder\name", "C:drive", "bad?name",
        "NUL", "con.txt", "COM1", "lpt9.log", "trail.", " padded",
    ] {
        assert!(
            session::validate_session_name(name).is_err(),
            "non-portable session name accepted: {name:?}"
        );
    }
    assert!(
        session::socket_path("runtime-probe")?.starts_with(cfg_dir.join("runtime")),
        "selfcheck session runtime escaped its isolated root"
    );
    let mut daemon_env = std::process::Command::new(std::env::current_exe()?);
    session::isolate_session_daemon_env(&mut daemon_env);
    for name in [
        "MARS_SESSION",
        "MARS_SESSION_ID",
        "MARS_AUTH_SOCK",
        "MARS_BROKER_CAPABILITY",
    ] {
        assert!(
            daemon_env
                .get_envs()
                .any(|(candidate, value)| candidate == std::ffi::OsStr::new(name) && value.is_none()),
            "nested session daemon still inherits parent route variable {name}"
        );
    }
    println!("[selfcheck] portable session names .... PASS");

    // 28. Config migration: a pre-rename ~/.config/ares is copied to mars/.
    {
        let mig_dir = std::env::temp_dir().join(format!("mars-migrate-{}", std::process::id()));
        let ares_dir = mig_dir.join("ares");
        std::fs::create_dir_all(&ares_dir)?;
        std::fs::write(
            ares_dir.join("keys.json"),
            r#"{ "edit": {}, "bar_open": ["ctrl-space", "M-x"] }"#,
        )?;
        std::fs::write(
            ares_dir.join("tuning.json"),
            r#"{ "max_panes": { "value": 3, "description": "migrated" } }"#,
        )?;
        std::env::set_var("XDG_CONFIG_HOME", &mig_dir);
        let app = App::new(None)?;
        assert_eq!(app.tuning.max_panes, 3, "ares→mars migration did not carry tuning");
        assert!(mig_dir.join("mars").join("keys.json").exists(), "keys.json not migrated");
        std::env::set_var("XDG_CONFIG_HOME", &cfg_dir); // back to the isolated dir
        let _ = std::fs::remove_dir_all(&mig_dir);
        println!("[selfcheck] ares→mars migration ....... PASS");
    }

    // 29. Provider detection (env-based, no network): the free tiers, the two new
    //     paid providers, and paid-first precedence.
    for v in ["MARS_LLM_KEY", "MARS_LLM_URL", "MARS_LLM_MODEL",
              "ARES_LLM_KEY", "ARES_LLM_URL", "ARES_LLM_MODEL", "MARS_AUTH_SOCK",
              "GROQ_API_KEY", "ANTHROPIC_API_KEY", "OPENAI_API_KEY",
              "AWS_BEARER_TOKEN_BEDROCK", "AZURE_OPENAI_API_KEY", "AZURE_OPENAI_ENDPOINT",
              "MARS_BEDROCK_REGION", "AWS_REGION", "AWS_DEFAULT_REGION",
              "MARS_AZURE_DEPLOYMENT", "MARS_AZURE_API_VERSION"] {
        std::env::remove_var(v);
    }
    std::env::set_var("GEMINI_API_KEY", "test-key");
    let cfg = agent::AgentConfig::from_env();
    assert!(cfg.is_configured(), "GEMINI_API_KEY not detected");
    assert_eq!(cfg.provider, "gemini");
    assert!(cfg.url.contains("generativelanguage"), "wrong Gemini endpoint: {}", cfg.url);
    assert!(cfg.model.starts_with("gemini"), "wrong Gemini model: {}", cfg.model);
    // OpenAI (OpenAI-compatible path).
    std::env::set_var("OPENAI_API_KEY", "test-key");
    let cfg = agent::AgentConfig::from_env();
    assert_eq!(cfg.provider, "openai", "OPENAI_API_KEY should beat GEMINI (paid-first)");
    assert!(cfg.url.contains("api.openai.com"), "wrong OpenAI endpoint: {}", cfg.url);
    assert!(cfg.model.starts_with("gpt-"), "wrong OpenAI default model: {}", cfg.model);
    // Anthropic (own Messages API) — highest of the named keys.
    std::env::set_var("ANTHROPIC_API_KEY", "test-key");
    let cfg = agent::AgentConfig::from_env();
    assert_eq!(cfg.provider, "anthropic", "ANTHROPIC should beat OPENAI (paid-first order)");
    assert!(cfg.url.contains("api.anthropic.com"), "wrong Anthropic endpoint: {}", cfg.url);
    assert!(cfg.model.contains("claude"), "wrong Claude default model: {}", cfg.model);
    // Azure OpenAI / Foundry: api-key + endpoint → a complete deployment URL.
    std::env::set_var("AZURE_OPENAI_API_KEY", "test-key");
    std::env::set_var("AZURE_OPENAI_ENDPOINT", "https://acme.openai.azure.com");
    std::env::set_var("MARS_AZURE_DEPLOYMENT", "gpt-4o");
    let cfg = agent::AgentConfig::from_env();
    assert_eq!(cfg.provider, "azure", "Azure creds should beat Anthropic (enterprise-first)");
    assert!(cfg.url.contains("/openai/deployments/gpt-4o/chat/completions"), "wrong Azure URL: {}", cfg.url);
    assert!(cfg.url.contains("api-version="), "Azure URL missing api-version: {}", cfg.url);
    assert_eq!(cfg.model, "gpt-4o", "Azure model should be the deployment name");
    // AWS Bedrock: bearer token + region → the Converse region base.
    std::env::set_var("AWS_BEARER_TOKEN_BEDROCK", "test-key");
    std::env::set_var("MARS_BEDROCK_REGION", "eu-west-1");
    let cfg = agent::AgentConfig::from_env();
    assert_eq!(cfg.provider, "bedrock", "Bedrock should beat Azure (chain order)");
    assert!(cfg.url.contains("bedrock-runtime.eu-west-1.amazonaws.com"), "wrong Bedrock URL: {}", cfg.url);
    assert!(cfg.model.contains("anthropic.claude"), "wrong Bedrock default model: {}", cfg.model);
    // The Converse body: system split out, content wrapped, inferenceConfig set.
    let body = agent::build_bedrock_body(
        &[serde_json::json!({"role":"system","content":"be terse"}),
          serde_json::json!({"role":"user","content":"hi"})],
        256, 0.3,
    );
    assert_eq!(body["system"][0]["text"], "be terse", "Bedrock system not split out");
    assert_eq!(body["messages"][0]["content"][0]["text"], "hi", "Bedrock content not wrapped");
    assert_eq!(body["inferenceConfig"]["maxTokens"], 256, "Bedrock inferenceConfig missing");
    assert!(body["messages"].as_array().unwrap().iter().all(|m| m["role"] != "system"),
        "Bedrock messages must not contain a system role");
    // Explicit MARS_LLM_KEY still overrides every provider, enterprise included.
    std::env::set_var("MARS_LLM_KEY", "test-key");
    assert_eq!(agent::AgentConfig::from_env().provider, "custom", "MARS_LLM_KEY must win");
    for v in ["GEMINI_API_KEY", "OPENAI_API_KEY", "ANTHROPIC_API_KEY", "MARS_LLM_KEY",
              "AWS_BEARER_TOKEN_BEDROCK", "AZURE_OPENAI_API_KEY", "AZURE_OPENAI_ENDPOINT",
              "MARS_BEDROCK_REGION", "MARS_AZURE_DEPLOYMENT"] {
        std::env::remove_var(v);
    }
    println!("[selfcheck] provider detection ........ PASS");

    // 29a. Failure taxonomy + retry backoff. None of the rotation logic had coverage, which is
    //      how a layering error (the broker returning above the retry loop) silently disabled
    //      auto-naming for three days. Pure classification — no network.
    {
        use agent::Failure;
        let c = |m: &str| agent::classify(&anyhow::anyhow!("{m}"));
        // A DNS failure reaching a provider is NOT that provider's fault. Reading it as one
        // burns a healthy credential and hides that this process cannot reach the network.
        assert_eq!(
            c("https://api.groq.com/v1/chat: Dns Failed: resolve dns name 'api.groq.com:443'"),
            Failure::Unreachable,
            "a DNS failure must be classified as unreachable, not as a provider fault"
        );
        assert_eq!(c("connection refused"), Failure::Unreachable);
        assert_eq!(c("operation timed out"), Failure::Unreachable);
        assert_eq!(c("Your credit balance is too low to access the Anthropic API"), Failure::Exhausted);
        assert_eq!(c("insufficient_quota"), Failure::Exhausted);
        assert_eq!(c("401 unauthorized"), Failure::Auth);
        assert_eq!(c("model 'nope' does not exist"), Failure::Fatal);
        // Everything except an outright bug is worth trying the next candidate for.
        assert!(Failure::Unreachable.recoverable() && Failure::Exhausted.recoverable());
        assert!(!Failure::Fatal.recoverable(), "a malformed request must not churn the ring");

        // Backoff: a failure costs a delay, never the feature. The old one-shot flag was set
        // BEFORE the call, so one blip retired naming for the life of the daemon.
        let poll = 50u64;
        let mut a = app::Attempt::default();
        assert!(a.ready(0), "a fresh attempt should be ready");
        a.dispatch(0, poll);
        assert!(!a.ready(1), "an in-flight attempt must not be dispatched again");
        a.failed(0, poll);
        assert!(!a.done, "one failure must not retire the task");
        assert!(!a.ready(10) && a.ready(60 * 1000 / poll), "first retry should wait ~60s");
        // Three backoff slots => three scheduled retries; the FOURTH loss goes dormant.
        let mut b = app::Attempt::default();
        for i in 0..3 {
            b.failed(0, poll);
            assert!(!b.done, "retired after only {} failures", i + 1);
        }
        b.failed(0, poll);
        assert!(b.done, "should go dormant once the backoff schedule is exhausted");
        let mut ok = app::Attempt::default();
        ok.succeeded();
        assert!(ok.done && !ok.ready(u64::MAX), "a success must settle the task");
    }
    println!("[selfcheck] llm failure taxonomy ...... PASS");

    // 29a2. The log must survive concurrent writers and record DECISIONS, not just calls.
    {
        assert!(
            llm_log::log_path().to_string_lossy().contains(&std::process::id().to_string()),
            "call log is not per-process; the daemon and keyd sharing one file tore records"
        );
        // Logging is off by default and this block runs before the suite turns it on — point
        // it at an isolated dir first, so a test never appends to the user's real capture.
        let early = cfg_dir.join("llm-early");
        std::env::set_var("MARS_LLM_LOG_DIR", &early);
        std::env::set_var("MARS_LLM_DEBUG", "1");
        llm_log::skipped("auto_name", "already_attempted");
        let joined: String = llm_log::log_files()
            .iter()
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("\"reason\":\"already_attempted\""),
            "a skipped background task left no trace — absence is not evidence"
        );
        for line in joined.lines().filter(|l| l.starts_with('{')) {
            assert!(
                serde_json::from_str::<serde_json::Value>(line).is_ok(),
                "torn log line: {}",
                &line[..line.len().min(80)]
            );
        }
        std::env::remove_var("MARS_LLM_DEBUG"); // back off; the later block enables its own
    }
    println!("[selfcheck] llm log integrity ......... PASS");

    // 29a3. The deterministic manager pass: reflex cards, briefing, timeline, index. No model,
    //       so every assertion here is about plumbing — which is exactly why this ships before
    //       any agent does: a bug found here can only be a plumbing bug.
    {
        use manager::{PaneRow, SessionSnap};
        let repo = cfg_dir.join("mgr");
        let pane = |pane_id: &str, name: &str, verdict: &str, why: &str, age: u64| PaneRow {
            id: pane_id.into(), pane_id: pane_id.into(), name: name.into(),
            verdict: verdict.into(), kind: "terminal".into(), why: why.into(),
            age_secs: age, blocked_prompt: (verdict == "blocked").then(|| why.to_string()),
        };
        let snap = |panes: Vec<PaneRow>| vec![SessionSnap {
            name: "0".into(), health: "up 3h · load 2.1".into(), panes,
        }];

        // T1 — a blocked pane and a failed pane each produce exactly one card.
        let s1 = snap(vec![
            pane("4", "claude", "blocked", "Edit src/session.rs? [y/N]", 20_400),
            pane("3", "sweep", "failed", "torchrun exited 1", 120),
            pane("9", "notes", "idle", "", 5),
        ]);
        let ev = manager::emit(&repo, "testbox", &s1, 1_000_000)?;
        assert_eq!(ev.len(), 2, "expected one event per needing-attention pane: {ev:?}");
        let feed = repo.join("sessions").join("0").join("cards");
        let cards = || -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(&feed).unwrap().flatten()
                .filter_map(|e| e.file_name().to_str().map(String::from))
                .filter(|n| n.starts_with("card-")).collect();
            v.sort(); v
        };
        assert_eq!(cards().len(), 2, "wrong card count: {:?}", cards());

        // The card must be a READABLE document even before it is a parsed one.
        let blocked_file = cards().into_iter().find(|n| n.contains("blocked")).expect("no blocked card");
        let body = std::fs::read_to_string(feed.join(&blocked_file))?;
        let (front, md) = manager::split_front(&body).expect("card has no frontmatter block");
        assert!(front.contains("severity: block"), "blocked card is not severity block");
        assert!(front.contains("source: reflex"), "card does not declare its provenance");
        assert!(front.contains("keys: \"y\\r\""), "blocked card carries no answer action: {front}");
        assert!(md.contains("Edit src/session.rs?"), "card body lost the prompt: {md}");

        // T2 — the SAME state again must not produce a second card. A pane blocked for six hours
        //      is one card, not one per tick; duplicate cards are the bug this project keeps
        //      re-learning one layer down.
        let ev2 = manager::emit(&repo, "testbox", &s1, 1_000_060)?;
        assert!(ev2.is_empty(), "re-emitted cards for unchanged state: {ev2:?}");
        assert_eq!(cards().len(), 2, "duplicate card written: {:?}", cards());

        // T3 — when the world moves on, the card expires. Declarative staleness: nothing
        //      re-examined the card, the condition simply stopped holding.
        let s2 = snap(vec![pane("4", "claude", "running", "", 3), pane("3", "sweep", "done", "", 9)]);
        let ev3 = manager::emit(&repo, "testbox", &s2, 1_000_120)?;
        assert_eq!(ev3.len(), 2, "expected two resolutions: {ev3:?}");
        for f in cards() {
            let t = std::fs::read_to_string(feed.join(&f))?;
            assert!(t.contains("expired: true"), "{f} did not expire when its pane moved on");
        }

        // T4 — the index is regenerated from the directory and ranks block above warn.
        let idx: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(repo.join("index.json"))?)?;
        assert_eq!(idx["cards"].as_array().map(|a| a.len()), Some(2), "index disagrees with disk");
        // The briefing rides INLINE per session — Rover reads one file, not N (single fs slot).
        let brief_inline = idx["sessions"][0]["briefing"].as_str().unwrap_or_default();
        assert!(brief_inline.contains("mission briefing"), "briefing not inlined per session: {idx}");
        let first = idx["cards"][0].as_object().expect("card entry");
        assert!(first.contains_key("body"), "card body not inlined: {first:?}");
        let s3 = snap(vec![
            pane("3", "sweep", "failed", "exited 1", 30),
            pane("4", "claude", "blocked", "continue? [y/N]", 90),
        ]);
        manager::emit(&repo, "testbox", &s3, 1_000_180)?;
        let idx: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(repo.join("index.json"))?)?;
        let live: Vec<&serde_json::Value> = idx["cards"].as_array().unwrap().iter()
            .filter(|c| !c["expired"].as_bool().unwrap_or(false)).collect();
        assert_eq!(live[0]["severity"], "block", "index does not rank block first: {idx}");

        // T5 — session → workspaces → summary, as markdown a human can read.
        let bpath = repo.join("sessions").join("0").join("mission_briefing.md");
        let brief = std::fs::read_to_string(&bpath)?;
        assert!(brief.contains("2 need you"), "briefing does not lead with what needs attention: {brief}");
        assert!(brief.contains("| workspace | state | age |"), "briefing lost its workspace table");
        // …and when nothing is wrong it says so, rather than padding.
        manager::emit(&repo, "testbox", &snap(vec![pane("9", "notes", "idle", "", 5)]), 1_000_240)?;
        assert!(std::fs::read_to_string(&bpath)?.contains("Nothing needs you"), "quiet briefing is not calm");

        // T5b — the guide the agent reads is scaffolded, and is NEVER overwritten afterwards.
        for f in ["AGENTS.md", "docs/layout.md", "docs/cards.md", "docs/memory.md",
                  "docs/tools.md", "policy.md", "memory/beliefs.md", ".gitignore"] {
            assert!(repo.join(f).exists(), "{f} not scaffolded");
        }
        std::fs::write(repo.join("AGENTS.md"), "EDITED BY HUMAN\n")?;
        std::fs::write(repo.join("memory/beliefs.md"), "MY BELIEFS\n")?;
        manager::emit(&repo, "testbox", &snap(vec![]), 1_000_300)?;
        assert_eq!(std::fs::read_to_string(repo.join("AGENTS.md"))?, "EDITED BY HUMAN\n",
                   "AGENTS.md was clobbered — the human owns it");
        assert_eq!(std::fs::read_to_string(repo.join("memory/beliefs.md"))?, "MY BELIEFS\n",
                   "memory/ was clobbered — the AGENT owns it");
        // The human-readable status index exists and points into the hierarchy.
        let status = std::fs::read_to_string(repo.join("index.md"))?;
        assert!(status.contains("sessions/0/mission_briefing.md"), "index.md does not link the tree: {status}");

        // T6 — the timeline is append-only, date-ordered and readable with no tooling.
        let tl = std::fs::read_to_string(repo.join("timeline.md"))?;
        assert!(repo.join("sessions/0/timeline.md").exists(), "no per-session timeline");
        let stamps: Vec<&str> = tl.lines().filter(|l| l.starts_with("- `"))
            .filter_map(|l| l.split('`').nth(1)).collect();
        assert!(stamps.len() >= 4, "timeline too short: {tl}");
        assert!(stamps.windows(2).all(|w| w[0] <= w[1]), "timeline is not date-ordered: {stamps:?}");
        assert!(tl.starts_with("# Timeline"), "timeline has no header");
    }
    println!("[selfcheck] manager reflex pass ....... PASS");

    // 29a4. The daemon writes the manager repo BY ITSELF. `mars snapshot` existing is not the
    //       feature; the feature is that nobody has to run it. Drives the real tick with the real
    //       board JSON, and asserts the tree appears and prunes.
    {
        let repo = cfg_dir.join("mgr-auto");
        let mut app = App::new(None)?;
        app.session_name = Some("auto".into());
        app.open_terminal();
        // Exactly the bytes the phone is sent — so the manager can never describe a world Rover
        // does not show.
        let board = app.mobile_board_json();
        for i in 0..4u64 {
            manager::tick_session(&repo, "testbox", &board, 2_000_000 + i * 60, 2)?;
        }
        let sdir = repo.join("sessions").join("auto");
        assert!(sdir.join("mission_briefing.md").exists(), "daemon tick wrote no briefing");
        assert!(repo.join("index.json").exists(), "daemon tick wrote no index");
        assert!(repo.join("AGENTS.md").exists(), "daemon tick did not scaffold the guide");
        let snaps = std::fs::read_dir(sdir.join("snapshots"))?.flatten().count();
        assert_eq!(snaps, 2, "stimuli not pruned to manager_snapshot_keep: {snaps}");
        // The index must describe the whole TREE, so a second session's daemon does not erase
        // the first from it.
        manager::tick_session(&repo, "testbox", &app.mobile_board_json().replace("\"auto\"", "\"other\""), 2_000_300, 2)?;
        let idx: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(repo.join("index.json"))?)?;
        let names: Vec<&str> =
            idx["sessions"].as_array().unwrap().iter().filter_map(|s| s["name"].as_str()).collect();
        assert!(names.contains(&"auto") && names.contains(&"other"),
                "index built from one process's view, not the tree: {names:?}");
        // A tick with the feature off must write nothing new.
        let before = std::fs::read_to_string(repo.join("index.json"))?;
        assert!(!before.is_empty());
    }
        // The repo must follow the suite's ISOLATION. A run that isolates its sockets but writes
        // cards into the user's real ~/.mars/manager is worse than one that isolates nothing —
        // this suite did exactly that and left a dozen throwaway sessions in a live repo.
        assert!(manager::repo_dir().is_some_and(|d| d.starts_with(&cfg_dir)),
                "manager repo escaped the isolated runtime dir: {:?}", manager::repo_dir());
    println!("[selfcheck] manager auto-tick ......... PASS");

    // 29a5. Naming RECOMMENDS, never renames. Auto-applying a generated session name moved the
    //       session's socket (it is named after the session) out from under the Rover bridge, the
    //       launchd agent and the manager repo — a helpful rename presenting as an outage.
    {
        let mut app = App::new(None)?;
        app.session_name = Some("0".into());
        let tab_id = app.tab().id;

        // A generated name arrives → it is a SUGGESTION; nothing is renamed.
        app.agent_tx.send(agent::AgentEvent::SessionName { name: "sweep-tuning".into() })?;
        app.agent_tx.send(agent::AgentEvent::AutoName { tab_id, name: "trainer".into() })?;
        app.tick();
        assert_eq!(app.session_name.as_deref(), Some("0"), "session was renamed automatically");
        assert!(app.rename_session_to.is_none(), "a rename was queued without being accepted");
        assert_eq!(app.session_name_suggestion.as_deref(), Some("sweep-tuning"));
        assert_eq!(app.tab_name_suggestion.get(&tab_id).map(String::as_str), Some("trainer"));
        assert!(app.tabs.iter().all(|t| t.name.chars().all(|c| c.is_ascii_digit())),
                "tab was renamed automatically");

        // …and it is offered at the TOP of the command bar.
        app.handle_key(kc(KeyCode::Char(' ')))?; // open the bar
        let rows = app.bar_rows();
        assert!(rows.len() >= 2, "suggestions not surfaced: {}", rows.len());
        assert!(rows[0].label.contains("sweep-tuning"), "session suggestion not first: {}", rows[0].label);
        assert!(rows[1].label.contains("trainer"), "tab suggestion not second: {}", rows[1].label);
        assert!(matches!(rows[0].kind, palette::ItemKind::Run(palette::Action::AcceptSessionName)));

        // A suggestion must never shoulder aside what the user typed.
        typ(&mut app, "spl")?;
        assert!(!app.bar_rows().first().is_some_and(|r| r.label.contains("sweep-tuning")),
                "suggestion outranked a typed query");
        app.handle_key(k(KeyCode::Esc))?;
        app.handle_key(k(KeyCode::Esc))?; // clear the query, then close

        // Accepting is what renames — and it consumes the suggestion.
        app.run_action(palette::Action::AcceptSessionName);
        assert_eq!(app.rename_session_to.as_deref(), Some("sweep-tuning"), "accept did not rename");
        assert!(app.session_name_suggestion.is_none(), "suggestion survived being accepted");
        app.run_action(palette::Action::AcceptTabName);
        assert_eq!(app.tab().name, "trainer", "accept did not rename the tab");
        assert!(app.tab_name_suggestion.is_empty(), "tab suggestion survived");

        // A name the user chose is never second-guessed.
        let mut app2 = App::new(None)?;
        app2.session_name = Some("my-session".into());
        app2.agent_tx.send(agent::AgentEvent::SessionName { name: "guessed".into() })?;
        app2.tick();
        assert!(app2.session_name_suggestion.is_none(), "suggested a name over a user's own");
    }
    println!("[selfcheck] naming recommends only ... PASS");

    // 29b. Cascade: one-tier-up escalation + same-tier rotation targets (pure
    //      logic, no network — the HTTP paths are exercised by the live eval).
    {
        for v in ["MARS_LLM_MODEL", "ARES_LLM_MODEL", "MARS_LLM_URL", "ARES_LLM_URL",
                  "MARS_LLM_KEY", "ARES_LLM_KEY"] {
            std::env::remove_var(v);
        }
        assert_eq!(
            tiers::model_above("groq", "translate").as_deref(),
            Some("llama-3.3-70b-versatile"),
            "mid-tier translate must escalate to groq high"
        );
        assert_eq!(
            tiers::model_above("openai", "auto_name").as_deref(),
            Some("gpt-4o"),
            "escalation must walk past a tier repointed to the same model"
        );
        assert_eq!(tiers::model_above("groq", "ask"), None, "top tier must not escalate");
        assert_eq!(tiers::model_above("groq", "no_such_task"), None);
        std::env::set_var("MARS_LLM_MODEL", "pinned");
        assert_eq!(tiers::model_above("groq", "translate"), None, "pin disables escalation");
        std::env::remove_var("MARS_LLM_MODEL");
        // The escalated retry is logged as `ask_escalated` — unmapped in the
        // ring, so the pinned higher model must pass through model_for untouched.
        assert_eq!(tiers::model_for("groq", "ask_escalated", "escalated-model"), "escalated-model");

        std::env::set_var("GROQ_API_KEY", "test-key");
        std::env::set_var("GEMINI_API_KEY", "test-key");
        let alts = agent::rotation_candidates("groq");
        assert_eq!(alts.len(), 1, "expected exactly one alternate");
        assert_eq!(alts[0].provider, "gemini");
        assert_eq!(agent::rotation_candidates("gemini")[0].provider, "groq");
        // Enterprise providers rotate too: a throttled Bedrock falls to the
        // consumer keys, and its own tier table routes by task.
        std::env::set_var("AWS_BEARER_TOKEN_BEDROCK", "test-key");
        assert!(agent::rotation_candidates("bedrock").iter().any(|c| c.provider == "gemini"),
            "Bedrock should rotate to a consumer key when throttled");
        assert_eq!(tiers::model_for("bedrock", "ask", "x"),
            "us.anthropic.claude-opus-4-20250514-v1:0", "Bedrock ask must route to the high tier");
        assert_eq!(tiers::model_for("bedrock", "auto_name", "x"),
            "us.anthropic.claude-3-5-haiku-20241022-v1:0", "Bedrock naming must route to low");
        // Azure has no tier table: model_for falls through to the deployment.
        assert_eq!(tiers::model_for("azure", "ask", "my-deployment"), "my-deployment",
            "Azure should fall through to the configured deployment");
        std::env::remove_var("AWS_BEARER_TOKEN_BEDROCK");
        std::env::set_var("MARS_LLM_MODEL", "pinned");
        assert!(agent::rotation_candidates("groq").is_empty(), "pin disables rotation");
        std::env::remove_var("MARS_LLM_MODEL");
        std::env::set_var("MARS_LLM_KEY", "test-key");
        assert!(agent::rotation_candidates("custom").is_empty(), "custom key never rotates away");
        for v in ["GROQ_API_KEY", "GEMINI_API_KEY", "MARS_LLM_KEY"] {
            std::env::remove_var(v);
        }
        // A 429 is typed so the rotation loop can tell throttling from real failures.
        let e = anyhow::Error::new(agent::RateLimited("throttled".into()));
        assert!(e.downcast_ref::<agent::RateLimited>().is_some());
        // A retired model (404 / "does not exist") is ALSO typed and recoverable —
        // the class of failure that silently froze every daemon task for days.
        let e = anyhow::Error::new(agent::ModelUnavailable("gone".into()));
        assert!(e.downcast_ref::<agent::ModelUnavailable>().is_some());
        assert!(agent::is_retired_model(404, None), "404 = retired");
        assert!(agent::is_retired_model(400, Some("The model foo does not exist")),
            "'does not exist' body = retired");
        assert!(!agent::is_retired_model(429, Some("rate limit reached")), "429 is not a retired model");
        // Back-compat: a tiers.json written in the old single-string format still
        // loads (deserializes each tier value as a one-element list).
        let old = r#"{"task_tier":{"ask":"high"},"tiers":{"groq":{"high":"llama-3.3-70b-versatile"}}}"#;
        let parsed: tiers::Tiers = serde_json::from_str(old).expect("old single-model format must still parse");
        assert_eq!(parsed.tiers["groq"]["high"], vec!["llama-3.3-70b-versatile".to_string()]);
        println!("[selfcheck] cascade rotate+escalate .. PASS");
    }

    // 29c. Memory hygiene: redaction before prompt injection, denylist, and
    //      recency/cwd-weighted memory ranking (memory builds only).
    #[cfg(feature = "memory")]
    {
        use retrieval::redact;
        // Credential prefixes are scrubbed; short lookalikes and prose survive.
        let r = redact("export ANTHROPIC_API_KEY=sk-ant-api03-abcdefghij0123456789XYZ");
        assert!(r.contains("[REDACTED]") && !r.contains("sk-ant"), "provider key survived: {r}");
        assert_eq!(redact("a risk-free plan"), "a risk-free plan", "prose false positive");
        assert_eq!(redact("sk-12"), "sk-12", "short token wrongly redacted");
        // Assignment values are scrubbed, the command shape kept.
        let r = redact("mysql -u root --password=hunter2 db");
        assert!(r.contains("--password=[REDACTED]") && !r.contains("hunter2"), "{r}");
        let r = redact("curl -H 'Authorization: Bearer abc123def456' api");
        assert!(r.contains("Bearer [REDACTED]") && !r.contains("abc123"), "{r}");
        // The new enterprise creds never ride into a prompt.
        let r = redact("export AWS_BEARER_TOKEN_BEDROCK=ABSKQmVkcm9ja0FQSUtleTEyMzQ1Njc4OTA");
        assert!(r.contains("[REDACTED]") && !r.contains("ABSKQmVkcm9ja0FQSUtleT"), "bedrock key survived: {r}");
        let r = redact("curl -H 'api-key: 0123456789abcdef0123456789abcdef' azure");
        assert!(r.contains("[REDACTED]") && !r.contains("0123456789abcdef0123"), "azure key survived: {r}");
        // URL credentials: password goes, user and host stay.
        let r = redact("git clone https://bob:s3cret@github.com/x.git");
        assert!(r.contains("bob:[REDACTED]@github.com") && !r.contains("s3cret"), "{r}");
        // Denylist: literal strings force-redacted; comments ignored.
        let dl = std::env::temp_dir().join(format!("mars-denylist-{}", std::process::id()));
        std::fs::write(&dl, "# comment\nmy-secret-host.internal\n")?;
        std::env::set_var("MARS_DENYLIST", &dl);
        let r = redact("ssh my-secret-host.internal");
        assert!(!r.contains("my-secret-host") && r.contains("[REDACTED]"), "{r}");
        assert_eq!(redact("# comment"), "# comment", "denylist comment line applied");
        std::env::remove_var("MARS_DENYLIST");
        let _ = std::fs::remove_file(&dl);

        // Weighted memory rank: lexical ties break toward same-cwd and recent;
        // metadata-free records (seeded eval stores) rank purely lexically; a
        // zero-score record is never resurrected by boosts.
        let mem = |req: &str, cmd: &str, ts: u64, cwd: &str| retrieval::CommandMemory {
            request: req.into(), command: cmd.into(), ts, session: String::new(), cwd: cwd.into(),
        };
        let now = 1_800_000_000u64;
        let records = vec![
            mem("run the tests", "npm test", now - 90 * 86_400, "/other"),
            mem("run the tests", "cargo test", now - 3_600, "/proj"),
            mem("deploy the site", "make deploy", now, "/proj"),
        ];
        let top = retrieval::rank_memories(&records, "run the tests", 2, "/proj", now, 0.25, 0.15, 14.0);
        assert_eq!(top[0], 1, "same-cwd + recent must win the lexical tie");
        assert_eq!(top[1], 0);
        assert!(!top.contains(&2), "lexically-irrelevant record resurrected by boost");
        let bare = vec![
            mem("run the tests", "npm test", 0, ""),
            mem("run the tests", "cargo test", 0, ""),
        ];
        let top = retrieval::rank_memories(&bare, "run the tests", 2, "/proj", now, 0.25, 0.15, 14.0);
        assert_eq!(top[0], 0, "metadata-free records must keep pure lexical order");

        // The facade gates on MARS_MEMORY internally (what the stub mirrors).
        std::env::remove_var("MARS_MEMORY");
        assert_eq!(retrieval::fewshot_for("run the tests"), "", "fewshot must gate on mode");
        assert!(retrieval::docs_context_for("how do I").is_none(), "docs must gate on mode");
        std::env::set_var("MARS_MEMORY", "docs");
        assert!(
            retrieval::docs_context_for("how do I turn on memory retrieval").is_some(),
            "docs mode must retrieve from the always-present reference corpus"
        );
        let cm = std::env::temp_dir().join(format!("mars-cm-{}", std::process::id()));
        std::fs::write(&cm, "{\"request\":\"ship it\",\"command\":\"cargo publish\"}\n")?;
        std::env::set_var("MARS_CMD_MEMORY", &cm);
        std::env::set_var("MARS_MEMORY", "history");
        assert!(
            retrieval::fewshot_for("ship it").contains("cargo publish"),
            "history mode must surface the seeded pair"
        );
        for v in ["MARS_MEMORY", "MARS_CMD_MEMORY"] {
            std::env::remove_var(v);
        }
        let _ = std::fs::remove_file(&cm);
        println!("[selfcheck] memory hygiene ........... PASS");
    }

    // 29f. Streaming: the incremental reasoning guard never leaks <think>
    //      content (even split across chunk boundaries) and never retracts
    //      emitted text; the AnswerStart/Delta/Answer event flow renders a
    //      live partial turn and resolves into ordinary history.
    {
        let chunks = ["Hel", "lo <thi", "nk>secret reasoning</th", "ink> world"];
        let mut raw = String::new();
        let mut emitted = 0usize;
        let mut seen = String::new();
        for c in chunks {
            raw.push_str(c);
            let vis = agent::stream_visible(&raw);
            assert!(vis.len() >= emitted, "visible prefix retracted at {c:?}");
            assert!(vis.starts_with(&seen), "emitted text not a stable prefix");
            if vis.len() > emitted {
                seen.push_str(&vis[emitted..]);
                emitted = vis.len();
            }
            assert!(!seen.contains("secret"), "reasoning leaked mid-stream: {seen}");
        }
        assert_eq!(seen, "Hello  world", "final streamed text wrong: {seen:?}");

        let mut app = App::new(None)?;
        app.agent_pending = true;
        app.agent_tx.send(agent::AgentEvent::AnswerStart)?;
        app.agent_tx.send(agent::AgentEvent::AnswerDelta { text: "streaming ".into() })?;
        app.agent_tx.send(agent::AgentEvent::AnswerDelta { text: "tokens".into() })?;
        app.tick();
        assert_eq!(app.agent_partial.as_deref(), Some("streaming tokens"));
        // An escalation retry starts a fresh stream — the partial resets.
        app.agent_tx.send(agent::AgentEvent::AnswerStart)?;
        app.agent_tx.send(agent::AgentEvent::AnswerDelta { text: "better".into() })?;
        app.agent_tx.send(agent::AgentEvent::Answer { text: "better answer".into(), directive: None })?;
        app.tick();
        assert!(app.agent_partial.is_none(), "final Answer did not clear the partial");
        assert!(!app.agent_pending, "final Answer left the spinner on");
        assert_eq!(
            app.agent_history.last().map(|(r, t)| (r.as_str(), t.as_str())),
            Some(("assistant", "better answer")),
            "streamed turn did not land in history"
        );
        println!("[selfcheck] streaming ask ............ PASS");
    }

    // 29h. Prompt templates (src/prompts/*.md, compile-time embedded): every
    //      template is non-empty and still carries the placeholders its call
    //      site substitutes — an edited .md can't silently break assembly.
    {
        for (name, p, holders) in [
            ("ask_system", prompts::ASK_SYSTEM, vec!["{registry}", "{screen}"]),
            ("translate_system", prompts::TRANSLATE_SYSTEM, vec!["{reasoning_cap}", "{examples_block}"]),
            ("translate_reasoning_cap", prompts::TRANSLATE_REASONING_CAP, vec![]),
            ("translate_examples", prompts::TRANSLATE_EXAMPLES, vec!["{examples}"]),
            ("watch_system", prompts::WATCH_SYSTEM, vec!["{hint}"]),
            ("watch_hint_exit", prompts::WATCH_HINT_EXIT, vec!["{code}"]),
            ("watch_hint_quiet", prompts::WATCH_HINT_QUIET, vec!["{secs}"]),
            ("mission_system", prompts::MISSION_SYSTEM, vec![]),
            ("auto_name_system", prompts::AUTO_NAME_SYSTEM, vec![]),
            ("name_session_system", prompts::NAME_SESSION_SYSTEM, vec![]),
            #[cfg(feature = "memory")]
            ("docs_context_preamble", prompts::DOCS_CONTEXT_PREAMBLE, vec!["{body}"]),
            ("cursor_insert", prompts::CURSOR_INSERT, vec!["{file}", "{line}", "{lang}"]),
            ("explain_this", prompts::EXPLAIN_THIS, vec![]),
            ("explain_failure", prompts::EXPLAIN_FAILURE, vec![]),
            ("persona_preamble", prompts::PERSONA_PREAMBLE, vec![]),
            ("persona_default", prompts::PERSONA_DEFAULT, vec![]),
            ("shift_brief", prompts::SHIFT_BRIEF, vec!["{away}", "{mission}", "{prev}", "{evidence}"]),
            ("capture_goals", prompts::CAPTURE_GOALS, vec!["{evidence}"]),
            ("rover_summary", prompts::ROVER_SUMMARY, vec![]),
            ("rover_brief", prompts::ROVER_BRIEF, vec!["{mission}", "{prev}", "{greeting}", "{summaries}"]),
        ] {
            assert!(!p.trim().is_empty(), "prompt template {name}.md is empty");
            for h in holders {
                assert!(p.contains(h), "prompt template {name}.md lost placeholder {h}");
            }
        }
        // The naming task tags must match the ring's keys, or tier routing
        // silently skips them (the bug this refactor caught).
        assert_eq!(tiers::model_for("groq", "auto_name", "x"), "llama-3.1-8b-instant");
        assert_eq!(tiers::model_for("groq", "name_session", "x"), "llama-3.1-8b-instant");
        assert_eq!(tiers::model_for("groq", "mission", "x"), "llama-3.1-8b-instant");
        // EVERY call-site tag routes: agent::TASKS ⊆ the default tier map, so a
        // tag rename can't silently fall through to the provider default model
        // again (the "shell"/"translate" bug, fixed 0.4).
        let ring = tiers::Tiers::default();
        for t in agent::TASKS {
            assert!(ring.task_tier.contains_key(*t), "task tag {t:?} is unmapped in tiers defaults");
        }
        assert_eq!(tiers::model_for("groq", "translate", "x"), "qwen/qwen3.6-27b",
            "translate must route to the head of the mid tier");
        // In-tier fallback: each tier is a LIST, so a retired head has a live
        // successor before the call ever leaves the provider.
        assert!(tiers::models_for("groq", "translate", "x").len() >= 2,
            "groq mid tier must list fallback models, not a single point of failure");
        assert_eq!(tiers::models_for("groq", "translate", "x")[0], "qwen/qwen3.6-27b");
        println!("[selfcheck] prompt templates ......... PASS");
    }

    // 29h2. Persona seam: the user's style file rides only into VOICE tasks
    //       (ask, watch) as the FINAL system message — hot-read, capped,
    //       redacted. FORMAT tasks (translate, naming, mission) never see it;
    //       an empty file is the kill switch; no file means the shipped voice.
    {
        let pf = std::env::temp_dir().join(format!("mars-persona-{}", std::process::id()));
        let _ = std::fs::remove_file(&pf);
        std::env::set_var("MARS_PERSONA", &pf);
        // No file → shipped default voice, preamble first, last system message.
        let msgs = agent::build_ask_messages("reg", "scr", &[], "q");
        assert_eq!(msgs.len(), 3, "base system + persona + question expected");
        let c = msgs[1]["content"].as_str().unwrap();
        assert!(c.starts_with("VOICE"), "persona must open with the precedence preamble");
        assert!(c.contains("mission control"), "shipped default voice missing");
        // Custom text is hot-read (denylist pattern — next call sees the edit).
        std::fs::write(&pf, "talk like a pirate\n")?;
        let msgs = agent::build_ask_messages("reg", "scr", &[], "q");
        assert!(msgs[1]["content"].as_str().unwrap().contains("talk like a pirate"),
            "persona edit not hot-read");
        // Watch (VOICE) carries it, before its user payload.
        let w = agent::build_watch_messages(app::WatchReason::Quiet, "the tail", None, 0);
        assert!(w.iter().any(|m| m["content"].as_str().unwrap_or("").contains("pirate")),
            "watch lost the voice");
        assert_eq!(w.last().unwrap()["role"], "user", "watch payload must come last");
        // Early-harvest: the exit code and quiet duration reach the verdict prompt.
        let we = agent::build_watch_messages(app::WatchReason::Exit, "boom", Some(127), 0);
        let we_sys = we[0]["content"].as_str().unwrap();
        assert!(we_sys.contains("127") && we_sys.contains("non-zero"), "watch verdict lost the exit code");
        let wq = agent::build_watch_messages(app::WatchReason::Quiet, "...", None, 20);
        assert!(wq[0]["content"].as_str().unwrap().contains("20s"), "quiet verdict lost the silence duration");
        // FORMAT tasks: machine-parsed output — persona must never appear.
        let t = agent::build_translate_messages("", "", "list files", "scr");
        let t_user = t[1]["content"].as_str().unwrap();
        assert!(t_user.contains("ENV: shell=") && t_user.contains("os="),
            "translator lost the ENV context line");
        let n = agent::format_task_messages(prompts::AUTO_NAME_SYSTEM, "scr");
        let m = agent::format_task_messages(prompts::MISSION_SYSTEM, "snapshots");
        for (name, ms) in [("translate", &t), ("naming", &n), ("mission", &m)] {
            assert!(
                ms.iter().all(|msg| {
                    let s = msg["content"].as_str().unwrap_or("");
                    !s.contains("pirate") && !s.contains("VOICE")
                }),
                "persona leaked into the {name} task"
            );
        }
        // Cap: an oversize persona is truncated, with a visible marker.
        std::fs::write(&pf, "y".repeat(5000))?;
        let msgs = agent::build_ask_messages("reg", "scr", &[], "q");
        let c = msgs[1]["content"].as_str().unwrap();
        assert!(c.chars().count() < 2500, "persona cap not applied");
        assert!(c.ends_with('…'), "truncation marker missing");
        // Kill switch: an emptied file disables the voice entirely.
        std::fs::write(&pf, "\n  \n")?;
        let msgs = agent::build_ask_messages("reg", "scr", &[], "q");
        assert_eq!(msgs.len(), 2, "empty persona file must turn the voice off");
        // Secrets pasted into the style file never ride into a prompt.
        #[cfg(feature = "memory")]
        {
            std::fs::write(&pf, "sign replies as --password=hunter2 always\n")?;
            let msgs = agent::build_ask_messages("reg", "scr", &[], "q");
            let c = msgs[1]["content"].as_str().unwrap();
            assert!(!c.contains("hunter2"), "persona leaked a secret into the prompt");
            assert!(c.contains("[REDACTED]"), "redaction marker missing from persona");
        }
        // The palette action seeds a self-documenting file on first open.
        let _ = std::fs::remove_file(&pf);
        let mut app = App::new(None)?;
        app.run_action(palette::Action::OpenPersona);
        assert!(pf.exists(), "OpenPersona did not seed the persona file");
        let seeded = std::fs::read_to_string(&pf)?;
        assert!(seeded.starts_with('#') && seeded.contains("mission control"),
            "seeded persona missing header/default voice");
        std::env::remove_var("MARS_PERSONA");
        let _ = std::fs::remove_file(&pf);
        println!("[selfcheck] persona voice/format ..... PASS");
    }

    // 29g. Work journal + mission + expand-all notices: watch verdicts persist
    //      as a session-scoped snapshot stream (separate from the LLM call
    //      log), the inferred mission round-trips for `mars ls`, and the
    //      expand-all action drains the notice queue into one digest turn.
    {
        let wl = std::env::temp_dir().join(format!("mars-worklog-{}", std::process::id()));
        let _ = std::fs::remove_file(&wl);
        std::env::set_var("MARS_WORKLOG", &wl);
        for (i, v) in ["done: build green", "failed: 3 tests red", "done: tests green"]
            .iter()
            .enumerate()
        {
            worklog::record(&worklog::WorkEntry {
                ts: 1000 + i as u64,
                session: "train".into(),
                tab: "build".into(),
                verdict: v.to_string(),
                failed: v.starts_with("failed"),
                dur_secs: Some(60),
                cwd: "/work/train".into(),
                command: v.starts_with("failed").then(|| "cargo test".to_string()),
                exit: v.starts_with("failed").then_some(101),
                error_excerpt: v.starts_with("failed").then(|| "assertion failed: left == right".to_string()),
            });
        }
        worklog::record(&worklog::WorkEntry {
            ts: 2000, session: "other".into(), tab: "t".into(),
            verdict: "done: unrelated".into(), failed: false, dur_secs: None,
            cwd: String::new(), command: None, exit: None, error_excerpt: None,
        });
        // A pre-0.4 line (no outcome fields) must still parse — append raw.
        {
            use std::io::Write as _;
            let mut f = std::fs::OpenOptions::new().append(true).open(&wl)?;
            writeln!(f, r#"{{"ts":1500,"session":"train","tab":"old","verdict":"done: legacy","failed":false,"dur_secs":null}}"#)?;
        }
        let r = worklog::recent("train", 2);
        assert_eq!(r.len(), 2, "recent() limit not applied");
        assert_eq!(r[1].verdict, "done: legacy", "pre-0.4 line (no outcome fields) failed to parse");
        assert!(r.iter().all(|e| e.session == "train"), "session filter leaked");
        // Outcome fields round-trip; entries without them read back as absent.
        let all = worklog::recent("train", 10);
        let f = all.iter().find(|e| e.failed).expect("failed entry lost");
        assert_eq!(f.cwd, "/work/train", "cwd lost");
        assert_eq!(f.command.as_deref(), Some("cargo test"), "command lost");
        assert_eq!(f.exit, Some(101), "exit code lost");
        assert_eq!(f.error_excerpt.as_deref(), Some("assertion failed: left == right"), "excerpt lost");
        let legacy = all.iter().find(|e| e.verdict == "done: legacy").unwrap();
        assert!(legacy.cwd.is_empty() && legacy.command.is_none() && legacy.exit.is_none()
            && legacy.error_excerpt.is_none(), "legacy line grew phantom outcome fields");
        // Phase A — the Notice-shaped ledger envelope: every record carries tier-0
        // classification (kind/severity/headline) + provenance (origin/seq/principal)
        // + a state_version, derived on write; pre-ledger lines get it re-derived on
        // read. (design_ideas/movement-1-ledger-spec.md)
        let led = worklog::records("train", 10);
        let lf = led.iter().find(|r| r.kind == "failed").expect("no failed ledger record");
        assert_eq!(lf.severity, "fail", "failed record must be severity=fail");
        assert!(lf.headline.contains("cargo test") && lf.headline.contains("exit 101"),
            "tier-0 headline should carry the deterministic facts: {}", lf.headline);
        assert_eq!(lf.semantic_status, "done", "a record with a verdict reads semantically done");
        assert!(!lf.origin.is_empty() && !lf.principal.is_empty(), "provenance (origin/principal) missing");
        assert!(lf.seq >= 1, "monotonic seq not assigned: {}", lf.seq);
        assert!(lf.state_version.starts_with("fnv1a:"), "state_version not bound: {}", lf.state_version);
        // A pre-ledger line has its envelope RE-DERIVED — never blank, never misclassified.
        let lg = led.iter().find(|r| r.verdict == "done: legacy").expect("legacy ledger record lost");
        assert_eq!((lg.kind.as_str(), lg.severity.as_str()), ("done", "info"), "legacy envelope misderived");
        assert_eq!(lg.headline, "done: legacy", "legacy headline should fall back to the verdict");
        assert_eq!(lg.origin, "local", "legacy origin default");
        // Phase B — OSC-133 scanner: exact command/cwd/exit from shell-integration
        // markers, reassembled across read boundaries; the noteworthy gate maps only
        // failures + long runs into the ledger. (movement-1-ledger-spec.md §Phase B)
        {
            use osc133::{CmdEvent, Scanner};
            let stream = b"\x1b]7;file://host/home/me/proj\x07\x1b]633;E;python train.py\x1b\\\x1b]133;C\x07out\r\n\x1b]133;D;137\x07";
            let evs = Scanner::new().feed(stream);
            assert_eq!(evs.len(), 2, "expected Start+End: {evs:?}");
            assert_eq!(evs[0], CmdEvent::Start);
            match &evs[1] {
                CmdEvent::End { command, cwd, exit } => {
                    assert_eq!(command.as_deref(), Some("python train.py"), "command text lost");
                    assert_eq!(cwd.as_deref(), Some("/home/me/proj"), "cwd lost");
                    assert_eq!(*exit, Some(137), "exit code lost");
                }
                other => panic!("expected End: {other:?}"),
            }
            // An OSC split across two reads still parses (partial state persists).
            let mut s2 = Scanner::new();
            assert!(s2.feed(b"\x1b]133;D").is_empty(), "partial OSC fired early");
            assert!(matches!(s2.feed(b";0\x07").as_slice(), [CmdEvent::End { exit: Some(0), .. }]),
                "reassembled OSC mis-parsed");
            // No markers → no events: additive, zero regression for plain shells.
            assert!(Scanner::new().feed(b"plain output, no OSC markers\r\n").is_empty());
            // The noteworthy gate: a failure records; a trivial quick success does
            // not; a long success does. Command text goes through redaction.
            let fe = osc133::to_ledger_entry("s", "0",
                Some("mysql --password=hunter2".into()), Some("/w".into()), Some(1), Some(2))
                .expect("a failure must record");
            assert!(fe.failed && fe.exit == Some(1) && fe.command.is_some(), "failure fields wrong");
            #[cfg(feature = "memory")]
            assert!(fe.command.as_deref().map(|c| c.contains("[REDACTED]") && !c.contains("hunter2")).unwrap_or(false),
                "command not redacted: {:?}", fe.command);
            assert!(osc133::to_ledger_entry("s", "0", Some("ls".into()), None, Some(0), Some(0)).is_none(),
                "a trivial quick success must not flood the ledger");
            assert!(osc133::to_ledger_entry("s", "0", Some("cargo build".into()), None, Some(0), Some(600)).is_some(),
                "a long-running success is noteworthy");
        }
        println!("[selfcheck] OSC-133 ledger capture . PASS");
        // compact(): past 2×max, the journal is rewritten to the newest max lines.
        worklog::compact(10_000); // way under threshold — must be a no-op
        assert_eq!(worklog::recent("train", 10).len(), 4, "compact under threshold rewrote the file");
        worklog::compact(2);
        let after = std::fs::read_to_string(&wl)?;
        assert_eq!(after.lines().count(), 2, "compact did not bound the journal");
        assert!(after.contains("done: legacy"), "compact dropped the newest lines");
        worklog::save_mission("train", "fixing the red tests", 1234);
        assert_eq!(
            worklog::load_mission("train"),
            Some(("fixing the red tests".to_string(), 1234)),
            "mission round-trip failed"
        );
        assert_eq!(worklog::load_mission("other"), None, "mission leaked across sessions");
        // The ls SUMMARY column, priority tested with now-relative data (the
        // seeded 1970-epoch lines age out of every recency gate, as they should).
        assert_eq!(session::session_summary("nowhere"), "active — nothing logged yet",
            "a live session must never render a blank summary — the floor stands in");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let mk = |sess: &str, verdict: &str, failed: bool, ts: u64| worklog::WorkEntry {
            ts, session: sess.into(), tab: "t".into(), verdict: verdict.into(), failed,
            dur_secs: None, cwd: String::new(), command: None, exit: None, error_excerpt: None,
        };
        // (1) A fresh failure/block leads, even with goals present.
        worklog::record(&mk("s_fail", "blocked: waiting on your input", false, now - 60));
        worklog::save_goals("s_fail", &["ship the release".into()], now);
        assert!(session::session_summary("s_fail").starts_with("blocked: waiting on your input · "),
            "a needs-you verdict must lead: {}", session::session_summary("s_fail"));
        // (2) No failure → the goals (the captured intent) win over a done
        //     verdict, and ALL goals show (one per line), not a "+N more" tease.
        worklog::record(&mk("s_goal", "done: built the thing", false, now - 30));
        worklog::save_goals("s_goal", &["test MARS features".into(), "write the doc".into()], now);
        assert_eq!(session::session_summary("s_goal"), "→ test MARS features\n→ write the doc",
            "all goals should summarize the session, one per line");
        // (3) Lifecycle noise never becomes the headline; a real done verdict does.
        worklog::record(&mk("s_done", "done: user exited terminal voluntarily", false, now - 5));
        worklog::record(&mk("s_done", "done: cargo build green", false, now - 90));
        assert!(session::session_summary("s_done").starts_with("done: cargo build green · "),
            "noise must be skipped for the real verdict: {}", session::session_summary("s_done"));
        // (4) A stale mission is dropped, not shown as if current — but the floor
        //     still stands in, so the column is never blank for a live session.
        worklog::save_mission("s_stale", "vague old mission", now - 5 * 86_400);
        worklog::record(&mk("s_stale", "done: user exited terminal", false, now - 10));
        let stale = session::session_summary("s_stale");
        assert!(!stale.contains("vague old mission"), "a days-old mission must not linger: {stale}");
        assert!(!stale.is_empty(), "the deterministic floor must fill the column: {stale}");
        // (5) Floor detail: with only lifecycle noise but a real cwd/command, the
        //     column shows WHERE and WHEN — dir · command · ago — with no LLM call.
        worklog::record(&worklog::WorkEntry {
            ts: now - 20, session: "s_floor".into(), tab: "t".into(),
            verdict: "user exited terminal".into(), failed: false, dur_secs: None,
            cwd: "/home/me/rerank".into(), command: Some("vim train.py".into()),
            exit: None, error_excerpt: None,
        });
        let floor = session::session_summary("s_floor");
        assert!(floor.starts_with("rerank · vim train.py · "),
            "floor should show dir · command · ago: {floor}");
        // (6) A STALE, rambling verdict (the "irrelevant garbage" bug): a 5-day-old
        //     verbose done-line must NOT surface as the headline — it ages out and
        //     the floor carries the honest dir · cmd · ago instead.
        worklog::record(&worklog::WorkEntry {
            ts: now - 5 * 86_400, session: "s_old".into(), tab: "t".into(),
            verdict: "done: commit and reinstall completed with video script at paper/VIDEO_RUNBOOK.md; auto-named the pane".into(),
            failed: false, dur_secs: None, cwd: "/home/me/mars".into(),
            command: Some("git commit".into()), exit: None, error_excerpt: None,
        });
        let old = session::session_summary("s_old");
        assert!(!old.contains("VIDEO_RUNBOOK"), "a 5-day-old rambling verdict must not be the headline: {old}");
        assert!(old.starts_with("mars · git commit · "), "stale verdict should fall to the floor: {old}");
        // (7) A FRESH but rambling verdict is trimmed to its first clause, not dumped
        //     whole — the ls column wants the headline, not the whole paragraph.
        worklog::record(&mk("s_wordy", "done: shipped the reranker; also refactored the loader; and cleaned up", false, now - 30));
        let wordy = session::session_summary("s_wordy");
        assert!(wordy.starts_with("done: shipped the reranker · "),
            "a rambling verdict must be trimmed to its first clause: {wordy}");
        // (8) "…summarizing…": while a fresh capture is in flight (marker recent) and
        //     nothing fresh exists yet, the column says so; once it expires, the floor
        //     takes over — the placeholder never lingers.
        worklog::record(&mk("s_prog", "done: user exited terminal", false, now - 8 * 86_400));
        worklog::mark_summarizing("s_prog", now - 5);
        assert_eq!(session::session_summary("s_prog"), "…summarizing…",
            "a fresh in-flight summary should show the placeholder");
        worklog::mark_summarizing("s_prog", now - 9_999); // long past the TTL
        assert_ne!(session::session_summary("s_prog"), "…summarizing…",
            "an expired summarizing marker must give way to the floor");
        // Overflowing summaries wrap into a block under the column: greedy
        // word-wrap, overlong words hard-split, empty input → no lines.
        assert_eq!(
            session::wrap_text("fixing the red tests in the training run", 16),
            vec!["fixing the red", "tests in the", "training run"],
            "word wrap broke"
        );
        assert!(
            session::wrap_text("supercalifragilisticexpialidocious", 10)
                .iter()
                .all(|l| l.chars().count() <= 10),
            "overlong word not hard-split to width"
        );
        assert!(session::wrap_text("", 20).is_empty(), "empty summary should wrap to no lines");
        assert!(
            session::wrap_text("short", 20) == vec!["short"],
            "short summary should stay one line"
        );
        std::env::set_var("MARS_WORKLOG", &worklog_default); // back to the suite default
        let _ = std::fs::remove_file(&wl);
        let _ = std::fs::remove_file(std::env::temp_dir().join(format!("mars-worklog-{}", std::process::id())).with_file_name("mission.json"));

        let mut app = App::new(None)?;
        app.notices.push(app::Notice { text: "failed: run A".into(), kind: app::NoticeKind::Failure });
        app.notices.push(app::Notice { text: "done: run B".into(), kind: app::NoticeKind::Info });
        app.run_action(palette::Action::ExpandNotices);
        assert!(app.notices.is_empty(), "expand-all did not clear the notice queue");
        let digest = &app.agent_history.last().expect("no digest turn").1;
        assert!(
            digest.contains("failed: run A") && digest.contains("done: run B"),
            "digest missing notices: {digest}"
        );

        // Reattach briefing: detach → reattach pushes a "where you left off"
        // turn built from the journal + mission (deterministic, no LLM call).
        let wl2 = std::env::temp_dir().join(format!("mars-worklog2-{}", std::process::id()));
        let _ = std::fs::remove_file(&wl2);
        std::env::set_var("MARS_WORKLOG", &wl2);
        worklog::record(&worklog::WorkEntry {
            ts: 1000, session: "standalone".into(), tab: "train".into(),
            verdict: "failed: OOM at step 40".into(), failed: true, dur_secs: Some(300),
            cwd: String::new(), command: None, exit: None, error_excerpt: None,
        });
        worklog::save_mission("standalone", "debugging the OOM in the training run", 1000);
        let mut app = App::new(None)?;
        let turns_before = app.agent_history.len();
        app.on_detach();
        app.on_attach();
        let brief = &app.agent_history.last().expect("no briefing turn").1;
        assert!(app.agent_history.len() > turns_before, "reattach pushed no briefing");
        assert!(
            brief.contains("Where you left off")
                && brief.contains("debugging the OOM")
                && brief.contains("failed: OOM at step 40"),
            "briefing missing mission or journal lines: {brief}"
        );
        std::env::set_var("MARS_WORKLOG", &worklog_default); // back to the suite default
        let _ = std::fs::remove_file(&wl2);
        let _ = std::fs::remove_file(wl2.with_file_name("mission.json"));
        println!("[selfcheck] worklog + mission + expand PASS");
    }

    // 29d. Memory actions are plain palette glue — present in EVERY build; in a
    //      no-memory build the stub facade returns neutral values so all their
    //      code paths degrade to status messages.
    {
        assert!(palette::Action::from_name("OpenCommandMemory").is_some());
        assert!(palette::Action::from_name("OpenDenylist").is_some());
        let clear = palette::Action::from_name("ClearCommandMemory").expect("action");
        assert!(clear.is_destructive(), "memory wipe must be confirmation-gated");
        // OpenTuning: the knobs file opens in a buffer (self-seeding via load()).
        let mut app = App::new(None)?;
        app.run_action(palette::Action::OpenTuning);
        let opened = app
            .buffers
            .values()
            .any(|b| b.path.as_deref().is_some_and(|p| p.ends_with("tuning.json")));
        assert!(opened, "OpenTuning did not open tuning.json in a buffer");
        drop(app);
        println!("[selfcheck] memory actions ........... PASS");
    }

    // 29e. The stub build: MARS_MEMORY is inert, every facade call is neutral,
    //      and nothing panics — the terminal works with memory deleted.
    #[cfg(not(feature = "memory"))]
    {
        std::env::set_var("MARS_MEMORY", "full");
        assert_eq!(retrieval::MemoryMode::from_env().as_str(), "none", "stub mode must be inert");
        assert!(retrieval::fewshot_for("x").is_empty());
        assert!(retrieval::docs_context_for("x").is_none());
        assert!(retrieval::command_memory_path().is_none());
        assert!(retrieval::denylist_path().is_none());
        assert!(retrieval::load_command_records().is_empty());
        retrieval::remember_command("a", "b");
        std::env::remove_var("MARS_MEMORY");
        println!("[selfcheck] memory stub (feature off)  PASS");
    }

    // 30. SSH broker: detection + precedence + honest availability + proxy round-trip.
    #[cfg(feature = "ssh")]
    {
        use std::io::{BufRead, BufReader};
        for v in ["GROQ_API_KEY", "GEMINI_API_KEY", "GOOGLE_API_KEY",
                  "ANTHROPIC_API_KEY", "OPENAI_API_KEY",
                  "MARS_LLM_KEY", "ARES_LLM_KEY", "MARS_LLM_MODEL", "ARES_LLM_MODEL"] {
            std::env::remove_var(v);
        }
        // A looping responder standing in for `mars keyd`.
        let dir = std::env::temp_dir().join(format!("mars-broker-sc-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let sock = dir.join("auth.sock");
        let sock_s = sock.to_string_lossy().to_string();
        let _ = std::fs::remove_file(&sock);
        let listener = crate::sys::control::bind(&sock)?;
        std::thread::spawn(move || {
            for conn in listener.incoming() {
                let Ok(stream) = conn else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    continue; // a bare connect probe (is_configured) — no request
                }
                let mut w = stream;
                let _ = session::write_frame(
                    &mut w,
                    &broker::BrokerResponse::Chat { text: "broker-ok".into() },
                );
            }
        });

        // Detection: a live forwarded socket ⇒ provider "broker", configured. This block IS
        // the broker test, so the suite-wide opt-out comes off around it — pointed at the
        // socket built above, never at the developer's own.
        std::env::remove_var("MARS_NO_BROKER");
        std::env::set_var("MARS_AUTH_SOCK", &sock_s);
        let c = agent::AgentConfig::from_env();
        assert_eq!(c.provider, "broker", "MARS_AUTH_SOCK not detected as broker");
        assert!(c.is_configured(), "live broker socket should be configured");

        // Precedence: an explicit key outranks the forwarded socket.
        std::env::set_var("MARS_LLM_KEY", "explicit");
        assert_ne!(agent::AgentConfig::from_env().provider, "broker",
            "explicit key should outrank the broker socket");
        std::env::remove_var("MARS_LLM_KEY");

        // Honest availability: a dead socket path ⇒ not configured.
        let dead = agent::AgentConfig {
            url: String::new(), key: String::new(), model: String::new(),
            provider: "broker", max_tokens: 512, temperature: 0.3,
            broker_sock: Some("/tmp/mars-nope-does-not-exist.sock".into()),
        };
        assert!(!dead.is_configured(), "dead broker socket reported configured");

        // Proxy round-trip: chat() in broker mode returns the broker's reply,
        // and NEVER constructs a key/Authorization header on this side.
        let out = agent::chat(&c, vec![], "test").unwrap_or_default();
        assert_eq!(out, "broker-ok", "broker proxy did not return the reply: {out:?}");

        std::env::remove_var("MARS_AUTH_SOCK");
        std::env::set_var("MARS_NO_BROKER", "1");
        let _ = std::fs::remove_file(&sock);
        println!("[selfcheck] ssh broker (proxy/detect) . PASS");
    }
    // 30b. Windows-home tunnels authenticate before exposing broker frames.
    #[cfg(feature = "ssh")]
    {
        use std::io::{BufRead as _, Read as _, Write as _};
        use std::time::Duration;

        let dir = std::env::temp_dir()
            .join(format!("mars-broker-cap-sc-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let addr = dir.join("auth.sock");
        let _ = std::fs::remove_file(&addr);
        let listener = crate::sys::control::bind(&addr)?;
        let expected = "0123456789abcdef0123456789abcdef";
        let server = std::thread::spawn(move || {
            let mut stream = listener.accept().expect("capability server accept");
            let mut reader = std::io::BufReader::new(
                stream.try_clone().expect("capability stream clone")
            );
            let mut capability = String::new();
            reader.read_line(&mut capability).expect("capability preamble");
            assert_eq!(capability.trim_end(), expected);
            let mut request = String::new();
            reader.read_line(&mut request).expect("broker request");
            assert!(
                serde_json::from_str::<broker::BrokerRequest>(request.trim()).is_ok(),
                "capability was not followed by a broker request"
            );
            session::write_frame(
                &mut stream,
                &broker::BrokerResponse::Chat { text: "capability-ok".into() },
            )
            .expect("capability response");
        });
        std::env::set_var(broker::BROKER_CAPABILITY_ENV, expected);
        let cfg = agent::AgentConfig {
            url: String::new(), key: String::new(), model: String::new(),
            provider: "broker", max_tokens: 512, temperature: 0.3,
            broker_sock: Some(addr.to_string_lossy().into_owned()),
        };
        assert_eq!(
            broker::chat_via_broker(
                cfg.broker_sock.as_deref().unwrap(), &cfg, Vec::new(), "selfcheck", 0
            )?,
            "capability-ok"
        );
        std::env::remove_var(broker::BROKER_CAPABILITY_ENV);
        server.join().expect("capability server thread");

        let relay_home = dir.join("relay-home.sock");
        let home_listener = crate::sys::control::bind(&relay_home)?;
        let home = std::thread::spawn(move || {
            let mut stream = home_listener.accept().expect("relay home accept");
            let mut reader =
                std::io::BufReader::new(stream.try_clone().expect("home clone"));
            let mut request = String::new();
            reader.read_line(&mut request).expect("relayed request");
            assert_eq!(request, "{\"probe\":true}\n");
            stream.write_all(b"{\"relayed\":true}\n").expect("relayed response");
            stream.flush().expect("relayed response flush");
        });
        let relay = ssh::BrokerRelay::start(&relay_home, expected)?;

        let mut wrong = std::net::TcpStream::connect(relay.addr())?;
        wrong.set_read_timeout(Some(Duration::from_secs(1)))?;
        wrong.write_all(b"wrong\n{\"probe\":true}\n")?;
        let mut byte = [0u8; 1];
        assert!(
            !matches!(wrong.read(&mut byte), Ok(n) if n > 0),
            "relay returned broker bytes to an unauthenticated client"
        );

        let mut tunneled = std::net::TcpStream::connect(relay.addr())?;
        tunneled.write_all(expected.as_bytes())?;
        tunneled.write_all(b"\n{\"probe\":true}\n")?;
        tunneled.flush()?;
        let mut response = String::new();
        std::io::BufReader::new(tunneled).read_line(&mut response)?;
        assert_eq!(response, "{\"relayed\":true}\n");
        drop(relay);
        home.join().expect("relay home thread");
        let _ = std::fs::remove_dir_all(&dir);
        println!("[selfcheck] ssh broker capability ..... PASS");
    }
    // 30-stub. Without the capability the stub must be inert and honest: no
    // broker is ever detected (even with the env var set), and the ssh/keyd
    // entry points explain themselves instead of half-working.
    #[cfg(not(feature = "ssh"))]
    {
        std::env::set_var("MARS_AUTH_SOCK", "/tmp/mars-stub-sc.sock");
        assert!(broker::detect_broker_sock().is_none(), "stub must never detect a broker");
        assert_ne!(
            agent::AgentConfig::from_env().provider, "broker",
            "stub build selected the broker provider"
        );
        std::env::remove_var("MARS_AUTH_SOCK");
        assert!(broker::keyd_main().is_err(), "stub keyd must refuse, not no-op");
        assert!(broker::ssh_main("x".into(), Vec::new()).is_err(), "stub ssh must refuse, not no-op");
        assert!(broker::broker_socket_path().is_err(), "stub socket path must be absent");
        assert!(broker::find_live_auth_sock(std::path::Path::new("/tmp")).is_none());
        println!("[selfcheck] ssh broker stub (no ssh) . PASS");
    }

    // 31. Fleet cache + `mars ls` follow-up resolver (ordinal + name/prefix).
    {
        let hosts = vec!["gpubox".to_string(), "prod-7".to_string()];
        assert_eq!(fleet::resolve_target(&hosts, "2"), Some("prod-7".into()), "ordinal");
        assert_eq!(fleet::resolve_target(&hosts, "gpubox"), Some("gpubox".into()), "exact name");
        assert_eq!(fleet::resolve_target(&hosts, "prod"), Some("prod-7".into()), "unique prefix");
        assert_eq!(fleet::resolve_target(&hosts, ""), None, "empty skips");
        assert_eq!(fleet::resolve_target(&hosts, "9"), None, "out-of-range ordinal");
        // Fleet round-trip under an isolated home dir (upsert dedupes, recency
        // orders). sys::paths names the env var, so this redirects on any OS.
        let saved = std::env::var(sys::paths::HOME_ENV).ok();
        let tmp = std::env::temp_dir().join(format!("mars-fleet-sc-{}", std::process::id()));
        std::fs::create_dir_all(&tmp)?;
        std::env::set_var(sys::paths::HOME_ENV, &tmp);
        fleet::fleet_record("prod-7", None);
        fleet::fleet_record("gpubox", None); // touched last → most recent
        fleet::fleet_record("prod-7", None); // upsert, not a dup
        let f = fleet::fleet_load();
        assert_eq!(f.len(), 2, "fleet upsert duplicated a host");
        assert_eq!(f[0].host, "prod-7", "fleet not ordered most-recent-first");
        // The status push (what a brokered agent call reports home) refreshes
        // session + last_status — the "latest status" mars ls renders.
        fleet::fleet_status("gpubox", Some("train".into()), "agent active");
        let f = fleet::fleet_load();
        let g = f.iter().find(|e| e.host == "gpubox").expect("status push dropped the host");
        assert_eq!(g.last_status.as_deref(), Some("agent active"));
        assert_eq!(g.session.as_deref(), Some("train"));
        assert!(
            f.iter().all(|e| g.as_of >= e.as_of),
            "status push did not refresh recency"
        );
        // The unified list: locals and remotes share one shape, one ordinal
        // space, and one status field; remotes carry the pushed status.
        let entries = session::all_sessions()?;
        let g = entries
            .iter()
            .find(|e| e.name == "gpubox")
            .expect("remote host missing from all_sessions");
        assert!(g.remote && g.as_of.is_some(), "remote entry lost its provenance");
        assert!(
            g.status.contains("agent active") && g.status.contains("session train"),
            "pushed status not plumbed into ls: {}",
            g.status
        );
        assert!(g.summary.is_empty(), "remotes have no LLM summary column");
        assert_eq!(g.connect, "mars ssh gpubox");
        assert!(
            entries.iter().all(|e| e.remote || e.as_of.is_none()),
            "a local session carried a stale as_of"
        );
        let names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
        assert_eq!(
            fleet::resolve_target(&names, "gpub").as_deref(),
            Some("gpubox"),
            "unified resolver lost prefix matching"
        );
        match saved {
            Some(h) => std::env::set_var(sys::paths::HOME_ENV, h),
            None => std::env::remove_var(sys::paths::HOME_ENV),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
    println!("[selfcheck] fleet cache + ls resolver . PASS");

    // 32. The embedded installer (pushed to remotes by `mars ssh`) is intact.
    #[cfg(feature = "ssh")]
    {
        assert!(broker::INSTALL_SH.starts_with("#!/bin/sh"), "install.sh lost its shebang");
        assert!(broker::INSTALL_SH.contains("sh.rustup.rs") && broker::INSTALL_SH.contains("mars-terminal"),
            "embedded install.sh missing its core steps");
        assert!(broker::INSTALL_SH.contains("MINGW"), "embedded install.sh lost the Windows guard");
        assert!(
            !ssh::installer_payload().contains('\r'),
            "embedded installer payload must use Unix line endings"
        );
        println!("[selfcheck] embedded installer ........ PASS");
    }

    // 32b. The ssh remote-command builders. The prelude must sweep a stale auth
    //      socket BEFORE the interactive ssh requests the -R forward (sshd won't
    //      bind over a leftover; client-side StreamLocalBindUnlink only covers
    //      local forwards), and the install check must probe the real install
    //      destinations, not just sshd's bare non-login PATH.
    #[cfg(feature = "ssh")]
    {
    let prelude = broker::remote_prelude_cmd("/tmp/mars-auth-42.sock", true, false);
    let sweep = prelude.find("rm -f /tmp/mars-auth-42.sock;")
        .expect("prelude lost the stale-socket sweep (or its ; separator)");
    assert!(sweep < prelude.find("install.sh").expect("prelude lost the installer drop"),
        "sweep must precede the installer drop");
    for needle in [
        "NEED_INSTALL=1",
        "sh \"$HOME/.mars/install.sh\"",
        "automatic installer did not produce a usable mars binary",
    ] {
        assert!(prelude.contains(needle), "prelude lost automatic bootstrap step: {needle}");
    }
    assert!(!prelude.contains("--broker-handoff-version"),
        "ordinary Unix bootstrap unexpectedly requires the capability protocol");
    // Reused ControlMaster ⇒ its old -R forward is still live on the existing
    // socket inode; sweeping would orphan a working tunnel.
    let reuse = broker::remote_prelude_cmd("/tmp/mars-auth-42.sock", false, false);
    assert!(!reuse.contains("rm -f"), "master-reuse prelude must NOT sweep the live socket");
    assert!(reuse.contains("install.sh"), "master-reuse prelude lost the installer drop");
    let windows_bootstrap = broker::remote_prelude_cmd(
        "/tmp/mars-auth-cap-home-nonce.sock", true, true
    );
    assert!(
        windows_bootstrap.contains("export PATH=\"$HOME/.cargo/bin:$HOME/.local/bin:$PATH\"")
            && windows_bootstrap.contains("--broker-handoff-version")
            && windows_bootstrap.contains(broker::BROKER_HANDOFF_PROTOCOL)
            && windows_bootstrap.contains("NEED_INSTALL=1")
            && windows_bootstrap.contains("still too old for broker handoff"),
        "Windows-home bootstrap lost its protocol-aware upgrade path"
    );
    let sess = broker::remote_session_cmd("/tmp/mars-auth-42.sock", true);
    for needle in ["command -v mars", "export PATH=\"$HOME/.cargo/bin:$HOME/.local/bin:$PATH\"",
                   "export MARS_AUTH_SOCK=/tmp/mars-auth-42.sock", "exec ${SHELL:-/bin/sh} -l",
                   "automatic bootstrap completed",
                   "[ -S /tmp/mars-auth-42.sock ]", "agent tunnel ready", "no agent tunnel",
                   "\"$M\" attach", "exec \"$M\" new main"] {
        assert!(sess.contains(needle), "session cmd missing: {needle}");
    }
    assert!(broker::remote_session_cmd("/x.sock", false).contains("sh.rustup.rs"),
        "no-installer nudge lost the manual install steps");
    let secured = broker::remote_session_cmd_with_capability(
        "/tmp/mars-auth-secure.sock", false, Some("0123456789abcdef")
    );
    assert!(secured.contains("export MARS_BROKER_CAPABILITY=0123456789abcdef"),
        "Windows-home command lost the tunnel capability");
    assert!(
        secured.contains("--broker-handoff-version")
            && secured.contains(broker::BROKER_HANDOFF_PROTOCOL)
            && secured.contains("remote Mars is outdated"),
        "Windows-home command lost the remote broker protocol gate"
    );
    let scrubbed = ssh::ssh_command();
    for name in agent::PROVIDER_CREDENTIAL_ENV_VARS {
        assert!(
            scrubbed
                .get_envs()
                .any(|(candidate, value)| candidate == std::ffi::OsStr::new(name) && value.is_none()),
            "ssh child still inherits provider credential {name}"
        );
    }
    let protocol = std::process::Command::new(std::env::current_exe()?)
        .arg("--broker-handoff-version")
        .output()?;
    assert!(
        protocol.status.success()
            && String::from_utf8_lossy(&protocol.stdout).trim()
                == broker::BROKER_HANDOFF_PROTOCOL,
        "remote broker protocol probe returned the wrong marker"
    );
    println!("[selfcheck] ssh remote commands ....... PASS");

    // 32c. Dead-socket self-heal: a leftover auth socket with no listener must
    //      read as absent AND be unlinked so the next `ssh -R` can bind; a live
    //      one must be kept. (Temp paths only — never the real /tmp socket.)
    let tmp = std::env::temp_dir().join(format!("mars-sock-probe-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    assert!(!broker::probe_and_sweep(&tmp.join("none.sock")), "nonexistent socket read as live");
    let dead = tmp.join("dead.sock");
    #[cfg(unix)]
    {
        // A real stale socket: bind, then drop the listener (Rust does not unlink
        // on drop), leaving a socket whose connect is refused → classified Dead →
        // swept. An empty *regular* file would connect with ENOTSOCK →
        // Indeterminate, which correctly is NOT swept (never delete a non-socket).
        drop(crate::sys::control::bind(&dead)?);
    }
    #[cfg(windows)]
    {
        let unused = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        let port = unused.local_addr()?.port();
        drop(unused);
        std::fs::write(
            &dead,
            format!("2 {port} 0123456789abcdef0123456789abcdef\n"),
        )?;
    }
    assert!(!broker::probe_and_sweep(&dead), "dead socket file read as live");
    assert!(!dead.exists(), "dead socket was not swept");
    #[cfg(windows)]
    {
        let legacy = tmp.join("legacy.sock");
        let legacy_listener =
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        std::fs::write(
            &legacy,
            format!(
                "{} 0123456789abcdef0123456789abcdef\n",
                legacy_listener.local_addr()?.port()
            ),
        )?;
        assert!(!broker::probe_and_sweep(&legacy));
        assert!(legacy.exists(), "legacy live descriptor was destructively swept");
        drop(legacy_listener);

        let busy = tmp.join("busy.sock");
        let busy_listener =
            std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))?;
        std::fs::write(
            &busy,
            format!(
                "2 {} 0123456789abcdef0123456789abcdef\n",
                busy_listener.local_addr()?.port()
            ),
        )?;
        let busy_peer = std::thread::spawn(move || {
            let _ = busy_listener.accept();
            std::thread::sleep(std::time::Duration::from_millis(700));
        });
        assert!(!broker::probe_and_sweep(&busy));
        assert!(busy.exists(), "timed-out live descriptor was destructively swept");
        busy_peer.join().expect("busy control peer");
    }
    let live = tmp.join("live.sock");
    let listener = crate::sys::control::bind(&live)?;
    #[cfg(windows)]
    let live_accept = std::thread::spawn(move || {
        listener.accept().expect("live socket probe accept")
    });
    #[cfg(unix)]
    let _listener = listener;
    assert!(broker::probe_and_sweep(&live), "bound socket read as dead");
    #[cfg(windows)]
    drop(live_accept.join().expect("live socket probe thread"));
    assert!(live.exists(), "live socket must not be swept");
    #[cfg(unix)]
    {
    // The glob fallback: the socket name carries the HOME uid, so the remote
    // must find any live mars-auth-*.sock, not just its own uid's — while
    // still preferring an own-uid socket when one is live.
    assert!(broker::find_live_auth_sock(&tmp).is_none(), "no candidates but one found");
    drop(crate::sys::control::bind(tmp.join("mars-auth-777.sock"))?); // dead stand-in: bound, then closed
    let _other = crate::sys::control::bind(tmp.join("mars-auth-888.sock"))?;
    assert_eq!(
        broker::find_live_auth_sock(&tmp).as_deref(),
        Some(tmp.join("mars-auth-888.sock").to_str().unwrap()),
        "glob fallback did not find the live foreign-uid socket"
    );
    assert!(!tmp.join("mars-auth-777.sock").exists(), "dead candidate not swept by scan");
    let own = tmp.join(format!("mars-auth-{}.sock", crate::sys::proc::uid_tag()));
    let _own = crate::sys::control::bind(&own)?;
    assert_eq!(broker::find_live_auth_sock(&tmp).as_deref(), own.to_str(),
        "own-uid socket must win over a foreign live one");
    }
    let _ = std::fs::remove_dir_all(&tmp);
    println!("[selfcheck] auth-socket liveness ...... PASS");
    }

    // 33. Closing a tab with a live terminal confirms, then reaps the PTY —
    //     never orphans the shell (P0.1). Decline keeps everything; confirm
    //     removes the tab AND drops the Term + its watch state.
    let mut app = App::new(None)?;
    app.new_tab(); // tab index 1 (active); open a live shell inside it
    app.open_terminal();
    let tid = match app.focused_pane().content {
        pane::PaneContent::Terminal(id) => id,
        _ => panic!("open_terminal did not attach a terminal"),
    };
    app.run_action(palette::Action::WatchPane); // give it watch state to reap too
    assert!(app.terms.contains_key(&tid) && app.watches.contains_key(&tid), "terminal/watch not registered");
    app.run_action(palette::Action::CloseTab);
    assert!(app.mode == mode::Mode::Prompt, "close with a live terminal did not confirm");
    assert!(app.terms.contains_key(&tid), "terminal reaped before confirmation");
    app.handle_key(k(KeyCode::Char('n')))?; // decline
    assert_eq!(app.tabs.len(), 2, "declined close still removed the tab");
    assert!(app.terms.contains_key(&tid), "declined close still reaped the terminal");
    app.run_action(palette::Action::CloseTab);
    app.handle_key(k(KeyCode::Char('y')))?; // confirm
    assert_eq!(app.tabs.len(), 1, "confirmed close did not remove the tab");
    assert!(!app.terms.contains_key(&tid), "confirmed close ORPHANED the terminal (not reaped)");
    assert!(!app.watches.contains_key(&tid), "watch state not cleaned on reap");
    println!("[selfcheck] close gate reaps PTYs ..... PASS");

    // 34. Close short-circuit: a CLEAN editor pane closes without the motor-slip
    //     confirm (a nice speed-up); a DIRTY buffer still gates. Live terminals are
    //     covered by test 33.
    let mut app = App::new(None)?;
    app.new_tab(); // 2 clean editor tabs, no edits
    app.handle_key(kc(KeyCode::Char('t')))?; // C-t → warp
    assert!(app.mode == mode::Mode::Tab, "C-t did not enter space warp");
    app.handle_key(k(KeyCode::Char('d')))?; // close a CLEAN tab → no confirm
    assert!(app.mode != mode::Mode::Prompt, "a clean-editor close should not confirm");
    assert_eq!(app.tabs.len(), 1, "clean tab did not close immediately");
    // A DIRTY buffer still gates (the motor-slip guard where it matters).
    let mut app = App::new(None)?;
    app.new_tab();          // tab 2, focused in Edit mode
    typ(&mut app, "edit")?; // modify the buffer
    assert!(app.focused_buf().modified, "typing did not mark the buffer modified");
    app.handle_key(kc(KeyCode::Char('t')))?; // warp
    app.handle_key(k(KeyCode::Char('d')))?;  // close the DIRTY tab
    assert!(app.mode == mode::Mode::Prompt, "a dirty-buffer close did not confirm (motor-slip guard)");
    app.handle_key(k(KeyCode::Char('n')))?;
    assert_eq!(app.tabs.len(), 2, "declined close still removed the tab");
    println!("[selfcheck] close short-circuit + gate .. PASS");

    // 34b. Unified space-warp grammar: ONE directional set (arrows) walks the whole
    //      workspace — between split panes by geometry, spilling into the adjacent
    //      tab at the pane-grid edge; z/Space zoom the focused pane; d closes the
    //      focused view (pane, or tab when it's the last pane).
    {
        let mut app = App::new(None)?;
        app.new_tab(); // two single-pane tabs; focus tab 1
        app.handle_key(kc(KeyCode::Char('t')))?; // C-t → warp
        // Single pane → no pane to the side → horizontal move spills to the tab.
        app.handle_key(k(KeyCode::Left))?;
        assert_eq!(app.active_tab, 0, "warp ← at the pane edge did not spill to prev tab");
        app.handle_key(k(KeyCode::Right))?;
        assert_eq!(app.active_tab, 1, "warp → at the pane edge did not spill to next tab");
        // Split → two panes; now arrows move BETWEEN panes (geometry), no spill.
        app.handle_key(k(KeyCode::Esc))?; // leave warp to split
        app.handle_key(kc(KeyCode::Char('\\')))?; // C-\ → split right
        assert_eq!(app.tab().layout.count(), 2, "C-\\ did not split");
        app.handle_key(kc(KeyCode::Char('t')))?; // C-t → warp again
        let mut term = Terminal::new(TestBackend::new(80, 20))?;
        term.draw(|f| ui::render(f, &mut app))?; // populate pane geometry
        let right = app.focused_pane_id();
        app.handle_key(k(KeyCode::Left))?;
        let left = app.focused_pane_id();
        assert_ne!(left, right, "warp ← did not move between split panes");
        assert_eq!(app.active_tab, 1, "warp ← between panes must NOT spill tabs");
        // z zooms the focused pane; Space toggles it back (same verb, unified).
        app.handle_key(k(KeyCode::Char('z')))?;
        assert_eq!(app.tab().zoomed, Some(left), "warp z did not zoom the focused pane");
        app.handle_key(k(KeyCode::Char(' ')))?;
        assert_eq!(app.tab().zoomed, None, "warp Space did not un-zoom");
        // d closes the focused view: in a 2-pane split it closes the pane. The panes
        // are clean editors, so it closes immediately (no motor-slip gate — see 34).
        app.handle_key(k(KeyCode::Char('d')))?;
        assert_eq!(app.tab().layout.count(), 1, "warp d did not close the focused pane");
        println!("[selfcheck] unified warp grammar ...... PASS");
    }

    // 34c. Tier 2 — the two-pane command board: a workspace that needs you opens the
    //      bar focused on the Workspaces column, pre-selected so ↵ jumps to it. The
    //      column reads the same pane_verdict/tab_status seam as the top bar; commands
    //      live in their own column. Quiet + solo → the plain single-column launcher.
    {
        let mut app = App::new(None)?;
        app.handle_key(k(KeyCode::Char('x')))?; // dismiss splash
        app.open_terminal();
        let tid = *app.terms.keys().next().expect("open_terminal makes a terminal");
        app.watches.entry(tid).or_default().verdict =
            Some("blocked: overwrite runs/best.pt? [y/N]".into());
        app.handle_key(kc(KeyCode::Char(' ')))?; // Ctrl-Space → command bar
        assert!(app.mode == mode::Mode::Bar, "Ctrl-Space did not open the bar");
        // The bar opens on the familiar Commands launcher; a blocked workspace makes
        // the separate WORKSPACES panel available beside it.
        assert_eq!(app.palette.as_ref().unwrap().column, palette::BarColumn::Commands,
            "the bar must open on the Commands launcher (previous behaviour)");
        assert!(app.bar_show_workspaces(), "a blocked workspace must show the WORKSPACES panel");
        let ws = app.bar_workspace_rows();
        assert!(matches!(ws.first().map(|r| &r.kind), Some(palette::ItemKind::Surface(_))),
            "the blocked workspace must lead the WORKSPACES panel");
        // ← moves focus into the WORKSPACES panel (the new thing), → returns.
        app.handle_key(k(KeyCode::Left))?;
        assert_eq!(app.palette.as_ref().unwrap().column, palette::BarColumn::Workspaces,
            "← must move focus into the WORKSPACES panel");
        // The panel renders its title, the ↵ verb on the selected row, and the
        // "status: …" summary.
        let mut term = Terminal::new(TestBackend::new(110, 24))?;
        term.draw(|f| ui::render(f, &mut app))?;
        let t = screen_text(&term);
        assert!(t.contains("SPACES") && t.contains("↵") && t.contains("status:") && t.contains("blocked"),
            "the panel must show its title, the ↵ verb, and the 'status: …' summary: {t}");
        app.handle_key(k(KeyCode::Right))?;
        assert_eq!(app.palette.as_ref().unwrap().column, palette::BarColumn::Commands,
            "→ must move focus back to the Commands launcher");
        // ↵ on the workspace (after ←) jumps and closes the bar.
        app.handle_key(k(KeyCode::Left))?;
        app.handle_key(k(KeyCode::Enter))?;
        assert!(app.palette.is_none(), "↵ on a workspace did not close the bar");
        // Quiet + solo: one idle workspace → no panel, the plain launcher stands.
        app.watches.get_mut(&tid).unwrap().verdict = None;
        app.terms.get_mut(&tid).unwrap().exited = false;
        app.watches.get_mut(&tid).unwrap().run_started_tick = 0;
        assert!(!app.bar_show_workspaces(),
            "a solo idle workspace must NOT show the panel — the plain launcher stands");
        println!("[selfcheck] workspaces panel + bar .. PASS");
    }

    // 34d. On-demand summary heuristics (the anti-excess-fire guards): at most one
    //      pull in flight per surface, and no re-pull unless new output has arrived
    //      since the last summary.
    {
        let mut app = App::new(None)?;
        app.open_terminal();
        let tid = *app.terms.keys().next().expect("open_terminal makes a terminal");
        {
            let w = app.watches.entry(tid).or_default();
            w.verdict = Some("done: build green".into());
            w.summ_output_tick = 10;
            w.last_output_tick = 10; // no new output since the last summary
        }
        // Freshness guard: a re-pull with no new output is dropped.
        app.status_msg = None;
        app.request_summary(tid);
        assert_eq!(app.status_msg.as_deref(), Some("summary is current"),
            "freshness guard: no new output must NOT re-fire");
        // New output releases the guard.
        app.watches.get_mut(&tid).unwrap().last_output_tick = 99;
        app.status_msg = None;
        app.request_summary(tid);
        assert_ne!(app.status_msg.as_deref(), Some("summary is current"),
            "new output must release the freshness guard");
        // In-flight guard: a pull while one is running is dropped.
        app.watches.get_mut(&tid).unwrap().summ_inflight = true;
        app.status_msg = None;
        app.request_summary(tid);
        assert_eq!(app.status_msg.as_deref(), Some("summarizing…"),
            "in-flight guard: only one pull at a time per surface");
        println!("[selfcheck] on-demand summary guards . PASS");
    }

    // 35. C-g cancels the command bar from every submode (doctrine §3.4).
    let mut app = App::new(None)?;
    app.handle_key(kc(KeyCode::Char(' ')))?; // → Bar (Command)
    assert!(app.mode == mode::Mode::Bar, "bar did not open");
    app.handle_key(kc(KeyCode::Char('g')))?;
    assert!(app.mode != mode::Mode::Bar && app.palette.is_none(), "C-g did not cancel Command bar");
    app.handle_key(kc(KeyCode::Char(' ')))?;
    app.handle_key(k(KeyCode::Char('!')))?; // → Shell submode
    assert!(matches!(app.palette.as_ref().map(|p| &p.bar_mode), Some(palette::BarMode::Shell)), "! did not reach shell");
    app.handle_key(kc(KeyCode::Char('g')))?;
    assert!(app.palette.is_none(), "C-g did not cancel shell submode");
    println!("[selfcheck] C-g cancels the bar ....... PASS");

    // 36. A plain click (anchor == end) must not copy — no clipboard clobber (P1.4).
    {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = App::new(None)?;
        app.open_terminal();
        let tid = match app.focused_pane().content {
            pane::PaneContent::Terminal(id) => id,
            _ => panic!("no terminal"),
        };
        let before = app.kill_ring.len();
        app.term_sel = Some(app::TermSel { tid, ox: 0, oy: 0, vw: 80, vh: 24, anchor: (2, 3), end: (2, 3) });
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 3, row: 2, modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.kill_ring.len(), before, "plain click copied to the kill ring");
        assert!(app.term_sel.is_none(), "term_sel not cleared on release");
    }
    println!("[selfcheck] click-no-drag no clobber .. PASS");

    // 36b. Terminal wheel = tmux's three-way dispatch: alternate-screen apps
    //      get arrow keys (DECCKM-aware), mouse-mode apps get encoded wheel
    //      events, the plain shell scrolls mars scrollback. Assertions read the
    //      PARSED screen (tty echo renders ESC as ^[), never raw bytes.
    {
        use crossterm::event::{MouseEvent, MouseEventKind};
        fn wheel(up: bool) -> MouseEvent {
            MouseEvent {
                kind: if up { MouseEventKind::ScrollUp } else { MouseEventKind::ScrollDown },
                column: 5, row: 5, modifiers: KeyModifiers::NONE,
            }
        }
        fn term_with(app: &mut App, setup: &[u8]) -> usize {
            app.open_terminal();
            let tid = match app.focused_pane().content {
                pane::PaneContent::Terminal(id) => id,
                _ => panic!("no terminal"),
            };
            std::thread::sleep(std::time::Duration::from_millis(400));
            app.tick();
            app.terms.get_mut(&tid).unwrap().send_bytes(setup);
            std::thread::sleep(std::time::Duration::from_millis(500));
            app.tick();
            tid
        }
        fn wait_for(app: &mut App, tid: usize, needle: &str) -> bool {
            for _ in 0..30 {
                app.tick();
                if app.terms.get(&tid).unwrap().screen().contents().contains(needle) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            false
        }
        // A slow shell start can outlast term_with's fixed settle sleeps, so
        // every screen-MODE precondition polls instead of asserting once (the
        // "alt screen not entered" flake).
        fn wait_mode(app: &mut App, tid: usize, cond: fn(&vt100::Screen) -> bool) -> bool {
            for _ in 0..50 {
                app.tick();
                if cond(&app.terms.get(&tid).unwrap().screen()) {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            false
        }

        // (a)-(c) drive POSIX `printf`+`cat` and assert on the pty's raw echo of
        // escape sequences — ConPTY translates those into key events instead of
        // echoing them, so on a PowerShell host only (d) can run honestly.
        if !shell_is_powershell() {
        // (a) Alternate screen, no mouse reporting → arrows, not a silent no-op.
        let mut app = App::new(None)?;
        let tid = term_with(&mut app, b"printf '\\033[?1049h'; cat\n");
        assert!(wait_mode(&mut app, tid, |s| s.alternate_screen()), "alt screen not entered");
        app.handle_mouse(wheel(true));
        assert!(wait_for(&mut app, tid, "^[[A^[[A^[[A"),
            "alt-screen wheel-up did not become arrow keys");
        assert_eq!(app.terms.get(&tid).unwrap().view_offset(), 0);

        // (b) DECCKM set → application-cursor arrows (^[OA), not ^[[A.
        let mut app = App::new(None)?;
        let tid = term_with(&mut app, b"printf '\\033[?1049h\\033[?1h'; cat\n");
        assert!(wait_mode(&mut app, tid, |s| s.application_cursor()), "DECCKM not set");
        app.handle_mouse(wheel(true));
        assert!(wait_for(&mut app, tid, "^[OA^[OA^[OA"),
            "DECCKM wheel-up did not send application-cursor arrows");

        // (c) Inner app enabled SGR mouse reporting → the wheel press itself is
        //     forwarded, encoded, for the app to interpret.
        let mut app = App::new(None)?;
        let tid = term_with(&mut app, b"printf '\\033[?1002h\\033[?1006h'; cat\n");
        assert!(
            wait_mode(&mut app, tid, |s| s.mouse_protocol_mode() != vt100::MouseProtocolMode::None),
            "mouse mode not entered"
        );
        app.handle_mouse(wheel(true));
        assert!(wait_for(&mut app, tid, "[<64;1;1M"), "SGR wheel-up not forwarded");
        app.handle_mouse(wheel(false));
        assert!(wait_for(&mut app, tid, "[<65;1;1M"), "SGR wheel-down not forwarded");
        } else {
            println!("[selfcheck] wheel → app modes ........ SKIP (POSIX tty-echo probe; PowerShell host)");
        }

        // (d) Plain shell (no modes): the wheel still browses mars scrollback.
        let mut app = App::new(None)?;
        let tid = term_with(
            &mut app,
            if shell_is_powershell() { &b"1..100\r"[..] } else { &b"seq 1 100\n"[..] },
        );
        assert!(wait_for(&mut app, tid, "99"), "seq output never arrived");
        app.handle_mouse(wheel(true));
        assert!(app.terms.get(&tid).unwrap().view_offset() > 0,
            "plain-shell wheel-up no longer scrolls mars scrollback");
        app.handle_mouse(wheel(false));
        println!("[selfcheck] terminal wheel dispatch .. PASS");
    }

    // 37. Capability-tiered, canonical-preferring binding_for (P1.1): teaching
    //     surfaces must show a chord the terminal can actually send, and the
    //     canonical one over an alias. Guards the whole honesty-invariant layer.
    {
        let app = App::new(None)?;
        let b = |a| app.keys.binding_for(&a).unwrap_or_default();
        assert_eq!(b(palette::Action::Save), "C-x C-s", "Save should teach the universal chord, not ⌘-s");
        assert_eq!(b(palette::Action::SelectAll), "C-x h", "SelectAll should not teach ⌘-a");
        assert_eq!(b(palette::Action::SplitVertical), "C-x 3", "SplitVertical should not teach C-\\/C-|");
        assert_eq!(b(palette::Action::Search), "C-s", "Search should teach canonical C-s, not the C-r alias");
        // No teaching surface should ever advertise a ⌘/super chord.
        for a in [palette::Action::Save, palette::Action::SelectAll, palette::Action::CopyRegion, palette::Action::Paste] {
            assert!(!b(a).contains('⌘'), "binding_for taught a kitty-only ⌘ chord");
        }
        println!("[selfcheck] tiered binding_for ........ PASS");
    }

    // 38. A notice up + a focused terminal: Esc dismisses the notice (its "Esc
    //     dismiss" hint must be honest here) instead of leaking 0x1b to the shell.
    {
        let mut app = App::new(None)?;
        app.open_terminal();
        assert!(app.mode == mode::Mode::Terminal, "not focused on the terminal");
        app.notices.push(app::Notice {
            text: "build failed".into(),
            kind: app::NoticeKind::Failure,
        });
        app.handle_key(k(KeyCode::Esc))?;
        assert!(app.notices.is_empty(), "Esc did not dismiss the notice from a terminal");
        assert!(app.mode == mode::Mode::Terminal, "notice-dismiss should keep terminal focus");
        println!("[selfcheck] terminal Esc dismisses .... PASS");
    }

    // 39. One gesture rules everything (P1.5): Ctrl+Space opens the bar from the
    //     transient nav modes that used to swallow it (space warp, time-travel,
    //     file tree).
    let mut app = App::new(None)?;
    app.handle_key(kc(KeyCode::Char('t')))?; // C-t → space warp
    assert!(app.mode == mode::Mode::Tab, "C-t did not enter space warp");
    app.handle_key(kc(KeyCode::Char(' ')))?;
    assert!(app.mode == mode::Mode::Bar, "Ctrl+Space dead in space warp");
    app.handle_key(k(KeyCode::Esc))?;
    app.handle_key(kc(KeyCode::Char('u')))?; // C-u → time-travel
    assert!(app.mode == mode::Mode::Undo, "C-u did not enter time-travel");
    app.handle_key(kc(KeyCode::Char(' ')))?;
    assert!(app.mode == mode::Mode::Bar, "Ctrl+Space dead in time-travel");
    app.handle_key(k(KeyCode::Esc))?;
    app.toggle_file_tree();
    assert!(app.mode == mode::Mode::Tree, "file tree did not open");
    app.handle_key(kc(KeyCode::Char(' ')))?;
    assert!(app.mode == mode::Mode::Bar, "Ctrl+Space dead in the file tree");
    println!("[selfcheck] bar opens from any mode .. PASS");

    // 40. Previously-invisible actions now have searchable menu rows (P1.9) — a
    //     capability for one actor is a capability for all four.
    {
        use std::collections::HashMap;
        let frec: HashMap<String, u32> = HashMap::new();
        let mut p = palette::Palette::root();
        p.query = "space warp".to_string();
        assert!(
            p.visible_items(&frec).iter().any(|r| matches!(r.kind, palette::ItemKind::Run(palette::Action::TabMode))),
            "TabMode not searchable in the command bar"
        );
        p.query = "kill buffer".to_string();
        assert!(
            p.visible_items(&frec).iter().any(|r| matches!(r.kind, palette::ItemKind::Run(palette::Action::KillBuffer))),
            "KillBuffer not searchable in the command bar"
        );
        println!("[selfcheck] orphan actions now in menu PASS");
    }

    // 40b. Markdown reading-mode: the toggle repaints the focused editor pane as a
    //      read-only, reflowed document (termimad → tables/wrapping). No cursor; the
    //      document scrolls with the editor's own motion grammar (↑/↓ · ⌥↑/⌥↓ ·
    //      M-</M->), clamps exactly to the measured length, and blocks buffer edits
    //      until it is toggled back off.
    {
        let mut app = App::new(None)?;
        // A short viewport + a doc taller than it, so there is something to scroll.
        let mut term = Terminal::new(TestBackend::new(100, 12))?;
        typ(&mut app, "# Title")?; app.handle_key(k(KeyCode::Enter))?;
        app.handle_key(k(KeyCode::Enter))?;
        for i in 0..40 { typ(&mut app, &format!("paragraph line number {i} with some words"))?; app.handle_key(k(KeyCode::Enter))?; }
        let before = app.focused_buf().rope.to_string();

        app.run_action(palette::Action::ToggleMarkdown);
        assert!(app.focused_pane().md_view, "md_view flag did not set");
        term.draw(|f| ui::render(f, &mut app))?;
        let t = screen_text(&term);
        assert!(t.contains("— markdown"), "markdown view title missing: {t}");
        assert!(t.contains("Title"), "reading-mode did not render the heading text");
        assert!(app.focused_pane().md_rendered_total.get() > 12, "render should measure a doc taller than the viewport");

        // Down scrolls the document (md_scroll) and must NOT move the cursor.
        let cr0 = app.focused_pane().cursor_row;
        app.handle_key(k(KeyCode::Down))?;
        assert_eq!(app.focused_pane().md_scroll, 1, "arrow did not scroll the document");
        assert_eq!(app.focused_pane().cursor_row, cr0, "reading-mode must not move the cursor");
        // M-> (editor's bottom-of-file) jumps to the exact cap; M-< returns to top.
        app.handle_key(KeyEvent::new(KeyCode::Char('>'), KeyModifiers::ALT))?;
        let cap = app.focused_pane().md_rendered_total.get().saturating_sub(app.focused_pane().view_h.max(1));
        assert_eq!(app.focused_pane().md_scroll, cap, "M-> did not jump to the bottom");
        // Clamp holds: another Down at the bottom does not run past the cap.
        app.handle_key(k(KeyCode::Down))?;
        assert_eq!(app.focused_pane().md_scroll, cap, "scroll ran past the measured end");
        app.handle_key(KeyEvent::new(KeyCode::Char('<'), KeyModifiers::ALT))?;
        assert_eq!(app.focused_pane().md_scroll, 0, "M-< did not jump to the top");

        // Read-only: typing / Enter / paste leave the buffer byte-for-byte identical.
        typ(&mut app, "SHOULD_NOT_APPEAR")?;
        app.handle_key(k(KeyCode::Enter))?;
        app.run_action(palette::Action::Paste);
        assert_eq!(app.focused_buf().rope.to_string(), before, "md_view did not block edits");

        // Toggle OFF → normal editing resumes.
        app.run_action(palette::Action::ToggleMarkdown);
        assert!(!app.focused_pane().md_view, "md_view flag did not clear");
        typ(&mut app, "EDITS")?;
        assert!(app.focused_buf().rope.to_string().contains("EDITS"), "editing broken after md_view off");
        println!("[selfcheck] markdown reading-mode ...... PASS");
    }

    // 40c. Editor feel: the mouse wheel scrolls the *viewport* in a normal editor and
    //      the *document* in reading-mode (not just the cursor); bracket_pair and
    //      fuzzy_positions back the passive-bracket and fuzzy-highlight cues.
    {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let wheel = |app: &mut App, down: bool| app.handle_mouse(MouseEvent {
            kind: if down { MouseEventKind::ScrollDown } else { MouseEventKind::ScrollUp },
            column: 3, row: 3, modifiers: KeyModifiers::NONE,
        });

        // Normal editor: a doc taller than the viewport; wheel-down raises scroll_row
        // without the cursor having to walk to the edge first.
        let mut app = App::new(None)?;
        let mut term = Terminal::new(TestBackend::new(80, 12))?;
        for i in 0..60 { typ(&mut app, &format!("line {i}"))?; app.handle_key(k(KeyCode::Enter))?; }
        app.run_action(palette::Action::GoTop);
        term.draw(|f| ui::render(f, &mut app))?; // sets view_h
        assert_eq!(app.focused_pane().scroll_row, 0, "GoTop did not reset scroll");
        wheel(&mut app, true);
        let s1 = app.focused_pane().scroll_row;
        assert!(s1 > 0, "wheel-down did not scroll the editor viewport");
        wheel(&mut app, false);
        assert!(app.focused_pane().scroll_row < s1, "wheel-up did not scroll back");

        // bracket_pair: cursor on '(' resolves its ')'.
        {
            let mut b = App::new(None)?;
            typ(&mut b, "(x)")?;
            b.run_action(palette::Action::GoTop);
            let bp = b.bracket_pair();
            assert_eq!(bp, Some(((0, 0), (0, 2))), "bracket_pair did not match the pair: {bp:?}");
        }

        // Reading-mode: the wheel scrolls the rendered document (md_scroll).
        app.run_action(palette::Action::ToggleMarkdown);
        term.draw(|f| ui::render(f, &mut app))?;
        let m0 = app.focused_pane().md_scroll;
        wheel(&mut app, true);
        assert!(app.focused_pane().md_scroll > m0, "wheel did not scroll reading-mode");

        println!("[selfcheck] editor feel (wheel/bracket) ... PASS");
    }

    // 40d. Themes: resolve() yields distinct palettes; the default is byte-identical
    //      Mission Control; parse_color handles hex + named; a theme reaches the frame.
    {
        use ratatui::style::Color as RColor;
        let mc = themes::resolve(None);
        let hk = themes::resolve(Some("hacker"));
        let pp = themes::resolve(Some("paper"));
        assert_eq!(mc.accent, RColor::Rgb(217, 119, 87), "default accent is not Mission Control clay");
        assert_eq!(mc.text, RColor::White, "default text should stay ANSI white (terminal-honoring)");
        assert_eq!(hk.accent, RColor::Rgb(57, 255, 20), "hacker accent is not neon green");
        assert_eq!(hk.surface, RColor::Rgb(0, 0, 0), "hacker surface is not true black");
        assert_ne!(hk.accent, mc.accent, "theme swap did not change the accent");
        assert_eq!(pp.surface, RColor::Rgb(244, 236, 224), "paper surface is not cream");
        assert!(!themes::exists("no-such-theme"), "unknown theme should not resolve");
        assert!(themes::exists("eclipse") && themes::exists("mission-control"), "bundled themes missing");
        assert_eq!(themes::parse_color("#39ff14"), Some(RColor::Rgb(57, 255, 20)), "hex parse");
        assert_eq!(themes::parse_color("darkgray"), Some(RColor::DarkGray), "named parse");
        assert_eq!(themes::parse_color("nope"), None, "bad color should be None");
        // A theme's colors actually reach a rendered cell: type to dismiss the splash,
        // so the focused editor pane draws its accent-colored border.
        let mut app = App::new(None)?;
        typ(&mut app, "x")?; // dismiss the day-0 banner → editor pane with an accent border
        app.tuning.palette = hk;
        let mut term = Terminal::new(TestBackend::new(60, 8))?;
        term.draw(|f| ui::render(f, &mut app))?;
        let green = RColor::Rgb(57, 255, 20);
        let has_green = term.backend().buffer().content().iter().any(|c| c.fg == green || c.bg == green);
        assert!(has_green, "the hacker palette did not reach any rendered cell");
        // The committed surface (black) fills the whole frame — not just where a
        // widget sets a bg — so a themed background is consistent everywhere.
        let black = RColor::Rgb(0, 0, 0);
        let count_black = |t: &Terminal<TestBackend>| t.backend().buffer().content().iter().filter(|c| c.bg == black).count();
        assert!(count_black(&term) > 60 * 8 / 2, "the surface did not fill most of the frame");
        // opaque_background = 0 forces transparent (terminal bg) even for a colored theme.
        app.tuning.opaque_background = 0;
        term.draw(|f| ui::render(f, &mut app))?;
        assert!(count_black(&term) < 60 * 8 / 4, "opaque_background=0 should not paint the surface");
        // The Theme ▸ picker reads live from disk (bundled + user), as SetTheme rows.
        let tmenu = palette::menu_for("themes");
        assert!(tmenu.len() >= 4, "theme menu should list the bundled themes ({} found)", tmenu.len());
        assert!(tmenu.iter().all(|m| matches!(m.kind, palette::ItemKind::Run(palette::Action::SetTheme(_)))), "theme rows must be SetTheme actions");
        assert!(tmenu.iter().any(|m| m.label.contains("Mission Control")), "Mission Control missing from the theme menu");
        println!("[selfcheck] themes (resolve/parse/render) . PASS");
    }

    // 40e. Live theme picker (beta): SetTheme applies the resolved palette and
    //      persists the choice. HOME is isolated so it never touches a real config.
    {
        let saved = std::env::var(sys::paths::HOME_ENV).ok();
        let tmp = std::env::temp_dir().join(format!("mars-theme-{}", std::process::id()));
        std::fs::create_dir_all(tmp.join(".mars"))?;
        std::env::set_var(sys::paths::HOME_ENV, &tmp);

        let mut app = App::new(None)?;
        let before = app.tuning.palette.accent;
        app.run_action(palette::Action::SetTheme("hacker".into()));
        assert_eq!(app.tuning.palette.accent, ratatui::style::Color::Rgb(57, 255, 20), "SetTheme did not apply the live palette");
        assert_ne!(app.tuning.palette.accent, before, "palette unchanged");
        assert_eq!(config::selected_theme().as_deref(), Some("hacker"), "SetTheme did not persist the theme");

        match saved {
            Some(h) => std::env::set_var(sys::paths::HOME_ENV, h),
            None => std::env::remove_var(sys::paths::HOME_ENV),
        }
        let _ = std::fs::remove_dir_all(&tmp);
        println!("[selfcheck] set theme live (beta) ..... PASS");
    }

    // 40f. Color-honesty guard: after the theming factor, no bare ANSI `Color::Named`
    //      literal may appear in ui.rs — every colored cell must resolve through a
    //      palette token. `Color::Reset`/`Rgb`/`Indexed` stay allowed (terminal
    //      passthrough, the RGB constructor, the default-look sentinel). This is the
    //      color analogue of the keybinding honesty invariant, with build teeth.
    {
        const UI_SRC: &str = include_str!("ui.rs");
        for forbidden in [
            "Color::White", "Color::Gray", "Color::DarkGray", "Color::Black",
            "Color::Green", "Color::Red", "Color::Yellow", "Color::Blue",
            "Color::Cyan", "Color::Magenta",
        ] {
            assert!(
                !UI_SRC.contains(forbidden),
                "color-honesty breach: `{forbidden}` is hardcoded in ui.rs — use a palette token"
            );
        }
        println!("[selfcheck] color-honesty guard ........ PASS");
    }

    // 40g. Syntax engine: with the `syntax` feature, a Rust snippet highlights into
    //      several palette-derived colors (keyword = accent); without it, the stub
    //      returns None and the caller renders plain.
    {
        let pal = tuning::Palette::mission_control();
        let hl = syntax::highlight("fn main() { let n = 42; }\n", "rs", &pal);
        #[cfg(feature = "syntax")]
        {
            use ratatui::style::Color;
            let lines = hl.expect("rust should highlight with the syntax feature on");
            let colors: std::collections::HashSet<Option<Color>> =
                lines.iter().flatten().map(|(st, _)| st.fg).collect();
            assert!(colors.len() >= 3, "expected several syntax colors, got {}", colors.len());
            assert!(colors.contains(&Some(pal.accent)), "a keyword should take the theme accent");

            // TypeScript is bundled (not in syntect's defaults) — `.ts` must color,
            // with `interface`/type keywords taking the accent.
            let ts = syntax::highlight(
                "interface User { id: number }\nconst greet = (): string => 42;\n", "ts", &pal,
            ).expect("typescript should highlight (bundled grammar)");
            let ts_colors: std::collections::HashSet<Option<Color>> =
                ts.iter().flatten().map(|(st, _)| st.fg).collect();
            assert!(ts_colors.len() >= 3, "expected several TS colors, got {}", ts_colors.len());
            assert!(ts_colors.contains(&Some(pal.accent)), "a TS keyword should take the theme accent");
        }
        #[cfg(not(feature = "syntax"))]
        assert!(hl.is_none(), "the syntax stub must return None (plain render)");

        // Worker seam: the real background worker streams chunks back over the channel,
        // colors keywords with the theme accent, and ends with a `complete` chunk.
        // Deterministic — recv() blocks on the thread, no polling.
        #[cfg(feature = "syntax")]
        {
            use ratatui::style::Color;
            let code = "fn main() {\n    let n = 42;\n}\n";
            let job = app::SyntaxJob {
                buf_id: 999, rev: 7, palette_id: 3,
                code: code.into(), ext: "rs".into(),
                palette: tuning::Palette::mission_control(), viewport_bottom: 2,
            };
            let (tx, rx) = std::sync::mpsc::channel();
            syntax::highlight_stream(job, tx);
            let (mut saw_accent, mut saw_complete, mut chunks) = (false, false, 0u32);
            while let Ok(app::SyntaxEvent::Chunk { styles, complete, rev, buf_id, .. }) =
                rx.recv_timeout(std::time::Duration::from_secs(5))
            {
                assert_eq!((rev, buf_id), (7, 999), "worker echoed the wrong job identity");
                if styles.iter().flatten().any(|s| s.fg == Some(Color::Rgb(217, 119, 87))) {
                    saw_accent = true;
                }
                chunks += 1;
                if complete { saw_complete = true; break; }
            }
            assert!(saw_accent, "worker did not color keywords with the theme accent");
            assert!(saw_complete, "worker stream never completed");
            assert!(chunks >= 1, "worker produced no chunks");
        }

        // Render check: the async pipeline paints the keyword accent into the editor.
        // Drive it deterministically — feed a chunk through the real channel and let
        // tick() drain it into the cache (no worker-thread timing), then render. When
        // syntax is toggled OFF, the same buffer must render with NO accent (plain).
        #[cfg(feature = "syntax")]
        {
            use ratatui::style::Color;
            let dir = std::env::temp_dir().join(format!("mars-syn-{}", std::process::id()));
            std::fs::create_dir_all(&dir)?;
            let f = dir.join("probe.rs");
            let code = "fn main() {\n    let n = 42;\n}\n";
            std::fs::write(&f, code)?;
            let mut app = App::new(None)?;
            app.open_file_in_new_tab(f.to_str().unwrap());
            app.show_splash = false; // the start banner overlays the editor until a keypress
            app.tuning.palette = tuning::Palette::mission_control(); // decouple from the user's theme
            // On by default now; the cold cache still renders plain until the worker delivers.
            assert!(app.syntax_on, "syntax highlighting should be on by default");
            let mut term = Terminal::new(TestBackend::new(60, 10))?;
            // The accent hue also paints chrome (focus border, cursor-line marker), so
            // count accent cells and require syntax to add MORE (the colored keywords).
            let accent_cells = |t: &Terminal<TestBackend>| {
                t.backend().buffer().content().iter().filter(|c| c.fg == Color::Rgb(217, 119, 87)).count()
            };
            // Baseline: with an empty cache the buffer paints plain (only chrome accent).
            app.syntax_on = false;
            term.draw(|f| ui::render(f, &mut app))?;
            let plain = accent_cells(&term);

            // Build the per-char styles the worker would produce, feed them through the
            // channel, and let tick() apply them to the cache.
            let per_char: Vec<Vec<ratatui::style::Style>> = syntax::highlight(code, "rs", &app.tuning.palette)
                .unwrap()
                .iter()
                .map(|runs| runs.iter().flat_map(|(st, txt)| std::iter::repeat(*st).take(txt.chars().count())).collect())
                .collect();
            let buf_id = app.focused_buf_id();
            app.syntax_on = true;
            // The drain applies only the pass we're waiting for — declare it, then feed
            // the chunk through the real channel and let tick() merge it into the cache.
            app.syntax_want.insert(buf_id, (1, 1));
            app.syntax_tx.send(app::SyntaxEvent::Chunk {
                buf_id, rev: 1, palette_id: 1, start_line: 0, styles: per_char, complete: true,
            }).unwrap();
            app.tick(); // drains syntax_rx → apply_syntax_chunk → cache
            assert!(app.syntax_cache.contains_key(&buf_id), "chunk did not reach the cache");
            term.draw(|f| ui::render(f, &mut app))?;
            assert!(accent_cells(&term) > plain, "syntax colors (keyword accent) did not reach the editor pane");
            let _ = std::fs::remove_dir_all(&dir);
        }
        println!("[selfcheck] syntax highlight (engine + cache) .. PASS");
    }

    // 40h. No-flash cache update: a fresh pass OVERWRITES in place (never clears), so
    //      lines not yet re-streamed keep their old colors instead of flashing white;
    //      a straggler chunk from a superseded pass is dropped.
    #[cfg(feature = "syntax")]
    {
        use ratatui::style::{Color, Style};
        let mut app = App::new(None)?;
        let bid = 4242usize;
        let x = Style::default().fg(Color::Rgb(1, 1, 1)); // pass A color
        let y = Style::default().fg(Color::Rgb(2, 2, 2)); // pass B color
        let fg = |app: &App, line: usize| app.syntax_cache[&bid].lines[line][0].fg;
        // Pass A: three lines, complete.
        app.syntax_want.insert(bid, (1, 9));
        app.syntax_tx.send(app::SyntaxEvent::Chunk {
            buf_id: bid, rev: 1, palette_id: 9, start_line: 0,
            styles: vec![vec![x], vec![x], vec![x]], complete: true,
        }).unwrap();
        app.tick();
        assert_eq!(app.syntax_cache[&bid].lines.len(), 3);
        // An edit supersedes → pass B. Its PARTIAL first chunk recolors only line 0.
        app.syntax_want.insert(bid, (2, 9));
        app.syntax_tx.send(app::SyntaxEvent::Chunk {
            buf_id: bid, rev: 2, palette_id: 9, start_line: 0, styles: vec![vec![y]], complete: false,
        }).unwrap();
        app.tick();
        assert_eq!(fg(&app, 0), Some(Color::Rgb(2, 2, 2)), "new pass did not recolor line 0");
        assert_eq!(fg(&app, 1), Some(Color::Rgb(1, 1, 1)), "re-highlight cleared un-streamed lines (would flash white)");
        assert_eq!(app.syntax_cache[&bid].lines.len(), 3, "cache lost coverage mid-pass");
        // A late straggler from the superseded pass A must be ignored.
        app.syntax_tx.send(app::SyntaxEvent::Chunk {
            buf_id: bid, rev: 1, palette_id: 9, start_line: 1, styles: vec![vec![y]], complete: true,
        }).unwrap();
        app.tick();
        assert_eq!(fg(&app, 1), Some(Color::Rgb(1, 1, 1)), "a stale straggler clobbered the current pass");
        // Pass B completes: the rest recolors and the cache advances to rev 2.
        app.syntax_tx.send(app::SyntaxEvent::Chunk {
            buf_id: bid, rev: 2, palette_id: 9, start_line: 1, styles: vec![vec![y], vec![y]], complete: true,
        }).unwrap();
        app.tick();
        assert!(app.syntax_cache[&bid].lines.iter().all(|l| l[0].fg == Some(Color::Rgb(2, 2, 2))), "completed pass not fully applied");
        assert_eq!(app.syntax_cache[&bid].rev, 2);
        println!("[selfcheck] syntax cache (no-flash overwrite) .. PASS");
    }

    // 40i. Cache splicing: Enter splits the cached color-line at the cursor (colors
    //      travel with their characters, so lines below don't show shifted colors);
    //      Backspace-join merges them back. Driven through the real key path.
    {
        use ratatui::style::{Color, Style};
        let mut app = App::new(None)?;
        typ(&mut app, "abcdef")?;
        let bid = app.focused_buf_id();
        // A distinct color per character so a mis-split is visible.
        let styled: Vec<Style> = (0..6).map(|i| Style::default().fg(Color::Rgb(i as u8, 0, 0))).collect();
        app.syntax_cache.insert(bid, app::SyntaxCache { rev: 1, palette_id: 1, lines: vec![styled] });
        // Cursor to column 3 (between 'c' and 'd'), then Enter.
        for _ in 0..3 { app.handle_key(k(KeyCode::Left))?; }
        app.handle_key(k(KeyCode::Enter))?;
        let fgs = |app: &App, line: usize| -> Vec<Option<Color>> {
            app.syntax_cache[&bid].lines[line].iter().map(|s| s.fg).collect()
        };
        assert_eq!(app.syntax_cache[&bid].lines.len(), 2, "Enter did not split the cached line");
        assert_eq!(fgs(&app, 0), vec![Some(Color::Rgb(0,0,0)), Some(Color::Rgb(1,0,0)), Some(Color::Rgb(2,0,0))],
            "split kept the wrong colors on the first line (colors did not travel with chars)");
        assert_eq!(fgs(&app, 1), vec![Some(Color::Rgb(3,0,0)), Some(Color::Rgb(4,0,0)), Some(Color::Rgb(5,0,0))],
            "split kept the wrong colors on the new line");
        // Backspace at column 0 joins the line back and merges the colors.
        app.handle_key(k(KeyCode::Backspace))?;
        assert_eq!(app.syntax_cache[&bid].lines.len(), 1, "Backspace did not join the cached lines");
        assert_eq!(fgs(&app, 0).len(), 6, "join lost characters' colors");
        println!("[selfcheck] syntax cache (line splice) .. PASS");
    }

    // 41. LLM debug logging: a record round-trips to JSONL with real token totals,
    //     stats aggregates it, and logging is a strict no-op when disabled.
    {
        // SAFETY: isolate the log to a temp dir so the suite NEVER touches the
        // user's real ~/.mars/logs (a day of captured eval data).
        let sc_dir = std::env::temp_dir().join(format!("mars-llmtest-{}", std::process::id()));
        std::fs::create_dir_all(&sc_dir)?;
        std::env::set_var("MARS_LLM_LOG_DIR", &sc_dir);
        assert!(llm_log::log_path().starts_with(&sc_dir), "log path not isolated to temp dir!");
        let _ = std::fs::remove_file(llm_log::log_path()); // start clean
        std::env::set_var("MARS_LLM_DEBUG", "1");
        let input = vec![serde_json::json!({"role": "user", "content": "hi"})];
        llm_log::record(&llm_log::CallRecord {
            call_id: 1, task: "ask", provider: "groq", model: "qwen/qwen3-32b", retrieval: "none",
            prompt_tokens: 100, completion_tokens: 20, latency_ms: 500,
            ok: true, error: None, input: &input, output: "hello",
        });
        let logged = std::fs::read_to_string(llm_log::log_path())?;
        assert!(logged.contains("\"task\":\"ask\"") && logged.contains("qwen/qwen3-32b"), "call not logged");
        assert!(logged.contains("\"total_tokens\":120"), "token total not computed");
        assert!(logged.contains("\"call_id\":1") && logged.contains("\"session_id\""), "call_id/session_id not logged");
        // Session boundary events + outcome sink round-trip.
        llm_log::session_start();
        llm_log::record_outcome(1, Some("git status"), false, false);
        let outc = std::fs::read_to_string(llm_log::outcomes_path())?;
        assert!(outc.contains("\"call_id\":1") && outc.contains("git status"), "outcome not logged");
        assert!(std::fs::read_to_string(llm_log::log_path())?.contains("session_start"), "session event not logged");
        llm_log::stats(false, false, false, None)?; // aggregation runs cleanly, skips session events
        // Disabled → strictly no writes.
        std::env::remove_var("MARS_LLM_DEBUG");
        let before = std::fs::metadata(llm_log::log_path())?.len();
        llm_log::record(&llm_log::CallRecord {
            call_id: 2, task: "ask", provider: "groq", model: "m", retrieval: "none",
            prompt_tokens: 1, completion_tokens: 1, latency_ms: 1,
            ok: true, error: None, input: &input, output: "x",
        });
        assert_eq!(std::fs::metadata(llm_log::log_path())?.len(), before, "record() wrote while disabled");
        let _ = std::fs::remove_file(llm_log::log_path());
        let _ = std::fs::remove_file(llm_log::outcomes_path());
        let _ = std::fs::remove_dir_all(&sc_dir);
        std::env::remove_var("MARS_LLM_LOG_DIR");
        println!("[selfcheck] llm debug log + stats ..... PASS");
    }

    // 41b. Global `~/.mars/config.json`: an `env` map is exported into the process
    //      environment, but the real environment wins, and a malformed file is a
    //      no-op (never blocks startup).
    {
        let dir = std::env::temp_dir().join(format!("mars-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let cfg = dir.join("config.json");

        // A fresh var is exported from the file.
        std::env::remove_var("MARS_TEST_CFG");
        std::fs::write(&cfg, r#"{"env":{"MARS_TEST_CFG":"from_file","MARS_TEST_CFG_HELD":"file"}}"#)?;
        apply_config_from(&cfg);
        assert_eq!(std::env::var("MARS_TEST_CFG").as_deref(), Ok("from_file"), "config env not exported");

        // The real environment wins over the file.
        std::env::set_var("MARS_TEST_CFG_HELD", "real");
        apply_config_from(&cfg);
        assert_eq!(std::env::var("MARS_TEST_CFG_HELD").as_deref(), Ok("real"), "file clobbered the real env");

        // A malformed file is ignored without panicking or setting anything.
        std::fs::write(&cfg, "{ not json")?;
        apply_config_from(&cfg); // must not panic
        // A missing file is a clean no-op.
        apply_config_from(&dir.join("nope.json"));

        std::env::remove_var("MARS_TEST_CFG");
        std::env::remove_var("MARS_TEST_CFG_HELD");
        let _ = std::fs::remove_dir_all(&dir);
        println!("[selfcheck] ~/.mars/config.json ........ PASS");
    }

    // 42. Retrieval: BM25 ranks the relevant doc first; memory-mode parsing
    //     (memory builds only — the stub has no ranker and an inert mode).
    #[cfg(feature = "memory")]
    {
        let docs = vec![
            "git status shows the working tree and staged changes".to_string(),
            "docker compose up starts the containers".to_string(),
            "list files in a directory with ls -la".to_string(),
        ];
        let top = retrieval::rank("how do I check the git working tree", &docs, 1);
        assert_eq!(top.first().copied(), Some(0), "BM25 did not rank the git doc first");
        std::env::set_var("MARS_MEMORY", "history");
        assert!(retrieval::MemoryMode::from_env().includes_history(), "MARS_MEMORY=history not parsed");
        std::env::set_var("MARS_MEMORY", "docs");
        assert!(retrieval::MemoryMode::from_env().includes_docs(), "MARS_MEMORY=docs not parsed");
        std::env::remove_var("MARS_MEMORY");
        assert_eq!(retrieval::MemoryMode::from_env().as_str(), "none");
        println!("[selfcheck] retrieval (bm25 + mode) ... PASS");
    }

    // 43. The shift report — the save-state restore. Tier-0 triage table; batch
    //     reply parsing; end-to-end keyless flow: watched pane fails while
    //     detached → deterministic verdict → reattach overlay, failures first,
    //     any key resumes, Enter types the suggestion; knob gates it all.
    {
        use briefing::{classify, triage, Verdict};
        // Triage: exit codes are ground truth, tails refine.
        assert_eq!(triage("all done", Some(0), false).verdict, Verdict::Done);
        assert!(!triage("ok\n", Some(0), false).ambiguous, "clean exit should not need a model");
        let t = triage("...", Some(137), false);
        assert_eq!(t.verdict, Verdict::Failed);
        assert!(t.text.contains("137") && t.ambiguous, "nonzero exit wants a modeled cause: {}", t.text);
        assert_eq!(triage("Continue? [y/N]", None, true).verdict, Verdict::Blocked);
        assert!(!triage("Continue? [y/N]", None, true).ambiguous);
        assert_eq!(triage("CUDA out of memory. Tried to allocate", None, true).verdict, Verdict::Failed);
        assert_eq!(triage("epoch 3/10  loss 0.42  1.2 it/s", None, true).verdict, Verdict::Running);
        assert!(!triage("epoch 3/10  loss 0.42  1.2 it/s", None, true).ambiguous);
        assert!(triage("some quiet text", None, true).ambiguous, "quiet-alive is the model's case");
        // Auto-watch noise gate: an idle shell / clean user-quit is NOT worth a
        // verdict (this is the "user quit" flood fix); failures, blocks, nonzero
        // exits, and real completed runs ARE.
        use briefing::is_noteworthy;
        assert!(!is_noteworthy("user@host:~/proj$ ", None), "idle prompt should be silent");
        assert!(!is_noteworthy("$ exit\nexit", Some(0)), "clean user-quit should be silent");
        assert!(!is_noteworthy("logout", Some(0)), "logout should be silent");
        assert!(is_noteworthy("error: build failed\n$ ", None), "a failure must speak");
        assert!(is_noteworthy("Continue? [y/N]", None), "a blocked prompt must speak");
        assert!(is_noteworthy("segfault", Some(139)), "a nonzero exit must speak");
        // A mars pane runs $SHELL, so a clean Some(0) exit is the shell ending
        // (user left), never a completed run — a finished command is a QUIET
        // (None) event and speaks only if it shows a failure/block.
        assert!(!is_noteworthy("Finished in 3.2s\n$ ", Some(0)), "clean shell exit stays silent");
        // Verdict-string classing (model/tier-0 authored prefixes).
        assert_eq!(classify("blocked: wants a password", Verdict::Done), Verdict::Blocked);
        assert_eq!(classify("failed: linker error", Verdict::Done), Verdict::Failed);
        assert_eq!(classify("done: tests green", Verdict::Failed), Verdict::Done);
        assert_eq!(briefing::fmt_secs(4212), "1h10m");

        // End-to-end, keyless and hermetic: two watched panes conclude while
        // detached — one fails, one succeeds — a third keeps running.
        let mut app = App::new(None)?;
        app.tuning.mission_briefing = 2;
        app.tuning.mission_briefing_animate = 0; // instant reveal → deterministic render
        app.tuning.watch_quiet_secs = 1000; // quiet timer out of the picture
        app.handle_key(kc(KeyCode::Char(' ')))?;
        app.handle_key(k(KeyCode::Char('!')))?;
        typ(&mut app, "exit 3")?; // ends the pane's shell itself → real exit code
        app.handle_key(k(KeyCode::Enter))?;
        let fail_tid = match app.focused_pane().content {
            pane::PaneContent::Terminal(id) => id,
            _ => panic!("no terminal"),
        };
        app.run_action(palette::Action::WatchPane);
        app.on_detach();
        // Let the shell run and exit; drain events (queues the exit trigger,
        // keyless fire produces the deterministic tier-0 verdict).
        wait_until(|| {
            app.frame_tick += 3; // simulated time passes while detached
            app.tick();
            app.watches.get(&fail_tid).map(|w| w.verdict.is_some()).unwrap_or(false)
        });
        assert!(
            app.watches.get(&fail_tid).and_then(|w| w.verdict.clone())
                .map(|v| v.starts_with("failed"))
                .unwrap_or(false),
            "keyless exit-3 did not produce a deterministic failed verdict"
        );
        app.on_attach();
        let rep = app.shift_report.as_ref().expect("no shift report after eventful away");
        assert!(rep.rows.iter().any(|r| r.verdict == Verdict::Failed), "failed row missing");
        assert_eq!(rep.rows.first().map(|r| r.verdict), Some(Verdict::Failed), "failures must lead");
        // The narrative opens with the deterministic plain-English line (keyless
        // → no model streams, so the template stands).
        assert!(rep.narrative.to_lowercase().contains("failed"), "narrative missing the failure: {}", rep.narrative);
        assert!(app.notices.is_empty() || app.shift_report.is_some(), "report should subsume notices");
        // Renders as a full overlay: title, the briefing prose, the failure glyph.
        let mut term = Terminal::new(TestBackend::new(100, 30))?;
        term.draw(|f| ui::render(f, &mut app))?;
        let t = screen_text(&term);
        assert!(t.contains("MISSION BRIEFING"), "overlay title missing");
        assert!(t.contains("✗"), "failure glyph missing from overlay");
        assert!(t.to_lowercase().contains("welcome back"), "plain-English briefing missing from overlay");
        // Any key resumes: swallowed, report gone, workspace intact.
        app.handle_key(k(KeyCode::Char('x')))?;
        assert!(app.shift_report.is_none(), "key did not dismiss the report");
        // The streamed briefing: first ShiftDelta replaces the template, the rest
        // append; ShiftDone stops the stream; an empty stream keeps a briefing.
        app.shift_report = Some(briefing::ShiftReport {
            away_secs: 1, mission: None,
            rows: vec![
                briefing::ReportRow {
                    verdict: Verdict::Failed, tab: "train".into(), text: "failed: OOM".into(),
                    ago_secs: None, dur_secs: None, term_id: None,
                    cwd: None, exit: Some(137), error_excerpt: Some("CUDA out of memory".into()), settling: false,
                },
                briefing::ReportRow {
                    verdict: Verdict::Done, tab: "build".into(), text: "training finished".into(),
                    ago_secs: None, dur_secs: Some(3600), term_id: None, // long success → ★
                    cwd: None, exit: None, error_excerpt: None, settling: false,
                },
            ],
            suggestion: None, narrative: "2 failed.".into(),
            narrative_streaming: true, narrative_from_model: false, facts: String::new(),
            stream_started_at: None, shown_at: std::time::Instant::now(),
        });
        // A four-block briefing (greeting / summary / action items / sign-off),
        // blank-line separated, streams in and replaces the template. The good-news
        // ★ and the sign-off (below the manifest) render.
        app.agent_tx.send(agent::AgentEvent::ShiftDelta { text: "Welcome back, captain.\n\n".into() })?;
        app.agent_tx.send(agent::AgentEvent::ShiftDelta { text: "The trainer OOM'd at epoch 3.\n\n".into() })?;
        app.agent_tx.send(agent::AgentEvent::ShiftDelta { text: "Rerun with a smaller batch.\n\n".into() })?;
        app.agent_tx.send(agent::AgentEvent::ShiftDelta { text: "We'll get it, captain.".into() })?;
        app.agent_tx.send(agent::AgentEvent::ShiftDone)?;
        app.tick();
        let rep = app.shift_report.as_ref().unwrap();
        assert!(rep.narrative.starts_with("Welcome back, captain.") && rep.narrative.contains("We'll get it"),
            "deltas did not replace+append the four-block briefing");
        assert!(!rep.narrative_streaming, "ShiftDone did not stop the stream");
        // Renders: the prose blocks, the failure "why" line, and the sign-off
        // below the manifest (after the row glyphs).
        let mut term = Terminal::new(TestBackend::new(100, 30))?;
        term.draw(|f| ui::render(f, &mut app))?;
        let t = screen_text(&term);
        assert!(t.contains("Welcome back") && t.contains("OOM'd at epoch 3") && t.contains("smaller batch"),
            "briefing blocks did not fully render");
        assert!(t.contains("exit 137") && t.contains("CUDA out of memory"), "failure detail missing");
        assert!(t.contains("We'll get it"), "sign-off did not render");
        assert!(t.find("We'll get it").unwrap() > t.find("exit 137").unwrap(),
            "sign-off must render below the manifest");
        // Systems-board manifest: the severity stripe renders; a long success
        // gets the good-news ★.
        assert!(t.contains("▎"), "manifest severity stripe missing");
        assert!(t.contains("★"), "good-news ★ on the long success missing");
        app.shift_report = None;
        // Fail-KEEP: the enrichment call finishing with NO model output (error /
        // timeout / no key reachable from a detached daemon) must KEEP the
        // deterministic briefing and render it. The mission board (clock, manifest,
        // greeting) IS the briefing; dismissing it was the "flashes then vanishes"
        // bug that hid the briefing on every reattach a daemon couldn't reach a model.
        app.shift_report = Some(briefing::ShiftReport {
            away_secs: 60, mission: None, rows: vec![], suggestion: None,
            narrative: "Welcome back — all quiet.".into(),
            narrative_streaming: true, narrative_from_model: false, facts: String::new(),
            stream_started_at: None, shown_at: std::time::Instant::now(),
        });
        app.agent_tx.send(agent::AgentEvent::ShiftDone)?; // no delta preceded it → failed call
        app.tick();
        let rep = app.shift_report.as_ref()
            .expect("a failed enrichment call must KEEP the deterministic briefing");
        assert!(!rep.narrative_streaming, "ShiftDone must settle the stream even when the call failed");
        let mut term = Terminal::new(TestBackend::new(100, 30))?;
        term.draw(|f| ui::render(f, &mut app))?;
        let t = screen_text(&term);
        assert!(t.to_lowercase().contains("welcome back") || t.contains("all quiet"),
            "the deterministic briefing must render after a failed enrichment call");
        app.shift_report = None;
        // Boot polish (animate=1): while the call is in flight a mission-control word
        // flashes in place of the deterministic backup line, so the prose never
        // visibly swaps a stub; and the model text types in behind a cursor rather
        // than appearing whole. Both are gated on the animate knob.
        app.tuning.mission_briefing_animate = 1;
        app.shift_report = Some(briefing::ShiftReport {
            away_secs: 5, mission: None, rows: vec![], suggestion: None,
            narrative: "2 failed.".into(), // the deterministic backup — must NOT be shown
            narrative_streaming: true, narrative_from_model: false, facts: String::new(),
            stream_started_at: None, shown_at: std::time::Instant::now(),
        });
        let mut term = Terminal::new(TestBackend::new(100, 30))?;
        term.draw(|f| ui::render(f, &mut app))?;
        let t = screen_text(&term);
        assert!(briefing::BRIEF_LOADING.iter().any(|w| t.contains(w)),
            "loading state should flash a mission-control word");
        assert!(!t.contains("2 failed"), "the backup line must not show under the loading flash");
        // Typewriter: with the stream just begun, the tail of a long briefing has not
        // been revealed yet (it types in on the clock), though the chrome is already up.
        if let Some(rep) = app.shift_report.as_mut() {
            rep.narrative_from_model = true;
            rep.narrative = "Welcome back, captain. This long briefing types itself in gradually behind a cursor, not all at once.".into();
            rep.stream_started_at = Some(std::time::Instant::now());
        }
        let mut term = Terminal::new(TestBackend::new(100, 30))?;
        term.draw(|f| ui::render(f, &mut app))?;
        let t = screen_text(&term);
        assert!(t.contains("MISSION BRIEFING"), "chrome should be up during the typewriter");
        assert!(!t.contains("not all at once"), "typewriter must not reveal the tail instantly");
        app.tuning.mission_briefing_animate = 0;
        app.shift_report = None;
        // Iteration mode: knob=2 greets on EVERY return, even a quiet one —
        // the overlay is present with zero rows and a "welcome back" line.
        let mut app = App::new(None)?;
        app.tuning.mission_briefing = 2;
        app.tuning.mission_briefing_animate = 0;
        app.on_detach();
        app.frame_tick += 20;
        app.on_attach(); // nothing happened while away
        let rep = app.shift_report.as_ref().expect("quiet return must still greet (iteration mode)");
        assert!(rep.rows.is_empty(), "quiet return should have no rows");
        assert!(rep.narrative.to_lowercase().contains("welcome back"), "quiet briefing missing greeting");
        let mut term = Terminal::new(TestBackend::new(100, 30))?;
        term.draw(|f| ui::render(f, &mut app))?;
        assert!(screen_text(&term).contains("all quiet"), "quiet-return caption missing");
        app.shift_report = None;
        // Knob 1 = classic notice; knob 0 = nothing (digest still scoped).
        let mut app = App::new(None)?;
        app.tuning.mission_briefing = 1;
        app.on_detach();
        app.frame_tick += 10;
        app.push_away(app::AwayKind::NeedsYou, "failed: x".into(), None);
        app.on_attach();
        assert!(app.shift_report.is_none(), "knob=1 must not build the overlay");
        assert!(app.notices.iter().any(|n| n.text.contains("while away")), "knob=1 lost the notice");
        let mut app = App::new(None)?;
        app.tuning.mission_briefing = 0;
        app.on_detach();
        app.frame_tick += 10;
        app.push_away(app::AwayKind::NeedsYou, "failed: x".into(), None);
        app.on_attach();
        assert!(app.shift_report.is_none() && app.notices.is_empty(), "knob=0 must be silent");
        // Continuity: briefings are logged and the last one round-trips for the
        // next return's "since last time." Session-scoped.
        let bwl = std::env::temp_dir().join(format!("mars-brief-{}", std::process::id()));
        std::env::set_var("MARS_WORKLOG", &bwl);
        worklog::log_briefing("s1", "Welcome back.", "failed: OOM · done: build", 300, 1000);
        worklog::log_briefing("s1", "Back again.", "done: OOM fixed", 60, 2000);
        worklog::log_briefing("s2", "Other.", "blocked: deploy", 10, 1500);
        let last = worklog::load_last_briefing("s1").expect("no briefing logged");
        assert_eq!(last.facts, "done: OOM fixed", "load_last_briefing returned the wrong/older one");
        assert_eq!(last.ts, 2000);
        assert_eq!(worklog::load_last_briefing("s2").map(|p| p.facts).as_deref(), Some("blocked: deploy"),
            "briefings leaked across sessions");
        assert!(worklog::load_last_briefing("nope").is_none());
        std::env::set_var("MARS_WORKLOG", &worklog_default);
        let _ = std::fs::remove_file(&bwl);
        let _ = std::fs::remove_file(bwl.with_file_name("briefings.jsonl"));
        // Pure boot-reveal + clock helpers.
        assert_eq!(briefing::fmt_clock(4212), "01:10:12");
        assert_eq!(briefing::fmt_clock(90061), "1:01:01:01");
        let full = briefing::reveal_at(u128::MAX, 3); // animation off → everything up
        assert!(full.rows == 3 && full.signoff, "animate-off must reveal all");
        let start = briefing::reveal_at(0, 3); // t=0 → chrome only, rows not yet
        assert!(start.rows == 0 && !start.signoff, "at t=0 the manifest has not cascaded in");
        println!("[selfcheck] mission briefing (save-state) PASS");
    }

    // 43b. Goals captured at detach: parse, round-trip, tier route, and feed the
    //      return briefing. The capture LLM call itself is gated on a key (never
    //      fires in the hermetic suite), so we test the deterministic seams.
    {
        // Parse tolerates list markers and caps at three.
        let g = agent::parse_goals("1. get the auth test green\n- finish numpy upgrade\n* land OOM fix\nextra");
        assert_eq!(g, vec!["get the auth test green", "finish numpy upgrade", "land OOM fix"],
            "goal parse/markers/cap wrong: {g:?}");
        assert!(agent::parse_goals("\n\n").is_empty(), "blank capture → no goals");
        // Round-trip, session-scoped.
        let gwl = std::env::temp_dir().join(format!("mars-goals-{}", std::process::id()));
        std::env::set_var("MARS_WORKLOG", &gwl);
        worklog::save_goals("demo", &["ship the overlay".into(), "fix the OOM".into()], 42);
        assert_eq!(worklog::load_goals("demo"), vec!["ship the overlay", "fix the OOM"], "goals round-trip");
        assert!(worklog::load_goals("other").is_empty(), "goals leaked across sessions");
        // Routes at the cheap tier; the tag is pinned in TASKS (checked in 29h).
        assert_eq!(tiers::model_for("groq", "capture_goals", "x"), "llama-3.1-8b-instant",
            "capture_goals must route to low");
        // The Goals event persists what the model returned.
        let mut app = App::new(None)?;
        app.agent_tx.send(agent::AgentEvent::Goals { goals: vec!["debug the daemon".into()] })?;
        app.tick();
        assert_eq!(worklog::load_goals(&app.session_label()), vec!["debug the daemon"],
            "Goals event did not persist");
        std::env::set_var("MARS_WORKLOG", &worklog_default);
        let _ = std::fs::remove_file(&gwl);
        let _ = std::fs::remove_file(gwl.with_file_name("goals.json"));
        println!("[selfcheck] goals capture + recall ... PASS");
    }

    // 44. Mouse: chrome is clickable. Renderers record what they drew into the
    //     hit registry, so a click resolves through the SAME funnel as a chord —
    //     no surface gets a private dialect. Every coordinate below is read back
    //     from the registry the frame actually produced, never hardcoded: the
    //     test asserts "what was drawn is what is clickable," which is the whole
    //     invariant. Registry chrome is live in every mode, so these clicks work
    //     while the bar, a prompt, or the tree owns the keyboard.
    {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        fn click(app: &mut App, r: ratatui::layout::Rect) {
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: r.x, row: r.y, modifiers: KeyModifiers::NONE,
            });
        }
        // The first region whose target matches a predicate, as drawn this frame.
        fn region_of(app: &App, pred: impl Fn(&app::HitTarget) -> bool) -> ratatui::layout::Rect {
            app.hits.borrow().iter().find(|h| pred(&h.target))
                .unwrap_or_else(|| panic!("no hit region matched")).rect
        }

        let mut term = Terminal::new(TestBackend::new(110, 24))?;

        // (a) Tab bar. Two tabs, click the first: the mouse reaches tab switching
        //     without the keyboard's M-1..9 or travel mode.
        let mut app = App::new(None)?;
        app.run_action(palette::Action::NewTab);
        assert_eq!(app.active_tab, 1, "NewTab did not focus the new tab");
        term.draw(|f| ui::render(f, &mut app))?;
        let tab0 = region_of(&app, |t| matches!(t, app::HitTarget::Row(app::RowKind::Tab, 0)));
        click(&mut app, tab0);
        assert_eq!(app.active_tab, 0, "clicking a tab did not switch to it");
        // Regions are per-frame: a stale registry must never dispatch.
        app.hits_clear();
        click(&mut app, tab0);
        assert_eq!(app.active_tab, 0, "a cleared registry still dispatched");

        // (b) The idle control bar opens mission control — the discovery on-ramp.
        //     It is the surface every action is reached through, so it is the one
        //     target with no Action behind it.
        let mut app = App::new(None)?;
        term.draw(|f| ui::render(f, &mut app))?;
        let bar = region_of(&app, |t| matches!(t, app::HitTarget::OpenBar));
        click(&mut app, bar);
        assert_eq!(app.mode, mode::Mode::Bar, "clicking the control bar did not open it");

        // (c) A dropdown row runs its action — through run_action, so the confirm
        //     gate and frecency apply to clicking exactly as to Enter.
        term.draw(|f| ui::render(f, &mut app))?;
        let rows = app.bar_rows();
        let idx = rows.iter().position(|r| matches!(&r.kind,
            palette::ItemKind::Run(palette::Action::ToggleFileTree)))
            .expect("navigator row missing from the launcher");
        let row = region_of(&app, |t| matches!(t, app::HitTarget::Row(app::RowKind::Command, i) if *i == idx));
        click(&mut app, row);
        assert!(app.tree_open, "clicking the navigator row did not open the tree");

        // (d) Tree rows, clicked while TREE mode owns the keyboard — the mode gate
        //     that used to swallow every non-Edit/Terminal click.
        assert_eq!(app.mode, mode::Mode::Tree, "navigator did not take focus");
        term.draw(|f| ui::render(f, &mut app))?;
        let n_rows = app.tree_rows.len();
        assert!(n_rows > 1, "no tree rows to click");
        let r1 = region_of(&app, |t| matches!(t, app::HitTarget::Row(app::RowKind::Tree, i) if *i == 1));
        click(&mut app, r1);
        assert_eq!(app.file_tree.as_ref().map(|t| t.selected), Some(1),
            "clicking a tree row did not select it");

        // (e) The notice line: click dismisses without Esc. Over a focused
        //     terminal Esc belongs to the shell (P1.2) — a click needs no mode.
        let mut app = App::new(None)?;
        app.show_splash = false; // the splash outranks the notice line
        app.notices.push(app::Notice {
            kind: app::NoticeKind::Failure,
            text: "tests failed".into(),
        });
        term.draw(|f| ui::render(f, &mut app))?;
        let dismiss = region_of(&app, |t| matches!(t, app::HitTarget::DismissNotice));
        click(&mut app, dismiss);
        assert!(app.notices.is_empty(), "clicking dismiss did not clear the notice");

        // (f) Pointer motion is not a repaint. Crossterm reports any-event
        //     tracking, so a sweep is one Moved per cell; blanket-repainting on
        //     those ships a full frame per motion over a session socket.
        assert!(!app::InputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Moved, column: 4, row: 4, modifiers: KeyModifiers::NONE,
        }).forces_redraw(), "a bare Moved event still forces a repaint");
        assert!(app::InputEvent::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left), column: 4, row: 4,
            modifiers: KeyModifiers::NONE,
        }).forces_redraw(), "a click must still repaint");
        println!("[selfcheck] mouse: clickable chrome ... PASS");
    }

    // 44f. The bottom-bar hints are real BUTTONS now, not inert labels: each chip
    //      registers its own click cells (routed through run_action), lights on
    //      hover, recesses when pressed, and flashes the chord it stands for on
    //      click (mouse as on-ramp to the keyboard). Also asserts the old invisible
    //      whole-row OpenBar is gone, and the priority reshuffle (warp surfaced).
    {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        fn at(app: &mut App, kind: MouseEventKind, r: ratatui::layout::Rect) {
            app.handle_mouse(MouseEvent { kind, column: r.x, row: r.y, modifiers: KeyModifiers::NONE });
        }
        fn region_of(app: &App, pred: impl Fn(&app::HitTarget) -> bool) -> Option<ratatui::layout::Rect> {
            app.hits.borrow().iter().find(|h| pred(&h.target)).map(|h| h.rect)
        }
        let mut term = Terminal::new(TestBackend::new(110, 24))?;

        // (a) Clicking the `save` status-bar chip flashes its chord — the chip is a
        //     registered Act target (not a label), and click-to-teach fires after
        //     dispatch so run_action can't clobber the lesson.
        let mut app = App::new(None)?;
        app.show_splash = false;
        term.draw(|f| ui::render(f, &mut app))?;
        let save_chip = region_of(&app, |t| matches!(t, app::HitTarget::Act(palette::Action::Save)))
            .expect("save chip is not a registered click target");
        at(&mut app, MouseEventKind::Down(MouseButton::Left), save_chip);
        let chord = app.keys.binding_for(&palette::Action::Save).unwrap();
        assert_eq!(app.status_msg.as_deref(), Some(format!("↦ {chord}").as_str()),
            "clicking the save chip did not flash its chord (click-to-teach)");

        // (b) Pressed-flash: left-down over a chip records it; release clears it.
        let mut app = App::new(None)?;
        app.show_splash = false;
        term.draw(|f| ui::render(f, &mut app))?;
        let warp = region_of(&app, |t| matches!(t, app::HitTarget::Act(palette::Action::TabMode)))
            .expect("warp chip missing from the Edit status bar (priority reshuffle)");
        at(&mut app, MouseEventKind::Down(MouseButton::Left), warp);
        assert!(app.pressed.is_some(), "left-down did not set the pressed-flash");
        at(&mut app, MouseEventKind::Up(MouseButton::Left), warp);
        assert!(app.pressed.is_none(), "release did not clear the pressed-flash");

        // (c) Hover: a Moved over a chip sets `hovered` and asks for a repaint; the
        //     same target again does NOT repaint (bare motion is otherwise free);
        //     moving to dead space clears the hover.
        let mut app = App::new(None)?;
        app.show_splash = false;
        term.draw(|f| ui::render(f, &mut app))?;
        let cmds = region_of(&app, |t| matches!(t, app::HitTarget::OpenBar)).expect("commands chip missing");
        app.needs_redraw = false;
        at(&mut app, MouseEventKind::Moved, cmds);
        assert!(app.hovered.is_some() && app.needs_redraw, "hover over a chip did not register / repaint");
        app.needs_redraw = false;
        at(&mut app, MouseEventKind::Moved, cmds);
        assert!(!app.needs_redraw, "an unchanged hover still forced a repaint");
        at(&mut app, MouseEventKind::Moved, ratatui::layout::Rect { x: 109, y: 23, width: 1, height: 1 });
        assert!(app.hovered.is_none(), "moving to dead space did not clear the hover");

        // (d) The redundant idle control bar (the bottom row, y=23 at height 24) is
        //     gone entirely: it registers NO click regions, and no near-full-width
        //     invisible target survives anywhere. Its command affordances live on the
        //     status bar now (the clickable chips exercised above).
        let mut app = App::new(None)?;
        app.show_splash = false;
        term.draw(|f| ui::render(f, &mut app))?;
        assert!(!app.hits.borrow().iter().any(|h| h.rect.y == 23),
            "the idle control bar still registers clicks on the bottom row");
        assert!(!app.hits.borrow().iter().any(|h| h.rect.width >= 100),
            "a near-full-width invisible click target still exists");

        // (e) Terminal-bar priority (static fallback parity): warp is advertised and
        //     "to shell" is gone. The live path additionally verifies C-t honesty in
        //     a real terminal — see the manual pass in the plan.
        let th = mode::Mode::Terminal.hints();
        assert!(th.iter().any(|(k, _)| *k == "C-t"), "terminal hints lost warp");
        assert!(!th.iter().any(|(_, a)| a.contains("shell")), "terminal hints still say 'to shell'");

        println!("[selfcheck] mouse: hint buttons + hover ... PASS");
    }

    // 44g. Navigator clicks PREVIEW instead of piling up tabs. A click is like
    //      arrowing to the row — highlight, stay in the tree — and a file swaps into
    //      one reusable preview tab. Editing that file pins it, so the next click
    //      starts a fresh preview (VS Code's italic-tab rule).
    {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        fn click(app: &mut App, r: ratatui::layout::Rect) {
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: r.x, row: r.y, modifiers: KeyModifiers::NONE,
            });
        }
        fn tree_row(app: &App, idx: usize) -> ratatui::layout::Rect {
            app.hits.borrow().iter()
                .find(|h| matches!(&h.target, app::HitTarget::Row(app::RowKind::Tree, i) if *i == idx))
                .unwrap_or_else(|| panic!("tree row {idx} not clickable")).rect
        }

        let mut term = Terminal::new(TestBackend::new(110, 24))?;
        let mut app = App::new(None)?;
        app.show_splash = false;
        app.run_action(palette::Action::ToggleFileTree);
        assert_eq!(app.mode, mode::Mode::Tree, "navigator did not take focus");
        term.draw(|f| ui::render(f, &mut app))?;

        // Two distinct file rows (the repo root has plenty: Cargo.toml, README.md…).
        let files: Vec<usize> = app.tree_rows.iter().enumerate()
            .filter(|(_, r)| !r.is_dir && !r.updir).map(|(i, _)| i).take(2).collect();
        assert!(files.len() == 2, "need two file rows in the tree to exercise preview");
        let base = app.tabs.len();

        // (a) Click file A: opens ONE preview tab, stays in the navigator, highlights.
        let ra = tree_row(&app, files[0]);
        click(&mut app, ra);
        assert_eq!(app.mode, mode::Mode::Tree, "a navigator click left the tree");
        assert_eq!(app.tabs.len(), base + 1, "preview did not open a tab");
        let pv = app.active_tab;
        assert!(app.tabs[pv].preview, "the opened tab is not flagged preview");
        assert_eq!(app.file_tree.as_ref().map(|t| t.selected), Some(files[0]), "clicked row not selected/highlighted");

        // (b) Click file B: the SAME preview tab is reused — no new tab.
        term.draw(|f| ui::render(f, &mut app))?;
        let rb = tree_row(&app, files[1]);
        click(&mut app, rb);
        assert_eq!(app.tabs.len(), base + 1, "a second click spawned a tab instead of swapping the preview");
        assert_eq!(app.active_tab, pv, "the preview tab changed identity mid-swap");

        // (c) Edit the preview, then click A again: the dirtied tab is PINNED and a
        //     fresh preview opens.
        let pid = app.tabs[pv].focused_pane;
        if let Some(pane::PaneContent::Editor(b)) = app.panes.get(&pid).map(|p| p.content.clone()) {
            app.buffers.get_mut(&b).unwrap().mark_edited();
        } else {
            panic!("preview tab is not showing an editor");
        }
        term.draw(|f| ui::render(f, &mut app))?;
        let ra2 = tree_row(&app, files[0]);
        click(&mut app, ra2);
        assert_eq!(app.tabs.len(), base + 2, "an edited preview was not pinned; the click should have opened a new preview");
        assert!(!app.tabs[pv].preview, "the edited preview tab was not promoted to a real tab");

        println!("[selfcheck] navigator: click previews (reuse + pin) ... PASS");
    }

    // 44h. From the navigator, a click on a PANE focuses it (so you can type) — pane
    //      interaction is no longer gated to Edit/Terminal, so the tree isn't a trap.
    {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut term = Terminal::new(TestBackend::new(110, 24))?;
        let mut app = App::new(None)?;
        app.show_splash = false;
        app.run_action(palette::Action::ToggleFileTree);
        assert_eq!(app.mode, mode::Mode::Tree, "navigator did not open");
        term.draw(|f| ui::render(f, &mut app))?;
        let (_, r) = app.pane_rects[0]; // the editor pane, right of the sidebar
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: r.x + r.width / 2, row: r.y + r.height / 2, modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.mode, mode::Mode::Edit, "clicking a pane from the navigator did not focus it");
        println!("[selfcheck] navigator: click a pane to focus it ... PASS");
    }

    // 44i. The terminal status bar's `editor` (C-g) chip is a real clickable target
    //      now — clicking it hands the keyboard back to the editor, exactly as C-g
    //      does — not the dead label it used to be.
    {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut term = Terminal::new(TestBackend::new(110, 24))?;
        let mut app = App::new(None)?;
        app.show_splash = false;
        app.open_terminal(); // focused pane → a shell; enters Terminal mode
        assert_eq!(app.mode, mode::Mode::Terminal, "open_terminal did not enter Terminal mode");
        term.draw(|f| ui::render(f, &mut app))?;
        let chip = app.hits.borrow().iter()
            .find(|h| matches!(h.target, app::HitTarget::FocusEditor)).map(|h| h.rect)
            .expect("no clickable editor (C-g) chip in the terminal status bar");
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: chip.x, row: chip.y, modifiers: KeyModifiers::NONE,
        });
        assert_eq!(app.mode, mode::Mode::Edit, "clicking the editor chip did not return to the editor");
        println!("[selfcheck] terminal: editor chip returns focus ... PASS");
    }

    // 44j. Host-health probes: uptime is always present, the line self-trims to
    //      whatever the OS answers, and GPU is omitted when not polled. The printed
    //      line doubles as a live read of this machine's probes.
    {
        let mut h = health::Health::new(1);
        let _ = h.maybe_sample(std::path::Path::new("."), false);
        let line = h.line();
        assert!(line.starts_with("up "), "health line missing uptime: {line}");
        assert!(!line.contains("gpu"), "gpu segment present without a GPU poll: {line}");
        println!("[selfcheck] health probes ({line}) ... PASS");
    }

    // 44k. The VITALS box RENDERS as its own panel stacked BELOW the SPACES box
    //      once the fleet shows (≥2 tabs). Reads the rendered cell grid (not the ANSI
    //      stream) for the box title and its UPTIME row.
    {
        let mut term = Terminal::new(TestBackend::new(120, 30))?;
        let mut app = App::new(None)?;
        app.show_splash = false;
        app.run_action(palette::Action::NewTab); // 2 tabs → the SPACES panel shows
        app.handle_key(kc(KeyCode::Char(' ')))?; // Ctrl+Space → mission control
        assert_eq!(app.mode, mode::Mode::Bar, "Ctrl+Space did not open mission control");
        app.tick(); // populate probes (uptime is present regardless)
        term.draw(|f| ui::render(f, &mut app))?;
        // Chunk the flat cell grid into rows to check WHERE the health box lands.
        let w = term.backend().buffer().area.width as usize;
        let flat: Vec<char> = screen_text(&term).chars().collect();
        let rows: Vec<String> = flat.chunks(w).map(|c| c.iter().collect()).collect();
        let spaces_row = rows.iter().position(|r| r.contains("SPACES")).expect("SPACES title row");
        let health_row = rows.iter().position(|r| r.contains("VITALS")).expect("VITALS title row");
        let uptime_row = rows.iter().position(|r| r.contains("UPTIME")).expect("UPTIME row");
        assert!(
            health_row > spaces_row && uptime_row > health_row,
            "VITALS box must sit BELOW the SPACES box (SPACES {spaces_row}, HEALTH {health_row}, UPTIME {uptime_row})"
        );
        println!("[selfcheck] VITALS box renders below SPACES ... PASS");
    }

    // 44b. Mouse selection in an EDITOR pane — key_design §7 rules "Selection =
    //      Shift+arrows + mouse", and the mouse half did not exist (P1.8). Drag,
    //      double-click (word), triple-click (line). All three leave the same
    //      selection object Shift+arrows makes, so every verb that consumes a
    //      selection works on them unchanged — and none of them touches the
    //      clipboard, which is what made a plain terminal click clobber it.
    {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        fn press(app: &mut App, col: u16, row: u16, kind: MouseEventKind) {
            app.handle_mouse(MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE });
        }
        let mut app = App::new(None)?;
        let mut term = Terminal::new(TestBackend::new(80, 12))?;
        typ(&mut app, "alpha beta gamma")?;
        app.handle_key(k(KeyCode::Enter))?;
        typ(&mut app, "second line here")?;
        term.draw(|f| ui::render(f, &mut app))?;
        // Text origin: pane border + gutter.
        let (px, py) = {
            let (_, r) = app.pane_rects[0];
            (r.x + 1 + ui::gutter_width(&app.tuning), r.y + 1)
        };
        let sel_text = |app: &App| {
            app.selection_range().map(|(b, s, e)| app.buffers[&b].rope.slice(s..e).to_string())
        };

        // Drag from "beta"'s start rightwards across "beta".
        press(&mut app, px + 6, py, MouseEventKind::Down(MouseButton::Left));
        assert!(sel_text(&app).is_none(), "a bare press must not select anything yet");
        // …and must not leave an ANCHOR either. Three call sites read "anchor is
        // Some" as "a region exists" — with a phantom empty one, Tab would indent
        // the line instead of inserting spaces and Esc would stop dismissing
        // notices. A plain click has to leave exactly the caret it always did.
        assert!(app.focused_pane().selection_anchor.is_none(),
            "a plain click left a phantom selection anchor");
        press(&mut app, px + 10, py, MouseEventKind::Drag(MouseButton::Left));
        assert_eq!(sel_text(&app).as_deref(), Some("beta"), "drag did not select the dragged span");
        let ring = app.kill_ring.len();
        press(&mut app, px + 10, py, MouseEventKind::Up(MouseButton::Left));
        assert_eq!(sel_text(&app).as_deref(), Some("beta"), "release dropped the selection");
        assert_eq!(app.kill_ring.len(), ring, "an editor drag must not copy on release");

        // Double-click inside "gamma" selects the word — from a click anywhere in it.
        press(&mut app, px + 13, py, MouseEventKind::Down(MouseButton::Left));
        press(&mut app, px + 13, py, MouseEventKind::Down(MouseButton::Left));
        assert_eq!(sel_text(&app).as_deref(), Some("gamma"), "double-click did not select the word");

        // A third click at the same cell takes the whole line.
        press(&mut app, px + 13, py, MouseEventKind::Down(MouseButton::Left));
        assert_eq!(sel_text(&app).as_deref(), Some("alpha beta gamma"),
            "triple-click did not select the line");

        // A slow second click is two single clicks, not a double: the selection
        // collapses back to a caret.
        let slow = app.tuning.multi_click_ms + 50;
        press(&mut app, px + 2, py, MouseEventKind::Down(MouseButton::Left));
        std::thread::sleep(std::time::Duration::from_millis(slow));
        press(&mut app, px + 2, py, MouseEventKind::Down(MouseButton::Left));
        assert!(sel_text(&app).is_none(), "clicks outside the window still counted as a double");

        // A selection made by mouse is a first-class one: the existing copy verb
        // takes it, with no mouse-specific path.
        press(&mut app, px, py, MouseEventKind::Down(MouseButton::Left));
        press(&mut app, px, py, MouseEventKind::Down(MouseButton::Left));
        assert_eq!(sel_text(&app).as_deref(), Some("alpha"), "word select at line start");
        app.run_action(palette::Action::CopyRegion);
        assert_eq!(app.kill_ring.last().map(|s| s.as_str()), Some("alpha"),
            "CopyRegion did not consume a mouse-made selection");
        println!("[selfcheck] mouse: editor selection .. PASS");
    }

    // 44c. Drag a split boundary to resize (Tier 2). The dragged divider must be
    //      the one you grabbed — the keyboard's `resize` adjusts the innermost
    //      split around the FOCUS, which is a different split whenever the pane
    //      you grabbed the edge of sits inside a nested one. Hence set_ratio by
    //      path, and hence a nested layout in this test.
    {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        fn ev(app: &mut App, col: u16, row: u16, kind: MouseEventKind) {
            app.handle_mouse(MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE });
        }
        let mut app = App::new(None)?;
        let mut term = Terminal::new(TestBackend::new(100, 30))?;
        // Vertical split at the root, then split the RIGHT side horizontally: the
        // vertical divider's split is not the innermost one around the focus.
        app.run_action(palette::Action::SplitVertical);
        app.run_action(palette::Action::SplitHorizontal);
        term.draw(|f| ui::render(f, &mut app))?;

        let dividers: Vec<(ratatui::layout::Rect, Vec<u8>, bool)> = app.hits.borrow().iter()
            .filter_map(|h| match &h.target {
                app::HitTarget::Divider { path, vertical, .. } => Some((h.rect, path.clone(), *vertical)),
                _ => None,
            }).collect();
        assert_eq!(dividers.len(), 2, "expected one divider per split, got {dividers:?}");
        let (vrect, _, _) = *dividers.iter().find(|(_, _, v)| *v).expect("no vertical divider");

        let width_of = |app: &App, i: usize| app.pane_rects[i].1.width;
        let before = width_of(&app, 0);
        // Grab the seam, drag it right; the left pane must grow to meet it.
        ev(&mut app, vrect.x, vrect.y + 1, MouseEventKind::Down(MouseButton::Left));
        ev(&mut app, vrect.x + 15, vrect.y + 1, MouseEventKind::Drag(MouseButton::Left));
        term.draw(|f| ui::render(f, &mut app))?;
        let after = width_of(&app, 0);
        assert!(after > before + 10, "drag right did not widen the left pane ({before} → {after})");
        // The divider tracks the pointer rather than accumulating a fixed nudge.
        assert!((app.pane_rects[0].1.right() as i32 - (vrect.x + 15) as i32).abs() <= 2,
            "divider did not follow the pointer: edge {} vs pointer {}",
            app.pane_rects[0].1.right(), vrect.x + 15);

        // Release ends it: further motion must not keep resizing.
        ev(&mut app, vrect.x + 15, vrect.y + 1, MouseEventKind::Up(MouseButton::Left));
        let settled = width_of(&app, 0);
        ev(&mut app, vrect.x + 40, vrect.y + 1, MouseEventKind::Drag(MouseButton::Left));
        term.draw(|f| ui::render(f, &mut app))?;
        assert_eq!(width_of(&app, 0), settled, "panes kept resizing after the button came up");

        // Clamped: a drag to the far edge cannot make a pane vanish.
        let seam = app.pane_rects[0].1.right() - 1;
        ev(&mut app, seam, vrect.y + 1, MouseEventKind::Down(MouseButton::Left));
        ev(&mut app, 0, vrect.y + 1, MouseEventKind::Drag(MouseButton::Left));
        term.draw(|f| ui::render(f, &mut app))?;
        assert!(app.pane_rects.iter().all(|(_, r)| r.width >= 3),
            "a dragged divider collapsed a pane: {:?}", app.pane_rects);
        println!("[selfcheck] mouse: drag to resize ... PASS");
    }

    let _ = std::fs::remove_file(&worklog_default);
    std::env::remove_var("MARS_WORKLOG");

    println!("\nALL SELFCHECKS PASSED ✓");
    Ok(())
}
