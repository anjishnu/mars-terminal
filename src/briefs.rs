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
    // The orders name these two as out of bounds, so the scope enforces them rather than trusting
    // an agent to remember a sentence. PLANNING-MODEL is another role's standing orders; the
    // manager directory is the memory every other part of this system reasons from.
    "**/briefs/PLANNING-MODEL.md",
    "**/.mars/manager/**",
];

/// The one entry that separates a planner from a worker.
///
/// A planner's entire job is to write `brief.md`; a worker is forbidden from touching it. Rather
/// than maintain a second list that can silently drift out of agreement with the first, the
/// planner's scope is the worker's with this line removed — so every executable-without-being-run
/// file stays protected in both roles by construction, and the diff between the roles is one
/// string that is visible right here.
pub const BRIEF_DENY: &str = "**/briefs/*/brief.md";

/// What a planner may not edit: everything a worker may not, except the brief itself.
pub fn planner_deny() -> Vec<&'static str> {
    WORKER_DENY.iter().copied().filter(|p| *p != BRIEF_DENY).collect()
}

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

/// Mint a brief and write the template. **The only path that creates one**, so the CLI and the
/// phone cannot mint ids two different ways or seed two different templates.
///
/// Returns the id and the path. Fails rather than overwrites: an id collides only when two briefs
/// were minted in the same second with the same title, and silently clobbering the first is worse
/// than making the second press again.
pub fn create(title: &str, ts: u64) -> std::io::Result<(String, PathBuf)> {
    let id = mint_id(title, ts);
    let root = dir().ok_or_else(|| std::io::Error::other("no home directory"))?;
    let bdir = root.join(&id);
    std::fs::create_dir_all(&bdir)?;
    let path = bdir.join("brief.md");
    if path.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", path.display()),
        ));
    }
    std::fs::write(&path, template(&id, title, ts))?;
    let _ = scaffold();
    Ok((id, path))
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
    //
    // ONE LINE. `typed_bytes` frames it; between them they are the only combination that sends —
    // see its comment for the four shapes that did not.
    Some(format!(
        "First read {} — that is how we work here. \
         Then read {} — that is what to build. \
         Start building.",
        briefs.join("WORKING-MODEL.md").display(),
        briefs.join(id).join("brief.md").display(),
    ))
}

/// What Rover types into the PLANNER's pane. Same shape as `assignment` for the same reason: the
/// rules are versioned in a doc, and the message is addresses plus an imperative.
///
/// The second address is a file that already exists — the daemon minted it and wrote the template
/// before typing this. A planner told to "create a brief somewhere" picks its own path, its own
/// id and its own section order, and then nothing downstream can find or read it.
pub fn draft_assignment(id: &str, home: &Path) -> Option<String> {
    if !safe_id(id) {
        return None;
    }
    let briefs = home.join(".mars").join("briefs");
    Some(format!(
        "First read {} — that is how we plan here. \
         Then fill in {} — it is scaffolded and empty. \
         Start planning.",
        briefs.join("PLANNING-MODEL.md").display(),
        briefs.join(id).join("brief.md").display(),
    ))
}

/// The command that starts a pane as a planner.
pub fn planner_start_command() -> String {
    format!(
        "claude --permission-mode acceptEdits --allowedTools {} --disallowedTools {}\n",
        allow_flags(PLANNER_ALLOW),
        deny_flags(&planner_deny()),
    )
}

/// Is the agent in this pane scoped as a planner? Same derivation-from-live-argv argument as
/// `worker_argv_ok`, and deliberately strict in the other direction too: a planner scope must NOT
/// deny the brief, or the agent cannot do the one thing it was started for and will spend the run
/// asking why its edits are refused.
pub fn planner_argv_ok(argv: &str) -> bool {
    if !argv.contains("claude") {
        return false;
    }
    if !argv.contains("--permission-mode acceptEdits") {
        return false;
    }
    // Denied the brief in EITHER verb, it cannot do the one thing it was started for. Matched on
    // the full flag, not the bare pattern: the pattern also appears inside `PLANNER_ALLOW`, where
    // its presence means the opposite.
    let (allow, deny) = (allow_section(argv), deny_section(argv));
    if deny.contains(BRIEF_DENY) {
        return false;
    }
    if !planner_deny().iter().all(|p| deny.contains(&format!("Edit({p})")) && deny.contains(&format!("Write({p})"))) {
        return false;
    }
    PLANNER_ALLOW.iter().all(|p| allow.contains(p))
}

/// The two halves of the scope, read separately.
///
/// A check that searches the whole argv cannot tell a permission from a prohibition — and once
/// both lists mention the same path, as the planner's do, "is the brief denied?" answered `true`
/// against the flag that ALLOWS it. Split at the flags and each question is asked where its
/// answer lives.
fn allow_section(argv: &str) -> &str {
    let Some(a) = argv.find("--allowedTools") else { return "" };
    match argv.find("--disallowedTools") {
        Some(d) if d > a => &argv[a..d],
        _ => &argv[a..],
    }
}

fn deny_section(argv: &str) -> &str {
    let Some(d) = argv.find("--disallowedTools") else { return "" };
    match argv.find("--allowedTools") {
        Some(a) if a > d => &argv[d..a],
        _ => &argv[d..],
    }
}

/// Turn a message into the bytes that actually SEND it to an agent TUI.
///
/// Every shape here was tried against a live pane, and every failure passed every test that
/// existed at the time — the tests read the string, and the defect was in the bytes.
///
/// | shape | result |
/// |---|---|
/// | multi-line, ends `\n` | not sent — `\n` is Ctrl-J, "insert a newline" |
/// | multi-line, ends `\r` | not sent — one large write reads as a paste, and the return with it |
/// | one line, ends `\r` | not sent — still one large write (measured: 209 bytes) |
/// | multi-line, bracketed, ends `\r` | not sent — a multi-line paste waits for its own Enter |
/// | **one line, bracketed, ends `\r`** | **sent** |
///
/// Every failure looked identical from outside: the message typed into the pane, no error, no
/// refusal, an agent that never started, and nothing on the board to say so.
///
/// So both halves are load-bearing and neither is decoration. `ESC[200~ … ESC[201~` says "this is
/// a paste" outright, which ends it at a known byte and leaves the `\r` after it unambiguously
/// Enter — in the same write, with no timing assumption. And the body stays one line, because a
/// multi-line paste is a block the composer holds for review. The alternative was a second write
/// delayed long enough to outlast a heuristic on a loaded machine, which is a race wearing a
/// fix's clothes.
///
/// The line breaks were only ever for us; `mars brief show` still wraps it for a human.
pub fn typed_bytes(text: &str) -> String {
    format!("\x1b[200~{}\x1b[201~\r", text.trim_end())
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
    // and it is invisible at a glance because the command still looks long and careful. Both verbs
    // per pattern, because `Edit` alone leaves `Write` open on every file here.
    let (allow, deny) = (allow_section(argv), deny_section(argv));
    if !WORKER_DENY.iter().all(|p| deny.contains(&format!("Edit({p})")) && deny.contains(&format!("Write({p})"))) {
        return false;
    }
    // AND every permission the orders require. A worker missing these is not a safer worker — it
    // is one that stalls at its first git command, silently, which is worse than a loud refusal
    // because nothing anywhere says it happened.
    WORKER_ALLOW.iter().all(|p| allow.contains(p))
}

/// Every pattern denied for BOTH ways of changing a file.
///
/// The list named only `Edit` until a live run watched an agent replace a whole file with `Write`
/// — a tool the `Edit` rule never sees. Everything this list protects is a file that executes
/// without being run, so a hole in it is not a smaller deny-list, it is none at all.
fn deny_flags(patterns: &[&str]) -> String {
    patterns
        .iter()
        .flat_map(|p| [format!("'Edit({p})'"), format!("'Write({p})'")])
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the standing orders REQUIRE a worker to do, pre-approved.
///
/// **A rule that mandates a command the scope then gates is not a rule, it is a stall.** Under
/// `acceptEdits` edits flow and everything else prompts — so a worker told to branch, push and
/// open a PR stops dead at its first `git checkout`, waiting for a human who by construction is
/// not there. Measured on the first live run: it stalled inside a minute and would have sat there
/// all night. Whatever WORKING-MODEL orders, this list must permit; they are edited together.
///
/// Narrow on purpose. `git push` and `gh pr create` are here because the orders demand them;
/// `git rebase`, `git reset` and anything that rewrites history are not, and still gate.
pub const WORKER_ALLOW: &[&str] = &[
    "Bash(git status:*)",
    "Bash(git diff:*)",
    "Bash(git log:*)",
    "Bash(git branch:*)",
    "Bash(git checkout:*)",
    "Bash(git add:*)",
    "Bash(git commit:*)",
    "Bash(git push:*)",
    "Bash(gh pr create:*)",
    // The two reports. `Write`, not `Edit`: both files are created rather than changed, and a
    // creation is the case `acceptEdits` does not cover.
    "Write(**/briefs/*/in_process.md)",
    "Write(**/briefs/*/completed.md)",
];

/// The planner's one required write. It exists for the same reason as `WORKER_ALLOW`: the planner
/// stalled asking permission to write the very file it was started to write.
pub const PLANNER_ALLOW: &[&str] = &["Write(**/briefs/*/brief.md)"];

fn allow_flags(patterns: &[&str]) -> String {
    patterns.iter().map(|p| format!("'{p}'")).collect::<Vec<_>>().join(" ")
}

/// The command that starts a pane as a worker. One composer, host-side, so the phone never builds
/// a shell line for a host whose version it does not know.
pub fn worker_start_command() -> String {
    format!(
        "claude --permission-mode acceptEdits --allowedTools {} --disallowedTools {}\n",
        allow_flags(WORKER_ALLOW),
        deny_flags(WORKER_DENY),
    )
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
    seed_doc(&root.join("WORKING-MODEL.md"), WORKING_MODEL)?;
    seed_doc(&root.join("PLANNING-MODEL.md"), PLANNING_MODEL)?;
    Ok(())
}

fn seed_doc(path: &Path, built_in: &str) -> std::io::Result<()> {
    let replace = match std::fs::read_to_string(path) {
        Err(_) => true,
        Ok(on_disk) => on_disk != built_in && doc_version(&on_disk) < doc_version(built_in),
    };
    if replace {
        std::fs::write(path, built_in)?;
    }
    Ok(())
}

pub const WORKING_MODEL: &str = include_str!("manager_docs/WORKING-MODEL.md");
pub const PLANNING_MODEL: &str = include_str!("manager_docs/PLANNING-MODEL.md");

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
