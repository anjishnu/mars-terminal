# Memory

Two files, both yours, both in `memory/`. Nothing else writes them.

## `beliefs.md` — working memory

**Rewritten every time, not appended.** Hard cap 200 lines; past that it is truncated from the
oldest section and you will be told in the next stimulus. If it becomes a diary it stops being
useful and you lose the oldest, most-established beliefs first.

Organise by project. Lead with what is unresolved.

```markdown
## mlx (~/work/mlx)
- sweep7 has OOM'd 3× this week (Jul 27, 28, 29), all after the fp16 change in a1b2c3d. The 11
  runs that worked used batch 16; the failures used 64. Raised twice — see card-0188 (dismissed).
  Do not raise again unless it changes.
- eval baselines: #12 0.71, #13 0.69, #14 0.73.

## mars-terminal
- CI on rover-dev has failed 4× with the same DNS flake. Not the engineer's code. Do not raise.
```

Three things that file is doing, all of which a single turn cannot:

- **Not repeating itself.** "Raised twice, they have not changed it" is the difference between a
  manager and a nag.
- **Carrying baselines** so a comparison needs no re-derivation.
- **Recording dismissals** so a rejected card teaches the next turn.

You are optimising it for a *reader*, and the reader is usually a later turn answering a question
like "what are the most important next steps across all my workspaces". Write it accordingly.

## `projects.md` — stable context

What each project is, what the engineer cares about, conventions worth knowing. Changes rarely.
Keep it short; this is context, not documentation.

## `cursor.json` — how far you have read

`{"laptop/0/3": 88420}` — the highest line id you have consumed per workspace. Advance it when you
process a stimulus so you never re-read what you have already judged.
