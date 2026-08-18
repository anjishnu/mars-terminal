<!-- mars-doc-version: 7 -->
# Memory

Two files. You read them first, every run, and revise them last.

| File | Role |
|---|---|
| `memory/beliefs.md` | **Read first.** A short index: what memory holds, and any belief that belongs to no single project. |
| `memory/projects.md` | Projects and workstreams, **with their workspaces nested underneath.** |

Everything else you write describes *now*. These describe what you understand, and they are the
only reason a briefing can rank anything — "the auth test failed" is a fact, "the auth test blocks
the release you are cutting" is a fact that matters, and the difference is entirely here.

## `projects.md`

A workspace lives inside its project, because the link is what gives it meaning. A pane with no
project attached is just a pane.

```markdown
## mars-terminal · ship the manager agent
Purpose: make the manager write briefings from real terminal output, not pane states.
State:   hierarchy wired; output capture landed today.
Next:    check the first briefing actually names a failure.
_confirmed 2026-07-30_

- **terminal 1** — cuts the release build.
  Failing on the auth test since 14:02. Next: re-run alone; suspect the migration in workspace 2.
```

**`Purpose:` is durable; `State:` and `Next:` are today.** "terminal 1 cuts the release build" is
worth carrying into every future run. "failing since 14:02" is this morning's news. Keep them on
separate lines and you will keep them straight; blur them and this file becomes a stale second
briefing that contradicts the real one.

## Rules, or the file rots

- **Revise, never append.** A belief that turned out wrong is corrected in place, not stacked
  under its replacement. Append-only memory contradicts itself within a week, and then it stops
  being read — which is worse than having none.
- **Bounded — 800 words per file, and it is measured.** Not a target you aim at: the batch reports
  each file's size, and a file over the bound is a scored fault on every run until it is fixed.
  If it cannot be read at the start of every run, it will not be, and your reflection becomes
  write-only. This bound was prose for a long time and prose is a rule you have to remember while
  doing everything else — by the time it was measured, `projects.md` held 28,052 words and was
  being re-read at the top of every run.
- **Split by lifecycle, not by topic.** `Purpose:` is durable; `State:` and `Next:` are today. In
  one file the durable half gets re-read at the cost of the volatile half, and the volatile half
  never gets pruned because the durable half looks load-bearing. When a file goes over, that split
  is usually the fix — not deleting history you will want.
- **Date what you confirm.** `_confirmed <date>_` per project, so a stale belief is visibly stale
  rather than quietly wrong.
- **Only write what you could cite.** Beliefs are not a licence to speculate about intent. If you
  are inferring what someone is working on, say that you inferred it.
- **Say what you got wrong.** A run that revises nothing should note that the picture held. That
  is a stable understanding, not a skipped step.

## Where the engineer's own words live

`~/.mars/goals.json` and `mission.json` arrive in the snapshot under `goals`. Those are *declared*
intent; your beliefs are *observed* reality. They disagree often, and the disagreement is usually
the most useful thing you know. When they conflict, say so rather than quietly siding with one.
