# Design docs, invariants, and unbuilt proposals

What the root docs are FOR, the invariants that outlive any one feature, and which
`design_ideas/` documents are proposals rather than descriptions.

## Doc roles
- `key_design.md` is a VISION document (what should exist + evolution horizons), not a
  status report — user ruling 2026-07-01. Don't rewrite it to describe current code.

## Invariants (from the approved v2 review plan)
- Every hint surface derives from `KeyBindings::binding_for` — never hardcode a
  keybinding string in menus/hints (remaps would make the UI lie).
- Empty-query bar menu is fixed-order; frecency is a search-result tiebreaker only.
- Destructive actions (quit/close/kill) always confirm; quit passes the dirty-buffer
  guard. Agent `RUN:` of a destructive action requires y/n.
- Frecency + nudge counters persist in `~/.config/mars/state.json`; user keybindings in
  `~/.config/mars/keys.json` are layered OVER defaults (new defaults appear in old configs;
  user entries win). Paths moved from `~/.config/ares/` in the rebrand, with a one-time
  auto-migration.

## speed_design.md (2026-07, PROPOSAL — not built, under review)
- Laser-fast editor+terminal movement & the anchored query. KEY BLOCKER: Ctrl/Alt+arrow
  are currently PANE nav (focus_direction, app.rs ~1347), word-jump only on M-f/M-b — so
  the intuitive "hold key + skip token" gesture is blocked; proposal reclaims Option+arrow
  for token movement, panes → C-o + C-t. Three granularities: word / code-token / subword
  (CamelHumps). Adds half-page (C-d/C-u), block/blank-line jump, symbol jump (col-0 fn/def
  heuristic), matching-bracket, and TELEPORT (avy/easymotion labels) = highest ROI. Part B:
  editor Ctrl+Space = anchored query over the SELECTION (explain/generate/tests/refactor/fix
  /review) — ship read-only+insert now, refactor-replace gated on undo checkpoint/journal.
  Part C: terminal ONE Ctrl+Space composer, shell-first with command suggestions (no more
  double-press), + prompt-jump in scrollback + copy-last-command/output + select-output→query.
  Unifying: Ctrl+Space = "do something here now" in both editor & terminal. 5 decisions to
  confirm before building (see doc). Current editor motions live in handle_edit (app.rs
  ~1333-1360), NOT the keymap; word=move_word_forward/backward, page_up/page_down exist.

## strategy.md (2026-07, strategy doc — review artifact)
- AI product strategy: sight×persistence thesis; 8 scenarios ranked by ownability×freq
  (1 triage = wedge, 2 remote/SSH, 3 watch-detached, 4 reattach-brief, 5 cross-pane…);
  before/after workflows with time-saved (~45-75 min/day for terminal-heavy dev); 6
  primitives w/ engineering designs (Context Bus, Trigger framework, Parameterized
  actions, Session-as-artifact, Transaction journal, Project index); anti-scenarios
  (no ghost-text, no context-free chat, no head-on Cursor competition — invert: be the
  substrate code-agents run IN); recommendation = own triage, build Trigger framework
  next (turns sight into vigilance, the moat-widener). Companion to agentic_inline.md
  (brief) / workflows_design.md (build spec) / delighters_design.md (nav+polish).

## delighters_design.md (2026-07, APPROVED-PENDING spec, NOT built)
- Navigation + polish delighters. Two substrates: (A) reusable fuzzy Picker (generalize
  the minibuffer Prompt + render_bar_dropdown, reuse fuzzy_score, Tab=longest-common-prefix
  NOT a trie), (B) Project index (lazy, session-cached, skip-list not .gitignore v1, git-root
  or startup_cwd, cap project_index_max=20k). Tier1: file finder (C-x C-f), quick-open
  (C-x p, file_frecency in state.json), buffer switcher (C-x b). Tier2: git gutter (shell
  `git diff -U0` async, marker in the 2nd gutter col, git_gutter knob), autosave ✓ pulse.
  Deferred: cmd-bar starter set (fixed-order ruling), smart paste, dashboard splash
  (terminal-default makes splash rare). User reviewing before implementation.

## Roadmap docs (2026-07, superseded for Phase 1 by the section above)
- `agentic_inline.md` = product brief (10 non-commoditized AI workflows, personas,
  wedge, retention loop). `workflows_design.md` = build spec for the first 7 (W1-W7)
  with enables/disables per choice. Both are DESIGN, no code written yet — user
  reviewing offline before implementation. Phase 1 = W1/W2/W3 (OPEN: directive,
  ExplainThis/ExplainFailure actions, shell Tab-translate + cursor-anchored overlay,
  no-essays prompt) + pane resize/zoom. Phase 2 = W4/W5 (context selectors, NEED:
  expansion). Phase 3 = W6/W7 (trigger framework, notices queue, detach/attach diff).
- Key design decisions to preserve when building: directive vocabulary stays
  trailing-line text (portability + readable confirm gate; parse_directive is the
  seam); one global agent_busy in-flight gate; proactive output is pull-rendered
  (notices queue, never pushed — enforces interruption budget structurally); OPEN:
  is line-only. Reuse the (cx,cy) that render_editor_pane/render_terminal_pane
  already return for the cursor-anchored overlay.
