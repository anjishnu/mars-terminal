# Rover — your sessions on your phone

Long agent runs don't need you at the keyboard. They need you to *notice* when something stops.

Rover is a phone client for the Mars sessions already running on your machine. Scan a QR once and
the work you left behind is readable from a pocket — with enough control to answer a prompt, run a
command, or point a worker at a problem without going back to the desk.

There is no account and no cloud service holding your data. The QR carries a one-time link to
**your** machine, over a tunnel that closes when you stop the bridge.

- [What you need](#what-you-need)
- [Setup](#setup)
- [Linking Claude Code](#linking-claude-code)
- [Using Rover](#using-rover)
- [Rover chat](#rover-chat)
- [The manager agent](#the-manager-agent)
- [Keeping the same link](#keeping-the-same-link)
- [Security](#security)
- [Troubleshooting](#troubleshooting)
- [Reference](#reference)

---

## What you need

| | Why | Without it |
|---|---|---|
| `mars` built with the `web` feature | the bridge lives behind it | `mars pair` exits telling you to rebuild |
| **ngrok** + a free authtoken | the public tunnel your phone reaches | pairing works on your LAN only |
| **Claude Code** (`claude` on PATH) | Rover chat and the manager agent | terminals and the board still work; the agent parts stay dark |

Claude Code is optional. Rover is a useful phone terminal without it — you just don't get the
conversational agent or the written briefings.

```bash
cargo install mars-terminal --features web --locked
```

Use `--locked`. `cargo install` re-resolves dependencies and ignores the lockfile unless told not
to, so without it you may build against versions nobody tested.

---

## Setup

### 1. Preflight

```bash
mars pair --check
```

It reports what is set up and **the fix for each thing that isn't**, rather than failing later and
quietly:

```
  Rover · preflight

  bridge
  ✓ session       mars-dev
  ✓ ngrok         3.39.10
  ✓ authtoken     configured
  · stable URL    not set — the QR changes on every restart. Reserve a free domain at
                  dashboard.ngrok.com/domains, then: mars pair --domain <name>.ngrok-free.dev

  manager
  ✓ claude        2.1.225
  ✓ can run       on your subscription (API key ignored)
  · agent         off
```

`✓` is ready, `✗` is broken and carries its fix, `·` is a deliberate skip — something optional you
have not turned on. Only `✗` blocks pairing.

The line worth reading twice is **`can run`**. It runs one tiny real query, because a `claude`
binary that exists is not the same as one that can answer — see [Linking Claude
Code](#linking-claude-code).

### 2. Pair

```bash
mars pair
```

This starts the bridge, opens the tunnel, and prints a QR. Scan it with your phone's camera.

**One scan covers the whole machine.** Every session on that host appears in your fleet list and
you switch between them without re-scanning. If you add a session later it shows up under *also on
this host* — tap it to enter.

If a bridge is already running and you just want the link again:

```bash
mars pair --link
```

### 3. Install it to your home screen

Rover is a PWA. Installing it means full-screen, no browser chrome, and your paired sessions
remembered. The fleet page offers a one-tap install where the browser allows it; on iOS use
**Share → Add to Home Screen**, which is the only route Safari provides.

---

## Linking Claude Code

Two different agents use Claude Code, with deliberately different powers. Understanding which is
which is most of understanding Rover's safety model.

| | Rover chat | Manager agent |
|---|---|---|
| **You talk to it** | yes, from the phone | no — it works while you're away |
| **Reads** | the repo, panes, memos | pane output, memos, session files |
| **Can change anything alone** | **no** — `Read,Grep,Glob` only | writes files under `~/.mars` |
| **Effects reach the machine** | only when you press a card | its own notes and briefings |

### Getting `claude` working for Mars

Mars runs `claude` as a subprocess. Two requirements:

1. **Claude Code installed and logged in.** `claude --version` should answer, and a plain
   `claude -p "ok"` should return something.
2. **No stale `ANTHROPIC_API_KEY` in the environment.**

The second one bites people, so it is worth being explicit. **A key in the environment takes
precedence over your claude.ai login.** If that key is empty, expired, or has no credit, `claude`
fails outright — while your subscription is perfectly fine. The symptom is a manager that never
writes anything and a chat that returns errors, with no obvious cause.

Mars handles this rather than leaving you to discover it: every Claude Code invocation scrubs
`ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN` first, so the subscription is used. `mars pair
--check` confirms the result and says `on your subscription (API key ignored)` when a key was
present and bypassed.

If you *want* to use an API key instead, unset it from Mars's view is the wrong lever — set it in
`~/.mars/manager/run.sh`, which is yours to edit (see below).

### Turning the manager on

```bash
touch ~/.mars/manager/agent.enabled     # on
rm    ~/.mars/manager/agent.enabled     # off
```

Or flip **Manager agent** in the phone's side menu, which writes the same file.

Run one turn immediately, without waiting for the daemon's clock:

```bash
mars manager
```

### How the manager is invoked, and changing it

Everything about *how* the agent runs lives in `~/.mars/manager/run.sh` — model, effort, permission
mode, which directories are in scope. Mars only decides *when* there is work. Edit that file with
an editor instead of rebuilding.

Two things about it are load-bearing:

**It runs `--permission-mode acceptEdits`.** File edits flow with nobody at the keyboard, which is
the entire point of a background agent; anything that is not an edit still gates, and a gate with
nobody watching means it does not happen. `auto` would additionally permit classifier-gated shell,
which is a wider surface than an agent reading untrusted terminal output should have.

**It denies the agent its own instructions.** `AGENTS.md`, `prompt.md`, `policy.md`, `run.sh`,
`docs/**` and `.claude/**` are refused. The agent reads terminal output, and text on a screen that
can rewrite the standing orders would turn one bad tick into a permanent condition.

If you edit `run.sh`, Mars will refuse to run the agent until you bless the change — it compares
the file to its built-in copy, and an unrecognised version stops the tick with the exact command to
approve it. Same for the instruction docs. This is not paranoia about you; it is that the file is
inside an agent's reach, and an edit to it is arbitrary shell on the next tick.

### Choosing a model for the chat

```bash
export MARS_ROVER_MODEL=claude-sonnet-5    # default
export MARS_ROVER_EFFORT=medium            # default
```

Effort is the lever worth reaching for, not model tier — capping effort on a good model beats
swapping in a lesser one, and costs nothing in judgement. Measured to first token on one machine:
sonnet-5 at low 2.6s, at medium 3.6s, haiku-4.5 3–7s with a weaker answer.

---

## Using Rover

### Three levels

**Fleet** → the machines and sessions you have paired. **Mission control** → one session's briefing
and board. **Dive** → a single pane, live.

Swipe left, or hold, to go deeper. The breadcrumb at the bottom shows where you are; hold its right
end to open the side menu.

### The gesture grammar

This is consistent everywhere, and worth learning once:

- **Tap** — reads something, or does something reversible.
- **Hold** — commits. The control fills as it charges, so you can see it happening and abandon it
  by lifting off. Entering a session, running a command, rebooting.
- **Red hold** — destructive, and it names what it will end before you finish the gesture.
  Unlinking a session, closing one.

A stray tap in a pocket cannot start, stop, or destroy anything.

### Mission control

The **briefing** is the same situation report `mars attach` gives you, in the mission-control voice:
what broke, what finished, what is still running.

The **board** lists every workstream with a verdict and a plain-English "why" under anything that
failed. Tap a row to expand it; hold to dive into its pane. A workstream waiting on a `[y/N]` shows
**Y · yes** and **N · no** buttons — you can unblock a run from a bus stop.

**Memos** are things worth your attention that are not a workstream: a stuck deploy, a credential
about to expire. Each shows its full text. You can pin, dismiss (recoverable — see *Dismissed
memos* in the side menu), or **assign** it to a worker.

### Panes

A dived pane is a real terminal with real scrollback. Tug at the top to page further back.

- **Type** with the on-screen input; **Run** sends it.
- **Arrow pad** — toggle it for anything that needs arrow keys. The pad is movable: drag its glowing
  grab bar wherever your thumb is.
- **Pickable chips** — URLs and backticked commands the agent printed become tappable chips that
  **copy to the clipboard**. They never execute; selecting text on a phone terminal is miserable and
  this is the fix for that, not a shortcut for running things.

### Assigning a worker

An assign starts a fresh Claude Code worker in a new pane, with the memo as its entire brief. It
runs `acceptEdits`, so it can write code without you approving each edit.

Read the memo before you hold — it is on screen above the button, and it is the worker's whole
instruction. Rover shows a link's real destination when the link text differs from it, so a label
cannot disguise where something points.

Workers are refused edits to files that **execute without being run**: `build.rs`, `Makefile`,
`package.json`, `Cargo.toml`, `.github/workflows/**`, `.git/hooks/**`, `.envrc`, `.claude/**`.
Those fire on your next build, push, or commit — long after the pane is closed.

---

## Rover chat

Ask about the machine in plain language: *"why did the build fail?"*

It is a real Claude Code session, so it reads the repo and the panes and answers with what is
actually true rather than a fluent guess. It keeps its thread, so follow-ups work.

It can also **offer** to act. An offer arrives as a card you press:

| Verb | What it does | Gesture |
|---|---|---|
| **open** | opens a file — checked to exist before the card is shown | tap |
| **note** | saves a memo | tap |
| **workspace** | opens a new workspace | tap |
| **run** | runs a command in a pane you can watch | **hold** |
| **close** | ends this session | **red hold** |

The agent cannot execute any of these itself. It proposes; the parser enforces the shape and caps
the count at three; you press. A `run` card prints the command verbatim — read it before holding,
the same way you would read a stranger's shell one-liner before pasting it.

Session lifecycle — creating and renaming — is deliberately **not** in the agent's vocabulary. Those
live in the fleet page and the side menu, where they are your decision.

---

## The manager agent

While you are away it reads what the panes did, writes the mission briefing and any memos, and
scores its own runs.

- **Manager telemetry** in the side menu shows the run tally and where the time went — `ramp` is
  process start and reading, `write` is producing the documents, `wrap` is filing. A slow run
  becomes diagnosable rather than merely slow. It also names which session's daemon is hosting the
  agent right now.
- **Manager archive** keeps what it wrote, by day.
- One manager serves every session, but each phone sees only **its own session's** memos and
  briefings.

---

## Keeping the same link

By default ngrok mints a fresh random URL on every restart, so every phone has to re-scan. This is
the single most common way Rover "stops working".

Reserve a free static domain at <https://dashboard.ngrok.com/domains>, then:

```bash
mars pair --domain your-name.ngrok-free.dev
```

It is stored in `~/.mars/config.json`, so a new shell and a supervised bridge both see it. After
this the QR survives restarts and reboots.

To keep the bridge running across crashes and logouts:

```bash
mars pair --supervise
```

---

## Security

**Treat the QR like an SSH private key.**

A phone that can type into your terminal can run anything you can run. The pairing token is
therefore a credential for code execution as you, and it does not expire on its own. If a QR is
photographed over your shoulder, screenshotted into a chat, or mailed to yourself, that is the whole
key.

```bash
mars serve --reset     # rotate the token, drop every paired phone
```

Keep the bridge up only while you're using it. The token is 128 bits from `/dev/urandom`, compared
in constant time and stored `0600`, so guessing is not a realistic attack — losing it is.

[`SECURITY.md`](./SECURITY.md) sets out the full boundary, including how text an agent reads is
treated as untrusted input.

---

## Troubleshooting

**"connection not established" where a spinner used to be.** The socket is not live. Rover now says
so instead of animating a promise it cannot keep. Usually the bridge is down (`mars pair --check`)
or the tunnel URL changed (see [Keeping the same link](#keeping-the-same-link)).

**The phone shows a session name I don't recognise, or one that isn't in `mars ls`.** Older builds
reported a session's *birth* name — the id of its directory — which stops being its name after a
rename. Update and re-pair; the link now carries the live name.

**The manager never writes anything.** Check `mars pair --check`. The usual cause is an
`ANTHROPIC_API_KEY` in the environment with no credit, which takes precedence over your
subscription. The second most common cause is that the agent is simply off (`agent · off`).

**"refusing to run the agent — run.sh differs from the built-in".** Something edited the runner. If
it was you, the message prints the exact command to bless it. If it was not you, delete the file and
it re-materialises clean.

**A rebooted session came back as a bare shell.** Expected when Mars could not identify which
conversation that pane held. It restores the ones it can name exactly, allows one directory-based
guess per directory, and leaves the rest empty rather than resuming somebody else's conversation
under your prompt. `mars reboot` prints this breakdown before it acts.

**Memos stopped appearing.** Memo filenames must be plain (`[A-Za-z0-9._-]`). A file named anything
else is not loaded — deliberately, because a memo's name reaches a shell when you assign it.

---

## Reference

### Commands

```bash
mars pair                      # QR + bridge + link
mars pair --check              # preflight, with fixes
mars pair --link               # reprint the link
mars pair --domain <d>         # pin the tunnel URL
mars pair --supervise          # hand the bridge to launchd
mars serve --reset             # rotate the token
mars manager                   # run one manager turn now
mars snapshot                  # board + briefing, no model
mars reboot [name]             # restart onto the installed binary
```

### Files

| Path | What |
|---|---|
| `~/.mars/serve.token` | the pairing token (`0600`) |
| `~/.mars/config.json` | `ngrok_domain`, and other host config |
| `~/.mars/manager/run.sh` | how the manager agent is invoked — yours to edit |
| `~/.mars/manager/agent.enabled` | the manager's off switch |
| `~/.mars/manager/memory/` | what the manager has learned |
| `~/.mars/sessions/<id>/` | per-session artifacts: memos, briefing, restore manifest |

### Environment

| Variable | Effect |
|---|---|
| `MARS_NGROK_DOMAIN` | overrides the configured stable domain |
| `MARS_ROVER_MODEL` | model for Rover chat (default `claude-sonnet-5`) |
| `MARS_ROVER_EFFORT` | effort for Rover chat (default `medium`) |
