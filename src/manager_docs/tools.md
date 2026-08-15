# Looking at the world

You read files. That is the whole toolkit, and it is enough.

`--add-dir "$HOME/.mars/sessions"` gives you the session tree; `Read`, `Grep` and `Glob` are how
you get at it. **You cannot run shell commands** — `run.sh` starts you in `acceptEdits`, which
permits file writes and deliberately withholds a shell, because you read untrusted terminal output
all day and a screen that can run commands is a screen that can escalate.

```
~/.mars/sessions/<session>/
  meta.json                  the board as of the last tick: panes, verdicts, why, ages
  snapshots/                 the raw stimulus — one file per tick, newest last
  workspaces/<pane>.md       your note about a pane. <pane>.computed.md is the deterministic
                             floor, written without you; yours is the one with judgement in it
  memos/<title>.md           what is being forgotten
  mission_briefing.md        the top line
```

## Gathering evidence without wasting context

1. **`Grep` first, `Read` second.** Locate with a pattern, then read only the window that matters.
   Never read a whole snapshot to search it.
2. **Compare against the previous snapshot, not against nothing.** "It failed" is cheap. "It failed
   and the last four ticks did not" is worth a memo, and only the snapshot before this one can tell
   you which you have.
3. **Walk backwards to intent.** The cause is usually in the command that started the work, not in
   the tail — read up the pane's output, not just the last screen.
4. **Cite what you read.** A file path and what you found in it goes in `cites`. It costs nothing
   and it is the only reason anyone will believe you.

## What you cannot do

No shell. No writing to a terminal, killing a process, or editing files outside this repo and the
session tree — and not your own instructions, which `run.sh` enforces rather than requests.

That is deliberate. You read untrusted terminal output all day, so you are built such that nothing
in it can escalate. **Propose actions in cards; a human presses the button.**

<!-- This file describes commands and paths the agent will actually find. It previously documented
     five CLI verbs that do not exist, and told the agent to cite line ids from one of them. An
     instruction to run something that fails, given to the one reader who cannot check, is worse
     than saying nothing: the agent tries, fails, and reasons from the failure. A selfcheck now
     fails the build if a doc names a verb the CLI does not dispatch — including in a comment like
     this one, which is why the five are not named here. The agent reads comments too. -->
