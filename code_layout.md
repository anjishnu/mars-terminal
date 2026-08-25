# Mars — Code Layout

*The mechanical map: what each file is, where a given change belongs, and what the
seams are. Kept short and current on purpose.*

Companion docs, in the order you'd reach for them:

| Doc | Answers |
|---|---|
| **`code_layout.md`** (this file) | *Where is X?* — file index, routing table, on-disk state |
| [`architecture_overview.md`](./architecture_overview.md) | *How does it fit together?* — narrative, file-by-file tour |
| [`DESIGN.md`](./DESIGN.md) | *Why is it like this?* — architecture rationale and tradeoffs |
| [`key_design.md`](./key_design.md) | *What is it for?* — UX doctrine, interaction philosophy, vision |
| [`AGENTS.md`](./AGENTS.md) / `CLAUDE.md` | Conventions and invariants agents must honor |
| [`design_ideas/`](./design_ideas/) | Proposals — may be unbuilt; not a description of the shipped system |

---

## 1. Shape of the thing

One Rust binary (`mars`), one `[[bin]]` (`src/main.rs`), **~23,300 lines across 34
`.rs` files**, no `tests/` directory — the test suite is `mars --selfcheck`, which
lives inside `main.rs`.

Built on `ratatui` + `crossterm` (TUI), `ropey` (text), `portable-pty` + `vt100`
(shell panes), `ureq` (LLM calls), `syntect` (highlighting), `termimad` (markdown),
and platform-native IPC (Unix sockets / Windows named pipes) for session persistence.

Three architectural facts explain most of the code:

1. **One action registry.** Everything runnable is an `Action` variant
   (`palette.rs`). Keybindings, the fuzzy command bar, travel mode, and the LLM
   agent's `RUN:` directives all resolve to an `Action` and funnel through
   `App::run_action` (`app.rs:4185`).
2. **A source-agnostic core.** `App` never reads or writes a TTY. Input arrives as
   `InputEvent`; output is `ui::render` painting a ratatui backend. The same `App`
   therefore runs standalone (real TTY), inside the session daemon (socket-backed,
   headless), and under `--selfcheck` (`TestBackend`) with zero forks.
3. **Thin client, server renders.** Session persistence is a process split, not
   save/restore. The daemon owns the `App` and streams ANSI frames; the client owns
   the TTY and pumps bytes.

---

## 2. File index

Lines are approximate (`wc -l`, current as of 0.5.2).

### Entry point & harness

| File | Lines | What it is |
|---|---:|---|
| `main.rs` | 4,817 | Module declarations, `HELP` text, CLI dispatch (`main()` at :186), standalone event loop (:450), `translate_cli`/`ask_cli`, and **`selfcheck()` at :579 — which runs to EOF and is ~88% of the file** (743 assertions). |

Everything under `src/` is declared as a module here, including the three
`#[cfg]`-selected stub swaps (§4).

### Application core & rendering

| File | Lines | What it is |
|---|---:|---|
| `app.rs` | 6,215 | All state and all behavior. `struct App` (:283) plus one ~5,700-line `impl` organized by banner comments: buffers, panes, focus, cursor motion, editing, selection, word/token/structural motion, kill-ring, search, undo, save, splits, tabs, key handlers, query-replace, minibuffer, command bar, file tree, memory, terminal panes, main loop, mouse, persisted state. Key entry points: `apply_input` (:5848), `tick` (:4606), `run_action` (:4185). |
| `ui.rs` | 2,321 | Stateless projection of `&App` onto a `Frame`. `render` (:59) dispatches to per-surface renderers: panes, editor, markdown (termimad), splash, shift report, terminal, status, control bar, dropdown, notices, file tree, shell overlay, ask panel. |

### Data model

| File | Lines | What it is |
|---|---:|---|
| `buffer.rs` | 155 | `Buffer` — a `ropey` rope plus path, modified flag, undo/redo stacks, and a `rev` counter the syntax cache keys on. |
| `pane.rs` | 66 | `Pane` / `PaneContent` (editor buffer or terminal). |
| `layout.rs` | 175 | `PaneLayout` — the recursive split tree with clamped ratios. |
| `tab.rs` | 24 | `Tab` — a named workspace holding one layout. |
| `mode.rs` | 85 | `Mode` — which surface owns the keyboard: `Edit`, `Bar`, `Prompt`, `Tab` (travel), `Terminal`, `Tree`, `Undo`. |

### Command surface & configuration

| File | Lines | What it is |
|---|---:|---|
| `palette.rs` | 585 | `Action` enum + `label()` + `is_destructive`, menus (`menu_for`), fuzzy scoring, `Palette` bar state, `registry_context()` (feeds the agent the action list). |
| `config.rs` | 433 | Keybindings. Chord parsing (Emacs `C-x C-s` and long form), `KeyBindings::binding_for`, defaults written on first run, theme selection, `mars reset`. |
| `tuning.rs` | 587 | Every behavioral magic number as a described knob, plus `Palette` (semantic color tokens). Layered over defaults so new knobs appear in old files. |
| `themes.rs` | 187 | Named theme → `Palette`. Bundled themes embedded; `~/.mars/themes/*.json` shadow them. |

### Subsystems

| File | Lines | What it is |
|---|---:|---|
| `terminal.rs` | 821 | PTY panes. `spawn` a shell, parse output with `vt100`, emit `TermEvent`. |
| `session.rs` | 3,562 | The daemon. `ClientFrame`/`ServerFrame` JSON-lines protocol, `server_main`/`client_main`, socket paths, name validation, `ls`/`kill`/`rename`/`killall`. |
| `agent.rs` | 1,662 | LLM layer. Provider detection and env precedence, `chat`/`chat_with_id_streaming`, per-task builders (`ask`, `translate_shell`, `watch_summary`, `auto_name`, `infer_mission`, `capture_goals`, `shift_brief`), `RUN:`/`TYPE:` directive parsing, rate-limit rotation. |
| `tiers.rs` | 219 | Model-tier ring: `task → tier → model` per provider, editable at `~/.config/mars/tiers.json`. Explicit `MARS_LLM_MODEL` always wins. |
| `prompts.rs` | 56 | `include_str!` consts for the 20 `.md` files in `src/prompts/`. **No prompt text lives in code.** |
| `persona.rs` | 87 | User voice file (`~/.mars/persona.md`) injected as the final system message for VOICE tasks only. |
| `retrieval.rs` | 524 | Memory: BM25 over command memory + docs corpus, redaction, denylist. Feature-gated. |
| `syntax.rs` | 195 | syntect highlighting, colors synthesized from the active theme; background streaming worker. Feature-gated. |
| `worklog.rs` | 493 | The work journal — watch verdicts, missions, goals, briefings as JSONL. Substrate for `mars ls` and the shift report. |
| `briefing.rs` | 358 | The shift report. Deterministic tier-0 triage (exit codes, tail shape); the LLM only replaces a defensible placeholder, never blocks a frame. |
| `llm_log.rs` | 401 | LLM call observability (`MARS_LLM_DEBUG`), plus `mars llm-stats` aggregation. |
| `osc133.rs` | 189 | Shell-integration marker scanner (OSC 133/633/7) → exact command boundaries for the ledger. Purely additive. |
| `broker.rs` | 574 | `mars keyd` — the key-never-leaves-home broker and the remote-side proxy call. Feature-gated. |
| `ssh.rs` | 514 | `mars ssh` — system-OpenSSH orchestration, remote bootstrap, capability relay. Feature-gated. |
| `fleet.rs` | 123 | The host registry behind `mars ls` (portable, so it survives a no-`ssh` build). |
| `manager.rs` | 4,417 | The machine's supervisor. One turn reads what the panes did and writes the briefing and any memos; every judgement carries how it was reached. |
| `briefs.rs` | 1,950 | Delegated work: an idea, its decisions, a verification step. State is which files exist, never liveness; `verify` runs argv, never a shell. |
| `health.rs` | 239 | Whether a manager run happened at all — distinct from whether it found anything. |
| `timeline.rs` | 619 | A transcript as typed rows for a person to skim. Unknown records become `Row::Unknown`, never a parse error; reads are bounded to the file's tail. |
| `conv.rs` | 251 | The same transcript as prose for a model to fold: gist + delta + cursor, so cost stays flat however long a conversation runs. Found by id, never by rebuilding a path. |
| `serve.rs` | 3,459 | The Rover bridge (feature `web`, **off by default**). One port, two protocols: a WS upgrade gets the session-socket pump, anything else gets the built Rover bundle from `$MARS_WEB_DIR` (a development path — the tunnel is the shipped route). Pairing links have exactly one builder. |
| `project.rs` | 61 | Bounded lazy file index behind the `@` picker. |
| `banner.rs` | 39 | Generated splash art (truecolor SGR + a plain block wordmark for themes). |

### Platform abstraction

| File | Lines | What it is |
|---|---:|---|
| `sys/mod.rs` | 30 | The PAL contract. Capability modules: `paths`, `control`, `tty`, `daemon`, `proc`, `fsperm`, `shell`. |
| `sys/unix.rs` | 187 | Unix adapter — the only place `libc`/`std::os::unix` may appear. |
| `sys/windows.rs` | 496 | Windows adapter — named pipes with HMAC-SHA256 mutual auth. |

**Enforced:** no module outside `src/sys/` may name an OS API.
`tools/check-platform-isolation.sh` runs in CI. See `WINDOWS_PORT.md`.

### Embedded assets

`src/prompts/*.md` (18) · `src/themes/*.json` (4: mission-control, eclipse, hacker,
paper) · `src/tiers_default.json` · `src/syntaxes/typescript.sublime-syntax` ·
`install.sh` (embedded by `ssh.rs` for remote bootstrap).

All of these must stay listed in `Cargo.toml`'s `include` or `cargo publish` breaks.

---

## 3. Where a keystroke goes

```
real TTY (crossterm)  ──┐
session client frames ──┼──► InputEvent ──► App::apply_input
TestBackend (selfcheck)─┘                        │
                                                 ▼
                                   mode dispatch (mode.rs)
                     editor / command bar / minibuffer / travel / terminal
                                                 │
                              chord match (config.rs) or bar selection
                                                 ▼
                                    Action ──► App::run_action
                                                 │
                          is_destructive? ──► confirmation gate ──► fire
                                                 ▼
                                    state mutation on App
                                                 ▼
                              ui::render(&App)  ──► ratatui backend
```

`App::tick` runs alongside: it drains PTY output, agent-thread events, and syntax
worker results, then handles autosave, watch fires, and background refreshes.

The same funnel serves the LLM: an agent reply carrying `RUN: <action>` is parsed in
`agent.rs`, checked against the registry, and dispatched through `run_action` — which
is why the destructive-action gate covers agents for free.

---

## 4. Feature flags and the stub seams

Three optional capabilities are default-on with an inert twin selected by `#[path]`
in `main.rs`, so **call sites carry no `cfg`** and never learn the capability is
missing:

| Feature | Real | Stub | Covers |
|---|---|---|---|
| `memory` | `retrieval.rs` | `retrieval_stub.rs` | command memory, docs corpus, redaction |
| `ssh` | `broker.rs`, `ssh.rs` | `broker_stub.rs` | `mars keyd`, `mars ssh` |
| `syntax` | `syntax.rs` | `syntax_stub.rs` | syntect highlighting (drops the dep entirely) |

A fourth, `web`, breaks both halves of that pattern — it is **off** by default and has
no twin:

| Feature | Real | Stub | Covers |
|---|---|---|---|
| `web` (off) | `serve.rs` | *none* | `mars pair`, `mars serve`, `mars qr` |

Nothing to stub, because the bridge is additive rather than replaceable: no editor
path calls into it, so its absence is a missing verb rather than a neutral value. The
consequences are worth knowing — ten real `#[cfg(feature = "web")]` sites in
`main.rs`, and **`serve.rs` is not compiled at all in a default build**. Verifying it
means `cargo run --features web -- --selfcheck`, which is a separate obligation.

`cargo build --no-default-features` must also pass `--selfcheck`. If you touch a real
module's public surface, mirror it in the stub in the same commit.

---

## 5. Routing table — "I want to change…"

| Goal | Go to |
|---|---|
| Add a runnable capability | `palette.rs` (`Action` variant + `menu_for` entry + `label()` arm + `is_destructive` if needed) → `app.rs` `run_action` arm. Keybinding optional, in `config.rs` defaults. |
| Change a keybinding default | `config.rs`. **Never** hardcode the string in a label — every hint derives from `binding_for()` at render time (the honesty invariant, `DESIGN.md` §2). |
| Tune a behavioral number | `tuning.rs`, as a named knob with a description. Not a literal at the call site. |
| Change what a model is told | the relevant `src/prompts/*.md`. New prompt = new `.md` + const in `prompts.rs` + placeholder assertion in the selfcheck's prompt-templates block. |
| Re-point a model / move a task between tiers | `tiers.rs` (or `~/.config/mars/tiers.json` at runtime). |
| Change how something looks | `ui.rs` for layout/structure; `themes.rs` + `src/themes/*.json` for color. Read semantic tokens from `Palette` — never a raw `Color::`. |
| Add a color to the palette | `tuning.rs` `Palette` + every `src/themes/*.json`. |
| Add a CLI subcommand | `main.rs` `match first.as_deref()` + the `HELP` const at :67. |
| Change what Rover shows | the client lives in a separate repo; the host side is `serve.rs` (frames), `timeline.rs` (rows), `manager.rs` (board and briefing). Build with `--features web` or none of it compiles. |
| Change how a pairing link is built | `serve.rs::build_pair_link_all` — the only builder, deliberately. Never mint a token at link time. |
| Touch OS behavior | `src/sys/` only. Anything else fails `tools/check-platform-isolation.sh`. |
| Add a test | extend `selfcheck()` in `main.rs`. Do not add a separate harness. |

---

## 6. Runtime state on disk

`~/.mars/` — data the user accumulates:

| Path | Contents |
|---|---|
| `config.json` | Global config; can export env overrides and pick a theme |
| `worklog.jsonl` | Work journal: watch verdicts, missions, goals, briefings |
| `cmd_memory.jsonl` | Accepted `(request → command)` pairs (feature `memory`) |
| `denylist` | Redaction patterns, hot-read on every prompt assembly |
| `persona.md` | User voice file (seeded on first run) |
| `fleet.json` | Hosts you've connected to, for `mars ls` |
| `logs/calls.jsonl` | LLM call log when `MARS_LLM_DEBUG=1` |
| `themes/*.json`, `syntaxes/*.sublime-syntax` | Runtime extension points — no rebuild |
| `auth.sock`, `keyd.log` | Broker socket and log |
| `machine-id` | This machine, **minted once and never derived** — see `session.rs::machine_id` |
| `serve.token` | The pairing credential the bridge validates against. Code execution as you |
| `serve.url`, `serve.pid`, `serve.session`, `serve.instance` | The running bridge: where it is, what it is, which session it fronts |
| `tunnel.log`, `serve-agent.log` | Tunnel and bridge logs |
| `manager/` | The manager's own working directory — it is a Claude session, so its turns land in `~/.claude/projects/` like any other |
| `briefs/` | Delegated work. `WORKING-MODEL.md` holds the standing orders, versioned rather than retyped |
| `sessions/<name>/` | Per-session state: `restore.json`, `conv/` cursors, memos, snapshots, workspaces |
| `briefings.jsonl`, `mission.json`, `goals.json` | What the manager has concluded |

`~/.config/mars/` — config written with annotated defaults on first run:
`keys.json`, `tuning.json`, `tiers.json`.

Session sockets live under the platform runtime dir; `MARS_RUNTIME_DIR` relocates
them (use this to isolate tests — **never** run `mars killall` against real sessions).

Notable env vars: `MARS_LLM_*` (provider/model/key/url), `MARS_MEMORY` (retrieval
ablation: `none|history|docs|full`), `MARS_LLM_DEBUG`, `MARS_RUNTIME_DIR`,
`MARS_WORKLOG`, `MARS_PERSONA`, `MARS_DENYLIST`, `MARS_NO_SYSTEM_CLIPBOARD`,
`MARS_SESSION`, `MARS_AUTH_SOCK`.

---

## 7. Build and verify

```bash
source ~/.cargo/env && cargo build          # cargo is not on the default PATH
./target/debug/mars --selfcheck             # the suite — run after every change
cargo build --no-default-features && ./target/debug/mars --selfcheck
```

`--selfcheck` drives the real `App` against `TestBackend` — no mocks. It spawns real
PTYs and a real session daemon over a real socket, and isolates itself with a temp
config dir, a scratch worklog, cleared provider keys, and a disabled clipboard.

**CI** (`.github/workflows/ci.yml`) runs both selfcheck configurations on
ubuntu-latest and windows-latest, plus the platform-isolation check.

**What headless testing cannot verify:** real terminal byte encodings (`M-<` as
ALT|SHIFT, `C-/` as `C-_`, kitty-protocol negotiation) and the daemon's
`setsid`/detachment behavior. Changes to `config.rs` chord parsing or `session.rs`
process spawning need a manual real-terminal pass — `DESIGN.md` §9.

**A durable gotcha:** ratatui's incremental cell-diffing interleaves cursor-motion
escapes *between* changed characters, so typed text is not a contiguous substring of
raw ANSI output. Assert on a parsed screen (`vt100`), never `bytes.contains(needle)`.

---

## 8. Non-source directories

| Dir | Contents |
|---|---|
| `design_ideas/` | Forward-looking proposals — may be unbuilt. Ships nothing. `shipped/` holds landed ones. |
| `eval/` | The two-axis memory evaluation: results JSONL, run logs, `REPORT.md`. |
| `paper/` | LaTeX build artifacts for the write-up. |
| `tools/` | `check-platform-isolation.sh` (CI-enforced). |
| `.claude/memory/` | Accumulated non-obvious operational facts. Read `INDEX.md` first. |
