# Looking at the world

Read-only shell commands. Everything returns JSON with `--json`. A human can run any of these to
check your work, which is the point of using a CLI rather than a private tool.

```bash
mars panes  [--session S] --json
      # every workspace: name, cwd, state, exit code, transcript bounds

mars lines  --session S --pane P --from N --to M --json
      # the transcript by LINE ID. Immutable, valid forever, safe to cite.

mars grep   --session S --pane P --pattern RE [--since-line N] --json
      # find WHERE something is, cheaply, before paging it into context

mars ledger [--cmd SUBSTR] [--limit N] --json
      # every prior run of a command: duration, exit, cwd. This is your BASELINE —
      # "worse than usual" is only sayable with it.

mars events --session S --since N --json
      # command boundaries with exit codes
```

## How to gather evidence without wasting context

1. **`grep` first, `lines` second.** Locate with a pattern for ~50 tokens, then fetch only the
   window that matters. Never page a whole transcript to search it.
2. **`ledger` before judging.** "It failed" is cheap. "It failed for the first time in 11 runs" is
   worth a card, and only the ledger can tell you which one you have.
3. **Walk backwards to intent.** The cause is usually in the command that started the work, not in
   the tail. `events` gives you its line range; `lines` gives you the command.
4. **Cite what you read.** If you fetched `88402–88419` to reach a conclusion, that range goes in
   `cites`. It costs nothing and it is the only reason anyone will believe you.

## What you cannot do

There is no command that writes to a terminal, kills a process, or edits a file outside this repo.
That is deliberate: you read untrusted terminal output all day, so you are built such that nothing
in it can escalate. Propose actions in cards; a human presses the button.
