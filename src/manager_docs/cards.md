# Cards

A card is one judgement, as a markdown file with YAML frontmatter, in
`sessions/<session>/cards/card-<something-unique>.md`.

**Structure in the frontmatter, meaning in the body.** If your frontmatter fails to parse the body
still renders as a document — so a mistake degrades to prose rather than to nothing. Never put
meaning only in the frontmatter.

```markdown
---
id: card-0-4-blocked-1785372000
v: 1
created: 2026-07-30T00:40:00Z
created_ts: 1785372000
source: manager
actor: manager@laptop
severity: block
origin: laptop
session: "0"
pane: "4"
kind: blocked
headline: "Claude wants to edit src/session.rs"
expired: false
cites:
  - {session: "0", pane: "4", from: 12004, to: 12031}
actions:
  - {id: allow, label: "Allow", keys: "y\r"}
  - {id: deny,  label: "Deny",  keys: "n\r"}
supersedes: [card-0-4-blocked-1785300000]
---
Rewriting the reconnect loop to retry with backoff. It has already changed `terminal.rs` (+40/−6)
this turn.
```

## Fields

| Field | Required | Notes |
|---|---|---|
| `id` | yes | unique; include session and pane so it is legible |
| `v` | yes | `1` |
| `created`, `created_ts` | yes | ISO and epoch seconds |
| `source` | yes | `manager` for you; `reflex` means a deterministic pass wrote it |
| `severity` | yes | `block` \| `warn` \| `info` \| `ok` |
| `session`, `pane` | yes | which workspace it is about |
| `kind` | yes | short slug; **`(pane, kind)` is a card's identity** |
| `headline` | yes | ≤72 chars, no trailing period |
| `cites` | whenever you claim a fact | line ranges you actually read |
| `actions` | when there is an obvious next step | `keys` are literal bytes |
| `expired` | yes | start `false` |
| `supersedes` | when correcting | ids you are replacing |

## Identity, and why duplicates are the worst bug

`(pane, kind)` identifies a card. If an open card already exists for the same pane and kind,
**do not write another** — update your belief instead, or supersede. A workspace blocked for six
hours must produce one card, not one per turn. Two cards about one thing is the failure that makes
an engineer stop reading the feed.

## Severity

- `block` — a human is the bottleneck; nothing proceeds until they act
- `warn` — something went wrong and they will want to know
- `info` — useful, not urgent
- `ok` — a resolution worth confirming

Use `block` sparingly. It is the one that interrupts someone.

## Actions

`keys` are sent verbatim into the pane after the engineer confirms on their device, showing the
literal bytes. `\r` is Enter. Never propose something destructive without saying so in the body.
