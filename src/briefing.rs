//! The shift report — a save-state restore for the workspace. Everything here
//! is pure and deterministic (tier 0 of the verdict triage ladder): exit
//! codes, durations, and tail-shape heuristics produce honest rows with zero
//! LLM involvement. The model only ever REPLACES a defensible placeholder —
//! ambiguous rows go, batched, to a low-tier model after the overlay is
//! already on screen (the frame is never blocked on a network call).

/// Row classes in fixed display order: what needs you first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Verdict {
    Failed,
    Blocked,
    /// A command is genuinely still executing and has produced nothing for a long
    /// time. Distinct from `Running` (which means "producing output right now") and
    /// from `Context` (which means "nothing is executing") — without it, a hung
    /// build and a shell sitting at a prompt were the same word.
    Stalled,
    Done,
    Running,
    Context,
}

impl Verdict {
    pub fn glyph(self) -> &'static str {
        match self {
            Verdict::Failed => "✗",
            Verdict::Blocked => "⏸",
            Verdict::Stalled => "◔",
            Verdict::Done => "✓",
            Verdict::Running => "●",
            Verdict::Context => "·",
        }
    }

    /// The one-word status label shown to the user (workspaces panel, tab tooltip).
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Failed => "failed",
            Verdict::Blocked => "blocked",
            Verdict::Stalled => "stalled",
            Verdict::Done => "done",
            Verdict::Running => "running",
            Verdict::Context => "idle",
        }
    }

    /// Live-attention rank — needs-you first. The ONE order every live view sorts
    /// by: the tab bar's worst-pane aggregate, the bottom summary, and the
    /// command-bar board. (Deliberately distinct from the derived `Ord`, which is
    /// Failed-first — tuned for the arrival briefing's "what happened" display.)
    ///
    /// Stalled outranks Running because it is the one that might want you: healthy
    /// work needs nothing, a wedged process needs a decision. It sits below Failed
    /// because "stalled" is still a suspicion about a live process, where a failure
    /// is a settled fact.
    pub fn rank(self) -> u8 {
        match self {
            Verdict::Blocked => 6, // needs you now — a keystroke unblocks it
            Verdict::Failed  => 5,
            Verdict::Stalled => 4,
            Verdict::Running => 3,
            Verdict::Done    => 2,
            Verdict::Context => 1,
        }
    }
}

pub struct ReportRow {
    pub verdict: Verdict,
    pub tab: String,
    pub text: String,
    /// Seconds since the event (None for still-running rows).
    pub ago_secs: Option<u64>,
    /// How long the run took / has been going.
    pub dur_secs: Option<u64>,
    /// The pane this row describes, when it still exists.
    pub term_id: Option<crate::terminal::TermId>,
    /// Where the pane was spawned — shown under failed/blocked rows.
    pub cwd: Option<String>,
    /// The shell exit code, when the pane concluded.
    pub exit: Option<i32>,
    /// The redacted error tail — the "why" under a failure, rendered as one dim
    /// line beneath failed/blocked rows only.
    pub error_excerpt: Option<String>,
    /// Reserved for the streaming-polish animation (unused in the prose model).
    pub settling: bool,
}

impl ReportRow {
    /// A compact evidence line for the narrative prompt (deterministic facts the
    /// model turns into prose).
    pub fn evidence(&self) -> String {
        let mut s = format!("{} {}", self.verdict.glyph(), self.text);
        if !self.tab.is_empty() {
            s = format!("[{}] {s}", self.tab);
        }
        if let Some(d) = self.dur_secs.filter(|d| *d > 0) {
            s.push_str(&format!(" (ran {})", fmt_secs(d)));
        }
        if let Some(x) = self.exit {
            s.push_str(&format!(" [exit {x}]"));
        }
        if let Some(e) = &self.error_excerpt {
            let one = e.lines().next().unwrap_or("").trim();
            if !one.is_empty() {
                s.push_str(&format!(" — {}", one.chars().take(160).collect::<String>()));
            }
        }
        s
    }
}

pub struct ShiftReport {
    pub away_secs: u64,
    pub mission: Option<String>,
    pub rows: Vec<ReportRow>,
    /// One suggested next move, shown only when a row failed or blocked.
    pub suggestion: Option<String>,
    /// The plain-English, persona-voiced situation report — the star of the
    /// overlay. Streams in token by token above the row manifest; starts with a
    /// deterministic one-liner so the frame is never blocked on the model.
    pub narrative: String,
    /// The narrative is still streaming from the model.
    pub narrative_streaming: bool,
    /// The first model delta has landed — the templated line has been replaced.
    pub narrative_from_model: bool,
    /// Compact manifest distillation, logged when the briefing finalizes so the
    /// next return can report progress against it (continuity).
    pub facts: String,
    /// When the first model delta landed — the origin for the typewriter reveal,
    /// so the prose types in at a steady rate behind a cursor rather than in the
    /// ragged bursts the network delivers. None until the model starts speaking.
    pub stream_started_at: Option<std::time::Instant>,
    /// Millis timestamp when the overlay first rendered (for instrumentation).
    pub shown_at: std::time::Instant,
}

/// The mission-control words that flash, one after another, while the briefing
/// call is in flight — shown in place of the deterministic backup line so the
/// prose types into a clean slot instead of visibly swapping a stub for the model
/// text. Cycled by the frame clock.
pub const BRIEF_LOADING: &[&str] = &["booting", "syncing", "triaging", "collating", "briefing"];
/// How long each loading word holds before the next (ms).
pub const LOADING_FLASH_MS: u128 = 200;

/// Tier-0 verdict for one pane, from evidence alone.
pub struct Triage {
    pub verdict: Verdict,
    pub text: String,
    /// True when the heuristics couldn't settle it — send to the model.
    pub ambiguous: bool,
}

const FAIL_MARKS: &[&str] = &[
    "error:", "error[", "failed", "failure", "panic", "traceback", "out of memory",
    "oom", "killed", "segmentation fault", "command not found", "fatal:",
];
const BLOCKED_MARKS: &[&str] = &[
    "[y/n]", "(y/n)", "[y/n]:", "continue?", "password:", "passphrase",
    "are you sure", "waiting for", "press enter", "(yes/no)", "proceed?",
];
const PROGRESS_MARKS: &[&str] = &["it/s", "eta ", "step ", "epoch ", "%|", "██"];

/// The tier-0 ladder. Order matters: exit codes are ground truth and beat
/// every tail heuristic; blocked shapes beat failure words (a confirm prompt
/// quoting an error is still a question); progress beats quiet-ambiguity.
pub fn triage(tail: &str, exit: Option<i32>, running: bool) -> Triage {
    let low = tail.to_lowercase();
    let last_lines: String = low.lines().rev().take(6).collect::<Vec<_>>().join("\n");
    match exit {
        Some(0) => {
            return Triage {
                verdict: Verdict::Done,
                text: "exited clean".into(),
                // A clean exit whose tail still shouts failure words is worth
                // one cheap model look (tests that "pass" by exiting 0 after
                // printing FAILED exist) — but the row is Done either way.
                ambiguous: FAIL_MARKS.iter().any(|m| last_lines.contains(m)),
            };
        }
        Some(code) => {
            let named = match code {
                130 => " (interrupted)".to_string(),
                137 => " (killed — OOM?)".to_string(),
                139 => " (segfault)".to_string(),
                _ => String::new(),
            };
            return Triage {
                verdict: Verdict::Failed,
                text: format!("exited {code}{named}"),
                ambiguous: true, // a one-line CAUSE is worth a cheap model look
            };
        }
        None => {}
    }
    // No exit code: the pane is alive (quiet) or evidence predates capture.
    if BLOCKED_MARKS.iter().any(|m| last_lines.contains(m))
        || last_lines.trim_end().ends_with('?')
    {
        return Triage {
            verdict: Verdict::Blocked,
            text: "waiting on your input".into(),
            ambiguous: false,
        };
    }
    if FAIL_MARKS.iter().any(|m| last_lines.contains(m)) {
        return Triage {
            verdict: Verdict::Failed,
            text: "failure in output".into(),
            ambiguous: true,
        };
    }
    if running && PROGRESS_MARKS.iter().any(|m| low.contains(m)) {
        return Triage {
            verdict: Verdict::Running,
            text: "still running".into(),
            ambiguous: false,
        };
    }
    if running {
        return Triage {
            verdict: Verdict::Running,
            text: "quiet".into(),
            ambiguous: true,
        };
    }
    Triage { verdict: Verdict::Done, text: "finished".into(), ambiguous: true }
}

/// Class of a verdict STRING (model- or tier-0-authored): the `blocked:` /
/// `failed:` / `done:` prefixes the prompts mandate, with keyword fallbacks.
pub fn classify(text: &str, default: Verdict) -> Verdict {
    let low = text.trim().to_lowercase();
    if low.starts_with("blocked") || low.contains("waiting on your input") {
        Verdict::Blocked
    } else if low.starts_with("failed") || low.starts_with("✗") {
        Verdict::Failed
    } else if low.starts_with("done") {
        Verdict::Done
    } else if low.contains("fail") || low.contains("error") {
        Verdict::Failed
    } else {
        default
    }
}

/// Does this pane state deserve a verdict from an AUTO-watched pane? The point
/// of auto-watch is ambient supervision, not narrating an interactive shell:
/// an idle prompt or a clean user-initiated `exit` is not "work" and floods the
/// journal (and mission inference) with "user quit"-style noise. So auto-watch
/// speaks only for things that need you (failures, blocked prompts) or a real
/// process conclusion — never the boring shell lifecycle. Manual `C-x w`
/// watches bypass this and summarize everything, as the user asked.
pub fn is_noteworthy(tail: &str, exit: Option<i32>) -> bool {
    match exit {
        // A mars pane runs $SHELL, so a clean pane exit means the SHELL itself
        // ended — the user left. That is not work; stay silent.
        Some(0) => false,
        // A nonzero exit is a crash or an error the user should see.
        Some(_) => true,
        // Still alive and quiet: speak only if the tail shows trouble — a failure
        // or a wait-for-input. An interactive shell idling at a prompt, or a
        // command that simply finished quietly, says nothing (auto-watch is for
        // things that NEED you, not a running commentary).
        None => {
            let low = tail.to_lowercase();
            let last6: String = low.lines().rev().take(6).collect::<Vec<_>>().join("\n");
            FAIL_MARKS.iter().any(|m| last6.contains(m))
                || BLOCKED_MARKS.iter().any(|m| last6.contains(m))
                || last6.trim_end().ends_with('?')
        }
    }
}

/// Total duration of the briefing's boot-up reveal (ms). After this, the page
/// is fully on screen and the animation clock goes quiet.
pub const BOOT_TOTAL_MS: u128 = 520;
/// A row that ran longer than this and succeeded is "good news" worth a ★.
pub const GOODNEWS_SECS: u64 = 60;

/// Which briefing elements have revealed at `elapsed` ms into the boot, given
/// `n` manifest rows. Pure, so render and the selfcheck agree. With animation
/// off, callers pass `u128::MAX` and everything is revealed at once. The
/// wordmark, caption, and prose are instant (the prose streams on its own); the
/// boot cascades the *manifest* (failures first) and holds the sign-off for last.
pub struct Reveal {
    /// How many manifest rows have cascaded in (failures first).
    pub rows: usize,
    /// The sign-off + footer have arrived (end of the boot).
    pub signoff: bool,
}

pub fn reveal_at(elapsed_ms: u128, n_rows: usize) -> Reveal {
    let rows = if elapsed_ms >= 360 {
        (((elapsed_ms - 360) / 40) as usize + 1).min(n_rows)
    } else {
        0
    };
    Reveal { rows, signoff: elapsed_ms >= BOOT_TOTAL_MS }
}

/// Mission-clock format for the caption: HH:MM:SS (or D:HH:MM:SS past a day).
/// Static — evocative without any idle-redraw cost.
pub fn fmt_clock(s: u64) -> String {
    let (d, h, m, sec) = (s / 86_400, (s % 86_400) / 3600, (s % 3600) / 60, s % 60);
    if d > 0 {
        format!("{d}:{h:02}:{m:02}:{sec:02}")
    } else {
        format!("{h:02}:{m:02}:{sec:02}")
    }
}

/// Humanize seconds: 42s, 4m12s, 1h02m, 2d3h.
pub fn fmt_secs(s: u64) -> String {
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m{:02}s", s / 60, s % 60),
        3600..=86_399 => format!("{}h{:02}m", s / 3600, (s % 3600) / 60),
        _ => format!("{}d{}h", s / 86_400, (s % 86_400) / 3600),
    }
}

impl ShiftReport {
    /// Sort rows into display order (what needs you first), stable within class.
    pub fn sort_rows(&mut self) {
        self.rows.sort_by_key(|r| r.verdict);
    }

    /// The deterministic briefing shown instantly (keyless sessions keep it; a
    /// keyed session replaces it with the persona-voiced version as it streams).
    /// Plain English from the counts — never blocked on a model, never wrong.
    pub fn deterministic_narrative(&self) -> String {
        let n = |v: Verdict| self.rows.iter().filter(|r| r.verdict == v).count();
        let (failed, blocked, done, running) =
            (n(Verdict::Failed), n(Verdict::Blocked), n(Verdict::Done), n(Verdict::Running));
        let mut parts = Vec::new();
        if failed > 0 {
            parts.push(format!("{failed} failed"));
        }
        if blocked > 0 {
            parts.push(format!("{blocked} waiting on you"));
        }
        if done > 0 {
            parts.push(format!("{done} finished clean"));
        }
        if running > 0 {
            parts.push(format!("{running} still running"));
        }
        let away = fmt_secs(self.away_secs);
        if parts.is_empty() {
            format!("Welcome back — nothing needs you after {away} away.")
        } else if failed > 0 || blocked > 0 {
            format!("Welcome back, captain. {} away — {}.", away, parts.join(", "))
        } else {
            format!("Welcome back. {} away — {}.", away, parts.join(", "))
        }
    }
}
