# Policy

**Human-edited only.** The manager reads this and never writes it. Nothing derived from terminal
output can widen these permissions — that is the one boundary with no recoverable failure mode.

Every key below is read by code, and a selfcheck fails the build if this file names one that is
not. A knob a document promises and nothing reads is worse than no knob: it tells you that you are
protected by something that does not exist.

**Absent means the safe answer.** A key you delete, misspell, or give an unparseable value takes
its default, and every default is the cautious one.

## Autonomy

```
confirm_all: true
autopilot: false
```

`confirm_all` — every proposed action requires confirmation on the device, showing the literal
bytes to be sent.

`autopilot` — whether the manager may assign an approved brief to a free worker and land a green
one without a press. Off by default. **Turning this on does not widen what a worker may do**: the
tool scope is still read from the worker's live argv, the deny-list is unchanged, and a brief still
has to carry an `approved.md` that a human wrote. What it changes is who presses, not what is
permitted.

The phone's **Self-driving** switch writes this same setting, so there is one answer to "is it on"
from either side.

## Bounds

```
max_continuations: 2
awaiting_human_max: 5
rot_days: 3
```

`max_continuations` — how many times a `partial` may be superseded before it must ask for a person.
Superseding can only narrow acceptance to the unmet criteria, so a chain cannot widen its own
scope; this is what stops it repeating forever.

`awaiting_human_max` — how many decisions may be waiting on you before autopilot stops starting new
work. Work in progress is limited here for **rot**, not for cash: an unlanded branch decays against
a moving `main`. One measured at twelve days and sixty-four commits of drift.

`rot_days` — after this long unlanded, a brief's branch is rebased and re-verified, or closed with
a note. Autopilot owns the freshness of its own output.

## Budget

```
wakes_per_hour: 12
yield_to_interactive: true
```

Ambient work must never be the reason an interactive session hits a plan limit.
