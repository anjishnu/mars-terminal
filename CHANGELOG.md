# Changelog

## 0.7.0

Mars grows a second head. A long agent run doesn't need you at the keyboard — it needs
you to notice when it stops — so this release puts your running sessions on your phone,
and gives the machine a manager that watches them while you're away and reports in plain
English when you come back. Everything the phone can do is something you press; nothing
acts on its own.

### Added
- **Name the workspace you are working in.** A workspace called `terminal 3` that has
  spent the morning on a migration tells you nothing from a phone. Three things can now
  rename one, and none acts alone: a rename row in Rover's side menu for the workspace
  you are standing in, a `rename` card Rover chat can offer, and — after each manager
  run — a suggested name on the workspace's own pane, adopted with one press. Taking a
  suggestion and dismissing it are the same act: both are recorded against the
  workspace, so it does not come back until the manager has a *different* name to
  propose. Nothing measures "divergence"; a different name is what divergence means.
- **Workspaces have a durable id.** `<unix-secs>-<directory>`, minted when the shell is
  spawned and carried across `mars reboot` in the restore manifest. Per-workspace files
  — the manager's summary, a conversation gist — are named after it instead of the
  pane's runtime handle, which is a counter that restarts at zero in every daemon. A
  reboot that dropped a middle workspace used to shift each of those files onto its
  neighbour, silently.
- **A tunnel is verified before you are handed a QR.** `mars pair --check` and
  `mars pair --link` fetch the public URL from outside and require this bridge's own
  header in the answer. ngrok's local API reports the agent's belief, so a tunnel whose
  edge session has died still lists a healthy URL — the state that looks fine at the
  desk and unreachable from the road. A failed probe is reported, never acted on: a
  reply proves the path, a silence could be this laptop's own network.
- **Rover — your sessions on your phone.** `mars pair` prints a QR; scan it and the
  sessions on that machine are readable from a pocket. There is no account and no cloud
  service holding your data: the code carries a one-time link to *your* daemon over a
  tunnel that closes when you stop the bridge. You get the **Mission Briefing** in the
  mission-control voice, a **board** of every workstream with a plain-English "why" under
  anything that failed, and **live panes** with real scrollback — answer a `[y/N]` with a
  button, type a line, or drive a TUI with an on-screen arrow pad. URLs and commands the
  agent printed become tappable chips that copy cleanly, because selecting text on a
  phone terminal is miserable. `mars pair --check` reports what is set up and what isn't,
  with the fix for each, rather than failing quietly.
- **One pairing covers the host.** Every session on that machine appears in the fleet
  list and you switch between them without re-scanning — one port, one tunnel, one token,
  with each phone routed to the session it asked for.
- **Rover chat.** Ask about the machine in plain language ("why did the build fail?").
  It is a real Claude Code session that reads the repo and the panes, so the answer is
  what is actually true rather than a fluent guess — and it can *offer* to act: open a
  file, start a workspace, run a command, write a note. Each offer is a card you press.
  The agent behind it is **read-only by construction**; every effect is a proposal.
- **A manager agent.** Between your visits it reads what the panes did, writes a mission
  briefing and short **memos** — a stuck deploy, a credential about to expire — and scores
  its own runs. Memos are assignable: point one at a worker and it starts on the job.
  `mars manager` runs a turn on demand; `agent.enabled` is the off switch.
- **`mars reboot`.** Bring a session back on the binary that is on disk *now*, restoring
  each workspace's directory and resuming the coding agent that was in it — so shipping a
  new build no longer means losing the desk you were working at.
- **`SECURITY.md`.** What a pairing token can do, what someone without one cannot, and the
  rule that governs the rest: text an agent reads is untrusted input, and the human press
  is the trust boundary.

### Changed
- **The AI, SSH and theming features leave beta.** The `?` ask flow, agent-proposed
  `RUN:`/`TYPE:` directives, refactors, triage, watch summaries and the away digest; the
  SSH broker (`mars ssh`, `mars keyd`, the fleet view, the remote installer); and the
  color themes have all been through enough releases to be part of the product rather
  than an experiment. The posture is unchanged: the agent is an assistant, not an
  authority, destructive actions stay gated, and you should still read what it proposes.
  **Rover and the Windows port are the only beta surfaces left** — both are the
  least-travelled paths here, and Rover's worst failure (a laptop that closed while you
  were out) is the hardest to test.
- **The bridge follows the session, not the process.** It resolves through the session's
  durable directory, so a rename or a daemon restart no longer strands a paired phone —
  and it upgrades itself in place, with nothing above it to drift out of date.
- **The manager view is computed on request** rather than stored, which removes the whole
  class of stale-index and concurrent-writer bugs that a cached view invited.
- **Session artifacts live in `~/.mars/sessions/<id>/`**, keyed by id rather than name, so
  renaming a session no longer orphans its history.

### Security
- **The agent cannot rewrite its own standing orders.** `run.sh` denies it the tool, Mars
  compares each instruction doc against the built-in before every tick and refuses to run
  on unblessed drift, and a doc claiming a version we did not write is replaced rather
  than trusted.
- **Agent-authored strings can no longer reach a shell.** A restored conversation id is
  validated rather than escaped; a memo whose *filename* is not a name is never turned
  into a card; and the phone refuses to type a path that is not a path.
- **Assigned workers cannot edit what executes without being run** — build scripts, CI
  workflows, git hooks, manifests, and `.claude/**`.
- The pairing token is compared in constant time, stored `0600`, and rotated by
  `mars serve --reset`; model spend has a ceiling; and the filesystem surface caps writes
  and refuses anything outside `$HOME`.

### Fixed
- **A reboot no longer restores the wrong conversation.** An agent pane with a known id
  resumes exactly that thread; where no id was captured, at most one pane per directory
  falls back to `--continue`, and the rest come back as bare shells. Three panes in one
  repo used to resume three copies of a single conversation and silently lose the other
  two — and a wrong conversation looks exactly like a right one.
- **Each phone sees only its own session's memos and briefings.** The manager is one agent
  across many sessions, and its view used to aggregate them.
- **Every quit leaves a death note**, so a session that vanished can say why.

## 0.6.0

Mars becomes usable with the mouse without ever punishing the keyboard: the bottom
bar and navigator turn into real, clickable, hover-lit surfaces, and mission control
gains a host-health readout.

### Added
- **Clickable, hover-lit chrome.** The bottom-bar hint chips are now real buttons,
  routed through the same `run_action` funnel as chords — so the confirm gate and
  frecency apply to a click exactly as to a keystroke. They **lighten on hover** and
  **recess when pressed**, and every click **flashes the chord it stands for**
  (`↦ C-x C-s`) — the mouse as an on-ramp to the keyboard, not a replacement. Tabs,
  command/workspace/tree rows, and dropdown rows all light on hover too.
- **Navigator preview.** A click in the file navigator now mirrors keyboard
  navigation: it highlights the row, keeps focus in the tree, and **previews the file
  into one reusable tab** — exploring a directory swaps that tab instead of piling up
  a dozen. The first edit **pins** the tab (VS Code's italic-tab rule), so the next
  click starts a fresh preview. Clicking a **pane** focuses it, and the **wheel scrolls
  the terminal under the pointer** — the navigator is no longer a keyboard-only trap.
- **Host-health line in SPACES.** The top of the SPACES panel shows session **uptime**,
  1-minute **load**, host **memory %** (smoothed over a few minutes), **free disk** on
  the working dir, and **GPU memory %** on machines with `nvidia-smi`. Any probe the OS
  can't answer is dropped, so the line self-trims. Probes live in the platform
  abstraction layer; the GPU poll runs off-thread so it never hitches the UI. Gated by
  `health_line` (default on) and `health_sample_secs`.
- **Tab hover shows workspace status** (`build · running`), and a **tooltip** surfaces a
  clipped tab's full name.

### Changed
- The redundant idle **control row** at the very bottom is gone — its commands lived on
  the status bar already, and clicking its empty space silently opened the command bar
  with no visual cue. The status bar's chips are the real, visible buttons now.
- The **Terminal** bar carries only what makes sense from a shell — `commands`, `warp`
  (`C-t`), and `editor` (`C-g`, now a real clickable target) — dropping the low-value
  `type to shell` reminder and surfacing space-warp.

## 0.5.2

Syntax highlighting — on by default, and built to never get in the way — plus a
smoother first run and sharper agent context.

### Added
- **Syntax highlighting (on by default)** — code in editor panes colorizes against
  the active theme. It runs on a **background worker** with a per-buffer cache, so the
  render path never blocks: a file paints plain instantly, then colorizes
  line-by-line as the worker delivers (the visible window first). Colors are
  synthesized from the theme palette, so switching themes restyles code too. Toggle
  per session with `C-x C-h` or the **Syntax highlighting** entry in the command bar;
  `syntax_highlight = 0` starts it off.
  - **Edits stay smooth.** The cache updates in place (never cleared, so nothing
    flashes to white), and Enter/Backspace **splice** the cached colors so they travel
    with their characters instead of shifting out of alignment. A debounced full
    reparse (`syntax_recolor_ms`, default 150ms) catches up once typing pauses.
  - **Languages**: syntect's bundled grammars plus a **bundled TypeScript** grammar
    (`.ts`/`.tsx`/`.mts`/`.cts` — syntect ships JavaScript but not TS). Drop a
    `.sublime-syntax` in `~/.mars/syntaxes/` to add more, no rebuild.
- **`mars setup`** — how to get a free API key (Groq or Gemini) and wire it in, with a
  live status line. The same guidance now appears at every no-key moment: after
  `install.sh`, from headless commands that need a key, and as a dismissible notice
  when the editor launches unconfigured.
- The **English→shell translator** now sees an `ENV` line (shell, OS, working
  directory), so it fits commands to your environment — macOS BSD vs GNU tools, `brew`
  vs `apt`, and the right shell syntax.

### Changed
- **Away/watch verdicts** name the process's **exit code** and how long it had been
  quiet; **explain-failure** ("why did this fail?") now includes the failing command
  and its exit code; **cursor-insert** tells the model the buffer's language — so each
  reads the situation it's actually in.
- **Closing a clean (unmodified) editor pane skips the confirmation** — only unsaved
  work asks. Opening a file from the **navigator opens a new tab** rather than
  replacing the current pane.

## 0.5.1

Themes, editor polish, and observability. The look is now fully tokenized, so a
color theme repaints the whole UI at once — plus a batch of editor-feel refinements
and richer `llm-stats`.

### Added
- **Color themes (beta)**: every colored cell resolves through one of 17 semantic
  tokens (accent, info, danger, text, border, surface, …), so a theme repaints the
  **whole** UI at once — panes, overlays, the terminal canvas, reading-mode, the
  splash, and the mission briefing (whose MARS wordmark takes the theme's accent).
  Four bundled themes: **Mission Control** (default, unchanged), **Eclipse** (bold
  electric high-contrast), **Paper** (warm light), **Hacker** (green-on-black). Pick
  one from the **Theme ▸** submenu in the command bar (applies live), or with
  `mars theme <name>` (recorded in `~/.mars/config.json`); `mars theme list` shows
  them. The picker reads live from disk, so a token→color JSON you drop in
  `~/.mars/themes/` just appears. A colored theme paints a solid background
  everywhere; the default honors the terminal's own background; `opaque_background = 0`
  forces transparency under any theme. Terminal panes follow the theme's base fg/bg
  while a program's explicit colors pass through. Custom `theme_*` tuning knobs still
  override per token, so existing customizations are untouched.
- **Current-line highlight** — a subtle tint on the cursor's row
  (`highlight_current_line` / `current_line_bg`).
- **Passive matched-bracket highlight** — the bracket at the cursor and its match
  render bold-accent.
- **Markdown reading-mode** — a read-only, reflowed view (tables, wrapping) capped
  at `reading_width` (default 90 cols) and centered; skin in a clay → sandstone →
  light-teal hierarchy; the wheel scrolls the document.
- **Live elapsed on the workspaces board** — a running workstream shows a
  seconds-precision counter (`4m 12s`) that ticks while the bar is open.
- **Navigator: `Ctrl+Space` on a folder re-roots into it** (descend — the mirror of
  `../`), and dotfiles now show by default (`.` still toggles).
- **`mars config`** — show the global config file and its contents.
- **`llm-stats` gains `--json`** (scriptable: rows + a per-day series), **`--daily`**
  (a day-by-day token-trend chart), and **`--since 7d`** (a trailing window; also
  `12h`, `30m`).
- The space-warp panel has a `@ · go to the navigator` row.

### Changed
- **Config moved to `~/.mars/config.json`** (alongside worklog/briefings/logs),
  replacing the project-local `.mars` rc — same `env`-override schema, now with a
  `theme` field. The real environment still wins.
- The mouse wheel scrolls the **viewport** in a normal editor and the **document**
  in reading-mode (was moving the cursor / a no-op).
- The space-warp WARP panel uses neutral grey/white chrome instead of teal.

### Removed
- The red ● REC status-bar chip (LLM logging is a persistent config state now).

## 0.5.0

The monitoring release: MARS watches the whole fleet and tells you what needs
you. A workspace ledger records every command's outcome, an ambient monitor
turns that into a needs-you-first board, and the command bar becomes mission
control — a workspaces board beside the launcher, one status bubble per
workstream, a plain-English summary on demand. Plus a termimad-powered Markdown
reading-mode, a unified space-warp navigation grammar, and a calmer, quieter UI.

### Added
- **Workspace ledger (Movement 1)**: OSC-133 command-boundary capture records an
  exact per-command entry — cwd, the command that ran, its exit code — into a
  Notice-shaped ledger, and a tier-0 deterministic engine reaches verdicts from
  it with zero model calls. This is the substrate the monitor reads.
- **Ambient workspace monitor**: the fleet's state surfaces without arming
  anything. Every workstream is ranked needs-you-first — blocked ⏸ and failed ✗
  at the top, then running, done, idle — so "anything need me?" is answerable at
  a glance.
- **Workspaces board in the command bar**: the bar splits into a workspaces
  board (reach it with ←) beside the commands launcher (→). The board is a
  full-height titled box; the empty sky below the list fills with a still,
  dim starfield, and each workstream carries a wrapped, plain-English status
  line with a teal rail.
- **On-demand summary** (`s`): select a workstream and pull a one-line "what is
  this doing?" — a single low-tier model call, guarded against excess firing
  (one in-flight at a time + a freshness gate), with a deterministic fallback
  when no API key is set.
- **Consistent status bubbles**: one ● bubble per surface, colored by state
  (amber = blocked, red = failed, green = running, teal = done, grey = idle),
  in both the tab bar and the board — position and color, never glyph soup.
- **Space-warp navigation** (`C-t`): one directional grammar walks the whole
  workspace — arrows / `hjkl` step between panes and spill into the adjacent tab
  at the edges, `1`–`9` jump to a tab, `z`/space zoom, `|`/`-` split, `d` close,
  `x` swap, `@` jumps to the navigator. Inside travel mode every verb is a bare
  key. A titled, teal-bordered WARP box shows the live grammar.
- **Markdown reading-mode**: toggle any editor pane into a read-only, reflowed
  document — real wrapping, tables, nested lists (via termimad) — dressed in the
  MARS palette (teal headings, accent bullets, lightened-teal code). The title
  shows a position %, and the document scrolls with the editor's own motion
  grammar (↑/↓ and `C-n`/`C-p` by line, `⌥↑`/`⌥↓`/`⌥v`/PgUp/PgDn by page,
  `M-<`/`M->` to the ends), clamped exactly to the rendered length.
- **Navigator dotfiles**: `.` toggles hidden files in the navigator so the
  important things in hidden folders are reachable (knob `tree_show_dotfiles`).

### Changed
- **The command bar is a board, not a list**: needs-you-first ranking, honest
  per-row content (verdict · command · exit; "summarizing…" while a summary
  runs), and a padded, teal-railed summary section.
- **Informative workspace names**: the tab bar shows "terminal N" and filenames
  instead of bare numbers, and idle tabs recede to grey.
- **A calmer sky**: the workspaces starfield is a still, dim scatter — no
  twinkle, no drifting comet, and therefore zero idle repaints while the bar is
  open.

### Removed
- **The hand-rolled Markdown prototype**: termimad reading-mode won the A/B, so
  the older line-aligned renderer and its `markdown_engine` knob / `m` engine
  toggle are gone.
- **The top-right status counter (beacon)**: status now lives in the tab labels
  and the board, not a corner tally.

### Fixed
- Idle terminals no longer render green (Running) — the verdict gates on recent
  output, so a quiet shell reads as idle.
- Markdown code color lightened (dark teal was near-invisible on a dark
  background); emphasis carries a warm color so italics read even on terminals
  that don't render the italic attribute.
- The reattach briefing keeps its deterministic report when the LLM enrichment
  call fails, instead of blanking.
- Resilient model-tier ring: tiers hold a list of models and rotate off retired
  ones, so a decommissioned model name no longer stalls a tier.
- Packaging: ship `src/tiers_default.json` in the crate `include` list so a fresh
  `cargo install mars-terminal` compiles (it's embedded via `include_str!`).

## 0.4.0

The mission-aware release: reattaching becomes a save-state restore narrated by
mission control, the assistant gains a configurable voice, and the work journal
starts carrying outcomes, not just verdicts.

### Added
- **Mission Briefing**: reattach to a session where things happened and the
  screen boots up like a console coming online — the MARS wordmark, a mission
  clock (`T+ HH:MM:SS`) and a status ribbon (`✗2 ⏸1 ✓3`), then a plain-English
  situation report in the mission-control voice that types itself in behind a
  cursor ("Welcome back, captain. The trainer went down at epoch 3 — CUDA OOM,
  needs a smaller batch before you relaunch. The build came home green."),
  then a systems-board manifest of every workstream (failures first, then
  blocked ⏸, done, running) with a left severity stripe and a "why" line
  (cwd · exit · error) under anything that failed. A long run that finished
  clean earns a ★ and renders in teal; the briefing closes with a one-line
  sign-off. Each briefing is logged, so the next return reports progress
  against the last ("the OOM you were chasing is still red"). The prose is one
  low-tier call that streams into an already-on-screen frame — zero perceived
  latency — and any key resumes exactly where you left off. Shows only when
  something happened. Knobs: `mission_briefing` (2 = full screen [default],
  1 = one-line notice, 0 = off), `mission_briefing_animate` (boot-up vs.
  instant, for thin SSH / reduced motion), `mission_briefing_type_ms`
  (typewriter speed).
- **Goal tracking**: when you detach, the agent captures what you were working
  toward (from the live panes + recent journal), so the reattach briefing
  reports progress against it — "you were trying to get the auth test green;
  it's still failing." Knob `goal_tracking` (default on).
- **Verdict triage ladder**: watch verdicts now escalate one way — free
  deterministic heuristics (exit codes, error/blocked/progress tail shapes),
  then ONE batched low-tier model call for ambiguous rows only. A mars with no
  API key at all now produces deterministic verdicts instead of silence, and
  the report renders instantly with model text streaming in afterwards
  ("telemetry coming in").
- **Auto-watch**: panes that stay busy past `watch_min_active_secs` (10s) are
  watched automatically — the fleet reaches verdicts without arming anything.
  The pane you're looking at is never summarized. A long run that finishes
  clean now surfaces as a win (teal ★), not just failures. Knob `auto_watch`.
- **Blocked verdicts**: a pane waiting on your input is its own class (⏸),
  sorted right after failures in notices and the report.
- **Persona**: the assistant speaks in a configurable voice
  (`~/.mars/persona.md`, "Open persona" in the command bar) — default: mission
  control addressing the ship's captain, in plain sentence case. Style only: it
  structurally cannot change what the agent does. Empty file turns it off.
- **Outcome-carrying work journal**: watch records now include cwd, the
  command mars ran, the exit code, and a redacted error excerpt on failure —
  the substrate for failure→fix recall. Journal self-compacts
  (`worklog_max_lines`).
- **AWS Bedrock + Azure OpenAI/Foundry**: MARS now speaks to enterprise model
  gateways. Set `AWS_BEARER_TOKEN_BEDROCK` (+ `AWS_REGION`) to use any Bedrock
  model through the Converse API, or `AZURE_OPENAI_API_KEY` +
  `AZURE_OPENAI_ENDPOINT` (+ `MARS_AZURE_DEPLOYMENT`) for Azure. Bearer/api-key
  auth only — no AWS SigV4, so the single static binary stays dependency-light.
  Both slot into the provider cascade (rotation + tiering) and work over the ssh
  broker with the key never leaving home. (Bedrock is non-streaming for now.)
- **Open tuning knobs** joins the command bar.

### Fixed
- **`mars ls` summaries were often blank, stale, or rambling**: the column read
  files only a fire-and-forget LLM call writes, so a skipped or failed call left
  it empty — and a days-old, verbose model verdict could show as if it were
  current state. Now every headline tier is age-gated (a stale line ages out),
  rambling verdicts are trimmed to their first clause, and a deterministic floor
  (`dir · command · ago`) keeps a live session's column from ever going blank —
  no LLM call required. While a fresh summary is being generated at detach, the
  column shows `…summarizing…` until it lands. The detach-time capture also no
  longer loses to a concurrent watch summary.
- **The reattach briefing never appeared after a normal detach**: the intended
  `C-x C-c` quit-detaches path didn't snapshot session state, so the save-state
  restore had nothing to diff against. Only an accidental disconnect armed it.
  Now both do.
- **Auto-watch flooded the journal with "user quit"**: a clean shell exit is the
  user leaving, not work — it's now silent, so the briefing and `mars ls` stop
  narrating lifecycle noise.
- Two panes concluding while detached no longer lose one verdict (the pending
  trigger queue was a single slot).
- Translate calls now actually route through their intended model tier (the
  task tag said "shell", the tier map said "translate" — nobody won).

## 0.3.3

### Added
- **Copy that works over ssh (OSC 52)**: every copy — editor kills, `C-c`,
  terminal mouse selection — now also emits an OSC 52 escape to the real
  terminal, so text copied inside a remote mars session lands on the clipboard
  of the machine you're sitting at. (Previously the daemon wrote to the remote
  box's clipboard, which over ssh is the wrong machine — usually a headless one
  with no clipboard at all.) Requires a terminal that supports OSC 52: iTerm2
  (enable "Applications in terminal may access clipboard"), kitty, WezTerm,
  Alacritty, Ghostty. macOS Terminal.app does not support it.
- **`mars killall` is now the reset button**: gracefully ends every session
  (autosaving), force-kills unresponsive daemons and the key broker, shuts down
  lingering ssh ControlMasters, and sweeps every stale socket. Memory files
  (command memory, worklog, denylist) are untouched, and it no longer starts a
  new session afterwards.

### Fixed
- **Reconnecting no longer breaks the agent tunnel**: reattaching while the ssh
  ControlMaster was still warm deleted the live forwarded socket (the sweep ran
  unconditionally) and the re-requested forward was a mux no-op — leaving the
  remote agent with "no API key". The sweep and the forward request now only
  run on a fresh connection; a reused master keeps its working tunnel.

## 0.3.2

### Added
- **`mars ssh` lands in a mars session**: instead of a bare login shell, you
  arrive inside a remote mars session — the most recent live one, or a fresh
  `main` — with the auth tunnel exported to the session daemon and every shell
  it spawns. Detaching (`C-x C-c`) ends the ssh and returns you to your home
  terminal, tmux-style. Plain `ssh` remains the way to get a bare shell.

## 0.3.1

Hardening release: `mars ssh` now recovers from the leftovers of a dead session
instead of failing on them.

### Fixed
- **Stale auth-socket sweep**: a previous session's leftover
  `/tmp/mars-auth-<uid>.sock` on the remote made the reverse tunnel fail to bind
  (with a confusing double password prompt). The ssh prelude now removes it before
  the forward is requested, and the remote side unlinks a dead socket when it finds
  one — no `sshd_config` changes needed.
- **Honest install detection**: the "[mars] not installed here" nudge checked
  `command -v` under sshd's bare non-login PATH, so a cargo-installed mars was
  reported missing on every connect. The check now probes `~/.cargo/bin` and
  `~/.local/bin` directly.
- **No dead-tunnel pinning**: a remote mars that finds a dead auth socket now falls
  back to its normal provider chain instead of sending every agent call into an
  unreachable broker.
- **Cross-uid socket discovery**: the forwarded socket is named with the home
  machine's uid (a Mac's 501), which rarely matches the remote's (Linux's 1000) —
  the remote now scans for any live `/tmp/mars-auth-*.sock` instead of guessing by
  its own uid, so the agent works in shells without `MARS_AUTH_SOCK` exported
  (cron, plain ssh, nested sessions).
- **Honest tunnel status**: `mars ssh` opens the remote shell with
  `[mars] agent tunnel ready` (or a warning if the forward failed) — a working
  connection is no longer indistinguishable from plain ssh.
- **ControlMaster keepalives** (`ServerAliveInterval=30`): a master whose TCP died
  (laptop sleep, network change) exits on its own instead of answering `-O check`
  and then breaking the next connection with "Broken pipe" + a surprise password
  prompt.

## 0.3.0

Agent quality-of-life batch: streaming, a work journal, and a memory subsystem you
can rip out.

### Added
- **Streaming replies**: agent answers render token-by-token in the ask panel
  (SSE for OpenAI-compatible and Anthropic providers), with reasoning-model
  `<think>` blocks stripped incrementally so they never flash on screen.
- **Work journal + mission**: watch-mode frame summaries are logged as work
  snapshots (`~/.mars/worklog.jsonl`); a low-tier model periodically infers the
  session's mission, which `mars ls` shows as the summary column and reattach
  opens with a "Where you left off" briefing.
- **Unified `mars ls`**: local sessions and fleet hosts in one numbered table with
  a shared open prompt; remote agent calls self-report host + session so status
  stays fresh.
- **Model cascade, completed**: rotation across keyed providers on rate limits and
  one-step escalation to a stronger tier on low-confidence answers.
- **Memory hygiene**: secret redaction (credential prefixes, `password=`-style
  values, URL credentials, a user-editable `~/.mars/denylist`) on every line
  bound for a prompt; recency/cwd-weighted retrieval; in-editor actions to open,
  inspect, and clear the command memory.
- **Deletion-proof memory seam**: the whole retrieval subsystem sits behind a
  default-on `memory` cargo feature; `cargo build --no-default-features` yields a
  fully working memory-free terminal.
- **Prompts as Markdown**: every model-facing instruction lives in
  `src/prompts/*.md`, embedded at compile time — editable without touching code.
- **Command bar overhaul**; `quit` now detaches (with `killall` for a hard stop).

### Fixed
- Mouse-wheel scrollback now reaches full-screen terminal apps (Claude Code, less,
  vim): wheel events are forwarded in the app's own mouse protocol, or translated
  to DECCKM-aware arrow keys on the alternate screen.

## 0.2.0

The first substantial release since 0.1.0 — remote agents, a unified terminal
composer, reattach briefings, and a top-to-bottom ergonomics pass.

> **Beta:** the AI/agent features and the SSH/remote path are new and still being
> hardened. The core editor, multiplexer, and sessions are stable.

### Added
- **SSH broker** (`mars ssh <host>`, `mars keyd`) — **beta**, still being hardened:
  your LLM key stays home and is
  served to remote boxes over the reverse-tunneled socket, so the agent works on a
  host that has no key on it. `mars ssh` auto-starts the home broker and drops a
  self-contained `install.sh` on the remote (rustup + `cargo install`, honest Windows
  error) so a fresh box is one command from running Mars.
- **Fleet view**: `mars ls` lists recent hosts with an interactive `→ ssh:` prompt
  (ordinal / name / unique-prefix resolution).
- **Away Digest** (`C-x g`): a duration-anchored briefing of what happened while you
  were detached — runs finished, shells that exited, files that changed.
- **Unified terminal composer** (`Ctrl+Space` in a terminal): one shell-first surface
  with the red inline overlay AND a ↑/↓ menu of Mars commands. Enter runs your typed
  command; arrow into the menu to run an action instead. `!` forces pure shell, `?`
  asks the agent.
- **Terminal mouse copy**: drag-select to the system clipboard.
- **Watch a pane** (`C-x w` / `C-t w`): summarize a terminal when it goes quiet or
  exits, even while you're detached.
- Nested `mars <file>` inside a session opens a new tab instead of a nested Mars.
- **Space warp** (`C-t`): renamed travel mode, with a `T` verb to open a terminal tab.
- **Mission control** — the command bar (`Ctrl+Space` / `M-x`) is now named mission
  control on every teaching surface (start screen, help, menus).
- **Navigator** — the file sidebar (`C-x C-f`, or `@` in mission control) is renamed Navigator
  and now surfaced on the start screen and as a searchable menu row with its shortcut.

### Changed / fixed (ergonomics)
- **No orphaned shells**: closing a pane/tab now reaps its PTY (kills the child) and a
  live terminal inside prompts for confirmation first.
- **Motor-slip guards**: space-warp `d`/`q`/`0` (destructive keys next to navigation)
  confirm before closing.
- **`C-g` cancels the command bar** from every submode (was silently swallowed).
- **Honest hints**: `binding_for` teaches only chords the terminal can actually send
  (universal over kitty-only ⌘/`C-|`; canonical over aliases) — Save shows `C-x C-s`,
  Search `C-s`, Split `C-x 3`. Reattach/notice hints are mode-aware.
- **Durable failures**: autosave errors go to the persistent notice queue, not the
  status line the next keystroke wipes.
- `Ctrl+Space` opens the command bar from every mode (space warp, time-travel, tree).
- A plain terminal click no longer clobbers the clipboard; scrollback offset reflects
  real history depth.
- Idle SSH sessions no longer flush no-op redraws (latency fix).

## 0.1.0

Initial release: non-modal Emacs-compatible terminal editor, command bar, built-in
LLM agent, tmux-style persistent sessions.
