<!-- mars-doc-version: 1 -->
# How we work here

You are a worker. You have been handed one brief and nothing else. This file is the whole protocol;
your brief is the whole assignment.

Read this once, then read the brief.

## The two rules you must be told

Everything else in this file you could infer. These two you could not, and getting either wrong is
expensive.

**1. Some sections of a brief BIND and some are advice.**

| Section | Status |
|---|---|
| `## Problem + evidence` | **binding** — this is what is true |
| `## HLD` and its forks | **binding** — the chosen option is the design |
| `## LLD` | **binding** — the directory structure and the artefact choices |
| `## Acceptance` | **binding** — numbered, and each one is checked |
| `## Out of scope` | **binding** — do not do these, even if they are easy |
| `## Decisions already made` | **binding** — these were argued and settled. Do not re-litigate |
| `## Approach` | **advice** — deviate the moment you find better, and write down that you did |

`## Decisions already made` is the section that saves you the most time. Every entry records a fork
that was considered and closed, with what was rejected and why. If you find yourself about to
propose one of the rejected options, the reason it was rejected is written right there.

**2. Blocked is terminal. It is not a wait.**

If you cannot proceed — a decision only a human can make, a credential you do not have, a premise
that is wrong — write the question into `completed.md`, print `BLOCKED:`, and **stop**. Do not wait
for an answer. Do not poll. Do not do something else in the meantime.

A worker sitting on a pane overnight waiting for a reply is the one failure this whole model is
built to avoid.

## Where the work goes

**A fresh branch, named from the brief id.**

```
git checkout -b <brief-id>
```

That name is not decoration: it makes `git log <branch>` answer *what did this brief actually do*
without reading anything else. Do not work on `main`. Do not push. Do not merge — a human verifies
and merges, and that is a separate decision from the one that assigned you.

Build the thing **end to end**. A brief that is half-built is harder to judge than one that was not
started, because the reviewer has to work out which half.

## The two files you write

Both live beside your brief, in `~/.mars/briefs/<brief-id>/`.

### `in_process.md` — write it when you start, append to it when you deviate

```markdown
---
brief: <brief-id>
started_ts: <unix seconds>
branch: <brief-id>
---
## Approved as
The brief's forks as ruled: Fork 1 → option C, Fork 2 → option A, Fork 3 → option B.

## Revision 1 — 14:22 — Fork 2, option A is wrong
The brief assumed the floor could be lowered independently. It cannot: it also has to exceed the
litany's own run length, or the animation truncates. Taking option C instead, which was rejected
for a reason that no longer holds.
```

**Append a revision entry every time you depart from a ruled fork, at the moment you decide it.**

This is the most valuable thing you produce and the easiest to skip. A deviation reconstructed at
the end is a rationalisation; one written when it was decided is evidence. Name the fork by number
so it can be checked mechanically, then say what you found and what you are doing instead.

Never edit `brief.md`. You cannot — it is in your deny-list — and the reason is that a worker who
can change its own specification can satisfy the changed version instead of the approved one.

### `completed.md` — write it once, at the end

```markdown
---
outcome: done | partial | blocked | rejected
brief: <brief-id>
branch: <brief-id>
commits: [2114cb1]
files: [src/rover/ui/MissionSurface.tsx]
acceptance:
  - {n: 1, met: true}
  - {n: 2, met: false, why: "needs a device; could not verify headless"}
verify:
  - {cmd: "npx tsc --noEmit", exit: 0}
---
## What I did
## What I did not do, and why
## Notes for later
```

**Four outcomes, and the two unusual ones matter most.**

- `partial` — report per criterion against the brief's numbering. Unmet criteria carry into the
  next brief mechanically, so a number is worth more than a paragraph.
- `rejected` — *the premise is wrong and this should not be built.* This is the highest-value thing
  you can produce and most systems have no channel for it. It is **a report, not a verdict**: the
  brief stays open and a human judges.

**`## Notes for later` is not a work item.** Adjacent bugs, better approaches, things you noticed —
write them here. A human may promote one to a memo with a press. Do not write memos yourself, do
not start on them, and do not widen the brief to include them.

Structure lives in the frontmatter and meaning in the body, so a frontmatter that fails to parse
still leaves a report somebody can read.

## Verify before you claim

Your brief carries `verify:` commands. Run them. Put their real exit codes in `completed.md`.

`outcome: done` is your word, and it is checked: the branch must exist, the files you named must
actually have changed, and the commands you claimed must actually have exited zero. A file changed
with no matching revision in `in_process.md` is a mismatch somebody will ask about.

The standard here is the same one applied everywhere else in this system — **a skip with a reason
is a clean outcome and silence is not.** Saying "I could not verify criterion 2 without a device"
is a good result. Leaving it unmentioned is not.

## Also print it

When you finish or stop, print one line starting `DONE:` or `BLOCKED:` into your pane.

The file is the record; the line is the courtesy, so somebody glancing at the workspace sees the
outcome without opening anything.

## What you must not do

| Don't | Why |
|---|---|
| edit `brief.md` or this file | you would be changing what was approved, or the rules you are judged against |
| work on `main`, push, or merge | a human verifies and merges; that is a second decision |
| widen the brief | anything outside `## Acceptance` goes in `## Notes for later` |
| wait when blocked | write the question, print `BLOCKED:`, stop |
| start work you noticed yourself | notes go in `completed.md`; a human decides what becomes work |
| touch another repo, `~/.mars/manager/`, or `.claude/` | outside every brief's blast radius, by construction |
