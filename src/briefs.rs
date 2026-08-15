//! Briefs — the specification a worker is handed, and everything derived from one.
//!
//! Design: `work_memos/the-work-model.md`. The parts that matter to this file:
//!
//! **Three files, split by when the fact becomes knowable.** `brief.md` at authoring,
//! `in_process.md` the moment a worker hits something the brief got wrong, `completed.md` at the
//! end. Nothing here splits on bookkeeping — there is no claim, no lock and no hash, because their
//! only job is arbitration between assigners and there is exactly one.
//!
//! **No state field anywhere.** State is which siblings exist, so nothing stored can disagree with
//! reality. This is the rule the whole manager audit was about.
//!
//! **Liveness is NOT in here.** `in_process.md` existing means a worker started, never that one is
//! running — a worker that dies leaves the file behind. Whether work is happening is a question
//! about a pane, answered by `fg_commands`, and asking this module would get a confident wrong
//! answer. There is a selfcheck asserting exactly that.

use std::path::{Path, PathBuf};

/// The tools a worker may never edit.
///
/// Under `acceptEdits` the dangerous class is not code but **files that execute without being
/// run** — a build script, a hook, a workflow. Plus `.claude/**`, because the first thing a
/// blocked agent proposes is writing itself an allow rule, and the brief itself, because a worker
/// that can edit its own specification can change what was approved and then satisfy the change.
///
/// One list, here, rather than composed on a phone for a host whose version it does not know.
pub const WORKER_DENY: &[&str] = &[
    "build.rs",
    "Makefile",
    "package.json",
    "Cargo.toml",
    ".github/workflows/**",
    ".git/hooks/**",
    ".envrc",
    ".claude/**",
    "**/briefs/*/brief.md",
    "**/briefs/WORKING-MODEL.md",
];

/// `~/.mars/briefs`. Machine-scoped, not session-scoped: a change to make does not belong to the
/// session where the condition happened to be noticed.
pub fn dir() -> Option<PathBuf> {
    crate::sys::paths::home_dir().map(|h| h.join(".mars").join("briefs"))
}

/// Where a worker's standing orders live. Assignment points at this path and says nothing else
/// about how we work, so that every rule in it is versioned rather than retyped.
pub fn working_model_path() -> Option<PathBuf> {
    dir().map(|d| d.join("WORKING-MODEL.md"))
}

/// Timestamp plus a semantic suffix, never derived from the title alone.
///
/// A name is a mutable value — retitle the brief and an id derived from it would point somewhere
/// else, or collide with the next brief that happens to be called the same thing.
pub fn mint_id(title: &str, ts: u64) -> String {
    let slug: String = title
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let slug: String = slug.chars().take(48).collect();
    if slug.is_empty() { format!("brief-{ts}") } else { format!("brief-{ts}-{slug}") }
}

/// Same argument as `safe_memo_name`: a brief id travels into a shell as part of a path, and it is
/// chosen upstream of this function. Constrain the alphabet at the reader — an id outside it is
/// not sanitized, it simply is not a brief, so it never reaches a pane.
pub fn safe_id(id: &str) -> bool {
    id.starts_with("brief-")
        && id.len() <= 128
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// What has happened to a brief, derived entirely from which files are present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// `brief.md` only. Written, not yet handed to anyone.
    Draft,
    /// `in_process.md` exists. A worker was told about it. **Says nothing about whether one is
    /// still running** — see the module docs.
    Started,
    /// `completed.md` exists. There is a report to read.
    Reported,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Draft => "draft",
            State::Started => "started",
            State::Reported => "reported",
        }
    }
}

/// Derive state from a brief directory. Order matters: a completed brief also has `in_process.md`,
/// and the later fact wins.
pub fn state_of(brief_dir: &Path) -> State {
    if brief_dir.join("completed.md").exists() {
        State::Reported
    } else if brief_dir.join("in_process.md").exists() {
        State::Started
    } else {
        State::Draft
    }
}

#[derive(Clone, Debug)]
pub struct Brief {
    pub id: String,
    pub title: String,
    pub state: State,
    pub priority: u32,
    pub branch: Option<String>,
    /// The memos this brief addresses. **The pointer lives here, not on the memo** — a memo is
    /// rewritten constantly (one measured at 50,570 times in seven days) and a pointer stored in a
    /// file with that churn is a pointer the next rewrite drops. The memo surface computes the
    /// reverse index by reading this directory.
    pub addresses: Vec<String>,
    pub created_ts: u64,
}

/// Read one brief. `None` when the directory holds no `brief.md`, which is how a half-created or
/// hand-made directory is ignored rather than half-reported.
pub fn read(brief_dir: &Path) -> Option<Brief> {
    let id = brief_dir.file_name()?.to_str()?.to_string();
    if !safe_id(&id) {
        return None;
    }
    let text = std::fs::read_to_string(brief_dir.join("brief.md")).ok()?;
    let (front, _) = crate::manager::split_front(&text)?;
    let f = |k: &str| {
        front.lines().find_map(|l| {
            let (key, v) = l.split_once(':')?;
            (key.trim() == k).then(|| v.trim().trim_matches('"').to_string())
        })
    };
    Some(Brief {
        title: f("title").unwrap_or_else(|| id.clone()),
        state: state_of(brief_dir),
        priority: f("priority").and_then(|p| p.parse().ok()).unwrap_or(0),
        branch: f("branch").filter(|b| !b.is_empty() && b != "null"),
        addresses: parse_addresses(front),
        created_ts: f("created_ts").and_then(|t| t.parse().ok()).unwrap_or(0),
        id,
    })
}

/// `addresses:` is a YAML list of `{session: x, memo: y}` maps. Parsed by hand rather than with a
/// YAML crate for the same reason every other frontmatter reader here is: a malformed list must
/// cost one field, never the whole document.
fn parse_addresses(front: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in front.lines() {
        if line.starts_with("addresses:") {
            inside = true;
            continue;
        }
        if inside {
            let t = line.trim();
            if !t.starts_with('-') {
                break; // the list ended at the next top-level key
            }
            let session = between(t, "session:", ',').unwrap_or_default();
            let memo = between(t, "memo:", '}').unwrap_or_default();
            if !memo.is_empty() {
                out.push(if session.is_empty() { memo } else { format!("{session}/{memo}") });
            }
        }
    }
    out
}

fn between(s: &str, key: &str, end: char) -> Option<String> {
    let at = s.find(key)? + key.len();
    let rest = &s[at..];
    let stop = rest.find(end).unwrap_or(rest.len());
    Some(rest[..stop].trim().trim_matches(|c| c == '"' || c == '}' || c == ' ').to_string())
}

/// Every brief on this machine, newest first. A directory read — there is no index to go stale.
pub fn list() -> Vec<Brief> {
    let Some(root) = dir() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(&root) else { return Vec::new() };
    let mut out: Vec<Brief> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .filter_map(|e| read(&e.path()))
        .collect();
    out.sort_by(|a, b| b.created_ts.cmp(&a.created_ts).then(a.id.cmp(&b.id)));
    out
}

/// What Rover types into the worker's pane.
///
/// **Two file locations, and nothing else.** Today's assign flow composes a paragraph — goal,
/// constraints, deny-list reasoning and reporting rules in one string — and every rule in that
/// paragraph exists in exactly one place, gets rewritten by whoever next edits the composer, and
/// is invisible to the person who approved the work. Grow it and it is a thousand-line prompt
/// nobody has read in months.
///
/// The rules belong in `WORKING-MODEL.md`, where they are versioned, diffable and identical for
/// every worker on every host. What is left to say is two addresses — which also makes *"what was
/// this worker actually told"* answerable, because it is the same bytes every time.
pub fn assignment(id: &str, home: &Path) -> Option<String> {
    if !safe_id(id) {
        return None;
    }
    let briefs = home.join(".mars").join("briefs");
    // First, then, go. An agent handed two paths and no imperative reads them and asks what you
    // would like it to do — a wasted round trip, and on a phone a wasted round trip is a round
    // trip you may not be there for. The old flow closed this with "start the work immediately";
    // dropping it while shortening the message would have been a quiet regression.
    Some(format!(
        "First read {} — that is how we work here.\n\
         Then read {} — that is what to build.\n\
         Start building.\n",
        briefs.join("WORKING-MODEL.md").display(),
        briefs.join(id).join("brief.md").display(),
    ))
}

/// Is the agent running in this pane scoped as a worker?
///
/// **Derived from the live process, never from a marker.** A recorded "this pane is a worker" flag
/// would keep saying so about a pane whose `claude` was interrupted and restarted bare — and the
/// thing being protected is the deny-list, so a flag that can be wrong in that direction is worse
/// than no flag. The argv of the process that is actually running is the fact itself.
///
/// Pure over the argv string so a selfcheck can drive every case without spawning anything.
pub fn worker_argv_ok(argv: &str) -> bool {
    if !argv.contains("claude") {
        return false;
    }
    if !argv.contains("--permission-mode acceptEdits") {
        return false;
    }
    // Every entry, not merely some: a partial deny-list is the failure this check exists to catch,
    // and it is invisible at a glance because the command still looks long and careful.
    WORKER_DENY.iter().all(|p| argv.contains(p))
}

/// The command that starts a pane as a worker. One composer, host-side, so the phone never builds
/// a shell line for a host whose version it does not know.
pub fn worker_start_command() -> String {
    let deny = WORKER_DENY.iter().map(|p| format!("'Edit({p})'")).collect::<Vec<_>>().join(" ");
    format!("claude --permission-mode acceptEdits --disallowedTools {deny}\n")
}

/// The full argv of a running process, for `worker_argv_ok`. `ps -o args=` rather than `comm=`,
/// because the flags are the whole question and `comm=` drops them.
pub fn argv_of(pid: i32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Seed the worker's standing orders, and repair them when a newer version ships.
///
/// Same mechanism as `manager_docs/`: `seed()` never overwrites a doc the human edited unless our
/// `mars-doc-version` is higher. The reason is the one `docs_drift` states — an agent running on
/// rewritten orders is not a smaller agent, it is somebody else's.
pub fn scaffold() -> std::io::Result<()> {
    let Some(root) = dir() else { return Ok(()) };
    std::fs::create_dir_all(&root)?;
    let path = root.join("WORKING-MODEL.md");
    let built_in = WORKING_MODEL;
    let replace = match std::fs::read_to_string(&path) {
        Err(_) => true,
        Ok(on_disk) => on_disk != built_in && doc_version(&on_disk) < doc_version(built_in),
    };
    if replace {
        std::fs::write(&path, built_in)?;
    }
    Ok(())
}

pub const WORKING_MODEL: &str = include_str!("manager_docs/WORKING-MODEL.md");

/// The `<!-- mars-doc-version: N -->` marker, or 0. Absent reads as oldest, so a hand-written file
/// with no marker is replaced by anything we ship rather than pinning the host forever.
pub fn doc_version(text: &str) -> u32 {
    text.find("mars-doc-version:")
        .and_then(|at| text[at + 17..].trim_start().split(|c: char| !c.is_ascii_digit()).next()?.parse().ok())
        .unwrap_or(0)
}

/// The starting shape of a brief.
///
/// **The template is the design decision, not a convenience.** A brief written from scratch is a
/// brief with no forks, and a design with no forks is prose — the reader cannot see what was
/// decided, so approving it means reading all of it. Three questions with three answers each is
/// scannable; two thousand words is not.
///
/// Note that every fork ships with an option already chosen. Three options and no recommendation
/// reads as thorough and is an abdication: it hands the reader the work the document existed to
/// do. The reader OVERRIDES a choice; they do not ANSWER a question.
pub fn template(id: &str, title: &str, ts: u64) -> String {
    format!(
        "---\n\
         id: {id}\n\
         title: \"{title}\"\n\
         priority: 50\n\
         created_ts: {ts}\n\
         branch: {id}\n\
         addresses: []\n\
         verify: []\n\
         ---\n\
         # {title}\n\
         \n\
         ## Problem + evidence\n\
         \n\
         BINDING. Exact paths and line anchors, not descriptions — the worker can read this\n\
         machine, so give it addresses.\n\
         \n\
         ## HLD\n\
         \n\
         ### Fork 1 — <the question>\n\
         \n\
         - **Option A** — … *For:* … *Against:* …\n\
         - **Option B** — … *For:* … *Against:* …\n\
         - **Option C ✅ chosen** — … *Why this and not the others:* …\n\
         \n\
         ### Fork 2 — <the question>\n\
         \n\
         ### Fork 3 — <the question>\n\
         \n\
         Three is a strong default, not a schema. Two real forks beat three with one invented.\n\
         \n\
         ## LLD\n\
         \n\
         ### Directory structure\n\
         \n\
         Every new file justified — and the files deliberately NOT created justified harder, since\n\
         that is where the design is actually being restrained.\n\
         \n\
         ### Artefact 1 of 3 — <the hardest thing to build>\n\
         \n\
         Three options considered, one chosen, with the reason the others lose.\n\
         \n\
         ### Artefact 2 of 3\n\
         \n\
         ### Artefact 3 of 3\n\
         \n\
         ## Acceptance\n\
         \n\
         BINDING. Numbered, each independently checkable. `unverifiable` is a first-class outcome —\n\
         a criterion nothing can check is a defect in this brief and should be visible as one.\n\
         \n\
         1. …\n\
         \n\
         ## Out of scope\n\
         \n\
         BINDING. Things a reasonable worker would otherwise do.\n\
         \n\
         ## Decisions already made\n\
         \n\
         BINDING. Every ruled fork, with what was rejected and why. Without this a worker\n\
         re-litigates settled ground and its questions come back to you at 3am.\n\
         \n\
         ## Approach\n\
         \n\
         ADVISORY. Deviate freely when you find better — and append the reason to in_process.md\n\
         when you do.\n"
    )
}
