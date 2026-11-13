//! THE CODING AGENTS MARS KNOWS: ONE ROW EACH, AND EVERYTHING MARS NEEDS FROM THEM.
//!
//! Mars grew up beside Claude Code, and "is this pane running an agent" was answered by comparing
//! the foreground command to `"claude"`, the manager's turn was a `claude` command, and
//! transcripts were looked for under `~/.claude`. Other agents work the same way from Mars's side
//! — a program that reads a prompt, works, and waits — so what differs between them is data, and
//! it lives here, in one row per agent. Code elsewhere asks the table; it does not name agents.
//!
//! A row says three things, each optional beyond the first:
//! - how to RECOGNISE the agent in a pane (always);
//! - the command for a MANAGER TURN, if the agent can take one (`mars manager agent <name>`);
//! - where its TRANSCRIPTS live and how they are named, if Mars can read them (the phone's
//!   timeline, the manager's own conversation).
//!
//! **Adding an agent** (Copilot, Codex, OpenCode…): add a row with its names. That alone makes
//! Mars recognise it, count it as an agent for notifications, and pass on its status reports if it
//! sends them (OSC 7501, agent-neutral; see `osc133.rs`). Fill `manager_turn` once a turn has been
//! run with it and produced the files `AGENTS.md` asks for. Fill `transcripts` once its files have
//! been seen; if their lines are a shape `timeline.rs` does not read yet, add that shape there,
//! next to Claude Code's and pi's. Leave a field `None` rather than guess: an unknown is reported
//! as unknown, a guess is reported as fact.
//!
//! **A name is not always visible.** An agent written in JavaScript or Python runs under its
//! interpreter: pi's launcher is a shell script that ends by running a Node program, so the
//! process table says `node`, not `pi`. For those interpreters the full argument list is read too,
//! and an agent is recognised by its install path. A native binary is recognised by name alone,
//! which costs nothing.

use std::path::PathBuf;

/// Where an agent files its conversations, and how a file's name carries the conversation's id.
pub struct Transcripts {
    /// The folder holding one sub-folder per working directory.
    pub root: fn() -> Option<PathBuf>,
    /// The sub-folder's name for a working directory.
    pub folder: fn(&str) -> String,
    /// A file's conversation id, from its name without `.jsonl`.
    pub id_of: fn(&str) -> &str,
    /// The exact file name for an id, when it can be derived; `None` means the folders are listed.
    pub file_for: fn(&str) -> Option<String>,
}

/// One coding agent, as Mars needs to know it.
pub struct Profile {
    /// The name Mars reports for it: on the board, to the phone, in notifications, and on the
    /// command line.
    pub name: &'static str,
    /// Process names that mean this agent outright.
    pub names: &'static [&'static str],
    /// Fragments of an interpreter's argument list that mean this agent.
    pub args: &'static [&'static str],
    /// The command typed for one manager turn. `{prompt}` marks where the prompt goes; a command
    /// without it gets Claude Code's `-p <prompt>` appended. `None`: not verified as a manager.
    pub manager_turn: Option<&'static str>,
    /// Where its transcripts are. `None`: Mars cannot read them yet, and shows the screen instead.
    pub transcripts: Option<Transcripts>,
}

/// The manager's turn under Claude Code, and the default. `ANTHROPIC_API_KEY` is removed so the
/// turn uses the person's Claude subscription; `acceptEdits` lets it write files with nobody at
/// the keyboard while still refusing a shell; `--add-dir` because what it reads lives in
/// `~/.mars/sessions`, outside the manager's own folder.
pub const CLAUDE_TURN: &str = "env -u ANTHROPIC_API_KEY -u ANTHROPIC_AUTH_TOKEN claude \
                  --model claude-sonnet-5 --effort medium \
                  --add-dir ~/.mars/sessions --permission-mode acceptEdits";

/// The manager's turn under pi: file tools only (read, search, write, edit; no shell), the same
/// capability Claude Code's `acceptEdits` gives it. pi has no per-file denials, which is why
/// `manager::lock_orders` exists. `--no-approve` because a turn has nobody to answer pi's
/// project-trust question, and the manager's folder holds nothing to trust. pi reads `AGENTS.md`
/// from the folder it runs in, so the orders need no symlink.
pub const PI_TURN: &str = "pi --print --no-approve --tools read,grep,find,ls,write,edit {prompt}";

/// Agents Mars recognises.
pub const AGENTS: &[Profile] = &[
    Profile {
        name: "claude",
        names: &["claude"],
        args: &["@anthropic-ai/claude-code", "/claude-code/cli"],
        manager_turn: Some(CLAUDE_TURN),
        transcripts: Some(Transcripts {
            root: claude_root,
            folder: crate::conv::project_slug,
            id_of: |stem| stem,
            file_for: |id| Some(format!("{id}.jsonl")),
        }),
    },
    Profile {
        name: "pi",
        names: &["pi"],
        args: &["pi-coding-agent", "/node_modules/.bin/pi", "/.pi/agent/install/"],
        manager_turn: Some(PI_TURN),
        transcripts: Some(Transcripts {
            root: pi_root,
            folder: pi_folder,
            // `<time>_<id>.jsonl`: the time it started, then the id.
            id_of: |stem| stem.split_once('_').map(|(_, id)| id).unwrap_or(stem),
            file_for: |_| None,
        }),
    },
    // Recognised, and counted as agents. Manager turns and transcripts not yet verified.
    Profile { name: "codex", names: &["codex"], args: &["@openai/codex", "/codex.js"], manager_turn: None, transcripts: None },
    Profile { name: "copilot", names: &["copilot"], args: &["@github/copilot"], manager_turn: None, transcripts: None },
    Profile { name: "opencode", names: &["opencode"], args: &["opencode-ai"], manager_turn: None, transcripts: None },
    Profile { name: "goose", names: &["goose"], args: &[], manager_turn: None, transcripts: None },
    Profile { name: "aider", names: &["aider"], args: &["/aider", "aider_chat", " -m aider"], manager_turn: None, transcripts: None },
    Profile { name: "gemini", names: &["gemini"], args: &["@google/gemini-cli", "/gemini-cli"], manager_turn: None, transcripts: None },
    Profile { name: "cursor-agent", names: &["cursor-agent"], args: &["cursor-agent"], manager_turn: None, transcripts: None },
];

/// Interpreters an agent may run under, whose argument list is worth reading.
pub const RUNTIMES: &[&str] = &["node", "bun", "deno", "python", "python3", "Python"];

/// An agent by the name Mars reports for it.
pub fn by_name(name: &str) -> Option<&'static Profile> {
    AGENTS.iter().find(|p| p.name == name)
}

/// The agent a process is, from its name and, for an interpreter, its arguments.
pub fn by_process(comm: &str, args: Option<&str>) -> Option<&'static Profile> {
    let comm = comm.rsplit('/').next().unwrap_or(comm);
    if let Some(p) = AGENTS.iter().find(|p| p.names.contains(&comm)) {
        return Some(p);
    }
    if !is_runtime(comm) {
        return None;
    }
    let args = args?;
    AGENTS.iter().find(|p| p.args.iter().any(|frag| args.contains(frag)))
}

/// Whether a process name is an interpreter whose arguments say what it is running.
pub fn is_runtime(comm: &str) -> bool {
    let comm = comm.rsplit('/').next().unwrap_or(comm);
    RUNTIMES.contains(&comm) || comm.starts_with("python3.")
}

/// Whether a reported command name is one of the agents above. Board rows carry the name
/// `by_process` resolved, so this is a name lookup, not a second detection.
pub fn is_agent_name(cmd: &str) -> bool {
    let base = cmd.rsplit('/').next().unwrap_or(cmd);
    let word = base.split_whitespace().next().unwrap_or(base);
    by_name(word).is_some()
}

/// Every agent whose transcripts Mars can read, with where they are.
pub fn transcript_stores() -> impl Iterator<Item = &'static Transcripts> {
    AGENTS.iter().filter_map(|p| p.transcripts.as_ref())
}

fn home() -> Option<PathBuf> {
    crate::sys::paths::home_dir()
}

fn claude_root() -> Option<PathBuf> {
    home().map(|h| h.join(".claude").join("projects"))
}

/// Where pi keeps sessions: `PI_CODING_AGENT_SESSION_DIR`, else `PI_CODING_AGENT_DIR/sessions`,
/// else `~/.pi/agent/sessions`.
fn pi_root() -> Option<PathBuf> {
    let set = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    set("PI_CODING_AGENT_SESSION_DIR")
        .or_else(|| set("PI_CODING_AGENT_DIR").map(|d| d.join("sessions")))
        .or_else(|| home().map(|h| h.join(".pi").join("agent").join("sessions")))
}

/// pi's folder name for a directory: the leading `/` dropped, every `/`, `\` and `:` made `-`,
/// and the whole wrapped in `--`. `/Users/x/.mars/manager` → `--Users-x-.mars-manager--`: unlike
/// Claude Code, pi leaves dots alone.
pub fn pi_folder(dir: &str) -> String {
    let inner: String = dir
        .trim_end_matches('/')
        .trim_start_matches('/')
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':') { '-' } else { c })
        .collect();
    format!("--{inner}--")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_agents_are_known_by_name() {
        assert_eq!(by_process("claude", None).map(|p| p.name), Some("claude"));
        assert_eq!(by_process("/usr/local/bin/codex", None).map(|p| p.name), Some("codex"));
        assert_eq!(by_process("copilot", None).map(|p| p.name), Some("copilot"));
        assert!(by_process("vim", None).is_none());
    }

    #[test]
    fn pi_under_node_is_known_by_its_install_path() {
        // What `ps -o args=` shows for pi started by its managed launcher.
        let args = "node /Users/x/.pi/agent/install/releases/1.3.0/node_modules/.bin/pi --session-id abc";
        assert_eq!(by_process("node", Some(args)).map(|p| p.name), Some("pi"));
        // And installed from npm.
        let npm = "node /opt/homebrew/lib/node_modules/@earendil-works/pi-coding-agent/dist/cli.js";
        assert_eq!(by_process("node", Some(npm)).map(|p| p.name), Some("pi"));
    }

    #[test]
    fn an_ordinary_node_program_is_not_an_agent() {
        assert!(by_process("node", Some("node server.js --port 3000")).is_none());
        assert!(by_process("node", None).is_none());
        // A non-interpreter is never identified from its arguments, whatever they say.
        assert!(by_process("grep", Some("grep pi-coding-agent notes.txt")).is_none());
    }

    #[test]
    fn board_names_are_recognised() {
        for p in AGENTS {
            assert!(is_agent_name(p.name), "{}", p.name);
        }
        assert!(is_agent_name("/usr/bin/claude --resume x"));
        assert!(!is_agent_name("node"));
        assert!(!is_agent_name("cargo"));
    }

    #[test]
    fn the_table_is_well_formed() {
        for (i, p) in AGENTS.iter().enumerate() {
            assert!(!p.names.is_empty(), "{} has no process name", p.name);
            assert!(AGENTS[..i].iter().all(|q| q.name != p.name), "{} is listed twice", p.name);
            // A manager turn either says where its prompt goes or is Claude Code's form.
            if let Some(turn) = p.manager_turn {
                assert!(turn.contains("{prompt}") || p.name == "claude", "{}: {turn}", p.name);
            }
        }
    }

    #[test]
    fn pi_folder_names_match_what_pi_writes() {
        // Observed: pi filed a session started in this directory under exactly this folder.
        assert_eq!(
            pi_folder("/private/tmp/claude-501/-Users-x-Mars-Mission/abc/scratchpad/pitest/manager"),
            "--private-tmp-claude-501--Users-x-Mars-Mission-abc-scratchpad-pitest-manager--"
        );
        assert_eq!(pi_folder("/Users/x/Code"), "--Users-x-Code--");
        assert_eq!(pi_folder("/Users/x/.mars/manager"), "--Users-x-.mars-manager--");
    }

    #[test]
    fn a_file_name_carries_its_id() {
        let pi = by_name("pi").unwrap().transcripts.as_ref().unwrap();
        assert_eq!(
            (pi.id_of)("2026-11-13T18-44-36-944Z_01a1d639-d5cf-7514-b0c9-de9925025ad1"),
            "01a1d639-d5cf-7514-b0c9-de9925025ad1"
        );
        let claude = by_name("claude").unwrap().transcripts.as_ref().unwrap();
        assert_eq!((claude.id_of)("9b2c4f1e-0000-4000-8000-000000000000"), "9b2c4f1e-0000-4000-8000-000000000000");
        assert!(crate::session::valid_chat_id("01a1d639-d5cf-7514-b0c9-de9925025ad1"));
    }
}
