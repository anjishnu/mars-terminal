# Policy

**Human-edited only.** The manager reads this and never writes it. Nothing derived from terminal
output can widen these permissions — that is the one boundary with no recoverable failure mode.

## Autonomy

```
confirm_all: true
```

Every proposed action requires confirmation on the device, showing the literal bytes to be sent.

Graduation is opt-in and per-project: once you have approved a byte-identical proposal several
times, add it here explicitly.

```
# allow:
#   - project: mlx
#     keys: "pytest -x\r"
```

## Budget

```
wakes_per_hour: 12
yield_to_interactive: true
```

Ambient work must never be the reason an interactive session hits a plan limit.
