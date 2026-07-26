# The agent, providers, and the AI workflows

Prompt assembly, provider routing, directives, and the shipped W1-W7 workflows.

## Provider precedence (CURRENT — the old ARES_LLM > GROQ > GEMINI note was stale)
- `AgentConfig::from_env()` resolves, in order: a forwarded broker socket (remote box —
  proxy home, never hold a key) > `MARS_LLM_KEY` (custom URL/model, e.g. local Ollama) >
  enterprise gateways `AWS_BEARER_TOKEN_BEDROCK` / Azure OpenAI > `ANTHROPIC_API_KEY` >
  `OPENAI_API_KEY` > `GROQ_API_KEY` > `GEMINI_API_KEY`/`GOOGLE_API_KEY`. Legacy `ARES_*`
  names still fall back. Most providers speak OpenAI-compatible `/chat/completions`;
  Anthropic's Messages API and Bedrock's Converse API have their own branches in
  `chat_inner`. `tiers.rs` maps task -> tier -> model per provider; an explicit
  `MARS_LLM_MODEL` always wins.
- Gemini via its OpenAI-compatible endpoint; use the `-lite`/alias line, not a pinned dated
  version — pinned versions age out of the free tier. Newer Gemini flash models think by
  default: keep max_tokens >= 512 or answers come back empty (finish_reason: length, the
  whole budget spent on reasoning).
- `mars ask "<question>"` = the headless end-to-end agent test (prints
  provider/model/answer/RUN directive).

## `@` Groq/qwen agent (2026-07)
- Agent providers: Groq default model is now qwen/qwen3-32b (was llama-3.1-8b-instant);
  Gemini = gemini-3.1-flash-lite. Reasoning models (qwen3/R1) emit <think>…</think> —
  chat() strips them via strip_reasoning() before display+parse. RUN: parsing now takes
  only the FIRST token (qwen appends prose to the directive line); TYPE/OPEN keep the
  full rest. agent_max_tokens default bumped 512→1024 (reasoning needs headroom).
  Validated live with the user's Groq key: RUN/OPEN directives clean, triage answers well.

## Grounded agent + renames (2026-07)
- Agent is conversational (agent_history, last 12 turns sent; C-l = new chat) and
  screen-grounded: app.screen_context() (~6KB cap) = session/tabs/pane contents
  (editor visible lines + terminal vt100 contents) — the first context-bus slice.
- Directives: RUN: <ActionName> + TYPE: <shell cmd> (agent::AgentDirective, parsed
  by pub agent::parse_directive, unit-tested). TYPE → run_shell_command on explicit
  Enter. Ask panel renders the transcript (you ›/mars ›), adaptive to 60% height,
  Up/Down scroll (ask_scroll = lines up from bottom).
- Renames: RenameTab (travel r), RenamePane (Pane.title override), RenameSession
  (live socket fs::rename — bound listener follows the inode, verified; CLI
  `mars rename <old> <new>` via ClientFrame::Rename). Attached clients survive.
- Auto-naming: tabs with default numeric names only; agent::auto_name kebab-labels
  from screen context every auto_name_secs (45; 0=off); manual rename opts out
  permanently (auto_name_attempted set); user wins races (numeric-name check on
  apply). Gutter now opt-in (line_numbers knob, default false — ui::gutter_width);
  terminal chrome is theme_terminal dark teal #0D7377.

## Phase 1 agentic workflows SHIPPED (2026-07, per workflows_design.md)
- W1 ExplainThis (C-x e) + W2 ExplainFailure (C-x ?, travel ?) → ask_prefilled()
  opens Ask, seeds a canned question, auto-submits (grounded in screen_context).
- OPEN: directive added to AgentDirective (Run/Type/Open); parse_directive now
  lenient (scans last 4 non-empty lines, strips backticks/list markers). app.open_at()
  parses path:line, splits if a terminal is focused, opens + goto line + recenter.
  System prompt gained OPEN + a no-essays rule. Live-verified: triage → OPEN: app.py:87.
- W3 shell Tab-translate: in bar shell mode (Ctrl+Space !), Tab → agent::translate_shell
  → ShellTranslation event replaces the query. Rendered as render_shell_overlay
  anchored at app.cursor_screen (captured in render_panes) — cursor-anchored, no
  eye-jump. Tab is special-cased in handle_bar BEFORE the CMD/ASK toggle.
- Pane resize + zoom: layout.rs HSplit/VSplit carry a `ratio` (15-85, clamped);
  PaneLayout::resize(focused, delta) nudges the innermost split. Tab.zoomed:
  Option<PaneId>; ui render zooms to one pane and auto-clears when focus moves away.
  Travel keys: z zoom, < > resize (- is split-below, can't reuse).
- Banner: src/banner.rs = raw truecolor-ANSI BANNER_LINES (user-supplied planet art)
  + print_banner() for `mars version`. TUI splash (ui::render_splash) parses them via
  ui::ansi_to_line (handles \x1b[38;2;r;g;bm + \x1b[0m only), uniform left-pad to keep
  art aligned, fallback "M A R S" when narrow. GOTCHA: ratatui shows styled Spans not
  raw ANSI — must parse escapes to Spans or you get literal escape codes on screen.
  Splash selfcheck matches "control for your terminal" (banner is capital "Mission").
- 44 selfchecks pass. Phase 2 (W4/W5 context selectors + NEED: expansion) and Phase 3
  (W6/W7 triggers/notices/reattach-brief) still per workflows_design.md, not built.
- Shell composer activation (user rev 2): Ctrl+Space in Mode::Terminal opens the
  INLINE shell composer (BarMode::Shell) directly — no `!` needed; second Ctrl+Space
  (in the bar) → full command bar (BarMode::Command). Editor Ctrl+Space still → command
  bar. `!` from the command bar still enters shell mode (editor path). Translation is
  now Enter-driven: Enter with a key translates NL→command via agent::translate_shell
  (shell_ready flag; command lands in the pill, 2nd Enter runs); Enter with NO key runs
  the text literally; typing/backspace clears shell_ready. Tab still translates (alias).
  "does nothing" was: no GEMINI_API_KEY, or user pressed Enter (ran literal English) —
  fixed by Enter-translates-when-key-present.
- Translate STUCK bug fixed: translate_shell now ALWAYS sends one event (Error if the
  command comes back empty — Gemini thinking models can return ""); chat() got a 30s
  ureq timeout so stalls surface instead of hanging the spinner. chat() also extracts
  real API error messages — GOTCHA: Gemini's OpenAI-compat error body is a JSON ARRAY
  [{"error":{"message"}}], not an object; handle j.is_array() → j[0]. Shell overlay
  shows the error in its hint line (cleared on edit / on successful translation).
- Gutter (user feedback rev): default is now a 1-glyph POINTER gutter (▸ on cursor
  line, POINTER_W=2) not line numbers; line_numbers knob still gives the 6-col number
  column. Status bar shows "Ln N, Col N" (sole position readout). Shell overlay
  repositioned: input row sits ON the cursor row (was cy+1), no [SH !] prefix (text
  starts where the cursor was), accent-pill styling, hint line shows
  "needs GEMINI_API_KEY" when unconfigured. Tab-translate does nothing without a key —
  that's expected; user must export GEMINI_API_KEY and press Tab (not Enter) in shell
  mode (Ctrl+Space then !).

## AI workflows W6/W7/W5/W4 shipped (workflows_eng.md, 2026-07)
- Trigger/Watch framework (daemon-resident, in app.rs:tick). W6 (commit 3183471): WatchState
  per TermId fed by term_rx drain (Output resets last_output_tick+triggered; Exit queues
  pending_watch); maybe_fire_watches fires quiet/exit → agent::watch_summary (auto_name clone,
  new AgentEvent::WatchSummary) under one bg_busy gate (renamed from auto_name_inflight; user
  asks preempt via agent_pending). notices: Vec<Notice{text,kind:Failure|Info}> pull-rendered
  by render_notice (bottom line, failures first, Esc=dismiss_notice). Action::WatchPane (C-t w).
  knobs watch_quiet_secs=20, agent_scrollback_context=200.
- W7 (commit 483e8c3): Snapshot{exited,dirty,verdicts} via on_detach/on_attach hooked into
  session.rs server_main ClientGone/Attach arms. on_attach diffs → one "while away — …"
  notice (deterministic, no key; absent if nothing changed). Pairs with W6 (detached verdicts).
- W5/W4 (commit 74d1130): AgentDirective::Need(NeedKind{Scrollback,Tab(String)}) — read-side,
  parsed by match_directive, taught in system_prompt. tick Answer arm: if Need && need_depth<1
  → reask_with_need (rebuilds context via expand_context: Term::history_tail(paged vt100
  scrollback, restores live view) OR named tab's panes) + continue (never surfaced); capped
  at 1. last_question/need_depth set in submit_agent_query. Single-tab cross-pane already in
  screen_context. GOTCHA: adding Need variant needs match arms in handle_bar_ask Enter, ui.rs
  directive label, main.rs ask_cli. 54 selfchecks. DEFERRED: Context Bus registry +
  parameterized actions (RunWith) — no W1-7 consumer, need transaction journal for plans.

## GOTCHA: bg_busy leak wedged all background AI (2026-07, FIXED)
- Symptom: watch (W6) never produced a summary. Cause: agent::watch_summary/auto_name/
  name_session only sent their AgentEvent inside `if let Ok(chat)…` — on ANY LLM failure
  (rate limit/timeout/bad key) they sent nothing, so bg_busy (set true before the call in
  maybe_fire_watches/maybe_auto_name*) was NEVER cleared → maybe_fire_watches' `if bg_busy
  { return }` gate blocked every future watch + auto-name permanently. One failed bg call
  wedged all background AI. FIX: AgentEvent::BgDone sent unconditionally at the end of every
  bg thread (tick: BgDone→bg_busy=false); watch_summary now also sends an error verdict on
  Err so failures are visible ("⚠ watch couldn't summarize — …"); toggle_watch_pane warns
  if no key. Refresh cadence: tick every poll_interval_ms(16ms); watch fires on TermEvent::
  Exited (shell exit) OR quiet = frame_tick-last_output_tick > watch_quiet_secs(20s)*1000/
  poll_ms. Verified live with GROQ + watch_quiet_secs=3.

## Away Digest (2026-07, shipped a1062f8)
- away_log: bounded (200) Vec<AwayEvent{tick, pane, kind: NeedsYou|Done|Context, text, dur_ticks}>
  on App; push_away() appends; ALSO the episodic Tier-1 substrate for the planned memory system.
- Sources: WatchSummary arm (verdict + duration from WatchState.run_started_tick — stamped when
  output resumes after triggered/first output), unwatched TermEvent::Exited ("shell exited"),
  dirty-file names folded as Context at on_attach.
- on_detach stamps detach_tick; on_attach builds ONE headline from events since detach_tick:
  "while away <dur> — ✗ fails · ✓ dones (+N more) · context · <binding> digest", dedupes W6
  notices it subsumes (retain on text equality), sets digest_from_tick. Quiet when empty.
- show_away_digest(): sectioned render (needs you/done/context, relative "Xs ago", "ran Xs")
  pushed into agent_history + open_bar(Ask) — deterministic, no key. Action::AwayDigest, C-x g.
- Broker-ready: only LLM part is verdict TEXT via existing watch_summary→chat seam.
- fmt_dur(ticks): secs = ticks*poll_interval_ms/1000 → "45s"/"4m12s"/"3h02m".
