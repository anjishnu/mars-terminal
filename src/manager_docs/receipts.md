<!-- mars-doc-version: 3 -->
# Run receipts

At the end of every run, write `~/.mars/manager/runs/<batch-filename>` — your own account of
what you just did. Mars checks that account against the filesystem.

```json
{
  "batch": "batch-2026-07-30T05-10-00Z.json",
  "wrote": [
    "/Users/me/.mars/sessions/0/workspaces/4.md",
    "/Users/me/.mars/sessions/0/memos/auth-test-flaky.md",
    "/Users/me/.mars/sessions/0/mission_briefing.md"
  ],
  "skipped": [
    {"session": "1", "why": "three snapshots, all identical — nothing moved"}
  ],
  "cursor": { "0": "2026-07-30T05-09-00Z.json", "1": "2026-07-30T05-04-00Z.json" }
}
```

## Why a receipt instead of a required file list

The obvious design is a checklist: name the files the run must produce, then assert they exist.
It fails in a specific and damaging way — **a rule that says "write a file per workspace"
reliably produces a file per workspace, including for the workspaces that did not change.** It
manufactures exactly the padding the briefing is supposed to be free of, and then scores it as
success. Checking that files exist measures compliance, not work.

So the deal is inverted. You decide what deserves writing; you state what you decided; Mars
verifies the statement. Three things are checked, and only these:

1. **Every file you say you wrote exists and post-dates the batch.** This catches the failure that
   matters most — a run that reports success and wrote nothing, which from the outside is
   indistinguishable from a quiet board.
**The filename is the batch's own, unchanged.** The batch is `batch-2026-07-30T05-10-00Z.json`,
so the receipt is `runs/batch-2026-07-30T05-10-00Z.json` — do not append a second `.json`. Twelve
receipts on this machine were written as `….json.json`, and every one of them was scored as a run
that did nothing, because the reader could not find it.

**`skipped[].session` and `cursor` both use the session's `id`, not its `name`.** The batch gives
you both — `{"id": "1", "name": "replyguy", …}` — and the id is the one that means something: a
name is a label the engineer can change between runs. A receipt keyed by name is still accepted, so
an old one does not suddenly fail, but write the id.

2. **Every session in the batch is accounted for** — written about, or in `skipped` with a
   non-empty reason. Deciding there is nothing to say is a correct outcome and is recorded as
   one. Silence is the only thing that counts as a fault.
3. **The cursor did not advance past what the batch offered.** Advancing further would silently
   mark snapshots as read that nobody read.

A run with three skips and one file is a clean run. A run with six files and no receipt is a
fault. The point is not volume; it is that your account of the work matches the work.

## Rules

- Write the receipt **last**, after every file it names is on disk. It is a report, not a plan.
- Use absolute paths, exactly as written.
- Do not list a file you did not actually change this run. An unchanged file left alone is good
  practice, but claiming it is a false statement and will be scored as one.
- `cursor` mirrors what you wrote into `memory/cursor.json`. If they disagree, one of them is
  wrong and the run is suspect.
