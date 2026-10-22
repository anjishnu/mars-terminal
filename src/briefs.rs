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
    /// Where the work happens, recorded at MINT time from the pane the draft was pressed in.
    ///
    /// A brief without this is a brief whose acceptance nobody can check: `verify:` names commands
    /// and commands need a directory, and by the time anyone asks, the pane may hold something
    /// else entirely. Captured once, when it is a fact.
    pub repo: Option<PathBuf>,
    /// The commands that decide whether this was built. Run by MARS, never by the worker — see
    /// `verify`.
    pub verify: Vec<String>,
    /// One paragraph: what this brief is for. See `idea` — the card leads with this rather than
    /// with the rulings, because a board is scanned and the rulings are the argument.
    pub idea: Option<String>,
    /// One line per fork, already ruled: the question and the option chosen.
    ///
    /// **This is what approval actually reads.** The forks ARE the design, so three lines is the
    /// decision; a prose summary would send the reader into the file, and a reader who has to open
    /// a 300-line document on a phone does not approve, they defer.
    pub forks: Vec<String>,
    pub report: Option<Report>,
    pub created_ts: u64,
}

/// What a worker reported, read from `completed.md`. `None` until there is one.
#[derive(Clone, Debug)]
pub struct Report {
    /// `done` | `partial` | `blocked` | `rejected`. Kept as written rather than parsed into an
    /// enum: an outcome we do not recognise must still reach the reader, because the two that
    /// matter most are the two nobody expects.
    pub outcome: String,
    pub pr: Option<String>,
    /// Criteria met, and how many there were. A tally, not a verdict — `9/11` and `11/11` are
    /// different things to walk into.
    pub met: usize,
    pub total: usize,
}

/// One verify command and what actually happened when it ran.
#[derive(Clone, Debug)]
pub struct VerifyRow {
    pub cmd: String,
    /// `None` when the command never ran — refused, or the program is not on this machine. A
    /// missing exit code and a non-zero one are different facts and must not render the same.
    pub exit: Option<i32>,
    pub note: String,
}

impl VerifyRow {
    pub fn ok(&self) -> bool {
        self.exit == Some(0)
    }
}

/// Read one brief. `None` when the directory holds no `brief.md`, which is how a half-created or
/// hand-made directory is ignored rather than half-reported.
pub fn read(brief_dir: &Path) -> Option<Brief> {
    let id = brief_dir.file_name()?.to_str()?.to_string();
    if !safe_id(&id) {
        return None;
    }
    let text = std::fs::read_to_string(brief_dir.join("brief.md")).ok()?;
    let (front, body) = crate::manager::split_front(&text)?;
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
        repo: f("repo").filter(|r| !r.is_empty() && r != "null").map(PathBuf::from),
        verify: parse_list(front, "verify:"),
        idea: idea(body),
        forks: parse_forks(body),
        report: read_report(brief_dir),
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

/// One line per fork: the question, and the option that was ruled.
///
/// Read from the body rather than recorded in the frontmatter, because the body is the thing a
/// human approved — a duplicated summary in the frontmatter is a summary that stops matching the
/// design the first time somebody edits one and not the other.
fn parse_forks(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut question: Option<String> = None;
    for line in body.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("### Fork") {
            // "### Fork 2 — where does it go?" → "Fork 2 — where does it go?"
            question = Some(format!("Fork{}", rest.trim_end_matches(':')).trim().to_string());
            continue;
        }
        // Headings other than a fork end the fork we were collecting, so a chosen marker further
        // down the document cannot attach itself to a question it does not belong to.
        if trimmed.starts_with("## ") {
            question = None;
            continue;
        }
        if question.is_some() && trimmed.contains("chosen") {
            let choice = strip_md(trimmed);
            if let Some(q) = question.take() {
                // The same guard `parse_decisions` applies. A template heading is not a ruled
                // fork, and putting one on an approval card invites a press on a decision nobody
                // made.
                let slot = q.split('—').next_back().unwrap_or(&q);
                if !is_stub(slot) && !is_stub(&choice) {
                    out.push(format!("{q} → {choice}"));
                }
            }
        }
    }
    out
}

/// Markdown down to the words, and only the first clause of them. A card shows the decision, not
/// the argument for it — the argument is in the brief, one tap away.
fn strip_md(line: &str) -> String {
    let s = line.trim_start_matches(['-', ' ', '*']);
    let s = s.split(" *For:*").next().unwrap_or(s);
    let s = s.split(" *Why").next().unwrap_or(s);
    let s = s.replace("**", "").replace("✅", "").replace('`', "");
    // "chosen" is what every one of these lines says, so it carries nothing — the line is only
    // here because it was chosen. Dropping it buys back the width the option text needs.
    let s = s.replace(" chosen ", " ").replace("chosen", "");
    // First sentence only. The rest is the justification, and the justification is in the brief.
    let s = s.split(". ").next().unwrap_or(&s).to_string();
    // Whitespace collapsed AFTER the removals, which is where the double spaces come from.
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let s = s.trim().trim_start_matches('—').trim().trim_end_matches(['.', ',', ' ']).to_string();
    if s.chars().count() > 72 {
        format!("{}…", s.chars().take(71).collect::<String>().trim_end())
    } else {
        s
    }
}

/// One option inside a decision — the thing an override picks.
#[derive(Clone, Debug)]
pub struct DecisionOption {
    /// "A" | "B" | "C". Addressable, because an override has to name one.
    pub key: String,
    pub text: String,
    pub chosen: bool,
    /// The `*For:*` / `*Against:*` / `*Why this and not the others:*` clause, kept whole.
    ///
    /// `strip_md` throws this away on purpose — "a card shows the decision, not the argument for
    /// it". That is right for a card you only read and wrong for a card you refine on: **you
    /// cannot override an option you cannot see.**
    pub why: Option<String>,
}

/// A ruled decision — one fork or one artefact.
///
/// A brief carries SIX of these, not three: three HLD forks (what shape) and three LLD artefacts
/// (the hardest things to build). `parse_forks` matched `### Fork` only, so the three hardest
/// decisions in the document never reached the surface at all.
#[derive(Clone, Debug)]
pub struct Decision {
    /// "hld-2" | "lld-1". Stable within a brief, so an override and a staleness note can name it.
    pub id: String,
    pub layer: &'static str,
    pub question: String,
    pub options: Vec<DecisionOption>,
    /// Earlier decisions this ruling assumed, from `*Assumes:* hld-1`.
    ///
    /// One field, written by the planner that is already writing the ruling — and the whole reason
    /// an override can be surgical. Without it the only honest response to a changed premise is
    /// re-running the planner over the entire brief, which discards every decision you already
    /// agreed with.
    pub depends_on: Vec<String>,
    /// An upstream override invalidated this ruling. Derived, never stored as its own state.
    pub stale: bool,
    
/// A human chose against the recommendation.
    pub overridden: bool,
}

impl Decision {
    pub fn chosen(&self) -> Option<&DecisionOption> {
        self.options.iter().find(|o| o.chosen)
    }
}

/// The raw text of one decision's `###` block — what a re-ruling has to change.
fn decision_block(body: &str, id: &str) -> Option<String> {
    let mut cur: Option<String> = None;
    let mut buf = String::new();
    for line in body.lines() {
        let t = line.trim();
        if let Some((did, _, _)) = decision_heading(t) {
            if cur.as_deref() == Some(id) {
                return Some(buf);
            }
            cur = Some(did);
            buf.clear();
        } else if t.starts_with("## ") {
            if cur.as_deref() == Some(id) {
                return Some(buf);
            }
            cur = None;
            buf.clear();
        }
        if cur.as_deref() == Some(id) {
            buf.push_str(line);
            buf.push('\n');
        }
    }
    (cur.as_deref() == Some(id)).then_some(buf)
}

/// A short, stable fingerprint of a ruling.
///
/// FNV-1a rather than a crypto hash: this is not defending against anyone, it is answering "did
/// these bytes change" — and the answer must be the same on every machine that reads the brief,
/// so a `DefaultHasher` (explicitly not stable across builds) would be the wrong tool.
fn ruling_mark(text: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.split_whitespace().collect::<Vec<_>>().join(" ").bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:06x}", h & 0xffffff)
}

/// An unfilled slot in the template, rather than something a planner wrote.
///
/// The template's own two marks: `<angled>` for a slot and `…` for a body. Shared by both
/// parsers — `parse_decisions` grew this test first and `parse_forks` did not, so a superseded
/// brief showed no decisions (correct) and then fell straight through to the legacy fork line and
/// rendered `Fork 1 — <the question> → Option C — …` on the card anyway. One filter, two readers.
fn is_stub(t: &str) -> bool {
    let t = t.trim();
    t.is_empty() || t == "…" || t == "..." || (t.starts_with('<') && t.ends_with('>'))
}

/// The heading that opens a decision, if this line is one.
fn decision_heading(trimmed: &str) -> Option<(String, &'static str, String)> {
    // "### Fork 2 — where does the claim live?"
    if let Some(rest) = trimmed.strip_prefix("### Fork") {
        let rest = rest.trim();
        let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if n.is_empty() {
            return None;
        }
        let q = rest[n.len()..].trim().trim_start_matches(['—', '-', ':']).trim();
        return Some((format!("hld-{n}"), "hld", q.to_string()));
    }
    // "### Artefact 1 of 3 — the hardest thing to build". The template's own words, so the parser
    // matches the document the planner is told to write rather than a second shape nobody writes.
    for lead in ["### Artefact", "### Artifact"] {
        if let Some(rest) = trimmed.strip_prefix(lead) {
            let rest = rest.trim();
            let n: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if n.is_empty() {
                return None;
            }
            let q = rest[n.len()..]
                .trim()
                .trim_start_matches("of 3")
                .trim()
                .trim_start_matches(['—', '-', ':'])
                .trim();
            return Some((format!("lld-{n}"), "lld", q.to_string()));
        }
    }
    None
}

/// `- **Option B** — text *For:* …` → one option. `None` when the line is not an option.
fn parse_option(trimmed: &str) -> Option<DecisionOption> {
    let s = trimmed.trim_start_matches(['-', '*', ' ']);
    let s = s.strip_prefix("**").unwrap_or(s);
    let rest = s.strip_prefix("Option ")?;
    let key = rest.chars().next().filter(|c| c.is_ascii_alphabetic())?.to_ascii_uppercase();
    let after = &rest[key.len_utf8()..];
    // The chosen marker rides inside the bold run: `**Option C ✅ chosen**`.
    let chosen = after.contains("chosen") || after.contains('✅');
    let body = after
        .split_once("**")
        .map(|(_, b)| b)
        .unwrap_or(after)
        .trim()
        .trim_start_matches(['—', '-', ':'])
        .trim();
    // The argument is kept, not split off — see `DecisionOption::why`.
    let (text, why) = match body.find(" *For:*").or_else(|| body.find(" *Why")) {
        Some(i) => (body[..i].trim().to_string(), Some(body[i..].trim().to_string())),
        None => (body.to_string(), None),
    };
    Some(DecisionOption {
        key: key.to_string(),
        text: text.replace("**", "").replace('`', "").trim().to_string(),
        chosen,
        why: why.map(|w| w.replace('*', "").trim().to_string()).filter(|w| !w.is_empty()),
    })
}

/// Every decision in a brief, with its options intact.
pub fn parse_decisions(body: &str) -> Vec<Decision> {
    // AN OPTION IS A BULLET, NOT A LINE, and real briefs are hard-wrapped.
    //
    // `parse_option` reads one line, so an option whose `*Why this and not the others:*` fell on a
    // continuation line lost its rationale entirely. Two things broke downstream and both looked
    // like something else: the second reader objected "ruled without naming why the others lose"
    // on almost every decision — measured at 4 to 9 objections across all five briefs a real
    // planner wrote, every one of them false — and the refinement card, which exists so that an
    // override is an informed act rather than a guess, had no reason to show.
    //
    // An audit louder than its evidence is one people learn to discount, so this is joined before
    // parsing rather than made lenient after it. A wrapped bullet is folded onto its own first
    // line; a new bullet, a heading or a blank line ends it.
    let mut joined: Vec<String> = Vec::new();
    for raw in body.lines() {
        let t = raw.trim();
        let starts_bullet = t.starts_with("- ") || t.starts_with("* ");
        let continues = !t.is_empty()
            && !starts_bullet
            && !t.starts_with('#')
            && joined.last().is_some_and(|p| {
                let p = p.trim_start();
                p.starts_with("- ") || p.starts_with("* ")
            });
        if continues {
            let last = joined.last_mut().expect("checked above");
            last.push(' ');
            last.push_str(t);
        } else {
            joined.push(raw.to_string());
        }
    }
    let mut out: Vec<Decision> = Vec::new();
    let mut cur: Option<Decision> = None;
    for line in &joined {
        let trimmed = line.trim();
        if let Some((id, layer, question)) = decision_heading(trimmed) {
            if let Some(d) = cur.take() {
                out.push(d);
            }
            cur = Some(Decision {
                id,
                layer,
                question,
                options: Vec::new(),
                depends_on: Vec::new(),
                stale: false,
                overridden: false,
            });
            continue;
        }
        // A `##` heading closes whatever was open, so an option further down the document cannot
        // attach itself to a question it does not belong to.
        if trimmed.starts_with("## ") {
            if let Some(d) = cur.take() {
                out.push(d);
            }
            continue;
        }
        let Some(d) = cur.as_mut() else { continue };
        if let Some(rest) = trimmed
            .trim_start_matches(['-', '*', ' '])
            .strip_prefix("Assumes:")
            .or_else(|| trimmed.strip_prefix("*Assumes:*"))
        {
            d.depends_on.extend(
                rest.replace('*', "")
                    .split(',')
                    .map(|x| x.trim().trim_matches('`').to_string())
                    .filter(|x| !x.is_empty()),
            );
            continue;
        }
        if let Some(o) = parse_option(trimmed) {
            d.options.push(o);
        }
    }
    if let Some(d) = cur.take() {
        out.push(d);
    }
    // TEMPLATE STUBS ARE NOT DECISIONS, and "has options" is not enough to tell them apart —
    // the template ships literal `- **Option A** — …` lines as an example of the shape, so a
    // freshly minted brief parsed as six fully-formed decisions and the card offered
    // `<the question> → …` for approval. Found by pointing the tracer at a superseded brief,
    // which is minted from the template and had never been read on a card before.
    //
    // The placeholders are the template's own two marks: `<angled>` for a slot and `…` for a
    // body. Nothing a planner writes looks like that, so the test stays specific rather than
    // guessing at "looks unfinished".
    out.retain(|d| {
        !d.options.is_empty() && !is_stub(&d.question) && !d.options.iter().all(|o| is_stub(&o.text))
    });
    out
}

/// Re-attach the trailing newline `lines()` drops.
///
/// Every rewrite here goes through `lines()` + `join("\n")`, which silently eats the final
/// newline — so each override left the file one byte shorter and made the LAST block in the
/// document differ from itself. Caught by the tracer's own acceptance check ("every other
/// decision is byte-identical before and after"), which is exactly the kind of thing that
/// assertion is for.
fn keep_trailing(original: &str, out: String) -> String {
    if original.ends_with('\n') && !out.ends_with('\n') { out + "\n" } else { out }
}

/// The section a heading opens, as a line range over the document.
fn section_span(lines: &[&str], heading: &str) -> Option<(usize, usize)> {
    let start = lines.iter().position(|l| l.trim() == heading)?;
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with("## "))
        .map(|i| start + 1 + i)
        .unwrap_or(lines.len());
    Some((start, end))
}

/// The section every override and every staleness note is written into.
///
/// **No new file, and no new state.** An override is definitionally a decision already made, so it
/// belongs in the section that is already binding on the worker, already parsed, and already
/// exists to stop settled ground being re-litigated at 3am. Inventing an `overrides.json` beside
/// it would mean two places a decision lives and a rule about which wins.
const MADE: &str = "## Decisions already made";

/// Overrides and staleness, read back out of the brief.
fn parse_made(body: &str) -> (Vec<(String, String)>, Vec<String>) {
    let lines: Vec<&str> = body.lines().collect();
    let Some((start, end)) = section_span(&lines, MADE) else { return (Vec::new(), Vec::new()) };
    let (mut over, mut stale) = (Vec::new(), Vec::new());
    for line in &lines[start..end] {
        let t = line.trim().trim_start_matches(['-', ' ']);
        let Some(rest) = t.strip_prefix('[') else { continue };
        let Some((id, rest)) = rest.split_once(']') else { continue };
        let id = id.trim().to_string();
        if rest.contains("stale") {
            stale.push(id);
        } else if let Some((_, choice)) = rest.split_once('→') {
            // "→ Option B — …" — the key is all that has to survive the round trip.
            if let Some(k) = choice.trim().strip_prefix("Option ").and_then(|c| c.chars().next()) {
                over.push((id, k.to_ascii_uppercase().to_string()));
            }
        }
    }
    (over, stale)
}

/// Fold the recorded overrides and staleness back onto the parsed decisions.
pub fn apply_made(decisions: &mut [Decision], body: &str) {
    let (over, stale) = parse_made(body);
    for (id, key) in over {
        if let Some(d) = decisions.iter_mut().find(|d| d.id == id) {
            // The human's pick replaces the planner's, rather than sitting beside it. Two "chosen"
            // marks on one decision is a card that cannot say what was decided.
            if d.options.iter().any(|o| o.key == key) {
                for o in d.options.iter_mut() {
                    o.chosen = o.key == key;
                }
                d.overridden = true;
            }
        }
    }
    for id in stale {
        if let Some(d) = decisions.iter_mut().find(|d| d.id == id) {
            d.stale = true;
        }
    }
}

/// Every decision in a brief, with overrides and staleness already folded in.
pub fn decisions_of(brief_dir: &Path) -> Vec<Decision> {
    let Ok(body) = std::fs::read_to_string(brief_dir.join("brief.md")) else { return Vec::new() };
    let mut d = parse_decisions(&body);
    apply_made(&mut d, &body);
    d
}

/// Append lines to `## Decisions already made`, creating the section if the brief lacks it.
fn append_to_made(body: &str, add: &[String]) -> String {
    let lines: Vec<&str> = body.lines().collect();
    match section_span(&lines, MADE) {
        Some((_, end)) => {
            let mut out: Vec<String> = lines[..end].iter().map(|s| s.to_string()).collect();
            // Trailing blank lines inside the section would push the new entry away from the ones
            // it belongs with.
            while out.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
                out.pop();
            }
            out.extend(add.iter().cloned());
            out.push(String::new());
            out.extend(lines[end..].iter().map(|s| s.to_string()));
            keep_trailing(body, out.join("\n"))
        }
        None => {
            let mut out = body.trim_end().to_string();
            out.push_str("\n\n");
            out.push_str(MADE);
            out.push_str("\n\n");
            out.push_str(&add.join("\n"));
            out.push('\n');
            out
        }
    }
}

/// Put a brief out of the way.
///
/// **A move, not a flag.** State here is which files exist — `brief` / `in_process` / `completed`
/// — and adding an `archived: true` somewhere would be the first piece of state in this design
/// that is asserted rather than observed, with all the drift that invites. Moving the directory
/// under `archive/` keeps the rule: `list` reads one level and `read` returns `None` for a
/// directory with no `brief.md`, so the archive is skipped without `list` learning it exists.
///
/// Reversible with `mv`, deliberately. Nothing is deleted — a brief carries the argument that
/// produced it, and the one thing you want when a decision resurfaces months later is the document
/// that settled it. "Delete" is not offered for the same reason.
pub fn archive(id: &str) -> std::io::Result<PathBuf> {
    if !safe_id(id) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad brief id"));
    }
    let Some(root) = dir() else {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no home directory"));
    };
    let from = root.join(id);
    if !from.join("brief.md").exists() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("no brief {id}")));
    }
    let to = root.join("archive").join(id);
    std::fs::create_dir_all(root.join("archive"))?;
    if to.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} is already archived", to.display()),
        ));
    }
    std::fs::rename(&from, &to)?;
    Ok(to)
}

/// WHAT THIS BRIEF IS FOR, in one paragraph.
///
/// The card used to lead with the six ruled decisions, which is the argument and not the proposal.
/// A board is SCANNED — six decision rows times N briefs is a wall — and the one thing a reader
/// needs in order to triage, "what is this even for", appeared nowhere on it: the problem
/// statement lived in the document and never reached the surface.
///
/// Deterministic, and no model call. The planner writes `## The idea` because it is already
/// writing six rulings and one paragraph is cheap; when a brief predates the section, the problem
/// statement's own prose stands in. A summariser here would add latency, cost and a chance of
/// being wrong to produce a sentence the document already contains.
pub fn idea(body: &str) -> Option<String> {
    let lines: Vec<&str> = body.lines().collect();
    // The dedicated section first, then the problem statement. Both are prose the planner already
    // wrote; neither is generated.
    for heading in ["## The idea", "## Problem + evidence"] {
        let Some((start, end)) = section_span(&lines, heading) else { continue };
        let mut para = String::new();
        for line in &lines[start + 1..end] {
            let t = line.trim();
            // A list is evidence — addresses and line anchors — not the idea. The first prose
            // paragraph is; it ends at the first blank line after it starts.
            if t.starts_with('-') || t.starts_with('*') || t.starts_with('#') {
                break;
            }
            if t.is_empty() {
                if !para.is_empty() {
                    break;
                }
                continue;
            }
            if !para.is_empty() {
                para.push(' ');
            }
            para.push_str(t);
        }
        // The section leads with its own binding-ness, which is a fact about the section rather
        // than a sentence about the work.
        let para = para
            .trim_start_matches("BINDING.")
            .trim_start_matches("ADVISORY.")
            .trim()
            .to_string();
        if para.chars().count() > 24 {
            return Some(para);
        }
    }
    None
}

/// The second reader, deterministic half.
///
/// D4 asks four questions of a brief: is the premise checkable, is any criterion unverifiable,
/// does the file list match the blast radius, and was a fork ruled without naming its rejection.
/// **Three of those are arithmetic**, and arithmetic does not need a model — it needs to be run.
/// So this is the half that can be built now and be right every time; the adversarial reader that
/// tries to REFUTE the design is the half that needs a model, and it is still open.
///
/// **It annotates; it does not gate.** A model call standing between you and your own work is a
/// worse failure than an unreviewed brief. Staleness gates because it is arithmetic about
/// references; an objection is a judgement about design, and judgements do not get a veto here.
pub fn review(brief_dir: &Path) -> std::io::Result<Vec<String>> {
    let body = std::fs::read_to_string(brief_dir.join("brief.md"))?;
    let mut decisions = parse_decisions(&body);
    apply_made(&mut decisions, &body);
    let mut out: Vec<String> = Vec::new();

    // 1 — every ruling must name what it rejected. A fork ruled without its rejection is a
    //     decision the worker will re-open, because nothing on the page says it was closed.
    for d in &decisions {
        let named = d
            .chosen()
            .and_then(|c| c.why.as_deref())
            .map(|w| w.to_lowercase().contains("why") || w.to_lowercase().contains("not the other"))
            .unwrap_or(false);
        if !named {
            out.push(format!(
                "- [{}] ruled without naming why the others lose. A worker reading this re-opens it.",
                d.id
            ));
        }
    }

    // 2 — every criterion needs something that checks it. `unverifiable` is a defect in the BRIEF,
    //     and the brief is exactly where it can still be cheaply fixed.
    let front = crate::manager::split_front(&body).map(|(f, _)| f).unwrap_or("");
    let verify = parse_list(front, "verify");
    let any_keyed = verify.iter().any(|c| !verify_key(c).is_empty());
    for (n, text) in acceptance(&body) {
        let covered = if any_keyed {
            verify.iter().any(|c| verify_key(c).contains(&n))
        } else {
            !verify.is_empty()
        };
        if !covered {
            out.push(format!(
                "- [acceptance {n}] nothing checks it — \"{}\". Key a `verify:` entry to it, or say plainly that it is unverifiable.",
                text.chars().take(70).collect::<String>()
            ));
        }
    }

    // 3 — a dependency on a decision that does not exist. A dangling reference means staleness
    //     will never fire for it, which silently disarms the only gate this design has.
    let ids: Vec<&str> = decisions.iter().map(|d| d.id.as_str()).collect();
    for d in &decisions {
        for dep in &d.depends_on {
            if !ids.contains(&dep.as_str()) {
                out.push(format!("- [{}] assumes `{dep}`, which is not a decision in this brief.", d.id));
            }
        }
    }

    // 4 — the brief has to have a design at all.
    if decisions.is_empty() {
        out.push("- [brief] no ruled decisions. There is nothing here to approve.".into());
    }

    let head = "# Review\n\n\
                Written by the deterministic second reader. It annotates and does not gate — an \
                objection is a judgement, and a judgement does not stand between you and your own \
                work. Each line names the decision or criterion it is about.\n\n";
    let doc = if out.is_empty() {
        format!("{head}Nothing to refute. Every ruling names its rejection, every criterion has \
                 something that checks it, and no decision assumes one that does not exist.\n")
    } else {
        format!("{head}{}\n", out.join("\n"))
    };
    std::fs::write(brief_dir.join("review.md"), doc)?;
    Ok(out)
}

/// File a nomination — the one thing that originates work.
///
/// **A nomination is a memo.** Ruled in the work model and reused here rather than rebuilt: Rover
/// already proposes memos, you already press them, and they already file and decay by going
/// unread. A `nominations/` class would buy a lifecycle nothing needs.
///
/// What makes it a nomination rather than a note is `kind: nomination` and the fact that it CITES
/// something. A nomination whose provenance is a sentence is an opinion; one that names the
/// snapshot it was read out of can be walked back to, which is the whole test this loop has to
/// pass — point at a line of code and walk it back to the observation that started it.
pub fn nominate(
    session: &str,
    pane: &str,
    headline: &str,
    body: &str,
    ts: u64,
) -> std::io::Result<PathBuf> {
    let Some(home) = crate::sys::paths::home_dir() else {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no home directory"));
    };
    let sdir = home.join(".mars").join("sessions").join(session);
    // The newest snapshot IS the observation. Read from disk rather than described, so the cite
    // names a file that exists and a reader can open.
    let snap = std::fs::read_dir(sdir.join("snapshots"))
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".json"))
        .max();
    let Some(snap) = snap else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("no snapshot under {} — there is nothing to cite, so there is nothing to nominate", sdir.join("snapshots").display()),
        ));
    };
    let slug: String = headline
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join("-");
    let dir = sdir.join("memos");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{slug}.md"));
    let doc = format!(
        "---\n\
         source: agent\n\
         title: {slug}\n\
         kind: nomination\n\
         v: 1\n\
         created_ts: {ts}\n\
         priority: 60\n\
         severity: info\n\
         session: \"{session}\"\n\
         pane: \"{pane}\"\n\
         headline: \"{headline}\"\n\
         expired: false\n\
         cites:\n\
         \x20 - {{snapshot: \"{snap}\", session: \"{session}\", pane: \"{pane}\"}}\n\
         ---\n\
         {body}\n\
         \n\
         Read out of `{snap}`. Press draft to turn this into a brief; the planner receives this \
         provenance as the problem statement.\n"
    );
    std::fs::write(&path, doc)?;
    Ok(path)
}

/// Write decisions settled in conversation into `## Decisions already made`.
///
/// The same section an override lands in, and for the same reason: both are decisions already
/// made, both bind the worker, and both are read back by `parse_made`. One section, one format,
/// one reader — a `prior.md` beside it would be a second place a settled decision lives and a rule
/// about which of them wins.
pub fn record_prior(brief_dir: &Path, prior: &[crate::session::PriorDecision]) -> std::io::Result<()> {
    if prior.is_empty() {
        return Ok(());
    }
    let path = brief_dir.join("brief.md");
    let body = std::fs::read_to_string(&path)?;
    let lines: Vec<String> = prior
        .iter()
        .map(|d| {
            let mut l = format!("- {} — **{}**", d.question.trim(), d.chose.trim());
            // The rejected option matters more than the chosen one here: it is the thing a worker
            // would otherwise re-propose, and the reason this section exists.
            if let Some(r) = d.rejected.as_deref().map(str::trim).filter(|r| !r.is_empty()) {
                l.push_str(&format!(", not {r}"));
            }
            if !d.why.trim().is_empty() {
                l.push_str(&format!(". {}", d.why.trim()));
            }
            l
        })
        .collect();
    std::fs::write(&path, append_to_made(&body, &lines))
}

/// A human chose against the recommendation.
///
/// Returns the ids this override made stale. **Exactly the declared dependents and nothing else**
/// — the alternative considered and rejected was re-running the planner over the whole brief,
/// which throws away every decision already agreed with, and the one thing refinement must not do
/// is discard the agreement it is building.

/// A human chose against the recommendation.
///
/// Returns the ids this override made stale.
pub fn override_decision(brief_dir: &Path, id: &str, key: &str) -> std::io::Result<Vec<String>> {
    let path = brief_dir.join("brief.md");
    let body = std::fs::read_to_string(&path)?;
    let decisions = parse_decisions(&body);
    let Some(d) = decisions.iter().find(|d| d.id == id) else {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("no decision {id}")));
    };
    let key = key.to_ascii_uppercase();
    let Some(opt) = d.options.iter().find(|o| o.key == key) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{id} has no option {key}"),
        ));
    };
    let was = d.chosen().map(|o| o.key.clone()).unwrap_or_default();
    if was == key {
        return Ok(Vec::new()); // choosing what is already chosen is not an override
    }
    let (already_over, already_stale) = parse_made(&body);
    // W10 — WHO AND WHEN, on the line itself. It recorded what was chosen and what the planner
    // chose, which is enough at n=1 and wrong the moment two people share a board: the section is
    // BINDING on a worker, and a binding instruction with no author is one nobody can ask about.
    let who = std::env::var("MARS_ACTOR")
        .ok()
        .filter(|w| !w.trim().is_empty())
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "unknown".into());
    let when = crate::worklog::now_secs();
    let mut add = vec![format!(
        "- [{id}] **overridden** → Option {key} — {} *(planner chose {was}; by {who} at {when})*",
        opt.text
    )];
    // W7 — TRANSITIVE, not one level deep.
    //
    // This walked `depends_on` once, so a decision assuming a decision that assumed the overridden
    // one was never marked. Silent, and the worst kind: the gate said nothing was stale while a
    // ruling downstream rested on a premise that had moved. Approving on that is approving
    // something nobody has read, which is the exact failure the gate exists to prevent.
    //
    // Fixed point rather than recursion: the graph is six nodes and a cycle in it is a planner
    // error, not something to crash on.
    let mut went_stale: Vec<String> = Vec::new();
    let mut frontier = vec![id.to_string()];
    while let Some(cause) = frontier.pop() {
        for other in &decisions {
            if other.id == id || !other.depends_on.iter().any(|x| *x == cause) {
                continue;
            }
            if went_stale.contains(&other.id) || already_stale.iter().any(|x| x == &other.id) {
                continue;
            }
            // W6 — THE RULING'S FINGERPRINT RIDES ALONG. Without it `rerule` was a state change
            // and not a re-ruling: it cleared the note whether or not anything had been
            // reconsidered, so the only gate in this design could be satisfied by pressing it.
            // With the mark, clearing requires the ruling to have actually moved.
            let mark = decision_block(&body, &other.id)
                .map(|b| ruling_mark(&b))
                .unwrap_or_else(|| "------".into());
            add.push(format!(
                "- [{}] **stale** — assumes {cause}, which {} *(ruling {mark})*",
                other.id,
                if cause == id { "was overridden" } else { "went stale" }
            ));
            went_stale.push(other.id.clone());
            frontier.push(other.id.clone());
        }
    }
    // Re-overriding the same decision replaces the earlier line rather than stacking a second.
    let body = if already_over.iter().any(|(i, _)| i == id) {
        strip_made_lines(&body, |t| t.starts_with(&format!("[{id}]")) && !t.contains("stale"))
    } else {
        body
    };
    std::fs::write(&path, append_to_made(&body, &add))?;
    Ok(went_stale)
}

/// Drop lines from `## Decisions already made` that match a predicate on their trimmed text.
fn strip_made_lines(body: &str, pred: impl Fn(&str) -> bool) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let Some((start, end)) = section_span(&lines, MADE) else { return body.to_string() };
    let mut out: Vec<String> = lines[..=start].iter().map(|s| s.to_string()).collect();
    for line in &lines[start + 1..end] {
        if !pred(line.trim().trim_start_matches(['-', ' '])) {
            out.push(line.to_string());
        }
    }
    out.extend(lines[end..].iter().map(|s| s.to_string()));
    keep_trailing(body, out.join("\n"))
}

/// The planner re-ruled this decision, so it is no longer stale.
///
/// Only the staleness note is removed; the override that caused it stays, because it is still a
/// decision that was made.
/// Has a human said this brief may be built?
///
/// **A fourth file, and deliberately not a fourth state.** `state_of` answers *how far has this
/// got* from the three files the work produces; approval answers a different question — *may this
/// run without me* — and it is the human's answer, not the work's. Folding it into the state
/// machine would have made a permission look like a stage.
///
/// It is still a presence check over siblings, for the same reason everything else here is: a
/// `state:` field can disagree with reality and a file cannot disagree with its own existence.
pub fn is_approved(brief_dir: &Path) -> bool {
    brief_dir.join("approved.md").exists()
}

/// Record that a human approved this brief, and what they were looking at when they did.
///
/// **The gate is arithmetic, not judgement.** A brief with a stale decision has a hole in its
/// design — some ruling assumed a fork that was later overridden — and approving it is approving
/// something nobody has read. That is a dangling reference, which is why it may gate; the second
/// reader's objections are judgements about design and deliberately may not.
///
/// **What this file is for.** Under autopilot the manager assigns without a press, so something
/// has to carry the human's consent forward in time from the moment it was given. This is that
/// thing, and it names who and when because an action ledger with no accountable person is a log,
/// not a ledger. It also records the decisions as ruled at approval time: the brief is editable
/// afterwards, and *"what was actually approved"* must not be a question answered by reading a
/// file that has since changed.
pub fn approve(brief_dir: &Path, who: &str, ts: u64) -> std::io::Result<()> {
    if !brief_dir.join("brief.md").exists() {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no brief.md"));
    }
    let decisions = decisions_of(brief_dir);
    let stale: Vec<&str> = decisions.iter().filter(|d| d.stale).map(|d| d.id.as_str()).collect();
    if !stale.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "approve is withheld: {} still stale. Each assumes a ruling that was overridden — \
                 re-rule it in brief.md, then `mars brief clear-stale <id>`.",
                stale.join(", ")
            ),
        ));
    }
    let ruled: String = decisions
        .iter()
        .map(|d| {
            let chosen = d.options.iter().find(|o| o.chosen).map(|o| o.key.as_str()).unwrap_or("—");
            format!("- [{}] {} → {}\n", d.id, d.question, chosen)
        })
        .collect();
    let body = format!(
        "---\napproved_by: {who}\napproved_ts: {ts}\ndecisions: {}\n---\n# Approved\n\n\
         The rulings as they stood when this was approved. The brief stays editable; this does \
         not, so that what was consented to remains answerable after the fact.\n\n{ruled}",
        decisions.len(),
    );
    std::fs::write(brief_dir.join("approved.md"), body)
}

pub fn clear_stale(brief_dir: &Path, id: &str) -> std::io::Result<()> {
    let path = brief_dir.join("brief.md");
    let body = std::fs::read_to_string(&path)?;
    // The mark recorded when it went stale, if there is one.
    let recorded = body.lines().find_map(|l| {
        let t = l.trim().trim_start_matches(['-', ' ']);
        (t.starts_with(&format!("[{id}]")) && t.contains("stale"))
            .then(|| t.split("*(ruling ").nth(1)?.split(')').next().map(str::to_string))
            .flatten()
    });
    if let Some(mark) = recorded {
        let now = decision_block(&body, id).map(|b| ruling_mark(&b)).unwrap_or_default();
        if now == mark {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "{id} has not been re-ruled — its options are byte-for-byte what they were \
                     when it went stale. Edit the ruling in brief.md, then clear it."
                ),
            ));
        }
    }
    let out = strip_made_lines(&body, |t| {
        t.starts_with(&format!("[{id}]")) && t.contains("stale")
    });
    std::fs::write(&path, out)
}

/// One numbered acceptance criterion, and what the audit made of it.
#[derive(Clone, Debug)]
pub struct Criterion {
    pub n: usize,
    pub text: String,
    /// `met` | `unmet` | `unverifiable`.
    ///
    /// **`unverifiable` is a first-class outcome, not a soft failure.** A criterion nothing can
    /// check is a defect in the BRIEF, and it has to read as one — rolling it into `unmet` blames
    /// the work for the specification's problem, and rolling it into `met` is how a brief passes
    /// without anybody having checked anything.
    pub verdict: &'static str,
    /// Set when the worker's claim and the observation disagree — the case the tier exists for.
    pub disputed: bool,
}

/// Numbered criteria from `## Acceptance`.
pub fn acceptance(body: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = body.lines().collect();
    let Some((start, end)) = section_span(&lines, "## Acceptance") else { return Vec::new() };
    let mut out = Vec::new();
    for line in &lines[start + 1..end] {
        let t = line.trim();
        let n: String = t.chars().take_while(|c| c.is_ascii_digit()).collect();
        if n.is_empty() {
            continue;
        }
        let rest = t[n.len()..].trim_start_matches(['.', ')', ' ']).trim();
        // The template ships `1. …` as a placeholder; an ellipsis is not a criterion.
        if rest.is_empty() || rest == "…" || rest == "..." {
            continue;
        }
        if let Ok(n) = n.parse::<usize>() {
            out.push((n, rest.to_string()));
        }
    }
    out
}

/// What the worker claimed, per criterion, from `completed.md`'s front matter.
fn claims(brief_dir: &Path) -> Vec<(usize, Option<bool>)> {
    let Ok(text) = std::fs::read_to_string(brief_dir.join("completed.md")) else { return Vec::new() };
    let Some((front, _)) = crate::manager::split_front(&text) else { return Vec::new() };
    let mut out = Vec::new();
    for line in front.lines().map(str::trim).filter(|l| l.starts_with("- {n:")) {
        let n: String = line[5..].trim().chars().take_while(|c| c.is_ascii_digit()).collect();
        let Ok(n) = n.parse::<usize>() else { continue };
        // A claim can decline to answer. `met: unverifiable` is the worker saying the criterion
        // cannot be checked, which is information — and different from claiming failure.
        let v = if line.contains("met: unverifiable") {
            None
        } else {
            Some(line.contains("met: true"))
        };
        out.push((n, v));
    }
    out
}

/// The entry with any `1,2:` key removed — what actually gets run.
pub fn strip_verify_key(cmd: &str) -> &str {
    if verify_key(cmd).is_empty() {
        cmd
    } else {
        cmd.split_once(':').map(|(_, rest)| rest.trim()).unwrap_or(cmd)
    }
}

/// The criteria a `verify:` entry claims to check — `"1,2: cargo test"` → `[1, 2]`.
///
/// Empty when the entry names none, which is the legacy shape and still valid: an unkeyed command
/// is a check on the brief rather than on any one criterion.
fn verify_key(cmd: &str) -> Vec<usize> {
    let Some((head, _)) = cmd.split_once(':') else { return Vec::new() };
    // Only a bare list of numbers is a key. `git rev-parse --verify HEAD` has no colon; a command
    // that does — a URL, a path — must not be mistaken for one.
    let parts: Vec<&str> = head.split(',').map(str::trim).collect();
    if parts.is_empty() || parts.iter().any(|p| p.is_empty() || !p.chars().all(|c| c.is_ascii_digit())) {
        return Vec::new();
    }
    parts.iter().filter_map(|p| p.parse().ok()).collect()
}

/// The audit, tiers 0 and 1.
///
/// Tier 0 is the brief's own `verify:` argv and is already built. Tier 1 is this: every numbered
/// criterion set against what the worker claimed and what tier 0 observed. It is free arithmetic,
/// and its whole value is the case where they disagree — `outcome: done` beside a failing command
/// is the thing a rounded-up card would hide.
pub struct Audit {
    pub tier0: Vec<VerifyRow>,
    pub tier1: Vec<Criterion>,
}

impl Audit {
    pub fn disagreements(&self) -> usize {
        self.tier1.iter().filter(|c| c.disputed).count()
    }
    pub fn unmet(&self) -> Vec<usize> {
        self.tier1.iter().filter(|c| c.verdict != "met").map(|c| c.n).collect()
    }
}

pub fn audit(b: &Brief, brief_dir: &Path, timeout: std::time::Duration) -> Audit {
    let tier0 = verify(b, timeout);
    let body = std::fs::read_to_string(brief_dir.join("brief.md")).unwrap_or_default();
    let claimed: Vec<(usize, Option<bool>)> = claims(brief_dir);
    // W4 — TIER 0 IS KEYED TO CRITERIA, when the brief says which.
    //
    // It was flat: `verify:` is a list against the whole brief, so ONE failing command disputed
    // EVERY claim. Measured in the tracer run — a single failing grep marked three criteria unmet,
    // one of which the worker had honestly reported as unverifiable. An audit louder than its
    // evidence is one people learn to discount, which costs more than the tier is worth.
    //
    // A `verify:` entry may name the criteria it checks: `- "1,2: cargo test -p mars"`. Then a
    // failure lands only on 1 and 2, and a criterion nothing names is `unverifiable` — a defect in
    // the BRIEF, said plainly, rather than a failure of the work.
    //
    // If NO entry is keyed the brief predates this, so the flat reading stands. A migration that
    // silently turned every existing brief's criteria unverifiable would be a worse lie than the
    // one being fixed.
    let keyed: Vec<(Vec<usize>, bool)> = tier0
        .iter()
        .map(|r| (verify_key(&r.cmd), r.ok()))
        .collect();
    let any_keyed = keyed.iter().any(|(k, _)| !k.is_empty());
    let commands_pass = !tier0.is_empty() && tier0.iter().all(|r| r.ok());
    let tier1 = acceptance(&body)
        .into_iter()
        .map(|(n, text)| {
            let claim = claimed.iter().find(|(cn, _)| *cn == n).map(|(_, v)| *v);
            // What tier 0 observed ABOUT THIS CRITERION: Some(true/false), or None when nothing
            // checks it.
            let observed: Option<bool> = if any_keyed {
                let mine: Vec<bool> =
                    keyed.iter().filter(|(k, _)| k.contains(&n)).map(|(_, ok)| *ok).collect();
                (!mine.is_empty()).then(|| mine.iter().all(|o| *o))
            } else if tier0.is_empty() {
                None
            } else {
                Some(commands_pass)
            };
            let (verdict, disputed) = match (claim, observed) {
                // Nothing said about it at all, by anyone. Not a failure of the work — nobody looked.
                (None, _) | (Some(None), None) => ("unverifiable", false),
                // The worker declined to answer and nothing checks it either.
                (Some(None), Some(_)) => ("unverifiable", false),
                // Claimed met, nothing checks it. `unverifiable` is a first-class outcome, and a
                // criterion no command covers is a hole in the brief rather than a pass.
                (Some(Some(true)), None) => ("unverifiable", false),
                (Some(Some(true)), Some(true)) => ("met", false),
                // Claimed met while the commands that cover it say otherwise. THE case tier 1
                // exists for, and now it names only the criteria actually implicated.
                (Some(Some(true)), Some(false)) => ("unmet", true),
                // Claimed unmet and the commands agree, or nothing checks it — either way, unmet.
                (Some(Some(false)), Some(true)) => ("unmet", true),
                (Some(Some(false)), _) => ("unmet", false),
            };
            Criterion { n, text, verdict, disputed }
        })
        .collect();
    Audit { tier0, tier1 }
}

/// How much of this may happen without a person, and it is a CEILING rather than a grant.
///
/// The governing rule: an agent may act autonomously in proportion to how reversible and how
/// verifiable the action is, bounded by blast radius. These are the four prices that rule pays out
/// at, ordered so that `min` is the safe combinator — every check may only lower the answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Autonomy {
    /// Nobody may do this unattended, whatever else is true.
    Never,
    /// A person must LOOK. No machine oracle decides it — rendered UI, copy, taste, a schema
    /// whose blast radius outruns what a test can see.
    Look,
    /// Build and verify unattended; a person presses to land.
    Press,
    /// Green means land it, with a receipt and an undo.
    Land,
}

impl Autonomy {
    pub fn label(self) -> &'static str {
        match self {
            Autonomy::Never => "never",
            Autonomy::Look => "must be seen",
            Autonomy::Press => "needs a press",
            Autonomy::Land => "lands on green",
        }
    }
}

/// Paths whose blast radius outruns any check a worker can run.
///
/// **Deliberately NOT added to `WORKER_DENY`, and the difference matters.** The deny-list forbids
/// *editing*; this forbids *landing unattended*. A schema change is not work nobody may do — it is
/// work nobody may merge while asleep, which is a checkpoint rather than a prohibition. Denying
/// the wire contract outright would make a large class of approved work impossible to build at
/// all, including work a human explicitly asked for; the honest control sits at the gate that
/// ends the loop, not at the one that starts it.
///
/// `session.rs` is the same argument twice over: it holds the frame enums AND most of the daemon,
/// so a file-level deny would ban ordinary work to reach a few type declarations.
const SCHEMA_PATHS: &[&str] = &[
    "protocol.ts",
    "src/rover/protocol.ts",
    "session.rs",
    "sw.js",
    "Cargo.toml",
    "package.json",
    "build.rs",
    "Makefile",
    ".envrc",
];

/// Lines whose removal means the verifier got weaker.
const CHECK_MARKS: &[&str] = &["assert", "panic!", "[selfcheck]", "#[test]", ".expect("];

/// Did this diff REMOVE or ALTER a check?
///
/// **The oracle is a file workers edit.** Mars grades a brief with `mars --selfcheck`, which is
/// compiled from `src/main.rs` — and the one brief that ever ran end to end added 61 lines to that
/// file. Mars already fixed the adjacent problem by running the verify commands itself rather than
/// trusting a worker's account of them; that stops an agent writing down its own exit codes, and
/// does nothing about an agent editing what the codes are testing.
///
/// Denying `main.rs` would be the wrong fix: adding a selfcheck beside a change is the house
/// standard and should stay cheap. So the rule is about DIRECTION. Additions are free; a removal
/// or a modification is never self-graded, and the specific lines go to a person.
///
/// A line removed and added back unchanged is a move, not a weakening — reindentation and block
/// shuffling would otherwise flag every large diff and train everyone to ignore this.
pub fn weakened_checks(diff: &str) -> Vec<String> {
    let added: Vec<&str> = diff
        .lines()
        .filter(|l| l.starts_with('+') && !l.starts_with("+++"))
        .map(|l| l[1..].trim())
        .collect();
    diff.lines()
        .filter(|l| l.starts_with('-') && !l.starts_with("---"))
        .map(|l| l[1..].trim())
        .filter(|l| CHECK_MARKS.iter().any(|m| l.contains(m)))
        .filter(|l| !added.contains(l))
        .map(str::to_string)
        .collect()
}

/// What a classification decided, and why — because a refusal that does not say what to do about
/// it is a button that does nothing for a reason nobody can see.
#[derive(Clone, Debug)]
pub struct Classified {
    pub autonomy: Autonomy,
    pub why: String,
    /// The checks this diff removed, if any. Shown verbatim: the whole point is that a person
    /// reads the actual lines rather than a claim about them.
    pub weakened: Vec<String>,
}

/// Price a finished brief against reversibility, verifiability and blast radius.
///
/// **Demote-only, and that asymmetry is the safety property.** A model may propose a ceiling by
/// writing one into the brief; this function may only lower it. The manager reads untrusted
/// terminal output all day and section headers in that input are forgeable, so anything a model
/// says about how safe its own work is has to be incapable of raising the limit.
pub fn classify(proposed: Autonomy, changed: &[String], diff: &str, verify: &[String]) -> Classified {
    let mut a = proposed;
    let mut why = String::from("small, verified, and reversible");

    let mut demote = |to: Autonomy, reason: &str, a: &mut Autonomy, why: &mut String| {
        if to < *a {
            *a = to;
            *why = reason.to_string();
        }
    };

    if verify.is_empty() {
        demote(Autonomy::Look, "no verify: commands — nothing but a person can decide this", &mut a, &mut why);
    }
    if let Some(p) = changed.iter().find(|c| SCHEMA_PATHS.iter().any(|s| c.ends_with(s))) {
        demote(Autonomy::Look, &format!("{p} is a wire contract or a file that executes without being run"), &mut a, &mut why);
    }
    // More than three files is the line the work model already draws for what earns a brief, and
    // it draws it on blast radius. The same measure decides what may land unwatched.
    if changed.len() > 3 {
        demote(Autonomy::Press, &format!("{} files changed — wide enough to want a pair of eyes", changed.len()), &mut a, &mut why);
    }
    let weakened = weakened_checks(diff);
    if !weakened.is_empty() {
        demote(
            Autonomy::Look,
            &format!("{} check(s) removed or altered — the gate cannot be moved by the thing it gates", weakened.len()),
            &mut a,
            &mut why,
        );
    }
    Classified { autonomy: a, why, weakened }
}

/// One git command, argv only, no shell — the same rule the verify path is built on, for the same
/// reason. Every argument here is composed by this file rather than read from a brief.
fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("git {}: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// What happened when we tried to end the loop.
#[derive(Clone, Debug)]
pub struct Landing {
    pub merged: bool,
    /// The merge commit, and the one command that takes it back.
    pub undo: Option<String>,
    pub why: String,
    pub autonomy: Autonomy,
    pub weakened: Vec<String>,
}

/// Land a reported brief, or say precisely why not.
///
/// **This is the only place in the system that makes a change nobody asked for at the moment it
/// happens**, so every rule it enforces is one somebody has to be able to check afterwards:
///
/// * the work must be REPORTED and every criterion met — a tally, not an opinion;
/// * a human must have left `approved.md` behind, and this cannot write one;
/// * `verify:` must be green, run here rather than taken from the worker's account of itself;
/// * the diff must price out at `Land` — small, reversible, no wire contract, no moved gate;
/// * the tree must be clean, because merging over somebody's uncommitted work is not reversible
///   in the way everything else here is.
///
/// **A merge, and not a push.** `--no-ff` so the whole brief is one commit to revert; local so the
/// louder, public half stays a human act. The undo is printed rather than described, because an
/// undo you have to look up is one you will not run in the ten seconds you have decided to spend.
pub fn land(
    b: &Brief,
    brief_dir: &Path,
    autopilot: bool,
    timeout: std::time::Duration,
) -> Landing {
    let no = |why: String, a: Autonomy| Landing { merged: false, undo: None, why, autonomy: a, weakened: Vec::new() };

    if state_of(brief_dir) != State::Reported {
        return no("no completed.md — there is nothing reported to land".into(), Autonomy::Never);
    }
    if !is_approved(brief_dir) {
        return no("no approved.md — nobody has said this may be built, and landing cannot say it".into(), Autonomy::Never);
    }
    let Some(repo) = b.repo.clone() else {
        return no("the brief records no repo, so nothing can be merged or checked".into(), Autonomy::Never);
    };
    let branch = b.branch.clone().unwrap_or_else(|| b.id.clone());
    if git(&repo, &["rev-parse", "--verify", &branch]).is_err() {
        return no(format!("branch {branch} does not exist in {}", repo.display()), Autonomy::Never);
    }

    // TIER 0, RUN HERE. The worker's own account of its exit codes is a claim; this is the fact.
    let a = audit(b, brief_dir, timeout);
    // RED FIRST, BECAUSE RED IS THE CAUSE. A failing command marks every criterion it covers as
    // unmet, so checking criteria first reports the symptom — "criteria [1] are not met" — for a
    // brief whose actual problem is one named command that exited non-zero. Both are true and
    // only one of them can be acted on.
    if a.tier0.iter().any(|r| r.exit != Some(0)) {
        let red: Vec<&str> = a.tier0.iter().filter(|r| r.exit != Some(0)).map(|r| r.cmd.as_str()).collect();
        return no(format!("verify is not green: {}", red.join(" · ")), Autonomy::Press);
    }
    let unmet = a.unmet();
    if !unmet.is_empty() {
        return no(format!("criteria {unmet:?} are not met — supersede rather than land"), Autonomy::Press);
    }

    let changed: Vec<String> = git(&repo, &["diff", "--name-only", &format!("main...{branch}")])
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .collect();
    let diff = git(&repo, &["diff", &format!("main...{branch}")]).unwrap_or_default();
    let c = classify(Autonomy::Land, &changed, &diff, &b.verify);
    if c.autonomy < Autonomy::Land {
        return Landing { merged: false, undo: None, why: c.why, autonomy: c.autonomy, weakened: c.weakened };
    }
    if !autopilot {
        return no("policy.md has autopilot off — everything else about this says land it".into(), Autonomy::Land);
    }
    // A DIRTY TREE IS NOT A MERGE CONFLICT, it is somebody's unsaved work, and it is the one thing
    // here a revert would not give back.
    if !git(&repo, &["status", "--porcelain"]).unwrap_or_default().trim().is_empty() {
        return no("the working tree has uncommitted changes — merging over them is the one thing revert cannot undo".into(), Autonomy::Land);
    }
    // THE WORKER LEAVES THE REPO ON ITS OWN BRANCH, so merging without moving first merges the
    // branch into itself: git says "Already up to date", this reports a landing, and `main` never
    // moved. Found by running a real worker end to end — every gate was green and nothing shipped.
    //
    // Checking out is safe here only because the tree was just proven clean, and it is where you
    // want to be standing after a landing anyway. Once a brief builds in its own worktree this
    // becomes a no-op, which is the right direction for it to age in.
    let head = git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default().trim().to_string();
    if head != "main" {
        if let Err(e) = git(&repo, &["checkout", "main"]) {
            return no(format!("on {head}, and checking out main failed: {e}"), Autonomy::Land);
        }
    }
    // Captured AFTER the checkout: the undo has to restore main's head, and main's head is only
    // knowable once we are on it.
    let before = git(&repo, &["rev-parse", "HEAD"]).unwrap_or_default().trim().to_string();
    if let Err(e) = git(&repo, &["merge", "--no-ff", "-m", &format!("Land {}: {}", b.id, b.title), &branch]) {
        return no(format!("merge refused: {e}"), Autonomy::Land);
    }
    // A MERGE THAT CHANGED NOTHING IS NOT A LANDING. `git merge` succeeds and prints "Already up
    // to date" when the branch is an ancestor, so an unchanged head is the signal that the work
    // was already in, or that we merged the wrong thing.
    if git(&repo, &["rev-parse", "HEAD"]).unwrap_or_default().trim() == before {
        return no(format!("merging {branch} into main changed nothing — it is already in"), Autonomy::Land);
    }
    let sha = git(&repo, &["rev-parse", "HEAD"]).unwrap_or_default().trim().to_string();
    Landing {
        merged: true,
        undo: Some(if before.is_empty() {
            format!("git -C {} revert -m 1 {sha}", repo.display())
        } else {
            format!("git -C {} reset --hard {before}", repo.display())
        }),
        why: format!("{} file(s), verify green, {} criteria met", changed.len(), a.tier1.len()),
        autonomy: Autonomy::Land,
        weakened: Vec::new(),
    }
}

/// Mint the next brief from what this one did not finish.
///
/// **Mechanical, and deliberately not a model call.** The tally is already keyed to the brief's
/// acceptance numbering, so the unmet subset is arithmetic — and a model rewriting criteria it
/// has just failed is the last thing wanted in this loop. `## Decisions already made` is inherited
/// whole, overrides included: they were settled, and settling them again is the cost this section
/// exists to avoid.
pub fn supersede(brief_dir: &Path, ts: u64) -> std::io::Result<(String, PathBuf)> {
    let body = std::fs::read_to_string(brief_dir.join("brief.md"))?;
    let Some(b) = read(brief_dir) else {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "unreadable brief"));
    };
    let a = audit(&b, brief_dir, std::time::Duration::from_secs(20));
    let unmet: Vec<usize> = a.unmet();
    if unmet.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "nothing unmet — there is nothing to supersede",
        ));
    }
    let all = acceptance(&body);
    let title = format!("{} (cont.)", b.title);
    let id = mint_id(&title, ts);
    let Some(dir) = dir().map(|d| d.join(&id)) else {
        return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "no home directory"))
    };
    std::fs::create_dir_all(&dir)?;

    let lines: Vec<&str> = body.lines().collect();
    let made = section_span(&lines, MADE)
        .map(|(s, e)| lines[s + 1..e].join("\n").trim().to_string())
        .unwrap_or_default();
    let carried: Vec<String> = all
        .iter()
        .filter(|(n, _)| unmet.contains(n))
        .enumerate()
        .map(|(i, (_, t))| format!("{}. {t}", i + 1))
        .collect();

    let mut out = template(&id, &title, ts, b.repo.as_deref());
    // The acceptance section becomes the unmet subset, renumbered from 1 so the next report's
    // tally lines up with it.
    out = replace_section(&out, "## Acceptance", &carried.join("\n"));
    // W5 — INHERIT THE RULINGS, NOT JUST THE PROSE.
    //
    // "Decisions inherited whole" carried the `## Decisions already made` lines and left `## HLD`
    // and `## LLD` as template stubs, so brief #2 parsed as zero decisions: nothing to approve,
    // nothing to refine, and no planner run to fix it because nothing triggers one. A continuation
    // that drops the design it is continuing is a new brief wearing an inheritance.
    //
    // The rulings come across verbatim. They were argued once and one of them may carry an
    // override; re-deriving them would re-open settled ground, which is the cost this whole
    // section exists to avoid.
    for section in ["## HLD", "## LLD"] {
        if let Some((s0, e0)) = section_span(&lines, section) {
            let carried_rulings = lines[s0 + 1..e0].join("\n").trim().to_string();
            if !carried_rulings.is_empty() {
                out = replace_section(&out, section, &carried_rulings);
            }
        }
    }
    out = replace_section(
        &out,
        MADE,
        &format!(
            "Inherited whole from `{}`, including any override. These are settled.\n\n{made}",
            b.id
        ),
    );
    out = replace_section(
        &out,
        "## Problem + evidence",
        &format!(
            "Supersedes `{}` — {} of {} acceptance criteria were not met:\n\n{}",
            b.id,
            unmet.len(),
            all.len(),
            all.iter()
                .filter(|(n, _)| unmet.contains(n))
                .map(|(n, t)| format!("- ({}) criterion {n}: {t}",
                    a.tier1.iter().find(|c| c.n == *n).map(|c| c.verdict).unwrap_or("unmet")))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    );
    let path = dir.join("brief.md");
    std::fs::write(&path, out)?;
    let _ = scaffold();
    Ok((id, path))
}

/// Replace a `##` section's body, keeping the heading.
fn replace_section(body: &str, heading: &str, content: &str) -> String {
    let lines: Vec<&str> = body.lines().collect();
    let Some((start, end)) = section_span(&lines, heading) else { return body.to_string() };
    let mut out: Vec<String> = lines[..=start].iter().map(|s| s.to_string()).collect();
    out.push(String::new());
    out.push(content.to_string());
    out.push(String::new());
    out.extend(lines[end..].iter().map(|s| s.to_string()));
    keep_trailing(body, out.join("\n"))
}

/// The worker's report, if there is one.
fn read_report(brief_dir: &Path) -> Option<Report> {
    let text = std::fs::read_to_string(brief_dir.join("completed.md")).ok()?;
    let (front, _) = crate::manager::split_front(&text)?;
    let f = |k: &str| {
        front.lines().find_map(|l| {
            let (key, v) = l.split_once(':')?;
            (key.trim() == k).then(|| v.trim().trim_matches('"').to_string())
        })
    };
    // Every `{n: …, met: …}` row. Counted rather than trusted: `outcome: done` beside 9 of 11 met
    // is exactly the disagreement worth showing, and a card that printed only the outcome would
    // hide it.
    let (mut met, mut total) = (0usize, 0usize);
    for line in front.lines().map(str::trim).filter(|l| l.starts_with("- {n:")) {
        total += 1;
        if line.contains("met: true") {
            met += 1;
        }
    }
    Some(Report {
        outcome: f("outcome").unwrap_or_else(|| "unknown".into()),
        pr: f("pr").filter(|p| p.starts_with("http")),
        met,
        total,
    })
}

/// A YAML list of plain strings under `key`./// A YAML list of plain strings under `key`. Hand-parsed for the same reason `parse_addresses` is:
/// a malformed list must cost one field, never the whole document.
fn parse_list(front: &str, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in front.lines() {
        if line.starts_with(key) {
            // `verify: []` on one line is an empty list, not the start of one.
            inside = !line[key.len()..].trim().starts_with('[');
            continue;
        }
        if inside {
            let trimmed = line.trim();
            let Some(rest) = trimmed.strip_prefix('-') else { break };
            let v = rest.trim().trim_matches('"').trim_matches('\'').trim();
            if !v.is_empty() {
                out.push(v.to_string());
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
pub fn create(title: &str, ts: u64, repo: Option<&Path>) -> std::io::Result<(String, PathBuf)> {
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
    std::fs::write(&path, template(&id, title, ts, repo))?;
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

/// Something worth planning against, and the evidence it came from.
///
/// **`cite` is not optional and is why this type exists.** An idea with no citation is a wish, and
/// a night shift that admits wishes fills the morning with confident work nobody asked for — the
/// exact failure the corpus refuses under "missions are inferred, never asked". Every constructor
/// below reads its citation off disk, so the cite names a file a reader can open rather than a
/// claim the planner made about one.
#[derive(Clone, Debug)]
pub struct Observation {
    pub headline: String,
    pub cite: String,
    pub body: String,
    pub repo: Option<PathBuf>,
}

/// What the night shift has to plan against, gathered from what is already on disk.
///
/// Two sources, and neither of them is a model:
///
/// * **nominations** — memos carrying `kind: nomination`, which `nominate` writes with the
///   snapshot it was read out of. The designed entry to the loop.
/// * **`## Notes for later`** — what workers wrote down and were forbidden from acting on. The
///   orders promise these are read by somebody; until now nothing read them, which makes the
///   promise the same shape of lie as a permission with no parser.
///
/// `design_ideas/` is deliberately not a source. Those are visions, and `AGENTS.md` already warns
/// they may be unbuilt or partly built — feeding a vision corpus to an autonomous builder produces
/// confident work on premises nobody checked.
pub fn observations() -> Vec<Observation> {
    let mut out = Vec::new();
    let Some(home) = crate::sys::paths::home_dir() else { return out };

    // 1. Nominations, from every session's memo folder.
    let sessions = home.join(".mars").join("sessions");
    for sess in std::fs::read_dir(&sessions).into_iter().flatten().flatten() {
        for memo in std::fs::read_dir(sess.path().join("memos")).into_iter().flatten().flatten() {
            let Ok(text) = std::fs::read_to_string(memo.path()) else { continue };
            let Some((front, body)) = crate::manager::split_front(&text) else { continue };
            if !front.lines().any(|l| l.trim() == "kind: nomination") {
                continue;
            }
            if front.lines().any(|l| l.trim() == "expired: true") {
                continue;
            }
            let field = |k: &str| {
                front.lines().find_map(|l| {
                    l.trim().strip_prefix(&format!("{k}:")).map(|v| v.trim().trim_matches('"').to_string())
                })
            };
            let cite = front
                .lines()
                .find(|l| l.trim().starts_with("- {snapshot:"))
                .map(|l| l.trim().to_string())
                .unwrap_or_else(|| memo.path().display().to_string());
            out.push(Observation {
                headline: field("headline").unwrap_or_else(|| field("title").unwrap_or_default()),
                cite,
                body: body.trim().to_string(),
                repo: None,
            });
        }
    }

    // 2. Notes for later, from every report ever filed — live and archived.
    let bdir = dir();
    for root in bdir.iter().flat_map(|d| [d.clone(), d.join("archive")]) {
        for e in std::fs::read_dir(&root).into_iter().flatten().flatten() {
            let bd = e.path();
            let Ok(done) = std::fs::read_to_string(bd.join("completed.md")) else { continue };
            let Some(notes) = section_after(&done, "## Notes for later") else { continue };
            let repo = read(&bd).and_then(|b| b.repo);
            for line in notes.lines().map(str::trim).filter(|l| l.starts_with('-') && l.len() > 4) {
                out.push(Observation {
                    headline: line.trim_start_matches(['-', ' ']).chars().take(90).collect(),
                    cite: format!("{}/completed.md § Notes for later", bd.display()),
                    body: line.to_string(),
                    repo: repo.clone(),
                });
            }
        }
    }
    out
}

/// The body of one `## ` section, to the next one.
fn section_after(body: &str, heading: &str) -> Option<String> {
    let start = body.find(heading)? + heading.len();
    let rest = &body[start..];
    let end = rest.find("\n## ").unwrap_or(rest.len());
    Some(rest[..end].trim().to_string())
}

/// What the night shift estimated about a brief before anybody built it.
///
/// **Distinct from `classify`, and the difference is the whole point.** `classify` prices a real
/// diff at landing time and is authoritative. This prices a brief's *declared intent* at planning
/// time, from what it says it will touch — an estimate, used only to decide whether a thing is
/// queued for tonight or written up and stored. Being wrong here costs an item sitting in the
/// wrong pile until morning; being wrong at landing time costs a merge.
///
/// So this may only ever be more cautious than the real thing, and the real thing still runs.
pub fn triage(b: &Brief, body: &str) -> Classified {
    // TWO CHECKS, AND DELIBERATELY NOT A FILE COUNT.
    //
    // The first version counted anything file-shaped in the document and handed that to
    // `classify`, whose `> 3 files` rule then fired on prose. Measured against five briefs a real
    // planner wrote: all five came back "must be seen", and one reported **59 files changed** —
    // a number counted out of an argument, shown to a human as if it were a fact.
    //
    // The count is not knowable here and `PLANNING-MODEL.md` says so about its own sizing rule:
    // *"the file count cannot be known before the design exists — you cannot count files you have
    // not decided to change."* An estimate dressed as a measurement is worse than no estimate, so
    // blast radius is left to `classify`, at landing time, off the real diff.
    //
    // What IS knowable now: whether the brief declares an oracle, and whether the section that
    // binds where files land names something whose blast radius outruns any check. Both are read
    // off `## LLD` only — the evidence and the rejected options mention paths the work will never
    // touch, and reading those as intent is what made every pile the same pile.
    let lld = section_after(body, "## LLD").unwrap_or_default();
    let touches: Vec<&str> = lld
        .split(['`', ' ', '(', ')', ',', '\n'])
        // Path-shaped, not merely dotted: `src/session.rs` is a path and "i.e." is not.
        .filter(|w| w.contains('/') && w.contains('.') && w.len() > 4)
        .collect();

    if b.verify.is_empty() {
        return Classified {
            autonomy: Autonomy::Look,
            why: "no verify: commands — nothing but a person can decide this".into(),
            weakened: Vec::new(),
        };
    }
    if let Some(p) = touches.iter().find(|c| SCHEMA_PATHS.iter().any(|s| c.ends_with(s))) {
        return Classified {
            autonomy: Autonomy::Look,
            why: format!("its LLD puts work in {p} — a wire contract, or a file that executes without being run"),
            weakened: Vec::new(),
        };
    }
    Classified {
        autonomy: Autonomy::Land,
        why: "declares a check and names nothing protected — blast radius is decided by the diff".into(),
        weakened: Vec::new(),
    }
}

/// Draft one brief from one observation, with a real planner.
///
/// **The daemon mints and the planner fills**, exactly as the pane flow does — an agent told to
/// "write a brief" chooses its own path and produces a file nothing downstream can find. Here the
/// file exists, scoped, before the model is started.
///
/// Headless `-p` rather than a pane: a night shift has no pane to take, and the planner's whole
/// output is one file. The scope is the same one the interactive planner gets, so the role's
/// boundary is one list rather than two that will drift.
pub fn plan_one(obs: &Observation, repo: Option<&Path>, ts: u64, model: &str) -> std::io::Result<String> {
    let home = crate::sys::paths::home_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home directory"))?;
    // `create` hands back the path to brief.md, not the directory that holds it. Reading
    // `dir/brief.md` against that gives `…/brief.md/brief.md`, which is never there — so every
    // planner run reported "wrote nothing readable" AFTER the model had already spent nine
    // minutes writing a perfectly good brief, and the cleanup that follows silently did nothing
    // because `remove_dir_all` on a file fails. Measured: five briefs drafted, five recorded as
    // failures, five scaffolds left behind.
    let (id, brief_path) = create(&obs.headline, ts, repo)?;
    let dir = brief_path.parent().unwrap_or(&brief_path).to_path_buf();
    let assignment = draft_assignment(&id, &home)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "bad id"))?;
    // THE OBSERVATION IS HANDED OVER AS EVIDENCE, WITH ITS CITE. `## Problem + evidence` binds the
    // worker as "what is true", so the planner is told where this came from and told to check it —
    // a brief whose premise turns out to be wrong costs a whole worker run.
    let prompt = format!(
        "{assignment}\n\nThe observation you are planning against, and its provenance:\n\n\
         HEADLINE: {}\n  CITED: {}\n\n{}\n\n\
         Open the cited evidence before you write anything. If the premise does not survive \
         reading the code, say so in `## Problem + evidence` and stop — that is the most valuable \
         brief you can write.",
        obs.headline, obs.cite, obs.body,
    );
    let mut cmd = std::process::Command::new("claude");
    cmd.current_dir(repo.unwrap_or(&home))
        // Same reason the manager scrubs these: a key here bypasses the subscription and fails
        // outright when it has no credit.
        .env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .arg("-p")
        .arg(&prompt)
        .arg("--permission-mode")
        .arg("acceptEdits")
        .arg("--add-dir")
        .arg(home.join(".mars").join("briefs"));
    for a in PLANNER_ALLOW {
        cmd.arg("--allowedTools").arg(a);
    }
    for p in planner_deny() {
        cmd.arg("--disallowedTools").arg(format!("Edit({p})")).arg(format!("Write({p})"));
    }
    // The hardest job in the system reads code and rules six forks, and a wrong premise costs a
    // whole worker run — so this is the one role that gets the better model.
    cmd.arg("--model").arg(model).arg("--effort").arg("high");
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let out = cmd.output()?;
    if !out.status.success() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(std::io::Error::other(format!(
            "planner exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    // DID IT ACTUALLY WRITE ONE? An empty brief in the queue every time a planner run fails is
    // how the morning list fills with nothing, and the scaffold looks identical to a real brief
    // until somebody opens it.
    let Some(b) = read(&dir) else {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(std::io::Error::other("planner wrote nothing readable"));
    };
    // AN UNFILLED SCAFFOLD PARSES AS A COMPLETE BRIEF, which is the trap here. The template ships
    // with all six headings and an `Option C ✅ chosen` placeholder, so `decisions_of` reports six
    // ruled decisions for a file the planner never touched — found by pointing this at a real run
    // and reading what landed. Counting decisions cannot tell a brief from its own stationery.
    //
    // So the test is for the stationery. These strings exist only in `template()`; a brief that
    // still carries one is a brief nobody wrote, and it must not reach the morning list looking
    // like work.
    let body_now = std::fs::read_to_string(dir.join("brief.md")).unwrap_or_default();
    const SCAFFOLD: &[&str] = &["<the question>", "BINDING. ONE paragraph", "BINDING. Exact paths"];
    if let Some(marker) = SCAFFOLD.iter().find(|m| body_now.contains(**m)) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(std::io::Error::other(format!(
            "planner left the scaffold unfilled (still contains {marker:?}) — an empty brief in the queue is worse than none"
        )));
    }
    if decisions_of(&dir).is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
        return Err(std::io::Error::other("planner left no ruled decisions — a brief with no forks is a wish"));
    }
    if b.verify.is_empty() {
        // Not fatal — `unverifiable` is a first-class outcome and some work genuinely has no
        // machine oracle. But it decides the pile, so it is recorded rather than shrugged at.
        let _ = std::fs::write(dir.join("no-oracle"), "");
    }
    // The deterministic second reader, and the plan-time estimate. Both written beside the brief
    // so the morning list is a directory read rather than a recomputation.
    let _ = review(&dir);
    let body = std::fs::read_to_string(dir.join("brief.md")).unwrap_or_default();
    let t = triage(&b, &body);
    let _ = std::fs::write(
        dir.join("triage.md"),
        format!(
            "---\nautonomy: {}\ncited: \"{}\"\n---\n# Triage\n\n{}\n\n\
             An estimate from what the brief SAYS it will touch, made before anything was built. \
             The diff decides at landing time and may only be more cautious than this.\n",
            t.autonomy.label(), obs.cite.replace('"', "'"), t.why
        ),
    );
    Ok(id)
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

/// Split a verify command into argv, or refuse it.
///
/// **There is no shell anywhere in this path, and that is the whole security argument.** These
/// strings are written by a planner — a model — into a file, and then executed by the daemon. Run
/// through `sh -c` they would be arbitrary code with the user's privileges; run as argv they can
/// only start a program with arguments, which is exactly what a verify command is.
///
/// So any byte that only means something to a shell is a refusal rather than an escape. That also
/// makes PLANNING-MODEL's "one plain command per entry" rule mechanical instead of advisory: a
/// planner that writes `source x && y` gets a refusal naming the character, not a command that
/// quietly does more than it looks like.
pub fn verify_argv(cmd: &str) -> Result<Vec<String>, String> {
    const SHELL_ONLY: &[char] = &['&', '|', ';', '$', '`', '>', '<', '(', ')', '{', '}', '*', '?', '~', '!', '\n'];
    if let Some(c) = cmd.chars().find(|c| SHELL_ONLY.contains(c)) {
        return Err(format!(
            "{c:?} only means something to a shell, and there is no shell here — \
             write one plain command per verify entry"
        ));
    }
    // Quotes are honoured so an argument may contain spaces; nothing else about them is special.
    let mut argv: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for ch in cmd.chars() {
        match ch {
            '"' | '\'' if quote.is_none() => quote = Some(ch),
            c if Some(c) == quote => quote = None,
            c if c.is_whitespace() && quote.is_none() => {
                if !cur.is_empty() {
                    argv.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if quote.is_some() {
        return Err("unclosed quote".into());
    }
    if !cur.is_empty() {
        argv.push(cur);
    }
    if argv.is_empty() {
        return Err("empty command".into());
    }
    // AND argv[0] MUST NOT BE AN INTERPRETER.
    //
    // Filtering metacharacters does nothing if the shell is the program: `sh -c "…"` carries its
    // whole payload inside a quoted argument, where no character is special. Found by a selfcheck
    // that then went on to run what it had smuggled, which is the most direct possible proof that
    // the character filter alone was not a boundary.
    //
    // This closes the escalation from "one command" to "arbitrary code". It does NOT make an
    // arbitrary command safe — `verify: ["rm -rf ~"]` needs no interpreter — and nothing here
    // pretends otherwise. What makes the list safe to run is that a human reads it: the commands
    // are shown on the approval card, and pressing assign is the authorization.
    const INTERPRETERS: &[&str] = &[
        "sh", "bash", "zsh", "dash", "ksh", "csh", "tcsh", "fish", "ash",
        "env", "eval", "exec", "xargs", "nohup", "sudo", "doas", "ssh", "script",
        "python", "python2", "python3", "perl", "ruby", "node", "deno", "bun", "osascript",
    ];
    let prog = std::path::Path::new(&argv[0])
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&argv[0]);
    if INTERPRETERS.contains(&prog) {
        return Err(format!(
            "{prog:?} runs whatever it is handed, so a quoted argument would be a shell by another \
             name — name the program the check actually runs"
        ));
    }
    Ok(argv)
}

/// Run a brief's `verify:` commands and report what actually happened.
///
/// **Mars runs these, not the worker, and that is a change of who is trusted.** A worker running
/// its own acceptance checks and then writing down its own exit codes is grading its own homework;
/// nothing downstream could tell a passing run from a confident claim. Worse, in practice it did
/// not even get that far — every verify command hit a permission prompt, so an unattended worker
/// stopped dead at the first one and waited for a human who by construction was not there.
///
/// Both problems have the same fix. The commands are in the brief's frontmatter, and a human read
/// them when they approved it, which is the moment they were actually reading. So run them here,
/// observe the exit codes, and let the worker get on with building.
///
/// Computed on demand, never stored: a verification is a fact about a tree at a moment, and a
/// recorded one starts lying the next time anybody commits.
pub fn verify(b: &Brief, timeout: std::time::Duration) -> Vec<VerifyRow> {
    let Some(repo) = b.repo.as_ref().filter(|r| r.is_dir()) else {
        return b.verify.iter().map(|c| VerifyRow {
            cmd: c.clone(),
            exit: None,
            note: match &b.repo {
                Some(r) => format!("{} is not a directory any more", r.display()),
                None => "this brief records no repo — it predates `repo:` in the template".into(),
            },
        }).collect();
    };
    b.verify.iter().map(|cmd| {
        // The key is addressing, not part of the command. Stripped here so `verify_argv` sees
        // exactly what it saw before this field existed — one place strips it, and it is the same
        // place that runs it.
        let bare = strip_verify_key(cmd);
        let argv = match verify_argv(bare) {
            Ok(a) => a,
            Err(why) => return VerifyRow { cmd: cmd.clone(), exit: None, note: format!("refused: {why}") },
        };
        match run_argv(&argv, repo, timeout) {
            Ok((code, tail)) => VerifyRow { cmd: cmd.clone(), exit: Some(code), note: tail },
            Err(why) => VerifyRow { cmd: cmd.clone(), exit: None, note: why },
        }
    }).collect()
}

/// Run one argv in a directory, with a wall-clock bound. Returns the exit code and the tail of
/// whatever it said — the last few lines, because that is where a build puts its verdict.
fn run_argv(
    argv: &[String],
    dir: &Path,
    timeout: std::time::Duration,
) -> Result<(i32, String), String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run {:?}: {e}", argv[0]))?;
    // A verify command that never returns must not hold the daemon. Polled rather than blocked on,
    // so the bound is real; a command still running at the deadline is reported as unfinished
    // rather than waited out, because "we do not know" is the honest answer and a hang is a defect
    // in the brief worth seeing.
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = child.wait_with_output().map(|o| {
                    let mut s = String::from_utf8_lossy(&o.stdout).to_string();
                    s.push_str(&String::from_utf8_lossy(&o.stderr));
                    s
                }).unwrap_or_default();
                let tail = out.lines().rev().take(3).collect::<Vec<_>>()
                    .into_iter().rev().collect::<Vec<_>>().join(" · ");
                return Ok((status.code().unwrap_or(-1), tail.chars().take(300).collect()));
            }
            Ok(None) if std::time::Instant::now() >= deadline => {
                return Err(format!("still running after {}s", timeout.as_secs()));
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(120)),
            Err(e) => return Err(format!("{e}")),
        }
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
/// The watermark on anything MARS types into a pane on your behalf.
///
/// An agent's scrollback is its memory, and until now a manager-issued instruction and something
/// you typed yourself were byte-identical once they landed. Neither the agent nor a person reading
/// back could tell "you were told this" from "you decided this" — which matters most in exactly
/// the case it was hardest to see: an assignment that went to the wrong pane, or a draft prompt
/// somebody thought a human had written.
///
/// Short, because it leads every manager-issued line and a long tag turns into furniture the eye
/// stops reading. Bracketed so it survives being quoted into a transcript intact, and so it cannot
/// be mistaken for the start of the instruction itself.
pub const MANAGER_MARK: &str = "[XO]";

/// `typed_bytes`, watermarked. Every manager-originated line goes through here.
pub fn manager_bytes(text: &str) -> String {
    typed_bytes(&format!("{MANAGER_MARK} {}", text.trim_start()))
}

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
pub fn template(id: &str, title: &str, ts: u64, repo: Option<&Path>) -> String {
    let repo = repo.map(|r| r.display().to_string()).unwrap_or_else(|| "null".into());
    format!(
        "---\n\
         id: {id}\n\
         title: \"{title}\"\n\
         priority: 50\n\
         created_ts: {ts}\n\
         branch: {id}\n\
         repo: {repo}\n\
         addresses: []\n\
         verify: []\n\
         ---\n\
         # {title}\n\
         \n\
         ## The idea\n\
         \n\
         BINDING. ONE paragraph: the problem, and the shape of the answer. This is the only part\n\
         of the brief that reaches the board, so it is what a reader triages on — write it for\n\
         somebody deciding whether to open this at all, not for somebody who already has.\n\
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
         *Assumes:* hld-1\n\
         \n\
         ### Fork 3 — <the question>\n\
         \n\
         Three is a strong default, not a schema. Two real forks beat three with one invented.\n\
         \n\
         `*Assumes:* hld-1` names the earlier decision a ruling rests on, and it is not optional\n\
         bookkeeping — it is what lets a reader override one recommendation without re-approving\n\
         the other five. Write it wherever a ruling would change if the named decision changed.\n\
         Decisions are addressed `hld-1..3` and `lld-1..3`, in the order they appear.\n\
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
         *Assumes:* hld-2\n\
         \n\
         - **Option A** — … *For:* … *Against:* …\n\
         - **Option B** — … *For:* … *Against:* …\n\
         - **Option C ✅ chosen** — … *Why this and not the others:* …\n\
         \n\
         Same shape as a fork, and for the same reason: these three are the decisions most likely\n\
         to be wrong, so they are the three most worth overriding. They reach the approval card.\n\
         \n\
         ### Artefact 2 of 3\n\
         \n\
         ### Artefact 3 of 3\n\
         \n\
         `verify:` is run by MARS, not by the worker — one plain command per entry, no shell\n\
         metacharacters, since there is no shell.\n\
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
