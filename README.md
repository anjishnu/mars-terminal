# MARS

*Mission control for your terminal* — a non-modal, Emacs-compatible terminal editor
with a Claude-Code-style mission-control command bar, a built-in LLM agent, real terminal panes, and
tmux/zellij-style persistent sessions. One tool, one set of keys.

```
███╗   ███╗ █████╗ ██████╗ ███████╗
████╗ ████║██╔══██╗██╔══██╗██╔════╝
██╔████╔██║███████║██████╔╝███████╗
██║╚██╔╝██║██╔══██║██╔══██╗╚════██║
██║ ╚═╝ ██║██║  ██║██║  ██║███████║
╚═╝     ╚═╝╚═╝  ╚═╝╚═╝  ╚═╝╚══════╝
```

## Build & install

Linux/macOS:

```bash
source ~/.cargo/env            # if cargo isn't on your PATH
cargo build --release
# put it on your PATH:
ln -s "$PWD/target/release/mars" ~/.local/bin/mars   # or copy it anywhere
mars --selfcheck               # optional: run the built-in test suite
```

Windows PowerShell (Rust's MSVC toolchain and Visual Studio Build Tools required):

```powershell
cargo build --release
.\target\release\mars.exe --selfcheck
```

## Quick start

```bash
mars                    # start a session: the MARS banner, then a shell in your cwd
mars notes.md           # edit a file (also inside a session)
mars -s notes.md        # standalone mode: no session daemon, just edit
mars help               # full CLI reference
```

Sessions are the default, tmux-style: a bare `mars` gets an auto-numbered session
that survives closing the window. Press any key to dismiss the startup banner.

Inside the editor, four keys carry you everywhere:

| Key | What it does |
|---|---|
| `Ctrl+Space` | **mission control** — a command launcher next to a needs-you-first **workspace board** (press `←` to reach it); type to filter commands, Enter to run. Works in terminal panes too |
| `!` (in mission control) | run a shell command in a terminal pane |
| `?` (in mission control) | ask the built-in agent anything ("how do I split the screen?") |
| `C-x C-f` | **Navigator** — browse & jump to any project file (type to fuzzy-filter) |
| `C-t` | space warp: tabs, panes, splits — with an on-screen cheat panel |

`C-g` cancels anything. Every menu row shows its real keybinding, so the fast path
teaches itself as you go.

**The workspace board** ranks every workstream so "anything need me?" is answerable
at a glance: blocked ⏸ and failed ✗ sort to the top, then running, done ✓, idle —
each with one status **bubble** (● colored by state) and a plain-English status line.
Select a row and press `s` for an on-demand, one-line "what is this doing?" summary
(a single low-tier model call, guarded so it never over-fires). Any Markdown buffer
can also toggle into a read-only **reading-mode** — real wrapping, tables, and nested
lists, dressed in the Mars palette, scrolled with the editor's own motion keys
(↑/↓, `⌥↑`/`⌥↓`, `M-<`/`M->`); toggle again to edit.

## Moving around fast

Fast motion is bound to **`⌥` (Option)** and to `C-x` jumps that work on every
terminal (enable "Use Option as Meta" if your terminal offers it).

| Do this | Keys | Also |
|---|---|---|
| Jump by code token (`foo·.·bar·(·baz·)`) | `⌥←` / `⌥→` | `M-b` / `M-f` (word) |
| Page up / down | `⌥↑` / `⌥↓` | `PageUp` / `PageDown` |
| Extend selection while jumping | add `Shift` (`⌥⇧→`) | `Shift`+`PageUp/Down` |
| Next/prev blank-line block | `C-x ]` / `C-x [` | |
| Next/prev definition (`fn`/`def`/`class`…) | `C-x }` / `C-x {` | |
| Matching bracket `()[]{}` | `C-x m` | |

**Search doubles as teleport.** `C-s`, type a word you can see — it jumps as you
type, with a `3/12` counter. Press `Tab` and every visible match gets a one-letter
label; press a label to teleport straight there. And you don't have to press Enter:
just start editing or hit a motion key — the search commits at the current match and
your key applies. `C-s`/`C-r` cycle matches, `C-g` cancels (restoring where you
started).

## Sessions — replace tmux/zellij

Sessions keep your buffers, panes, and **running shells** alive when the window
closes, the SSH connection drops, or you just walk away.

```bash
mars new work           # start (or re-attach) a session named "work"
mars ls                 # what's running, and whether anything is attached
mars attach             # reattach the most recent session
mars attach work        # reattach a specific one
mars rename work api    # rename a running session (live — nothing restarts)
mars kill work          # end a session from outside (autosaves first)
```

The daily rhythm:

1. **Start**: `mars` or `mars new work` — everything from here on lives in the daemon.
2. **Detach** when you want the terminal back: press `C-t` then `D` — or just close
   the window. Both leave shells running and buffers intact.
3. **Come back**: `mars attach` (or `mars attach work`). Your layout, buffers, and
   that build you left running in a terminal pane are exactly where you left them.
   If anything happened while you were gone, the **Mission Briefing** boots up like
   a console coming online — a mission clock, a plain-English situation report in
   the mission-control voice that types itself in ("The trainer went down at epoch
   3 — CUDA OOM, needs a smaller batch. The build came home green."), then a
   systems board of every workstream (failures first, then blocked ⏸, done ✓,
   running) with a "why" line under anything that failed and a teal ★ on a long run
   that finished clean. Any key resumes exactly where you left off. Each briefing
   is remembered, so the next return reports progress against it. Nothing happened
   → no briefing. (`mission_briefing = 1` swaps it for a one-line notice; `C-x g`
   still opens the full **Away Digest** timeline anytime.)
4. **Finish for real**: quitting (`C-x C-c`) just detaches — a session only ends
   when you *kill* it: **Kill session** in the command bar (confirm-gated),
   `mars kill work` from outside, or `mars killall` to sweep every session and
   start fresh.

`mars ls` tells you the state at a glance:

```
SESSION              STATUS
work                 detached — reattach: mars attach work
review               attached
```

Safety nets, on by default: modified files autosave every 30s and on every
detach/disconnect (scratch buffers are never touched), and each daemon logs to
`~/.local/state/mars/<name>.log` — if a session ever dies, the postmortem is there.

Notes: one client per session — attaching from a second window takes over from the
first (it gets a clean "another client attached" message). Attaching from a
different-sized terminal just reflows.

## The agent

> **Out of beta as of 0.7.0.** The AI features — the `?` ask flow, agent-proposed
> `RUN:`/`TYPE:` directives, refactors, triage, watch summaries, and the away digest —
> have been through enough releases to be treated as part of the product rather than an
> experiment. What has not changed is the posture: the agent is an assistant, not an
> authority, so review what it proposes before running it. Destructive actions are gated,
> and you should still read them. **Rover** (phone) and the **Windows** port remain in
> beta.

Works out of the box with a free-tier key from any of:

```bash
export ANTHROPIC_API_KEY=...   # Claude — defaults to claude-haiku-4-5
export OPENAI_API_KEY=...       # OpenAI — defaults to gpt-4o-mini
export GROQ_API_KEY=...         # Groq (free tier) — defaults to qwen/qwen3-32b
export GEMINI_API_KEY=...       # Google AI Studio (free tier) — gemini-3.1-flash-lite
# or any OpenAI-compatible endpoint (e.g. local Ollama):
export MARS_LLM_KEY=... MARS_LLM_URL=http://localhost:11434/v1 MARS_LLM_MODEL=llama3
# override the model for any provider:
export MARS_LLM_MODEL=qwen/qwen3-32b
```

Detection is **paid-first**: if several keys are set, Claude → OpenAI → Groq → Gemini
(an explicit `MARS_LLM_KEY` always wins). Cheap defaults per provider — reach for a
bigger model with `MARS_LLM_MODEL`, not by default.

**Calibrating cost/latency (debug mode).** To see where tokens and time actually go —
so you can right-size the model per task — run with `--llm-debug` (or `MARS_LLM_DEBUG=1`)
to log every call, then profile it:

```bash
mars --llm-debug              # logs prompts, models, tokens, latency to ~/.mars/logs/
mars llm-stats                # per task×model, ranked by total tokens:
#   TASK       MODEL            N  AVG_IN  AVG_OUT  TOT_TOK  %TOK  AVG_MS  ERR
#   watch      qwen/qwen3-32b   2    6050      195    12490   62%    3025    0   ← heaviest
#   ask        qwen/qwen3-32b   2    1720      215     3870   19%     900    0
mars llm-stats --raw          # full inputs/outputs per call
mars llm-stats --daily        # day-by-day token trend (a bar per day)
mars llm-stats --json         # machine-readable (rows + a daily series)
mars llm-stats --since 7d     # only the last 7 days (also 12h, 30m)
```

To keep it on without exporting the env var every time, set it in the global MARS
config, `~/.mars/config.json` (alongside the rest of Mars's state) — like a shell rc:

```json
{ "env": { "MARS_LLM_DEBUG": "1" } }
```

Its `env` entries are exported at startup (the real environment still wins), so the
session daemon inherits them too.

Reasoning models (Qwen3, DeepSeek-R1) work — their `<think>` blocks are stripped from
answers automatically.

Then `?` in mission control, or from the shell:

```bash
mars ask "how do I move a pane to the other side?"
```

### The agent works on every box — your key never leaves home

> **Beta.** The SSH features (`mars ssh`, `mars keyd`, the fleet view, and the
> remote installer) are still being hardened. The AI features they carry left beta in
> 0.7.0 (see [The agent](#the-agent)); this note is about the remote path itself. The
> core editor, multiplexer, and sessions are stable; the remote/tunnel path may have
> rough edges — please report anything you hit.
> Native Windows can be the home host when the remote is Unix. It uses stock OpenSSH
> without `ControlMaster`: a short bootstrap connection checks/installs Mars, then
> one foreground connection owns the session and tunnel. Password/2FA users may
> therefore authenticate twice. Windows as the remote host is pending.

You set your key **once**, on your own machine, and the agent works on every host you
SSH into — without the key ever landing on a remote box (not in its env, not in its
shell history, not on its disk).

```bash
mars ssh gpubox           # ssh in — forwards the auth socket AND auto-starts the
                          # key broker if needed (inheriting this shell's API key).
                          # `mars` on gpubox → the agent just works. No key on the box.
```

(The broker — `mars keyd` — starts on demand the first time you `mars ssh`; run it
explicitly only if you want it in a specific shell.)

On a Windows home host, the bootstrap and interactive connections authenticate
independently. The attached remote session receives the current tunnel route during
its attach handshake, so detach and reattach preserve both the workspace and broker
access.

**Installing mars on a fresh host.** Mars needs a modern Rust toolchain (≥ 1.85) — a
distro-packaged `cargo` (e.g. Ubuntu's 1.75) is too old and fails with a cryptic
`edition2024` error. Don't `apt install cargo`; install rustup (the official way,
from [rust-lang.org/tools/install](https://www.rust-lang.org/tools/install)), then
install mars from crates.io:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # Rust toolchain
. "$HOME/.cargo/env"
cargo install mars-terminal                                       # the `mars` binary
```

(`install.sh` in this repo automates exactly those steps, including detecting a
too-old distro cargo. `mars ssh` stages this script and runs it when Mars is missing,
or when a Windows-home handoff needs a newer protocol.) Your API key never lands on
the box — it's served from home over the tunnel.

The remote never makes the LLM call itself — it proxies the request home through the
SSH tunnel, and the completion comes back. Compromise the box and there's nothing to
steal; close your laptop and remote access ends with the tunnel. Jump hosts,
`ProxyCommand`, and hardware keys all work (it wraps your real `ssh`). `mars ls` shows
the hosts you've been on, newest first — type a number or a name to hop back.

The agent **sees your screen** — editor buffers, terminal output, your layout — so
"why did this build fail?" needs no copy-paste. It holds a conversation (`C-l`
starts a fresh one), and it can *act*: `RUN:` fires an editor action, `TYPE:` types
a shell command into your terminal pane, `OPEN:` jumps to a `path:line` from a stack
trace — always shown first, always one explicit Enter away, never automatic.

What that unlocks:

- **Ask about a selection.** Select code (`Shift`+arrows or `C-x h`), then `?` —
  the exact selection goes along as context, not just the visible screen.
- **Reversible refactors.** Ask "simplify this" / "add error handling" on a
  selection: the panel shows `▶ Enter to replace the selection (N lines)`. Enter
  applies it as **one undo step**, so a single `C-/` reverts the whole AI edit.
- **Triage.** `C-x ?` (or "why did this fail?" in the bar) grounds the agent in the
  focused terminal's output; `C-x e` explains what's at the cursor.
- **Watch a pane.** Kick off a long command, then `C-t w`. When it exits or goes
  quiet (~20s), Mars leaves a one-line verdict at the bottom, failures first
  (`✗ failed: linker error · build`). `Esc` dismisses. This fires **even while
  detached** — the daemon keeps watching, so `mars attach` later lands on the verdict.
- **Ask beyond the visible screen.** Questions like "when did this first start
  failing?" or "does the error in the api tab match this code?" let the model
  request more context (the pane's full scrollback, or another tab); Mars supplies
  it and re-asks once, silently. You do nothing special.
- **Shell translation.** In a terminal pane, `Ctrl+Space` then plain English
  ("find big files here") → the agent translates it to a shell command and shows it
  for you to confirm. Typed a real Mars command instead? It's recognized and run
  directly. (`!` still forces shell, `?` asks, `@` opens Navigator.)

With an agent connected, tabs you haven't named get a quiet auto-generated label
from their content (rename one yourself and it's yours forever; `auto_name_secs = 0`
turns it off).

## Rover — your sessions on your phone

> **Beta.** Rover is new in 0.7.0. The bridge, the tunnel and the phone client are the
> least-travelled paths in Mars, and the failure mode that matters — a laptop that closed
> while you were out — is the hardest to test. Expect rough edges there and report them.

Long agent runs don't need you at the keyboard; they need you to *notice* when
something stops. Rover is a phone client for the sessions already running on your
machine: scan a QR once, and the work you left behind is readable from a pocket —
with the ability to answer a prompt, run a command, or point a worker at a problem
without going back to the desk.

```bash
mars pair               # print the QR, start the bridge, print the link
mars pair --check       # what's set up and what isn't, with the fix for each
mars pair --link        # reprint the link for an already-running bridge
mars serve --reset      # rotate the pairing token (drops every paired phone)
```

Scan the code and the phone is linked. There is no account, no cloud service holding
your data, and nothing to sign up for: the QR carries a one-time link to *your*
machine, and the phone talks to your daemon over a tunnel that closes when you stop
the bridge.

**What you get on the phone:**

- **Mission Briefing** — the same situation report `mars attach` shows, in the
  mission-control voice, typed out as it loads. What broke, what finished, what is
  still running.
- **The board** — every workstream with a verdict and a plain-English "why" under
  anything that failed. Tap a card to see the pane behind it.
- **Live panes** — a real view of any terminal, scrollback included. Answer a `[y/N]`
  with a button, type a line, or use the on-screen arrow pad for a TUI. URLs and
  commands the agent printed become tappable chips that copy cleanly, because
  selecting text on a phone terminal is miserable.
- **Memos** — things the manager thinks are worth your attention that aren't a
  workspace: a stuck deploy, a credential about to expire. Assign one to a worker
  and it starts on it.
- **Rover chat** — ask about the machine in plain language ("why did the build
  fail?"). It reads the repo and the panes and answers with what's actually true,
  and can *offer* to act: open a file, start a workspace, run a command, write a
  note. Each offer is a card you press. It never acts on its own.

One pairing covers the whole host: every session on that machine shows up in the
fleet list, and you switch between them without re-scanning.

**Naming what you are looking at.** A workspace called `terminal 3` that has spent the
morning on a migration tells you nothing from a phone, so three things can rename one:
the sidebar (a rename row for the workspace you are in, beside the one for the session),
Rover chat (it can *offer* a name — you press), and the manager, which after each run
suggests a name for any workspace whose current one says nothing about the work. A
suggestion appears on the workspace's own pane as one press. Take it or wave it away and
it stops; the manager only speaks again when it has a *different* name to propose, which
is what "the work has moved on" looks like from the outside. Nothing renames anything on
its own.

**When the link is down, it says why.** The three ways a phone loses its machine used to
look identical — an empty screen. They are now told apart and named: a host that never
answered (asleep, or its tunnel is gone), a token that was refused (re-scan the QR), and
a link that keeps dropping (your network). On the host side, `mars pair --check` and
`mars pair --link` probe the public URL from outside before handing you a QR, because
ngrok's local API will cheerfully report a healthy tunnel whose edge session is dead —
which is exactly the state that looks fine from the desk and unreachable from the road.

**What Rover deliberately will not do.** The agent behind Rover chat is read-only —
it looks, and every effect is a proposal you approve on the device. Anything that
changes the machine takes a deliberate press, not a tap, and anything destructive
takes a red one that names what it will end. Commands land in a pane you can watch,
not somewhere invisible.

**Treat the QR like a private key.** A phone that can type into your terminal can run
anything you can run, so the pairing token is a credential for code execution as you,
and it does not expire on its own. Rotate it with `mars serve --reset` after any
exposure, and keep the bridge up only while you're using it.
[`SECURITY.md`](./SECURITY.md) sets out the whole boundary — what a token holder can
do, what someone without one can't, and how text an agent reads is treated as
untrusted input.

Rover needs the `web` feature (`cargo install mars-terminal --features web --locked`)
and a tunnel binary on PATH for access from outside your LAN; `mars pair` walks you
through the setup and tells you which piece is missing rather than failing quietly.

**[`ROVER.md`](./ROVER.md) is the full manual** — setup, how to link Claude Code (and the
one environment variable that silently breaks it), the gesture grammar, what the agent may
and may not do, and troubleshooting.

## Keys you already know

Mars speaks three dialects at once — whichever your fingers know:

- **Navigator (browse files)**: `C-x C-f` — or `Ctrl+Space` then `@` — opens **Navigator**, the
  file sidebar on the left.
  Folders are bold + colored and collapsed — arrow to one and `Enter`/`→` expands it in
  place (`←` collapses); on a file, `→` previews it (reversible) and `Enter` opens it;
  `../` at the top steps up a directory, and `Ctrl+Space` on a folder re-roots *into*
  it (descend — the mirror of `../`). Press `.` to show/hide dotfiles (the important
  things often live in hidden folders). Start **typing** to fuzzy-filter the whole
  project to a shortlist; `Esc` closes.
- **Emacs**: `C-x C-s` save · `C-x C-f` open · `C-s` isearch · `M-%` query-replace
  (`y`/`n` step, `!` all) · `C-k`/`C-y` kill/yank · `C-x 2`/`C-x 3`/`C-x o` windows ·
  `M-x` mission control
- **Modern/Mac**: `C-c`/`C-v` copy/paste (system clipboard) · Shift+arrows select ·
  typing replaces selection · `Tab`/`Shift-Tab` indent/dedent a selected block ·
  mouse click/scroll/wheel
- **tmux/zellij**: `C-t` space warp · `M-{`/`M-}` or `C-PgUp/PgDn` switch tabs ·
  `M-1..9` jump to tab · `C-o`/`Ctrl+arrows` move between panes · `C-|`/`C--` splits ·
  scrollback with the wheel or `Shift+PgUp/PgDn`

**Undo, two ways.** `C-x u` (or `C-/`) undoes — a typed run coalesces into one step, and
an applied AI refactor is always exactly one step. `M-/` redoes. For bigger jumps, **`C-u`
opens time-travel**: `←`/`→` scrub back and forward through your edit history, `Home`
rewinds to the very start, `End` returns to now, `Esc` done.

Everything is remappable in `~/.config/mars/keys.json`; behavior knobs (autosave
interval, scrollback depth, colors, timings, watch quiet threshold) live in
`~/.config/mars/tuning.json`, each with a plain-English description of what it does.
Broke your config experimenting? **`mars reset`** restores default keys + tuning (your
old files are kept as `*.bak`).

**Color themes (beta).** `mars theme list` shows the bundled themes — **Mission
Control** (the default), **Eclipse** (high-contrast), **Paper** (warm light), and
**Hacker** (green-on-black); `mars theme <name>` switches (recorded in
`~/.mars/config.json`). Or the **Theme ▸** picker in the command bar switches live. Drop your own token→color JSON in `~/.mars/themes/`. A running session keeps
its look until it's restarted (or you switch from inside it). Every color is a single
semantic token now, so a theme repaints the whole UI at once. A colored theme (Paper,
Hacker) paints a solid background everywhere; the default honors your terminal's own
background. Set `opaque_background = 0` in `tuning.json` to keep the terminal
background (transparency) even under a colored theme. It's new — please report
anything that reads wrong (especially on the light theme).

## Troubleshooting

- **Staircase output** (lines drifting right, like `mars help` printing diagonally):
  your shell's terminal was left in raw mode — usually by a force-killed program.
  Run any `mars` command (it repairs the terminal automatically on startup) or
  `stty sane`.
- **`M-…` keys do nothing (macOS)**: enable "Use Option as Meta" in Terminal/iTerm —
  or use the `Ctrl`-based twins (`C-o`, `Ctrl+arrows`), which always work.
- **A session shows `dead (cleaned up)`** in `mars ls`: the daemon crashed or the
  machine rebooted. Check `~/.local/state/mars/<name>.log` for the reason; autosaved
  file changes are already on disk.
- **Fancy chords (`C-{`, `C--`) don't fire**: they need a kitty-protocol
  terminal. The Alt-based twins work everywhere.

## More

- [`architecture_overview.md`](./architecture_overview.md) — a file-by-file tour of
  the code: what lives where and how the pieces connect.
- [`DESIGN.md`](./DESIGN.md) — architecture rationale, tradeoffs, and how the pieces fit.
- [`key_design.md`](./key_design.md) — the design doctrine and product vision
  (why the keys are what they are, and where Mars is going).
- [`ROVER.md`](./ROVER.md) — the Rover manual: pairing, Claude Code, and the phone UI.
- [`SECURITY.md`](./SECURITY.md) — the security boundary and how to report a vulnerability.
- [`AGENTS.md`](./AGENTS.md) — instructions for AI coding agents working on Mars.
