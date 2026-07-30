<!-- mars-doc-version: 9 -->
# Workspace summaries

**Read second**, after the briefing, when a row is tapped. One file per workspace at
`workspaces/<pane-id>.md`.

Read **only that pane's `output`** — its tail, its delta, its signals. Not the other panes, not
the briefing you have not written yet. The signals are the shortcut: an exit code or a `[y/N]`
tells you the interesting thing without reading the text around it.

The briefing answers *does anything need me?* This answers the next question down: **what is going
on here, and what do I do about it?**

## Shape

```markdown
---
source: agent
---
Last: cargo test --workspace
Intent: verifying the manager hierarchy before installing it.
State: failing — 3 of 140, all in session.rs, the same assertion each time.

Next:
- **Re-run that one test alone to see whether it is deterministic**
- Check whether the migration in workspace 2 touched the schema
- Ship anyway; it was failing before today too
```

| Line | What goes in it |
|---|---|
| `Last:` | The last command actually run. Verbatim, from the output — not a paraphrase. If you cannot see one, say `Last: unknown`. |
| `Intent:` | Why they ran it, in one clause. This is inference — you have `memory/projects.md` and the surrounding output. If you genuinely cannot tell, say so rather than inventing a reason. |
| `State:` | Where it is now, with the number that makes it real. "failing — 3 of 140" beats "failing". |
| `Next:` | Exactly three bullets. |

## The three next actions

Three, always — not two, not five. Three forces you past the obvious one and stops short of
padding. **Bold exactly one**, the one you would do.

Order them so the bold one is not always first; if the recommended action is invariably the top
bullet, the other two stop being read. Make them genuinely different — a retry, an investigation
and a decision to move on are three options. Three flavours of "look into it" are one.

The recommendation is where you are most useful and most exposed. Base it on what the output
actually shows and on what `memory/projects.md` says they are trying to do — a test failure
matters differently the day before a release than in the middle of a refactor.

## Rules

- **Never restate the row.** Name, state and age are already on screen above this.
- **Skip workspaces whose delta is empty.** An untouched file beats a rewritten identical one,
  and it is how they can trust that a changed file means something changed.
- **Formatting is allowed here** — bullets and one bold item — unlike the mission briefing, which
  is plain prose. This is read deliberately, not at a glance.
- **Do not invent a command.** If the output does not show one, `Last: unknown` is the honest
  answer and the rest of the note is still worth writing.
