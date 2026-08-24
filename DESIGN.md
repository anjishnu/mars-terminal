# Mars — Design

*Mars — mission control for your terminal.*

*System architecture, tradeoffs, and engineering philosophy. For the UX/interaction
doctrine and product vision (why the keymap looks the way it does, what Mars becomes
next), see [`key_design.md`](./key_design.md) — this document covers how the code is
built to serve that vision.*

---

## 1. What Mars is

Mars is a terminal editor built on one bet: **a single `Action` registry, exposed
through graduated retrieval paths (direct keys, a search-first command bar, an LLM
agent), can serve an Emacs power-user, a Claude-Code-native newcomer, and an
autonomous agent with the same code** — no separate "beginner mode," no parallel
command language per audience. Concretely, today, that means:

- **Non-modal editing** (type = insert) with **Emacs-compatible chords** (`C-x C-s`,
  `C-k`/`C-y`, prefix sequences) — Emacs muscle memory works on day one.
- **A `Ctrl+Space`/`M-x` command bar** with fuzzy search, live keybinding hints, a
  which-key continuation panel, and frecency-ranked results — the Claude-Code /
  VS-Code palette contract, generalized.
- **An LLM ask-agent** (`?` in the bar) that answers "how do I…" against the real
  action registry and can execute the resolved action (`RUN: <ActionName>`).
- **Split panes and tabs** (Zellij-flavored), **terminal panes** (real PTYs), and
  **session persistence** (`--session`/`--resume`, tmux/zellij-style detach/reattach).

## 2. Core abstraction: one registry, three retrieval paths

Everything runnable is a variant of `Action` (`src/palette.rs`). Three UIs resolve a
user's intent to an `Action` and hand it to `App::run_action`:

| Path | Where | Retrieval mode |
|---|---|---|
| Direct chord | `config.rs` keymap → `app.rs handle_edit` | procedural (fast, no lookup) |
| Command bar | `palette.rs` fuzzy search → `app.rs handle_bar_command` | recognition (search, browse) |
| Ask-agent | `agent.rs` → `RUN:` directive → `app.rs handle_bar_ask` | natural language → registry lookup |

Because all three terminate in the same `run_action(Action)` dispatch, a capability
added once (say, `Action::GotoLine`) is automatically bar-searchable, chord-bindable,
and agent-invokable — there's no per-surface plumbing to duplicate. `key_design.md`
argues for *why* this shape; this document is about keeping it structurally true as
the codebase grows.

**Honesty invariant (load-bearing for the whole design):** every hint surface —
status-bar hints, dropdown badges, which-key panels, the idle control-bar hint —
derives its keybinding text from `KeyBindings::binding_for(&Action)` at render time
(`ui.rs`), never from a hardcoded string. A user remap in `keys.json` updates every
surface simultaneously. No hint can lie about a binding, because no hint *stores* one.

## 3. Module map

```
main.rs      CLI entry: standalone mode, --selfcheck, --ask, --server/--session/
             --resume/--list routing (session.rs), TTY setup for standalone mode.
app.rs       App: owns buffers/panes/tabs/keymap/agent-channels/terminal-channels.
             handle_edit / handle_bar / handle_prompt / handle_tab / handle_terminal
             — one handler per Mode. run_action() is the single Action dispatch point.
             InputEvent + apply_input() are the source-agnostic input boundary that
             makes standalone mode and the session daemon share one App::step loop.
mode.rs      Mode enum (Edit/Bar/Prompt/Tab/Terminal) + per-mode status-bar hints.
config.rs    KeyChord/KeyBindings: chord parsing (Emacs C-/M- notation, cmd-/super-,
             named keys), sequence lookup, prefix detection, keys.json load/persist.
tuning.rs    Tuning: every behavioral magic number as {value, description} in
             tuning.json — layered over defaults like keys.json, agent-editable.
palette.rs   Action enum (the registry), menu structure, fuzzy_score, frecency
             tiebreak, registry_context() (the agent's action catalog).
buffer.rs    Buffer: ropey-backed text, snapshot undo/redo stack, save/save_as.
pane.rs      Pane: cursor/scroll/selection state; PaneContent::{Editor,Terminal}.
layout.rs    PaneLayout: a binary tree of HSplit/VSplit/Single — split/close/
             next/prev pane navigation over the tree.
tab.rs       Tab: a PaneLayout + focused pane + name.
terminal.rs  Term: a PTY (portable-pty) + vt100::Parser, pumped by a reader thread,
             with a separate child-process watcher for reliable ConPTY exits —
             independent of whether anyone is watching (the property that makes
             session-detach free).
agent.rs     AgentConfig (provider detection: custom/Groq/Gemini via env), the
             OpenAI-compatible chat call, RUN: directive parsing.
broker.rs    Key-never-leaves-home protocol + portable `mars keyd` daemon +
             chat_via_broker (remote side).
ssh.rs       System-OpenSSH orchestration. Unix keeps its ControlMaster path;
             Windows-home uses a per-invocation authenticated TCP relay and a
             mixed `-R remote-unix-socket:local-tcp` forward to Unix remotes.
             A separate prelude stages the embedded Unix installer and runs it
             when Mars is missing or lacks the required handoff protocol.
             The remote binary and any persistent session daemon must pass
             explicit handoff/session protocol checks before broker routing.
             Mars subprocesses inside persistent PTYs query the daemon's live
             route instead of trusting the shell's inherited, expired route.
             Every ssh subprocess explicitly removes supported provider-key
             variables, so user `SendEnv` rules cannot export home credentials.
             Stale sockets are swept from both ends — the ssh prelude `rm -f`s
             the remote path before the forward is requested, and the remote's
             detect probe unlinks a dead socket — because sshd refuses to bind
             a -R unix-socket forward over a leftover file and the client-side
             StreamLocalBindUnlink option only applies to local forwards
             (server-side sshd_config is the only other cure, which we can't
             assume). The socket name carries the HOME uid, which rarely
             matches the remote's, so detection globs any live
             /tmp/mars-auth-*.sock (own uid preferred). The install nudge
             probes ~/.cargo/bin and ~/.local/bin explicitly (`command -v`
             only sees sshd's bare non-login PATH), and the session opens with
             an honest tunnel-state line — success must not be
             indistinguishable from plain ssh. `mars ssh` lands the user IN a
             remote mars session (attach most-recent, else create "main");
             detaching ends the ssh and returns home, tmux-style — a bare
             remote shell is what plain `ssh` is for.
prompts.rs   Every model-facing instruction as editable Markdown in src/prompts/
             (include_str!-embedded, so the single binary still ships whole);
             {name} placeholders substituted at call sites.
retrieval.rs The whole memory subsystem behind a ten-symbol facade: command
             memory + shell history few-shot (fewshot_for), the self-knowledge
             docs corpus (docs_context_for), BM25 ranking, secret redaction.
             Compiled behind the default-on `memory` cargo feature; without it,
             retrieval_stub.rs supplies the same facade with neutral values, so
             a memory-free build works unchanged (the deletion-proof seam —
             core never depends on memory, only agent prompt assembly does).
session.rs   The client/server split: ClientFrame/ServerFrame protocol over a
             platform-local control channel, FrameWriter (ratatui output → stream),
             server_main/client_main, session lifecycle CLI.
serve.rs     The Rover bridge (cargo feature `web`, OFF by default): a second
             client of the session socket, plus a static server for the browser
             app, on one port distinguished by whether the request is a WS
             upgrade. Nothing in the editor calls into it — see §7.
manager.rs   The machine's supervisor: one turn reads what the panes did and
             writes the briefing and any memos, every judgement carrying how it
             was reached. briefs.rs holds delegated work; health.rs holds whether
             a run happened at all.
timeline.rs  A Claude Code transcript as typed rows for a person to skim. conv.rs
             reads the same file as prose for a model to fold (gist + delta +
             cursor). Two readers, one source, deliberately not one module.
ui.rs        ratatui rendering: layout, panes, status/control bars, dropdown,
             which-key panel, travel-mode panel, ask panel — all read live state
             (tuning, keymap) rather than baking in constants.
```

## 4. Editing model

**Non-modal by design** (`app.rs handle_edit`): a keypress is checked against the
pending-prefix state machine first (Emacs sequences like `C-x C-s`), then against a
table of Ctrl/Alt-modified editing primitives (movement, kill-ring, selection), and
falls through to plain insertion only if nothing else claims it. This makes the
destructive surface *provably* the modified-key table — a bare keystroke can never
run a command, which is the mode-error mitigation Vim's `dd`-doubling achieves
differently (see `key_design.md` §2.1 for the cognitive-science argument).

**Selection** is anchor-based (`Pane::selection_anchor`), extended by Shift+arrows,
and honored by kill/copy/paste/delete — typing over a selection replaces it (the Mac
contract), matching what Claude-Code-native and Mac users expect.

**Undo** is per-buffer rope snapshots (`Buffer::checkpoint/undo/redo`) — simple,
correct, and the known limitation the vision doc flags: it cannot express "undo
everything this agent run touched across five files," which is why a cross-buffer
transaction journal is listed as the #2 blocking substrate in `key_design.md` §6.

## 5. Command bar & agent

`Palette` (`palette.rs`) holds a menu stack (root → submenus), a query string, and a
`BarMode` (`Command`/`Ask`/`Shell`). Empty query renders the current menu in **fixed
order** (spatial stability — frecency never reorders the resting menu); a non-empty
query flattens every leaf action and ranks by `fuzzy_score` with frecency as a
tiebreak only. `!<cmd>` routes to a terminal pane; `?` routes to the agent.

The agent (`agent.rs`) is provider-agnostic over any OpenAI-compatible chat endpoint.
`AgentConfig::from_env()` resolves, in order: `MARS_LLM_KEY` (+ `MARS_LLM_URL`/
`_MODEL`, for local Ollama or anything else; legacy `ARES_*` names still honored),
`GROQ_API_KEY`, then `GEMINI_API_KEY`/`GOOGLE_API_KEY` (Gemini's `/v1beta/openai`
endpoint). The system prompt hands the
model `registry_context()` — a live-generated list of every `Action` with its
description — and asks it to cite the resolved command and, optionally, emit
`RUN: <ActionName>` on the last line for the editor to execute. Destructive actions
(`Quit`, `CloseTab`, `KillBuffer`, `ClosePane`) always confirm before an agent-proposed
`RUN:` fires them (`Action::is_destructive`) — the agent is treated as a fifth input
device, not a bypass around the editor's safety gates.

## 6. Terminal panes

`terminal.rs` spawns the platform shell on a `portable-pty` PTY and pumps its output
into a `vt100::Parser` on a dedicated reader thread, signaling the main loop via
`mpsc` only to trigger a repaint. A second thread owns and polls the child process
because ConPTY can keep its output pipe open after process exit; pane-close kill
requests go to that same handle-owning thread. The parser and shell run independently
of whether the pane is visible or focused. This property is what makes "shell
survives session detach" free. Every fresh terminal buffers input until the parser
recognizes a prompt. Empty or unusual prompts fall back to an echoed readiness
marker that is retried until the shell accepts it, so profile startup cannot discard
shell-bar commands, typing, or paste.

Keyboard chrome (pane/tab navigation, splits) is intercepted the same way inside a
terminal pane as in the editor (`App::is_chrome_action`), while editing chords
(`C-c`, `C-k`, `C-x`…) pass through untouched to the shell — the "one key language,
two audiences" rule recorded in `key_design.md`'s decision log.

## 7. Session persistence: the client/server split

**Architecture: thin client, server renders.** This was the single largest
architectural fork in the project (flagged early in `key_design.md`'s vision as
needing a decision before terminal features accreted), resolved as follows:

- The **server** (`mars --server <name>`, spawned by `--session`) runs the entire
  `App` headless — exactly the configuration `--selfcheck` already proved works with
  no real TTY. Input arrives as deserialized `crossterm` events over a local control
  stream (Unix-domain socket on Unix; token-authenticated loopback TCP on Windows)
  instead of `event::poll`; output is ratatui's own ANSI byte stream, captured by
  pointing `CrosstermBackend` at a stream-backed `Write` sink (`FrameWriter`).
- The **client** (`mars --resume`) owns the real TTY — raw mode, alternate screen,
  mouse, bracketed paste, the kitty keyboard protocol where supported — and is a thin
  pump: serialize input events to the socket, write output frames verbatim to stdout.
- **Why not a structured (tmux-style) protocol:** server-renders-ANSI reuses 100% of
  the existing ratatui rendering pipeline. A structured protocol would require
  building and maintaining a second renderer on the client side.
- **Lifecycle:** one client per session; a new attach takes over (the old client gets
  an `Exit` frame with a clear message). Disconnect leaves everything running — PTYs,
  agent threads, buffer state. `Action::Detach` (travel mode `D`, or the bar) ends the
  *connection*; `Action::Quit` (through the existing dirty-buffer guard) ends the
  *session* and removes the socket.
- **Remote broker handoff:** `Hello` also carries an optional current auth socket
  and tunnel capability. A persistent remote daemon replaces the dead route from
  its prior SSH invocation on every attach, without mutating process-global
  environment variables or restarting the workspace.
- **Daemonization:** `mars --session <name>` spawns `mars --server <name>` as a
  detached child (`setsid` on Unix; detached process-group flags on Windows), redirects
  stdio, and waits for its control address before attaching. `mars --resume [name]`
  attaches an existing session (most recently active, if unnamed); `mars --list`
  pings every session and prunes dead addresses.

Because `terminal.rs`'s PTYs and `agent.rs`'s request threads never depended on the
render loop, none of this required changes to editing, panes, or the command bar —
only a new input/output boundary around the already-decoupled `App` (see
`App::apply_input`/`App::tick` in `app.rs`, shared verbatim by standalone mode and
the session server).

**A second render target, and the invariant it forced (0.7.1).** The Rover bridge
attaches to the same socket as any other client, which made "how big is this session"
a question with two askers for the first time. `ui::render` used to write every pane's
PTY size as it drew, which is correct while "the size I am drawing at" and "the size
the session is" are the same fact — and is a race the moment they are not. Two targets
then resized the panes against each other every frame: a shell shrugs that off, a
full-screen TUI re-lays-out on each SIGWINCH and emits diffs against a width that has
already moved.

The rule now is that **the session owns its own size and every target is a viewport
that copies it**. The session renders once into a grid belonging to no target, the
target you are typing in decides the dimensions, and a mirror is told the real size so
it can fit the whole screen rather than crop to its own corner of one. Adding a third
target changes nothing about this; that is the point of stating it as an invariant
rather than as a fix.

## 8. Configuration & tuning — agent-editable by design

Two JSON files, same lifecycle (defaults written on first run, user values layered
over defaults so new fields appear in old files):

- **`~/.config/mars/keys.json`** (`config.rs`): the keymap. Every binding is a chord
  sequence string (`"C-x C-s"`, `"cmd-c"`, `"M-<"`) mapped to an `Action` name.
- **`~/.config/mars/tuning.json`** (`tuning.rs`): every behavioral constant — delays,
  thresholds, panel widths, colors, PTY defaults, agent sampling params — stored as
  `{"value": ..., "description": "..."}`. The description is not documentation for
  humans only: it's what makes the file safely editable *by an agent asked to change
  editor behavior*, without the agent needing to read Rust source to know what a
  number does. This is the first concrete instance of the "agent-editable config
  surface" pattern the vision doc's Context Bus (§6) will generalize.

Frecency and bar-usage-nudge counters persist separately in
`~/.config/mars/state.json`, loaded once at startup and saved on quit.

## 9. Testing strategy

The primary test suite is `mars --selfcheck` (`main.rs`) — a headless run against
`ratatui::backend::TestBackend` that drives the real `App` through `handle_key`/
`handle_mouse`/`paste_text` exactly as a live terminal would, with **no mocks**: it
spawns real PTYs, runs a real session daemon over the platform control channel, and (when
`GEMINI_API_KEY` etc. is set) can hit a real LLM endpoint via `--ask`. As of this
writing it covers ~90 areas end-to-end, including a fully headless test of session
detach/reattach, PTY survival across disconnect, client takeover, and version
handshake refusal. CI runs it on both Ubuntu and Windows.

**What headless testing cannot verify** (and why real-terminal passes remain
mandatory for certain changes): raw terminal byte encodings (`M-<` arrives as
ALT|SHIFT in real terminals; `C-/` arrives as `C-_` on many; the kitty keyboard
protocol's actual negotiation), and the session daemon's `setsid`/process-detachment
behavior (verified manually via `script(1)` + `ps` inspection rather than in
`--selfcheck`, since spawning a real detached child process is awkward to assert on
headlessly).

**A durable testing gotcha, worth restating here:** ratatui's incremental cell-diffing
interleaves cursor-repositioning escape codes *between* individual changed
characters when redraws happen one keystroke at a time — so typed text never appears
as a contiguous substring in the raw accumulated ANSI byte stream. Any test that needs
to assert "this text is visible on screen" must interpret the byte stream through a
real ANSI parser (the project already depends on `vt100` for terminal panes) and
check the *parsed screen contents*, not `bytes.contains(needle)`.

## 10. Key tradeoffs (see `key_design.md` §7 for the full, dated decision log)

| Decision | Alternative considered | Why this way |
|---|---|---|
| Non-modal core | Vim-modal (composable operator·motion grammar) | Explicit user ruling: Emacs/Mac/Claude-Code feel over Vim recall cost; composability argument preserved one layer up (macros over actions, not keystrokes) |
| Chords kept (`C-x C-s`, etc.) | Spacemacs-style leader-only | Emacs-compatibility / day-one productivity outweighs the documented RSI cost; mitigated by single-chord high-frequency routes and full mouse/bar alternatives |
| Server renders ANSI | Structured (tmux-style) client/server protocol | Reuses the entire existing ratatui pipeline; no second renderer to build/maintain |
| One client per session | N simultaneous mirrored clients | Held until 0.7.1, when Rover supplied the real demand. Resolved by making the session own its size and every target copy it (§7) — not by letting targets negotiate |
| Per-buffer snapshot undo | Global transaction journal | Ships today; explicitly flagged as insufficient for multi-file agent actions — journal is a planned substrate, not an oversight |
| `C-c`/`C-v` = copy/paste | Emacs `C-c` as a prefix | Modern-editor muscle memory wins for this audience; Emacs never used `C-c` as a live prefix in this keymap, so nothing is actually lost |

## 11. Non-goals (for now)

Cross-crash session save/restore (a separate feature from live detach); OSC-52
clipboard forwarding for remote/SSH attach; Windows as an SSH remote; a Vim-grammar
compatibility layer. See `key_design.md` §4 "Deliberately deferred" for the reasoning
behind each.

*Multiple simultaneous clients per session left this list in 0.7.1* — the Rover
mirror is a second render target, and §7's "the session owns its own size" exists
precisely because that stopped being hypothetical.

## 12. Where to go next

`key_design.md` is the living vision document — read it for the cognitive-science
rationale behind specific bindings, the evolution horizons (project-file layer,
deeper workspace features, autonomy layer), and the ordered list of architectural
substrates (parameterized actions, transaction journal, context bus) that upcoming
features load onto. `.claude/memory/ares-dev.md` holds accumulated, non-obvious
operational facts (build quirks, terminal-encoding gotchas, testing pitfalls) that
aren't part of the permanent design record but are worth knowing before touching
related code.
