---
name: manage-task-to-completion
description: How to carry a piece of work from first observation to merged, across many runs. Use when tracking something that spans more than one turn — a recurring condition, a brief that has been assigned, a workspace that has been stuck, or when asked what happened to a piece of work. Covers what earns a brief, how to hold state between runs, recognising completion, and closing the loop.
---
<!-- mars-doc-version: 1 -->

# Managing a task to completion

Your runs are minutes apart and the work is days long. **Nothing you learn survives a run except
what you write down**, so managing a task to completion is mostly the discipline of leaving the
right fact in the right file at the right moment.

You do not do the work. You notice it, describe it, hand it over, watch it, and close it out.

## The loop, and where you sit in it

```
OBSERVE ─► MEMO ──────────────────────────────► expires
 (you)      │  a condition, glanceable
            │  recurs across episodes ─► worth attention
            │
            │  more than ~3 files of work?
            ├─► conversation ─► BRIEF ─► PRESS ─► WORK ─► COMPLETED ─► PRESS ─► mainline
            │   (human + Rover)  (forks   assign  (worker) (report)    verify    (merge)
            │                     ruled)
            ▼
      you track State/Next across every run in between
```

**Two presses, and they are yours to prepare and never to make.** Assign asks *is this the right
thing to build*; verify asks *did it get built right*. You make both cheap; a human makes both.

## What earns which artifact

The axis is **how much has been decided.**

| | Says | Expires when | Reader |
|---|---|---|---|
| **memo** | a condition holds | the condition clears, or it is dismissed | a human, glancing, on a phone |
| **brief** | here is what to build, and what was ruled out | it is built | a worker, once, carefully |
| **completed** | here is what was actually built | never | a human, and the next brief |

### A memo is not a small brief

A memo is a headline behind a folder. A brief carrying three forks is 1,500 words minimum. **Putting
a brief in the phone feed is a known, ruled-against failure.** If what you have to say does not fit
as a headline plus a few lines, it is not a memo yet — it is a candidate for a conversation.

### What earns a brief: blast radius

*Ruled.* Not recurrence, not your judgement of importance — **how much of the world a change
touches.** Roughly: more than about three files, or any change that crosses a trust boundary, or
anything that needs a fork argued before it can be built.

Below that line, say it in a memo and let a human do it in two minutes.

**You may nominate. You may not brief.** A brief is written by a human and Rover in conversation,
because its value is the forks that were argued and closed. What you contribute is the observation,
the evidence, and the recurrence.

## Holding state between runs

This is the whole skill. Three mechanisms, and they have different jobs.

### `memory/projects.md` — Purpose, State, Next

```markdown
## mars-manager · the scorer bugs
Purpose: the manager's fault rate is a string comparison, not a real signal.
State:   brief written, assigned to terminal 2 at 09:14. Branch f8-f9-scorer.
Next:    check for completed.md; if present, verify the doc-version bump landed.
_confirmed 2026-08-18_
```

**`Purpose:` is durable; `State:` and `Next:` are today.** Keeping them on separate lines is what
stops this file becoming a stale second briefing. Revise in place — **never append.**

### The tracking questions to answer every run

For anything in flight, three reads and no more:

1. **Does `~/.mars/briefs/<id>/completed.md` exist?** If yes, the work is done and the loop moves to
   verify. One read, no parsing terminal output.
2. **Does `in_process.md` have new deviations since your last run?** That is the mid-flight signal
   worth surfacing — a worker that has deviated three times may be building something other than
   what was approved.
3. **Has the pane gone quiet?** `stalled` on an agent pane means the process is alive and silent,
   which is a *question*, never a confirmed failure. Say what you can see and what you cannot.

### Never reconstruct from the pane

The pane scrolls, transcripts get pruned, daemons restart. Determining what an agent had completed
once took a merge-base check, three greps and a field count — because the work left commits and no
statement of intent. **`completed.md` is one read. Prefer it to everything.**

## Recognising completion, and closing out

A task is complete when **all four** are true. Fewer is not "nearly done", it is a different state
and should be reported as one:

1. `completed.md` exists and names its acceptance results.
2. The branch exists and its commits match the declared file list.
3. Any acceptance criterion marked NOT MET has a matching deviation in `in_process.md`.
4. A human has pressed verify.

**Only after 4 does the memo that motivated it expire.** Until then the condition still holds — the
work being finished is not the same as the condition clearing, and conflating them is how a memo
disappears before the fix ships.

### What to write at the close

- Update `projects.md`: `State:` becomes what shipped, `Next:` becomes what it enables or `—`.
- If `completed.md` has a **Notes for later** section, each note is a candidate memo. **A note the
  worker left and nobody promoted is a note that never existed.**
- Say in the briefing what changed, once. Do not restate it every run afterwards.

## Writing nothing is a success

A quiet board earns three short lines and no memos. **Do not manufacture activity to look useful.**
The deterministic briefing already covers the board; you are writing the part arithmetic cannot.

If a run has nothing to add, say so in the receipt with a reason and stop. That is a clean run.

## Gotchas

These are measured on real runs, not hypothetical.

- **Key your receipt's `skipped` entries by session *id*, not name.** The scorer compares against
  one of the two and a mismatch scores your correct run as silence. Include both if unsure:
  the batch entry carries `id` and `name`.
- **Your receipt filename is `runs/<batch-stem>.json`** — the batch name already ends in `.json`.
  Writing `<batch-filename>.json` produces a double suffix, the scorer finds nothing, and a good run
  is recorded as a total failure with no way to tell it from an absent one.
- **Read `memory/runs.jsonl` when you want to know how you have been doing.** It holds your own
  scored history and nothing else surfaces it to you.
- **`projects.md` has a bound of a few hundred words and it is not enforced.** It has reached 26,000
  words on a real host, which is read at the start of every run and is the dominant cost of each
  one. If a section has grown past a screen, **compress it in place** — `Purpose:` survives,
  yesterday's `State:` does not.
- **Do not fold conversations you cannot see.** If a workspace entry carries no conversation, the
  instruction to rewrite `conv/<pane-id>.md` does not apply — its guard is *"whose entry carried a
  conversation"*, and that is often never true.
- **`stalled` is a question, not a verdict.** Mars checked the process table, so something really is
  running. Raise it; never report it as failed.
- **Never write about a workspace marked `watching`** — that is the pane a human is looking at right
  now, and they judge it better than you. The exception is `blocked`.
- **Absence must say why.** *"No briefs in flight"* and *"could not read the briefs directory"* are
  opposite facts. Never let them render the same.
- **You cannot execute anything.** Actions attached to a memo are proposals a human confirms. Do not
  pretend otherwise and do not ask for a way around it.

## The one-line test

> If your account of a task cannot be checked against a file somebody else can open, it is not a
> record — it is a claim. Write the file.
