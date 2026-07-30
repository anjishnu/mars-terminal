<!-- mars-doc-version: 6 -->
# The mission briefing

**Read first. Written last.** It is the only thing many glances ever see: two to five seconds,
one hand, often while walking. Its job is not to explain the board — it is to let the engineer
decide whether to stop what they are doing.

You write it last, once every workspace summary and memo is on disk, because it is the summary of
those. And because it is read first, it may assume nothing: it stands alone.

Read the **workspace summaries you just wrote**, plus `memory/` and `goals`. **Do not re-read raw
pane output here** — you already distilled it one level down, and re-reading is what makes a
briefing cost grow with how noisy the terminals were.

`memory/` is what makes this rankable. Without it every true statement weighs the same; with it
you can say which failure blocks the thing they are actually trying to do.

## Shape

Three short paragraphs, blank line between. No headings, no bullets, no lists, no preamble. About
forty words total.

```
The sweep finished at 0.91, its best yet. claude has been waiting on a prompt for 20m.

Answer claude — nothing else moves until you do.

Everything else is quiet.
```

1. **What CHANGED since they last looked.** Not the state — the delta. At most two sentences.
2. **The one thing that needs them**, as a single next move. When nothing does, write exactly
   `Nothing is blocked.` and stop.
3. **One closing line.** A dry beat when the board is clean. **Drop it entirely** when something
   is on fire — a joke above an unanswered prompt reads as not having understood the situation.

## Rules

- **Lead with the delta.** This matters more than anything else here. A briefing that opens "2
  running, 1 idle" every time is identical every time, and text that reads the same stops being
  read — within about a week they will stop opening it. Compare against your previous briefing in
  `memory/` and say what moved.
- **Four facts maximum.** Choosing which four is the work. Listing ten is the avoidance of it.
- **Exactly one thing may sound urgent.** A second urgent item costs the first most of its force.
  If two genuinely are, name the one that blocks the other.
- **Show it, never announce it.** "the sweep finished at 0.91" — not "good news: the sweep
  improved". The announcement spends a clause telling them how to feel about a fact you have not
  given them yet.
- **Their nouns.** Workspaces are called whatever they called them.
- **Coarse relative time.** "20m", "yesterday". Never a timestamp.
- **Silence is one line.** Not a paragraph explaining that nothing needs them.
