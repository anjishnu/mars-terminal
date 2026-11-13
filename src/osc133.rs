//! OSC-133 shell-integration scanner — exact command boundaries for the ledger.
//!
//! A terminal is an opaque byte grid; the verdict ladder can only *guess* a
//! command's outcome from the tail. Shell integration (the FinalTerm/iTerm2/VS
//! Code "semantic prompt" markers) annotates the byte stream with ground truth:
//! where each command starts and ends, its exit code, its cwd, and — with the VS
//! Code `633;E` extension — the command text. This scanner reads those markers
//! out of the raw PTY stream (alongside vt100 rendering) and turns them into
//! exact ledger records. See design_ideas/movement-1-ledger-spec.md §Phase B.
//!
//! It is purely additive: a shell that emits no markers produces no events, so
//! existing panes are unaffected. Making Mars *inject* the integration into
//! spawned shells (so every shell emits markers, not just pre-integrated ones)
//! is the real-terminal-gated remainder — this scanner already captures markers
//! from any source (a user's existing iTerm2/VS Code integration) today.
//!
//! Markers handled (each `ESC ] … ST`, ST = BEL or `ESC \`):
//!   - `133 ; C`            command pre-execution (start)
//!   - `133 ; D [; <exit>]` command finished, with exit code
//!   - `633 ; E ; <cmd>`    the command line (VS Code extension)
//!   - `7 ; file://host/path`  the cwd
//!   - `7501 ; state=… : kind=…` a program's own report of its state (the Program Status
//!     Protocol). pi sends these: `working`, `blocked`, `done`, `error`, `idle`. Unlike every
//!     other signal a terminal has about an agent, it is the agent saying so, not Mars guessing.

const MAX_OSC: usize = 8192;

#[derive(PartialEq, Eq, Clone, Copy)]
enum State {
    Ground,
    Esc,
    Osc,
    OscEsc,
}

/// A command boundary event surfaced from the stream.
#[derive(Debug, PartialEq, Eq)]
pub enum CmdEvent {
    /// A command began executing (`133;C`) — the caller stamps the start time.
    Start,
    /// A command finished (`133;D`) with its exact facts.
    End {
        command: Option<String>,
        cwd: Option<String>,
        exit: Option<i32>,
    },
    /// A program reported its own state (OSC 7501). `kind` qualifies `blocked`: `permission`,
    /// `question` or `auth`. Reports carry no prompt or output text, only these words.
    Status { state: String, kind: Option<String> },
}

/// Incremental OSC scanner. One per terminal, fed the raw PTY bytes; state (a
/// partial OSC split across reads, the latest cwd, the pending command text)
/// persists across `feed` calls.
pub struct Scanner {
    state: State,
    buf: Vec<u8>,
    cwd: Option<String>,
    command: Option<String>,
}

impl Default for Scanner {
    fn default() -> Self {
        Scanner { state: State::Ground, buf: Vec::new(), cwd: None, command: None }
    }
}

impl Scanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of PTY bytes; returns any command-boundary events completed
    /// within it. Non-OSC bytes are ignored (vt100 renders them).
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<CmdEvent> {
        let mut out = Vec::new();
        for &b in bytes {
            match self.state {
                State::Ground => {
                    if b == 0x1b {
                        self.state = State::Esc;
                    }
                }
                State::Esc => {
                    self.state = if b == b']' {
                        self.buf.clear();
                        State::Osc
                    } else {
                        State::Ground // some other escape; vt100 handles it
                    };
                }
                State::Osc => {
                    if b == 0x07 {
                        self.finish(&mut out);
                        self.state = State::Ground;
                    } else if b == 0x1b {
                        self.state = State::OscEsc;
                    } else if self.buf.len() < MAX_OSC {
                        self.buf.push(b);
                    } else {
                        self.buf.clear();
                        self.state = State::Ground; // runaway OSC; drop
                    }
                }
                State::OscEsc => {
                    if b == b'\\' {
                        self.finish(&mut out);
                    }
                    // A non-`\` after ESC is malformed; drop the OSC either way.
                    self.state = State::Ground;
                }
            }
        }
        out
    }

    fn finish(&mut self, out: &mut Vec<CmdEvent>) {
        let s = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned();
        let (num, rest) = match s.split_once(';') {
            Some((n, r)) => (n, r),
            None => (s.as_str(), ""),
        };
        match num {
            "133" => {
                let code = rest.split(';').next().unwrap_or("");
                match code {
                    "C" => out.push(CmdEvent::Start),
                    "D" => {
                        let exit = rest
                            .split(';')
                            .nth(1)
                            .and_then(|x| x.trim().parse::<i32>().ok());
                        out.push(CmdEvent::End {
                            command: self.command.take(),
                            cwd: self.cwd.clone(),
                            exit,
                        });
                    }
                    _ => {} // A / B: prompt boundaries, not needed for the ledger
                }
            }
            // `633;E;<cmd>` carries the command line (VS Code). A trailing `;nonce`
            // is rare and kept verbatim in v1 (commands can contain `;` anyway).
            "633" => {
                if let Some(cmd) = rest.strip_prefix("E;") {
                    self.command = Some(cmd.to_string());
                }
            }
            // `7 ; file://host/path` — the live cwd.
            "7" => {
                if let Some(after) = rest.strip_prefix("file://") {
                    if let Some(i) = after.find('/') {
                        self.cwd = Some(after[i..].to_string());
                    }
                }
            }
            // `7501 ; key=value : key=value`. A program's support query (`7501 ; ?`) is not a
            // report and is ignored: Mars asks agents to report unconditionally instead
            // (`PI_PROGRAM_STATUS=1`), which needs no reply written back into the pane.
            "7501" => {
                let mut state = None;
                let mut kind = None;
                for pair in rest.split(':') {
                    let Some((k, v)) = pair.split_once('=') else { continue };
                    match k.trim() {
                        "state" => state = Some(v.trim().to_string()),
                        "kind" => kind = Some(v.trim().to_string()),
                        _ => {}
                    }
                }
                let known = ["idle", "working", "done", "blocked", "error", "clear"];
                if let Some(state) = state.filter(|s| known.contains(&s.as_str())) {
                    out.push(CmdEvent::Status { state, kind });
                }
            }
            _ => {}
        }
    }
}

/// Map a completed command event to a ledger entry — but only when it is
/// *noteworthy*: an exact failure (nonzero exit), or a long-running command's
/// conclusion. Trivial quick successes stay unlogged (as today), keeping the
/// shared journal — and its summary/mission consumers — lean. Full per-command
/// trace capture (every `ls`) is a Ground-Control-scale separate store, deferred.
/// The command text is redacted before it is stored.
pub fn to_ledger_entry(
    session: &str,
    surface: &str,
    command: Option<String>,
    cwd: Option<String>,
    exit: Option<i32>,
    dur_secs: Option<u64>,
) -> Option<crate::worklog::WorkEntry> {
    let failed = exit.map(|c| c != 0).unwrap_or(false);
    let long = dur_secs.map(|d| d >= crate::briefing::GOODNEWS_SECS).unwrap_or(false);
    if !failed && !long {
        return None;
    }
    Some(crate::worklog::WorkEntry {
        ts: crate::worklog::now_secs(),
        session: session.to_string(),
        tab: surface.to_string(),
        // Tier-0 only: no LLM verdict yet, so the record reads `semantic:pending`
        // (an exact deterministic headline awaiting enrichment).
        verdict: String::new(),
        failed,
        dur_secs,
        cwd: cwd.unwrap_or_default(),
        command: command.map(|c| crate::retrieval::redact(&c)),
        exit,
        error_excerpt: None,
    })
}

#[cfg(test)]
mod status_tests {
    use super::*;

    #[test]
    fn a_program_status_report_is_read_with_either_terminator() {
        let evs = Scanner::new().feed(
            b"text\x1b]7501;state=working:app=pi:msg=c2Vzc2lvbg==\x1b\\more\x1b]7501;state=blocked:kind=auth\x07",
        );
        assert_eq!(
            evs,
            vec![
                CmdEvent::Status { state: "working".into(), kind: None },
                CmdEvent::Status { state: "blocked".into(), kind: Some("auth".into()) },
            ]
        );
    }

    #[test]
    fn a_query_or_an_unknown_state_is_not_a_report() {
        assert!(Scanner::new().feed(b"\x1b]7501;?\x1b\\").is_empty());
        assert!(Scanner::new().feed(b"\x1b]7501;state=dancing\x07").is_empty());
    }

    #[test]
    fn a_report_split_across_reads_still_arrives() {
        let mut s = Scanner::new();
        assert!(s.feed(b"\x1b]7501;sta").is_empty());
        assert_eq!(s.feed(b"te=done\x07"), vec![CmdEvent::Status { state: "done".into(), kind: None }]);
    }
}
