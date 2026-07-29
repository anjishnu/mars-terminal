# Mars — Architecture Overview

*A file-by-file tour of the codebase: what lives where, how the pieces connect, and
the patterns that hold it together. Companion to [`DESIGN.md`](./DESIGN.md) (rationale
and tradeoffs) and [`key_design.md`](./key_design.md) (UX doctrine and vision) — this
document is the map; those are the argument. For the terse index —
"which file do I open to change X" — see [`code_layout.md`](./code_layout.md).*

Mars is a single Rust binary (`src/main.rs` is the only `[[bin]]`), ~23,300 lines
across 34 modules, built on ratatui + crossterm, with `ropey` for text,
`portable-pty` + `vt100` for terminal panes, `syntect` for highlighting, `termimad`
for markdown, `ureq` for the LLM agent, and a platform-native local control channel
(Unix domain sockets / authenticated loopback TCP on Windows) for session persistence.

## 1. The big picture

Three ideas shape everything:

1. **One action registry, many retrieval paths.** Everything runnable is one of the
   ~64 variants of `Action` (`palette.rs`). Keybindings, the fuzzy command bar,
   travel mode, and the LLM agent's `RUN:` directives all resolve to an `Action` and
   funnel through a single dispatch point, `App::run_action`. Adding one `Action`
   variant makes a capability chord-bindable, bar-searchable, and agent-invokable at
   once.

2. **A source-agnostic core.** `App` (`app.rs`) never reads a TTY and never writes
   to one. Input arrives as `InputEvent` values (key / mouse / paste / resize);
   output happens by `ui::render` painting a ratatui backend. Because the core
   doesn't care whether that backend is a real terminal, a socket, or a test buffer,
   the same `App` runs in three configurations with zero forks: standalone mode
   (real TTY), the session daemon (socket-backed, headless), and `--selfcheck`
   (`TestBackend`, no TTY at all).

3. **Thin client, server renders.** Session persistence is not a save/restore
   layer — it's a process split. The daemon owns the `App` and renders frames;
   the client owns the TTY and pumps bytes. Terminal panes and agent threads never
   depended on anyone watching them, so detach is free.

A fourth pattern shows up everywhere once you look for it: **deletion-proof seams**.
Three subsystems — memory, ssh/broker, syntax — are cargo features whose absence
swaps in an inert twin (`*_stub.rs`) with the same signatures, so no call site
carries a `cfg` and both builds pass the same suite (§9).

```
                 ┌────────────────────────────────────────────┐
                 │              mars (one binary)             │
                 └────────────────────────────────────────────┘
   mars -s file          mars / mars new work          mars --selfcheck
   (standalone)        (sessions by default)             (headless CI)
        │                        │                            │
        │             ┌──────────┴──────────┐                 │
        │             │ client   ⇄  daemon  │ local control:  │
        │             │ (TTY)      (App)    │ ClientFrame /   │
        │             └──────────┬──────────┘ ServerFrame     │
        ▼                        ▼                            ▼
   ┌─────────────────────────────────────────────────────────────┐
   │  App (app.rs) — all state, all behavior                     │
   │    apply_input(InputEvent) → mode handlers → run_action()   │
   │    tick() — drains PTY + agent + syntax channels, autosave, │
   │             watches, mission/goal refresh                   │
   ├─────────────────────────────────────────────────────────────┤
   │  ui.rs      renders &App each frame (stateless projection)  │
   │  palette.rs Action registry + command-bar menus/search      │
   │  config.rs  keymap    tuning.rs  knobs    themes.rs  color  │
   │  buffer/pane/layout/tab/mode  — the data model              │
   │  terminal.rs PTY panes    agent.rs + tiers/prompts/persona  │
   │  worklog.rs journal   briefing.rs shift report              │
   │  retrieval.rs memory  syntax.rs highlighting  llm_log.rs    │
   ├─────────────────────────────────────────────────────────────┤
   │  sys/ — the ONE place the operating system leaks in         │
   └─────────────────────────────────────────────────────────────┘
```

## 2. Module map by layer

| Layer | Files | Lines |
|---|---|---:|
| Entry & test harness | `main.rs` (~88% of it is `selfcheck`) | 4,817 |
| Application core | `app.rs` | 6,215 |
| Rendering | `ui.rs` | 2,321 |
| Data model | `buffer.rs`, `pane.rs`, `layout.rs`, `tab.rs`, `mode.rs` | 505 |
| Command surface | `palette.rs`, `config.rs`, `tuning.rs`, `themes.rs` | 1,792 |
| Terminal & session | `terminal.rs`, `session.rs`, `osc133.rs` | 1,914 |
| Agent stack | `agent.rs`, `tiers.rs`, `prompts.rs`, `persona.rs` | 1,723 |
| Journal & briefing | `worklog.rs`, `briefing.rs`, `llm_log.rs` | 1,178 |
| Platform abstraction | `sys/mod.rs`, `sys/unix.rs`, `sys/windows.rs` | 713 |
| Remote (feature `ssh`) | `broker.rs`, `ssh.rs`, `fleet.rs` (`broker_stub.rs` when off) | 1,151 |
| Memory (feature `memory`) | `retrieval.rs` (`retrieval_stub.rs` when off) | 524 |
| Highlighting (feature `syntax`) | `syntax.rs` (`syntax_stub.rs` when off) | 195 |
| Misc | `project.rs`, `banner.rs` | 100 |

Dependencies point downward-ish: `main` → `session`/`app`; `session` → `app`;
`app` → everything; `ui` reads `app`; the data-model and subsystem files depend
only on each other in small, local ways; everything reaches the OS only through
`sys`. There are no circular ownership relationships — `App` owns all state;
`ui.rs` is functions over `&App`.

## 3. Entry point and orchestration — `main.rs`

The CLI dispatcher and the test suite.

- **Subcommands**: `help`/`version`; `new`/`session <name>` (create-or-attach);
  `attach`/`resume [name]`; `ls`/`list`; `kill <name>`; `killall`;
  `rename <old> <new>`; `reset`; `ask "<q>"` (headless one-shot agent query);
  `translate "<q>"` (headless NL→shell); `setup`/`keys` (API-key instructions);
  `config` (show `~/.mars/config.json`); `theme [list|<name>]`; `ssh <host>`;
  `keyd` (the key broker); `llm-stats` (profile the LLM debug log);
  `--selfcheck`; `--server <name>` (internal — the daemon body, spawned by
  `session_main`, never called directly); `-s`/`--standalone` (no daemon).
  A bare `mars [file]` is **sessions-by-default**: it computes the next free
  auto-numbered session name and delegates to `session::session_main`, tmux-style.
  Unknown `-`/`--` flags exit 2 with help; bare arguments are filenames.
- **Global flags parsed before dispatch**: `--llm-debug` (sets `MARS_LLM_DEBUG`, so
  the daemon inherits it) and `--memory <none|history|docs|full>` (sets
  `MARS_MEMORY` — the eval's retrieval ablation knob). Both are stripped from the
  argument stream so they can't be mistaken for a filename.
- **`~/.mars/config.json` is applied first**, before anything reads the
  environment: it can export an `env` map and select a theme. The real environment
  still wins.
- **Standalone event loop**: raw mode + alternate screen + mouse + bracketed paste
  + kitty keyboard flags (when supported), a TTY-reader thread mapping crossterm
  events to `InputEvent`s over an mpsc channel, then `App::run`. The *daemon's*
  event loop is not here — it lives in `session.rs`.
- **`sanitize_tty()` runs first thing** in `main`, before crossterm can snapshot
  terminal state: a SIGKILL'd previous client leaves the TTY raw, and every `mars`
  invocation repairs that on startup.
- **`selfcheck()`** (`main.rs:579` → EOF) is most of the file: **96 numbered checks
  and 743 assertions** driving the real `App` against `ratatui::TestBackend` with no
  mocks. It is hermetic — it clears inherited agent keys, redirects the config dir
  to a temp path, points `MARS_WORKLOG` at a scratch file so no block can pollute
  the user's real journal, and disables the system clipboard. Coverage runs from
  kill-ring semantics and search-teleport labels through the terminal composer,
  watch notices (W6), the reattach briefing (W7), NEED-directive depth-capping
  (W5/W4), theming and the color-honesty guard, the syntax engine and its
  no-flash cache splicing, LLM debug logging, BM25 retrieval, the shift report, and
  goal capture. Check 27 starts a **real session daemon on a thread** and drives it
  through a `TestClient` over a real socket — version-handshake refusal, client
  takeover, PTY survival across disconnect, live rename. Screen assertions go
  through a `vt100` parser, never raw byte matching (see the gotcha in `AGENTS.md`).

## 4. The application core — `app.rs`

The largest file by design: `App` is the single owner of all state — buffers,
panes, tabs, terminals, the palette, prompts, search state, the agent
conversation, watch state, notices, the syntax cache — and all behavior. No
rendering lives here.

**Input path.** `apply_input(InputEvent)` fans out to `handle_key` /
`handle_mouse` / `paste_text`. `handle_key` routes on the current `Mode` to one
handler per mode: `handle_edit`, `handle_bar` (which sub-routes to
`handle_bar_command` / `handle_bar_ask` / `handle_bar_shell`), `handle_prompt`,
`handle_tab` (travel mode), `handle_terminal`, `handle_tree`, `handle_undo_mode`.
`handle_edit` runs the Emacs prefix-key state machine (`pending_prefix` +
`KeyBindings::lookup`), then a table of modified-key editing primitives, and only
then falls through to plain insertion — so a bare keystroke can never run a command
(the non-modal safety argument in `DESIGN.md` §4).

**Command path.** `run_action(Action)` is the one dispatch point for every
command regardless of origin (chord, bar, travel mode, agent `RUN:`). It also
maintains frecency counters and breaks the `M-y` yank chain — cross-cutting
invariants live at the funnel, not at each call site.

**Main loop.** `run` is draw → `tick` → block on the input channel with a
timeout. `tick` does the per-frame housekeeping: drain `TermEvent`s from PTY
reader threads, `AgentEvent`s from LLM threads, and `SyntaxEvent`s from the
highlight worker; autosave on a timer; fire watch summaries; auto-name
tabs/sessions; refresh the inferred mission. The session daemon calls exactly the
same `tick` — which is why watches keep firing while you're detached. `needs_redraw`
gates the actual paint, so an idle screen doesn't flush 60×/s (invisible locally,
pure noise over SSH).

Functional areas worth knowing (all methods on `App`):

- **Editing primitives** — cursor motion, insert/delete, selection
  (anchor-based, `selection_range`), kill-ring + system clipboard (`push_kill`
  writes both, and queues an OSC 52 escape for the daemon case where `arboard`
  would write to the wrong machine), paste routing per mode.
- **Fast motion** — `move_token_forward/backward` (code-token hops for ⌘/⌥
  arrows), `jump_block` (blank-line blocks), `jump_symbol` (column-0
  `fn`/`def`/`class` heuristic), `match_bracket`.
- **Incremental search / teleport** — `update_isearch` jumps live while typing;
  `build_search_labels` assigns home-row labels to visible matches (Tab);
  land-on-any-key commits the search and applies the key. `search_origin` makes
  `C-g` restore where you started.
- **Panes/tabs** — splits bounded by `tuning.max_panes`, geometric
  focus-by-direction using last-render `pane_rects`, zoom, swap;
  `kill_buffer` retargets every pane showing the killed buffer (stale-`BufferId`
  safety invariant).
- **File tree** — `FileTree` state + `compute_tree_rows` (browse tree vs.
  fuzzy-filtered shortlist over `project::Index`), preview (`→`, reversible) vs.
  open (`Enter`); `Ctrl+Space` on a folder re-roots into it.
- **Terminal panes** — `is_chrome_action` defines exactly which chords (pane/tab
  navigation) pierce a focused terminal; everything else is translated to PTY
  bytes by `key_to_bytes`. `Ctrl+Space` opens the unified composer.
- **Syntax highlighting** — a per-session toggle (`C-x C-h`, seeded from
  `tuning.syntax_highlight`) plus a per-buffer `SyntaxCache`. Work goes to a
  background worker keyed on `(buffer rev, palette id)`: `syntax_want` records the
  pass currently wanted, and the drain applies only matching chunks, so a worker
  superseded by a newer edit or a theme change is dropped rather than clobbering
  the current colors. Fresh passes **overwrite in place** (never clear) so there is
  no flash, and Enter splices the cached color-line at the cursor so colors survive
  a line split until the real pass lands.
- **Agent integration** — `submit_agent_query` ships question + `screen_context`
  (a size-capped slice of what you see, plus the exact selection if any) +
  action registry + conversation history to a background thread; the reply
  **streams** into `agent_partial` and is rendered live, then the final `Answer`
  event replaces it with directive-parsed text. Directives are confirm-gated
  (§8). `apply_refactor` replaces the captured selection with the model's code
  block as **one undo step**; with no selection the target is an empty range at
  the cursor, so the block inserts at point ("write a limerick about potatoes").
- **Watch, journal & briefing (W6/W7)** — `WatchState` per terminal fed by the PTY
  drain; `maybe_fire_watches` summarizes on exit or quiet via a background thread;
  results land in a pull-model `notices` queue (the agent's *only* path to the
  screen) and in the `worklog` journal. `on_detach` snapshots cheap facts and
  captures goals; `on_attach` builds the `ShiftReport` overlay. The report's window
  starts at `last_input_tick` — "since your keyboard went silent", not since a
  formal detach — so a job that ran while you sat idle is still summarized.
  `maybe_infer_mission` keeps a one-line "what is this person working on" fresh in
  the background for `mars ls`.
- **Themes** — `SetTheme` resolves a named palette into `tuning.palette` live;
  `palette_id()` hashes it so the syntax cache invalidates on a theme change.
- **Persistence** — `PersistedState` (frecency, bar-usage nudge counters, file
  frecency) in the app config dir's `state.json`; `autosave` writes dirty
  file-backed buffers on a timer and on detach.

**Undo grouping**: `Buffer::checkpoint()` is called once per logical edit — a paste,
a typed run (coalesced via `edit_run`), an applied refactor — so each is one
reversible unit. `Undo` mode turns ←/→ into time-travel through that history.

## 5. Rendering — `ui.rs`

Free functions over `&App`; ratatui repaints the full frame each tick and diffs
cells itself. Rendering is a pure projection of App state with one deliberate
exception: it writes back render-derived geometry (`pane_rects`,
`cursor_screen`, per-pane `view_h`) that App's mouse hit-testing and overlay
anchoring need next frame.

- **Fixed chrome**: tab bar (1 row) · pane area · status bar (1) · control bar
  (1); the file-tree sidebar is carved from the pane area's left edge.
- **Panes**: `compute_rects` walks the `PaneLayout` tree into rectangles;
  editor panes render with a per-character highlight map (syntax colors /
  selection / search match / teleport label composited in a single styled pass);
  markdown buffers in reading mode render through `termimad` with a theme-derived
  skin; terminal panes render the `vt100` screen grid cell-by-cell, including a
  scrollback-offset indicator and an exit banner for dead shells.
- **Overlays**, drawn last and grown upward from the control bar: the command-bar
  dropdown (with a workspaces panel and a detail column), the ask panel (agent
  transcript, streaming partials, confirm lines for pending directives/refactors),
  the inline shell composer (anchored at the terminal cursor — no eye-jump; it
  yields to the dropdown when the two would collide), the which-key continuation
  panel (appears after a hesitation delay on a pending prefix), and the travel-mode
  cheat panel. The splash banner (`banner.rs` art, parsed from raw ANSI by
  `ansi_to_line`, over a procedural `starfield`) overlays everything at startup
  until the first keypress. `render_shift_report` takes the whole pane area on
  reattach, revealing rows on a timed cadence (`briefing::reveal_at`).
- **Color discipline**: every colored cell reads exactly one semantic token from
  `tuning.palette` — never a raw `Color::`. A selfcheck guard (check 40f) fails the
  build if a bare ANSI `Color::Named` reappears, and `opaque_bg` keeps non-default
  themes from letting the terminal's own background show through.
- **The honesty invariant lives here**: every hint surface — status-bar hints,
  dropdown badges, which-key rows, the idle control-bar line — calls
  `KeyBindings::binding_for(&Action)` at render time. No binding string is ever
  stored in UI code, so a remap in `keys.json` updates every surface at once.

## 6. The data model — `buffer.rs`, `pane.rs`, `layout.rs`, `tab.rs`, `mode.rs`

Five small files, deliberately dumb:

- **`buffer.rs`** — `Buffer`: a `ropey::Rope` plus name/path/modified flag, a
  snapshot undo/redo stack (`checkpoint`/`undo`/`redo` clone the whole rope —
  simple and correct; the planned cross-buffer transaction journal is a known
  future substrate, per `DESIGN.md` §4), and a monotonic `rev` counter the syntax
  cache keys on.
- **`pane.rs`** — `Pane`: cursor, column affinity, scroll, selection anchor,
  optional title, and `PaneContent::Editor(BufferId) | Terminal(TermId)` — a
  pane is a *view*, pointing at content it doesn't own.
- **`layout.rs`** — `PaneLayout`: a binary tree of `Single`/`HSplit`/`VSplit`
  with clamped split ratios; supports split/remove (sibling promotion),
  next/prev traversal, and deepest-first resize around the focused pane.
- **`tab.rs`** — `Tab`: a `PaneLayout` + focused pane + name + optional zoomed
  pane. Eleven lines of state; all behavior lives in `App`.
- **`mode.rs`** — `Mode`: `Edit`, `Bar`, `Prompt`, `Tab` (travel), `Terminal`,
  `Tree`, `Undo` — the top of `handle_key`'s routing, plus each mode's status-bar
  chip and hint pairs (Edit's hints are intentionally empty here: they're derived
  live from the keymap in `ui.rs`).

## 7. The command surface — `palette.rs`, `config.rs`, `tuning.rs`, `themes.rs`

- **`palette.rs`** — the `Action` enum (the registry), each action's `label()`,
  the `is_destructive()` set (Quit/CloseTab/KillBuffer/ClosePane — these confirm
  before firing, whether a human or an agent asked), the curated menu tree
  behind the command bar (`menu_for`, with `ItemKind`/`SurfaceRef` so a row can
  point at a submenu or a live surface), `fuzzy_score` (subsequence match with
  contiguity and word-boundary bonuses), and `Palette` state (menu stack, query,
  `BarMode: Command|Ask|Shell`, `BarColumn`). Two rules encoded here: an **empty
  query renders the menu in fixed order** (spatial memory; frecency is only ever a
  tiebreaker on searches), and `registry_context()` generates the live action
  catalog the LLM receives — so agent answers cite real commands, and
  `Action::from_name` round-trips its `RUN:` directives.
- **`config.rs`** — `KeyChord`/`KeyBindings`: parses Emacs notation (`C-x C-s`,
  `M-<`), long forms (`ctrl-x`), and `cmd-` (⌘, kitty-protocol terminals only)
  into chord *sequences*; computes the prefix set for the pending-prefix state
  machine; loads `~/.config/mars/keys.json`, layering defaults under user
  entries so new default bindings appear in old files. `binding_for(&Action)`
  (shortest, capability-tiered, canonical-preferring) is the single source of truth
  for every UI hint. `chord_of` normalizes real-terminal quirks (e.g. dropping
  SHIFT on non-alphabetic chars, since `M-<` arrives as ALT|SHIFT+`<`). Also owns
  config-dir resolution (including the one-time `~/.config/ares` → `mars`
  migration), theme selection, and `mars reset`.
- **`tuning.rs`** — every behavioral magic number as a named knob in
  `~/.config/mars/tuning.json`, stored as `{"value": …, "description": "…"}`.
  The description makes the file safely editable by a human *or an agent* asked
  to change editor behavior. Covers timings (poll interval, which-key delay,
  watch quiet threshold), limits (max panes, scrollback, project index cap),
  agent sampling parameters, and the `Palette` struct itself — the semantic color
  tokens every rendered cell reads from. Same layering lifecycle as `keys.json`.
- **`themes.rs`** — resolves a named theme into that `Palette`. Themes are flat
  JSON token→color maps; the four bundled ones (`mission-control`, `eclipse`,
  `hacker`, `paper`) are embedded at compile time, and `~/.mars/themes/*.json`
  shadow bundled names — a runtime extension point with no rebuild. Values are
  `#rrggbb` or a named terminal color; an under-specified theme falls back
  per-token to Mission Control, which is byte-identical to the shipped look.

## 8. The subsystems

### `terminal.rs` + `osc133.rs` — PTY panes and command boundaries

`spawn` runs the platform shell on a `portable-pty` PTY and pumps its output into
a `vt100::Parser` on a dedicated reader thread. A separate process watcher owns and
polls the child, handles pane-close kill requests, and emits `TermEvent::Exited`
after a bounded final-output drain; this is required because ConPTY can keep its
output pipe open after the child exits. **The parser and shell run whether or not
anyone is watching** — this property is what makes session detach free.
`Term` also owns scrollback view state (`scroll_view`, `scroll_to_live`) and
`history_tail(lines)` — the method that pages back through vt100 scrollback
(and restores the live view) to satisfy the agent's `NEED: scrollback` requests.
Every fresh terminal buffers input until a recognized prompt or retryable
shell-readiness marker proves that profile startup has completed.

The same reader thread feeds an `osc133::Scanner`. A terminal is an opaque byte
grid, so the verdict ladder can only *guess* an outcome from the tail; shell
integration (FinalTerm/iTerm2/VS Code "semantic prompt" markers — `133;C`,
`133;D`, `633;E`, `7`) annotates the stream with ground truth: exact command
boundaries, exit codes, cwd, and the command text. Matched pairs become exact
ledger records in the worklog. It is **purely additive**: a shell that emits no
markers produces no events, so un-integrated panes are unaffected. Making Mars
*inject* the integration into spawned shells is the real-terminal-gated remainder.

### The agent stack — `agent.rs`, `tiers.rs`, `prompts.rs`, `persona.rs`

**`agent.rs`** is stateless functions over a provider. `AgentConfig::from_env()`
resolves by precedence: a forwarded broker socket (we're on a remote box — proxy
home, never hold a key) → `MARS_LLM_KEY` (custom URL/model, e.g. local Ollama) →
enterprise gateways `AWS_BEARER_TOKEN_BEDROCK` / Azure OpenAI (a box deliberately
configured for one means to use it) → `ANTHROPIC_API_KEY` → `OPENAI_API_KEY` →
`GROQ_API_KEY` → `GEMINI_API_KEY`/`GOOGLE_API_KEY`; legacy `ARES_*` names still
honored. Most providers speak the OpenAI-compatible `/chat/completions` shape;
Anthropic's Messages API and Bedrock's Converse API get their own branches in
`chat_inner`. Fire-and-forget entry points (`ask`, `translate_shell`,
`watch_summary`, `auto_name`, `name_session`, `infer_mission`, `capture_goals`,
`shift_brief`) each spawn a thread and deliver `AgentEvent`s over the caller's
channel — nothing here ever blocks the UI; `App::tick` drains results. `ask` and
`shift_brief` stream, so text appears as it arrives.

The system prompt is a contract: be terse, ground answers in the embedded live
screen and the `registry_context()` action catalog, and end with **exactly one
directive** on the final line:

| Directive | Meaning | Gating |
|---|---|---|
| `RUN: <ActionName>` | fire an editor action from the registry | confirm-gated; destructive actions get the full confirmation prompt |
| `TYPE: <command>` | type a shell command into the terminal pane | confirm-gated |
| `OPEN: path:line` | jump to a file/line (stack traces) | confirm-gated |
| `NEED: scrollback` / `NEED: tab <name>` | request more context (W5/W4) | never shown to the user; Mars re-asks **once** with the extra source (`need_depth` hard-capped at 1 — a loop is structurally impossible) |

`parse_directive` tolerates markdown noise and post-directive sign-offs;
`strip_reasoning` removes `<think>…</think>` blocks from reasoning models on every
response. Failures are typed so the cascade can react: `RateLimited` (429 → rotate
to another keyed provider at the same tier) and `ModelUnavailable` (a retired model
→ fall back in-tier).

**`tiers.rs`** is the routing table under all of that: agent tasks are not equally
hard, so the ring maps *task class → tier (`low`/`mid`/`high`) → concrete model per
provider*, editable at `~/.config/mars/tiers.json` (written with annotated defaults
on first run). An explicit `MARS_LLM_MODEL` always wins — a deliberate model choice
is never second-guessed, which is also what pins the eval to one model. Two runtime
moves complete the cascade, both disabled by an explicit pin: *rotation for limits*
and *escalation for quality* (an `ask` whose `RUN:` fails the registry check retries
once one tier up, via `model_above`).

**`prompts.rs`** — every instruction the binary sends to a model lives as one of the
18 `.md` files in `src/prompts/`, embedded with `include_str!`. **No prompt text
lives in code.** `{name}` substrings are placeholders the call sites fill with
`.replace()`, and user/screen-derived content is always substituted *last* so
injected text is never re-scanned for placeholders. The selfcheck asserts each
template still carries its placeholders.

**`persona.rs`** — the voice seam. A user-editable `~/.mars/persona.md` rides into
VOICE tasks (ask, watch) as the **final** system message, positionally under every
rule it is forbidden to override; FORMAT tasks (translate, naming, mission,
cursor-insert) never see it, because their output is machine-parsed. Hot-read on
every prompt assembly, per-line redacted, hard-capped.

### `retrieval.rs` — memory (feature `memory`)

Lightweight retrieval over Mars's *own* context, and the substrate for the two eval
axes. Deliberately simple — lexical BM25, no embeddings: the claim is that sitting
at the terminal and retrieving the user's own commands beats a generic model, not
that the retriever is fancy. Two corpora, both ranked by `rank` and injected by
`agent.rs`: **(A) command memory**, the `(request → accepted_command)` pairs the
user actually ran, injected as few-shot into shell translation; **(B) system
knowledge**, Mars's docs + action registry + tuning descriptions, injected into
`ask` so the agent can answer about and reconfigure itself. `MemoryMode`
(`MARS_MEMORY`) selects the active variant so the eval can ablate implementations.
A hot-read denylist plus `redact` scrub secrets from anything stored or sent.

### `syntax.rs` — highlighting (feature `syntax`)

syntect with the pure-Rust `fancy-regex` backend (no Oniguruma, so it
cross-compiles and builds on Windows with no C toolchain). Colors are **synthesized
from the active theme `Palette`** — a keyword is `accent`, a string is `success`, a
comment is `text-faint` — so a theme restyles code by construction rather than by
shipping a second color table. Grammars are syntect's bundled set plus any
`.sublime-syntax` dropped in `~/.mars/syntaxes/` (the runtime language-pack seam).
`highlight_stream` feeds results back in chunks so a large file colorizes
progressively; see §4 for the cache-coherence rules that keep it flash-free.

### `worklog.rs` + `briefing.rs` — the journal and the shift report

**`worklog.rs`** is the work journal: watch verdicts persisted as a stream of "what
was happening" snapshots, deliberately separate from `llm_log` (that log is about
the cost and behavior of LLM calls; this one is about the user's work). The
`WorkEntry` schema doubles as the per-command ledger the OSC-133 scanner writes, so
the two stores never fork — a watch verdict is just a ledger entry whose verdict is
LLM-compressed. It also persists missions, goals, and previous briefings, and
`compact` bounds the file. Consumers: `mars ls`, the mission line, the notices
digest, and the shift report.

**`briefing.rs`** is the save-state restore. Everything in it is pure and
deterministic — **tier 0 of the triage ladder**: exit codes, durations, and
tail-shape heuristics produce honest `ReportRow`s with zero LLM involvement,
ordered by `Verdict` (what needs you first). The model only ever *replaces* a
defensible placeholder: ambiguous rows go, batched, to a low-tier model **after**
the overlay is already on screen. A frame is never blocked on a network call.

### `llm_log.rs` — observability

With `MARS_LLM_DEBUG=1` (or `mars --llm-debug`), every `chat()` call appends a JSON
line to `~/.mars/logs/calls.jsonl`: task, provider, model, real input/output token
counts, latency, and the full prompt and reply. Accept/edit/reject outcomes for
shell translations are recorded alongside. `mars llm-stats` aggregates it into a
per-task×model profile ranked by token consumption, with `--daily`, `--since`,
`--raw`, and `--json` views — so you can see where the budget goes and right-size
the model (or trim the prompt) for each kind of call.

### `broker.rs` + `ssh.rs` + `fleet.rs` — key-never-leaves-home (feature `ssh`)

`broker.rs` owns the portable JSON request protocol, the `mars keyd` service, and
the remote chat proxy. keyd runs on your home machine, holds the LLM key, and
answers `Chat` requests arriving through `sys::control`: a protected Unix socket on
Unix or token-authenticated loopback TCP on Windows.

`ssh.rs` owns system-OpenSSH lifecycle and remote POSIX command construction.
Unix retains connection multiplexing. A Windows home uses a per-invocation
capability relay and `-R remote-unix-socket:local-tcp`; the relay authenticates
the remote bytes, then opens the protected local keyd channel. The current socket
and capability travel in the session `Hello`, so reattaching a persistent remote
daemon replaces its dead prior tunnel route. SSH child environments explicitly
remove provider credentials before OpenSSH can apply user `SendEnv` rules. A
separate prelude stages the embedded installer (`install.sh`, `include_str!`'d) and
runs it only for a missing or handoff-incompatible remote Mars; Windows may
therefore authenticate twice.

`fleet.rs` is deliberately *outside* the feature gate: the host registry behind
`mars ls` is portable JSON state under `~/.mars`, refreshed by the broker's status
push, and a build without `ssh` still lists whatever the file holds.

### `session.rs` — persistence as a process split

The tmux-style client/server implementation. The wire protocol is newline-delimited
JSON over a platform-local control stream: a mode-0700 Unix-domain socket on Unix,
or nonce/HMAC mutually-authenticated loopback TCP with a rendezvous file on Windows.
Addresses normally live under the platform temp directory in `mars-<user-tag>`; the
`MARS_RUNTIME_DIR` base override makes selfcheck isolation explicit.
Control probes distinguish live, definitively dead, and indeterminate endpoints,
so an upgrade or authentication timeout never unlinks a live daemon's address.
Client→server is `ClientFrame` — `Hello{cols,rows,version,broker_*}` (strict
protocol-qualified version handshake plus optional live SSH broker handoff),
`Key`/`Mouse`/`Paste`/`Resize`, plus one-shot control frames
`Status`/`Kill`/`Rename` used by `mars ls`/`kill`/`rename`. Server→client is
`ServerFrame` — `Output{b64}` (one rendered frame's ANSI bytes), `Exit{message}`,
`Status`.

- **`server_main`** (the daemon): owns the `App`, accepts connections on a
  listener thread, and runs draw → `tick` → recv. The ratatui terminal writes
  into a `FrameWriter` — a `Write` impl that buffers a frame and ships it as one
  `Output` on flush, marking the client dead on IO error (2s write timeout) so a
  wedged client can never stall the session. With no client attached there is
  simply no terminal to draw to; `tick` keeps running, which is what keeps PTYs,
  autosave, and watch summaries alive while detached. Attach triggers
  `App::on_attach` (the shift report); disconnect triggers `on_detach` +
  autosave. Generation counters on connections guard against a stale client's
  disconnect or input affecting its successor. The one-shot `BrokerRoute`
  control frame lets Mars subprocesses in persistent PTYs resolve the current
  attach's socket and capability rather than their inherited environment; an
  immutable instance ID keeps that lookup valid across session renames.
- **`client_main`**: owns the real TTY (raw mode, alt screen, mouse, bracketed
  paste, kitty flags), one thread pumping `Output` frames to stdout, one loop
  serializing input events to the socket. One client per session; a new attach
  sends the old client a clean takeover `Exit`.
- **`session_main`**: attach-if-alive, else spawn `mars --server <name>` fully
  detached (`setsid` on Unix, detached process flags on Windows; stdio goes to the
  per-session postmortem log), wait for the address, attach. Live rename moves the
  address file without disturbing the already-bound listener or attached clients.
- **TTY hygiene**: `sanitize_tty` (idempotent raw-mode repair) and a panic hook
  that restores the terminal before the panic message prints.

### `sys/` — the platform abstraction layer

The ONE place the operating system leaks in. Every capability is reached as
`sys::<capability>`; **no module outside `src/sys/` may name `std::os::unix`,
`std::os::windows`, `libc`, or another OS API** — enforced in CI by
`tools/check-platform-isolation.sh`. The adapter is chosen at compile time, and each
adapter exposes the same modules with the same signatures — that shared signature
*is* the port. The abstraction is over *capabilities* ("a named local channel",
"where my files live", "spawn a detached process"), never individual syscalls:
`paths`, `control`, `tty`, `daemon`, `proc`, `fsperm`, `shell`. See
`WINDOWS_PORT.md`.

### `project.rs` — the file index

A bounded, lazily built, session-cached walk of the project root (nearest
ancestor with `.git`), skipping dotdirs and the `tuning.project_ignore` list,
capped at `project_index_max` files. Feeds the `@` file tree's fuzzy filter.
(v1 is a skip-list, not `.gitignore`-aware — the `ignore` crate is the
documented upgrade path.)

### `banner.rs` — the splash

Generated ANSI art (`BANNER_LINES`, truecolor SGR escapes) with `print_banner`
for `mars version`; the TUI splash parses the same lines into ratatui spans via
`ui::ansi_to_line`. `MARS_BLOCK` is the uncolored wordmark used under non-default
themes, where the baked terracotta would clash. Machine-generated — don't edit by
hand.

## 9. Feature flags and the deletion-proof seams

Three capabilities are cargo features, all default-on. Each has an inert twin
selected by `#[path]` in `main.rs`, so **call sites carry no `cfg`** and never learn
the capability is missing — at runtime they simply see neutral values:

| Feature | Real | Stub | Covers |
|---|---|---|---|
| `memory` | `retrieval.rs` | `retrieval_stub.rs` | command memory, docs corpus, redaction |
| `ssh` | `broker.rs`, `ssh.rs` | `broker_stub.rs` | `mars keyd`, `mars ssh` |
| `syntax` | `syntax.rs` | `syntax_stub.rs` | highlighting (drops the syntect dep entirely) |

`cargo build --no-default-features` must also pass `--selfcheck`. Changing a real
module's public surface means mirroring it in the stub in the same commit.

## 10. Threading model

The main thread owns `App` exclusively — there are no locks around editor state.
Everything else is a producer on an mpsc channel:

| Thread | Created by | Sends |
|---|---|---|
| TTY reader (standalone) / socket connection threads (daemon) | `main.rs` / `session.rs` | `InputEvent` / `SrvEvent` |
| One PTY reader per terminal pane (also runs the OSC-133 scanner) | `terminal::spawn` | `TermEvent::Output/Exited` |
| One process watcher per terminal pane | `terminal::spawn` | `TermEvent::Exited` |
| One thread per LLM call | `agent.rs` entry points | `AgentEvent`s (streaming deltas, then one terminal event), then exits |
| Syntax highlight worker | `syntax::highlight_stream` | `SyntaxEvent` chunks |
| Client frame pump | `client_main` | decoded ANSI → stdout |

The only shared-state exceptions: each terminal's `vt100::Parser` sits behind an
`Arc<Mutex<…>>` (written by its reader thread, read by the renderer), and the
daemon shares two atomics (`attached`, a connection generation counter) with its
connection threads. Background agent work is additionally serialized by a single
`bg_busy` slot in `App`, and foreground asks preempt it.

## 11. Testing

`./target/debug/mars --selfcheck` is the primary suite (see §3 and `AGENTS.md`):
headless, hermetic, no mocks, real PTYs, a real daemon over a real socket. Extend
it for new behavior rather than adding a parallel harness. **Both feature
configurations must stay green** — the default build and
`cargo build --no-default-features`.

CI (`.github/workflows/ci.yml`) runs both configurations on ubuntu-latest and
windows-latest, plus `tools/check-platform-isolation.sh`.

What the suite *cannot* verify — real terminal byte encodings, kitty-protocol
negotiation, and `setsid` process-detachment — needs a manual real-terminal pass
(`DESIGN.md` §9).

## 12. Deferred, by design

Two substrates are designed (`design_ideas/workflows_eng.md`) but deliberately
unbuilt because no shipped feature needs them yet: the **Context Bus registry**
(formalizing `screen_context` into consented `ContextSource` objects) and
**parameterized actions** (`RUN: FindFile("x")`), which gates multi-step agent plans
and itself waits on a cross-buffer **transaction journal** (reversibility before
autonomy). Injecting shell integration into spawned shells — so every pane emits
OSC-133 markers rather than only pre-integrated ones — is real-terminal-gated.
Subword motion (`⌘⌥←/→`) is a planned fast-follow.
