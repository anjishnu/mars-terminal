<!-- mars-doc-version: 3 -->
# Memos

A memo is **something worth keeping track of that is not what a workspace is doing right now.**

That distinction is the whole point. The board already shows what every workspace is doing; a row
that says `blocked` needs no memo repeating it. A memo is for the thing that has fallen out of
focus — the failure everyone stopped looking at, the decision deferred twice, the workspace that
has been "nearly done" since this morning. It is what the engineer would have forgotten.

One memo is one markdown file with YAML frontmatter, in
`~/.mars/sessions/<session-id>/memos/<title>.md`.

**Structure in the frontmatter, meaning in the body.** If the frontmatter fails to parse, the body
still renders as a document — a mistake degrades to prose rather than to nothing. Never put
meaning only in the frontmatter.

```markdown
---
title: auth-test-flaky
v: 1
created: 2026-07-30T00:40:00Z
created_ts: 1785372000
priority: 70
severity: warn
session: "0"
pane: "4"
headline: "The auth test has failed three runs running"
expired: false
cites:
  - {snapshot: "2026-07-30T00-38-00Z.json", session: "0", pane: "4"}
actions:
  - {id: rerun, label: "Re-run", keys: "cargo test auth\r"}
---
It failed at 00:12, 00:26 and 00:38, each time on the same assertion. Nothing else in the run
changed between them, so this is unlikely to be flakiness.
```

## Fields

| Field | Required | Notes |
|---|---|---|
| `title` | yes | kebab-case, stable, meaningful. **`(session, title)` is a memo's identity.** |
| `priority` | yes | `0`–`100`. Ordering is by priority descending, then most recent. |
| `severity` | yes | `block` \| `warn` \| `info` \| `ok` |
| `headline` | yes | ≤72 chars, no trailing period. What a person reads first. |
| `v`, `created`, `created_ts` | yes | `1`, and the ISO + epoch timestamps |
| `session`, `pane` | yes | `pane` may be empty if the memo is about the session as a whole |
| `cites` | whenever you claim a fact | the snapshots you actually read |
| `actions` | when there is an obvious next step | `keys` are literal bytes a human confirms |
| `expired` | yes | start `false` |
| `supersedes` | when correcting | titles you are replacing |

## What a memo has to say

Three or four lines, in this shape, and nothing else:

1. **What is wrong**, concretely, with the number or name that makes it real. Not "the build is
   unhappy" — "the auth test has failed the last three runs, each on the same assertion".
2. **Why it matters**, if that is not already obvious from the first line. One clause. Skip it
   when it would only be padding.
3. **`Next:` the proposed move.** Always end with this. A memo that describes a problem and stops
   hands the engineer the work of deciding what to do, which is the work they came to you to
   avoid. If you genuinely do not know the next move, say what you would look at first.

```markdown
The auth test failed at 00:12, 00:26 and 00:38, each time on the same assertion. Nothing else in
the run changed between them, so this is unlikely to be flakiness.

Next: re-run it alone — if it fails a fourth time on that assertion, the migration in workspace 2
is the only thing that touched the schema today.
```

Do not write a memo that says only "X is failing". The row already says that. If you have nothing
to add beyond the board, write no memo — that is a success, not a gap.

## Titles carry the identity

Pick a title that names the *thing*, not the moment: `auth-test-flaky`, `migration-blocks-deploy`,
`sweep-3-stalled`. Never `memo-1`, never a timestamp.

This matters because **a memo often outlives the pane that prompted it** — which is precisely why
it is a memo and not a workspace row. Identifying it by title rather than by pane means you can
recognise on the next run that you already wrote this one, and update it instead of writing a
second. Two memos about one thing is the failure that makes an engineer stop reading.

Before writing, list `memos/`. If a memo with that title exists and is not expired, **update it**
— rewrite the body, raise or lower `priority` — rather than adding another.

## Priority

Priority is about *what should be read first*, which is not the same as severity. A `block` the
engineer already knows about ranks below a `warn` they have never seen.

| Range | Means |
|---|---|
| 80–100 | They should act on this now; it is costing them something every minute |
| 50–79 | They should know before they next pick the machine up |
| 20–49 | Worth their attention this session |
| 0–19 | Background; true, mildly useful, safe to never read |

Be honest about the low end. A board where everything is 90 sorts identically to one where
everything is 10, and it teaches them to ignore the ordering.

## Expiry

Set `expired: true` when the condition stops holding. A memo that has been resolved and left open
is worse than one never written — it is a false claim about the present.
