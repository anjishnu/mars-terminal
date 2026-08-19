---
name: executing-a-brief
description: How to execute a Mars brief end to end. Use when you have been assigned a brief, told to read ~/.mars/briefs/<id>/brief.md, asked to implement a spec from ~/.mars/briefs/, or when starting work that came with a brief id. Covers which sections bind, the branch, in_process.md deviations, completed.md, and when to stop.
---

# Executing a brief

You were assigned a brief. It is a specification somebody argued over and then pressed a button to
approve. Your job is to build it, record where you departed from it, and report what you actually
built.

**Read `~/.mars/briefs/<id>/brief.md` first.** Everything below is how to work it.

## The two rules you must be told rather than discover

### 1 · Which sections bind

A brief has binding sections and advisory ones, and confusing them is the most expensive mistake
available here.

| Section | Status | What that means |
|---|---|---|
| `## Problem + evidence` | **BINDING** | The paths and line anchors are facts. If one is wrong, that is a deviation — record it |
| `## Acceptance` | **BINDING** | Numbered, each independently checkable. **This is the definition of done.** Not your judgement of done |
| `## Out of scope` | **BINDING** | Do not do these things, even if they are obviously right. They were excluded deliberately |
| `## Decisions already made` | **BINDING** | Every ruled fork, with what was rejected and why. **Do not re-litigate a ruled fork.** If you think a ruling is wrong, say so in `in_process.md` and follow it anyway |
| `## Approach` | **ADVISORY** | Deviate freely. It is somebody's best guess before contact with the code |
| `## HLD` / `## LLD` | **ADVISORY** in shape, **BINDING** where it restates a ruled fork | Build what works; keep the rulings |

The asymmetry is deliberate: **what to achieve is fixed, how to achieve it is yours.**

### 2 · Blocked is terminal

If you cannot proceed — a dependency is missing, an acceptance criterion is impossible, a ruled fork
turns out to be unbuildable — **stop and report `BLOCKED:`**. Do not:

- pick a different interpretation and continue
- mock, stub or fake the thing you could not get working
- narrow the acceptance criteria to something you can satisfy

A blocked brief that stops is a small cost. A blocked brief that quietly redefines itself and
reports success is how a system loses the ability to trust any report.

## The branch

**Work on a fresh branch named from the brief id.** That makes `git log <branch>` answer *what did
this brief actually do* with no file to keep in sync.

```bash
git checkout -b <brief-id>
```

**Before you do:** check `git status`. If the working tree has uncommitted changes that are not
yours, **you are in somebody's pane** — `git checkout -b` will switch *their* checkout and disturb
work you cannot see. Stop and report it. Use a worktree if one was set up for you.

Never merge to mainline yourself. Merging is the human's second press.

## The three files, and when each is written

They are written at different times **because each holds a fact that is only true at that time.**

```
~/.mars/briefs/<brief-id>/
├── brief.md        READ ONLY — you may never edit this
├── in_process.md   WRITE AS YOU GO — every deviation, at the moment you decide it
└── completed.md    WRITE AT THE END — what was actually built
```

### `brief.md` is read-only, and this is enforced

`Edit(**/briefs/*/brief.md)` is on the deny list. A worker that can edit its own specification can
change what was approved and then satisfy the change. If the brief is wrong, that is what
`in_process.md` is for.

### `in_process.md` — write it *when you deviate*, not afterwards

This is the one artifact with no precedent, and its whole value is timing. **A deviation
reconstructed at completion is a rationalisation; one written at the moment is evidence.**

Record *why*, not *what*. A diff already records what.

```markdown
## Deviation 1 — 14:32
The Approach assumed `send()` queues when offline. It does not — it drops.
So I added a queue at the call site rather than relying on the transport.
Acceptance 3 still holds; nothing in Out of scope was touched.
```

Every deviation gets: what the brief assumed, what is actually true, what you did instead, and
whether any binding section is affected.

### `completed.md` — the record, not the pane

The pane scrolls, the transcript gets pruned, the daemon restarts. **One file makes "what did this
build" one read.**

```markdown
## What was built
<two or three sentences>

## Acceptance
1. <criterion> — PASS, verified by `<command>`
2. <criterion> — PASS
3. <criterion> — NOT MET, see Deviation 2

## Files changed
<the list — this is checked against `git diff --name-only`>

## Commits
<sha> <subject>

## Notes for later
<anything you noticed and did not do — it can be promoted to a memo by a press>
```

`DONE:` / `BLOCKED:` still prints to the pane. **The file is the record; the line is the courtesy.**

## Verification comes to you first

A tier-0 audit runs before a human is shown the card: the brief's own `verify:` commands,
`git diff --name-only` against your declared file list, and **any file changed without a matching
revision in `in_process.md`.**

That last one is why the deviation record matters mechanically and not just ethically: an
undeclared file change is a fault, and the only way to clear it is to have written down why.

So before you report done:

```bash
git diff --name-only <base>..HEAD    # matches your Files changed list?
<the brief's verify: commands>       # all green?
```

## Gotchas

- **Do not re-litigate a ruled fork.** `## Decisions already made` records what was rejected *and
  why*. Arriving at the rejected option independently is common and is not evidence it was wrong.
  Note your disagreement in `in_process.md` and build the ruled option.
- **`git checkout -b` in a shared pane disturbs a human's working tree**, and "return to the
  original branch" afterwards restores a name, not a state. Check `git status` first.
- **`~/.mars` is not a git repo.** Do not try to commit anything under it.
- **Never mock or stub something you could not get working**, then report done. Surface it. This is
  the single most damaging failure available to a worker.
- **Deny list — files that execute without being run.** `build.rs`, `Makefile`, `package.json`,
  `Cargo.toml`, `.github/workflows/**`, `.git/hooks/**`, `.envrc`, `.claude/**`. If your brief needs
  one of these changed, say so in `in_process.md` and leave that line for a human.
- **In `mars-terminal`, run `--selfcheck` on all three build configs** before reporting done —
  default, `--features web`, and `--no-default-features`. The memory-free build has caught
  regressions that the default build passed.
- **Extend `--selfcheck` rather than adding a test harness.** It drives the real `App` against
  `TestBackend` with real PTYs and a real daemon.
- **Editing anything under `src/manager_docs/` requires a `mars-doc-version` bump.** An unbumped
  edit **stops the manager on every host at once**.
- **Never assert text appears by searching raw ANSI bytes.** ratatui interleaves cursor moves
  between characters, so typed text is not a contiguous substring. Parse through `vt100` and check
  the rendered screen.

## When you are done

1. `in_process.md` is complete and every deviation is dated.
2. `completed.md` names the acceptance results, the files, and the commits.
3. The branch is pushed; **you do not merge.**
4. Print `DONE: <brief-id>` — or `BLOCKED: <brief-id> — <one line>`.

---

**Note for maintainers:** when `~/.mars/briefs/WORKING-MODEL.md` ships, seed it from this file
rather than writing the protocol twice. Same source, two delivery mechanisms — assignment names a
path, discovery matches a description.
