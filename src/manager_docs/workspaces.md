<!-- mars-doc-version: 6 -->
# Workspace summaries

**Read second**, after the briefing and only when a row is tapped. One file per workspace at
`workspaces/<pane-id>.md`.

By the time this is read, the engineer has already read the briefing and is looking at a row that
shows the name, the state and the age. So this line answers exactly one question they still have:

> **Should I care about this one?**

Read **only that pane's `output`** — its tail, its delta, its signals. Not the other panes, not
the briefing you have not written yet. The signals are the shortcut: an exit code or a `[y/N]`
already tells you the interesting thing without reading the text around it.

## Shape

One line. Twelve words or fewer. Verb first, present tense, no subject — the row above already
names it.

```
building · 3m in
waiting on y/N since 14:02
failed on the same assertion as the last two runs
```

Not:

```
The build process is currently executing and has been for 3 minutes.   ← restates the row
claude is blocked.                                                     ← the row already says blocked
Everything looks good here!                                            ← says nothing
```

## Rules

- **Never restate the row.** Name, state and age are already on screen. Repeating them spends the
  whole line and adds nothing.
- **Never repeat the briefing.** If the briefing already said "the sweep finished at 0.91", the
  sweep's summary says what is true *now* that the briefing did not cover.
- **Add the one fact the row cannot show** — what it is waiting on, what it changed, how far
  through it is, why it failed.
- **Skip workspaces that have not moved.** Leaving an accurate file untouched is better than
  rewriting it identically; it is also how the engineer can trust that a changed file means
  something changed.
- **Nothing interesting is a valid outcome.** Write nothing rather than filler.
