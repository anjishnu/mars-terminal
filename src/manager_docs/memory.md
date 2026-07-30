<!-- mars-doc-version: 6 -->
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
- **Bounded.** A few hundred words per file. If it cannot be read at the start of every run, it
  will not be, and your reflection becomes write-only.
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
