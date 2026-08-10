<!-- mars-doc-version: 15 -->
# Workspace notes

**Read second**, after the briefing, when a row is tapped. One file per workspace at
`workspaces/<pane-id>.md`.

Read **only that pane's `output`** — its tail, its delta, its signals. Not the other panes.

The briefing answers *does anything need me?* This answers the next question down: **what is this
pane for, where has it got to, and can I do anything useful about it?**

## Shape

```markdown
---
source: agent
---
**Problem** — getting the manager to write briefings from real terminal output.

**State** — `cargo test` failing: **3 of 140**, all in `session.rs`, the same assertion each run.

**Do**
- **Re-run that test alone** — is it deterministic?
- Check whether the migration in workspace 2 touched the schema
```

## Make the `Do` block tappable

Whatever you put under `Do`, repeat as frontmatter so the phone can offer it as a button rather
than as a sentence to read and retype. Same shape memos already use:

```markdown
---
source: agent
suggested_name: schema-migration
actions:
  - {id: rerun, label: "Re-run that test alone", keys: "cargo test session -- --nocapture\r"}
  - {id: schema, label: "Check the migration in workspace 2"}
---
```

`suggested_name` is a name this workspace has earned, offered when the one it has says nothing
about the work. kebab-case, a few words, no quotes needed. **Omit it when the current name is
already honest** — that is the common case, and a suggestion on every workspace is one nobody
reads. Write it fresh each run from what the workspace is doing now; a suggestion matching the
current name is dropped before the phone sees it, so there is no way to nag by accident and no
bookkeeping for you to keep.

`keys` is what gets typed into that pane, ending in `\r`. **Omit `keys` entirely** for anything
that is a suggestion rather than a command — a thought the captain should have is still worth
offering, and it becomes a tappable note rather than an execution.

The best `keys` action is the exact command that UNBLOCKS the pane — the rerun, the missing
install, the resume, the retry with the flag the error asked for. Offer it at the moment it is
the obvious next step, phrased so the label alone says what will happen. On the phone these run
under a deliberate long-press, and the record of which get taken is what earns their automation
later — a suggestion taken every time is a suggestion that should eventually run itself.

Rules, and they matter more than the format:

- **Never more than three, and prefer one.** Three plausible actions is a way of not choosing.
- **Only what is genuinely open to them right now.** "Wait for it to finish" is not an action, and
  neither is anything they cannot do from a phone.
- **Nothing destructive without it being obvious in the label.** `keys` goes to a live terminal.
- **Omit the block entirely when there is nothing to do.** An empty suggestion costs more than
  silence: it teaches people the buttons are noise.

Every tap and every dismissal is recorded. Suggestions that get waved away consistently are the
signal that this block is being written badly — treat that as feedback about your judgement, not
about the reader.

Use markdown, and use it for meaning rather than decoration:

- **Bold the three field labels** so the eye lands on the structure before the prose.
- **Backtick commands, filenames and identifiers** — `cargo test`, `session.rs`. They are the
  things being scanned for, and a monospace run is far easier to pick out of a sentence.
- **Bold the one number that matters** in `State`, and nothing else. Two bold numbers is none.
- **Bold the recommended action** under `Do`, never more than one.
- No headings (`#`) — this is a note inside a row, not a document. No tables, no blockquotes.

**Seventy words, hard.** Three fields, and the third is often absent. If it runs longer, the
`State` line is carrying narrative it should not — cut back to the result, not the road to it.

| Field | Answers |
|---|---|
| `Problem:` | What is this workspace *for* — the broader thing being solved here. |
| `State:` | Where it has actually got to, with the number or result that makes it real. |
| `Do` | Anything useful they can do. Zero to three. Omit the whole block when none. |

## Never quote them back at themselves

`Problem` is **not the last command**, and it is **never the engineer's own words**. Nor is
`State` a retelling of the conversation: "moved from presenting four options to a five-point
test" is the dialogue, not the work. Say what exists now that did not before. A note that
opens by repeating the request they typed four minutes ago tells them nothing they did not type.
The same goes for recapping their session — "you moved on to X, then circled back to Y" is a diary
of something they lived through.

Ground `Problem` in [`memory/projects.md`](../memory/projects.md). That file exists so this line
can say *why this pane matters* instead of merely describing it. If the project is genuinely
unknown, say so — an honest "not sure what this belongs to" beats an invented purpose.

## The fields are fixed; the evidence is not

The shape never changes. What fills it depends on what kind of pane it is.

| Pane | `Problem` | `State` | `Intervention` |
|---|---|---|---|
| shell, build, test | what is being built or tested | the command and its result — counts, exit code | re-run, fix, ignore |
| agent session | the task the agent was set | working / blocked / finished, how long, what it changed | answer its question, redirect it, leave it |
| editor | which file, and what it belongs to | unsaved? | usually none — write nothing |

**For an agent pane, never summarise the conversation.** The text on screen is a dialogue the
engineer is half of; retelling it is worthless. Report what the agent has *produced* — files
touched, commands run, results — or what it is *waiting on*. A question it has asked is the single
most valuable thing you can surface.

## Interventions

Only actions genuinely open to them. **"Wait for it to finish" is not an action.** A status fact
about another thread is not an action. When there is nothing to do, **omit the `Do` block entirely** — do not write
"none needed". An absent line is honest and costs them nothing to read; a line saying there is
nothing to say costs them a line.

With more than one, bold the one you would take, and keep them genuinely different: a retry, an
investigation and a decision to move on are three options; three flavours of "look into it" are
one.

## Skip the pane they are looking at

A workspace with **`"focused": true`** in the snapshot gets **no note at all**. That is the pane on
their screen right now; a note about it describes back to them what they are already reading.

The one exception is `blocked`. A pane waiting on an answer is worth naming even when focused,
because it is the thing stopping everything else.

## Rules

- **Never restate the row.** Name, state and age are already on screen above this.
- **Skip a workspace whose TAIL is unchanged since the previous snapshot** — not one whose delta is
  empty. `delta` only fills from lines that scroll off, so a full-screen program that repaints in
  place has an empty delta forever no matter how much it does.
- **Formatting is allowed here** — bullets and one bold item — unlike the mission briefing, which
  is plain prose. This is read deliberately, not at a glance.
