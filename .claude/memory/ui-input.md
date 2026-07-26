# UI, input, and the editing surfaces

Keybinding rulings, the mouse, selection, undo, the navigator, motion, and theming.

## Mouse: hit registry + editor selection (2026-07-25, design_ideas/mouse-strategy.md)
- ROOT CAUSE of "the mouse does nothing outside panes": the renderer recorded only
  `pane_rects`, so no other surface was hit-testable — the Edit/Terminal mode gate was a
  symptom, not the cause. Fix: `App.hits: RefCell<Vec<HitRegion>>`, cleared at the top of
  `ui::render`, pushed by whichever renderer drew the rect (RefCell because chrome
  renderers take `&App`; precedent = `Pane::md_rendered_total: Cell`). Hit-test walks the
  vec BACKWARDS so paint order == z-order for free. `HitTarget::Act(Action)` routes clicks
  through `run_action`, so the destructive-confirm gate covers the mouse with no new code.
  Registry is consulted in EVERY mode before the pane path; pane interiors register nothing
  and fall through, which also keeps the inner-app (vim/Claude Code) mouse boundary intact.
- GOTCHA that nearly shipped a regression: three sites read `selection_anchor.is_some()` as
  "a region exists" — app.rs Tab (indents the region) and Esc (clears instead of dismissing
  a notice), plus the ui.rs highlight. Setting the anchor on mouse-DOWN therefore made Tab
  indent instead of inserting spaces after any click. Anchor must be set on the first DRAG
  event (press only records the origin in `editor_drag`). Selfcheck 44b pins it.
- Editor drag/double/triple-click select but never copy — auto-copy on release is what made
  a plain terminal click clobber the clipboard (P1.4). Terminals copy because there's no
  other way out; editors have C-w/M-w. Double/triple are timed in-app against the new
  `multi_click_ms` knob — terminals report no click count.
- Border-drag resize: the keyboard's `PaneLayout::resize(focused, delta)` adjusts the
  INNERMOST split around the focus, which is the wrong split whenever the pane whose edge
  you grabbed sits inside a nested one. Dragging therefore needs split identity, not a
  pane: `set_ratio(path: &[u8], ratio)` addresses one split from the root, and
  `ui::compute_dividers` (a mirror of `compute_rects` — keep them in step) emits the seam
  rect plus the parent's origin/span. Ratio is computed ABSOLUTELY from the pointer, so the
  divider tracks it instead of accumulating rounding drift.
- Accept only `Drag` for a live border drag, never `Moved`: SGR reports held-button motion
  as Drag, so bare motion buys nothing and would make panes follow the pointer forever if a
  release were ever missed.
- `MouseEventKind::Moved` arrives per cell (crossterm enables any-event tracking) and
  `apply_input` used to force `needs_redraw` for ANY input → a pointer sweep shipped a full
  ANSI frame per motion over a session socket. `InputEvent::forces_redraw()` now excludes
  bare Moved; both render loops consult it.

## Render only when changed + terminal mouse-copy (2026-07, shipped 43150bc + next)
- SSH lag root cause: both render loops (`App::run`, `session::server_main`) drew+flushed EVERY
  tick (~61/s at poll=16) even idle → 61 no-op packets/s over SSH. Fix: `pub needs_redraw: bool`
  on App (init true); tick() sets it on term_rx events, agent_rx events, agent_pending (spinner),
  or non-empty pending_prefix (which-key). Loops reordered tick→draw-if-needs_redraw→recv; input
  arms set it. server_main uses `std::mem::take(&mut app.needs_redraw)`. Idle = zero flushes.
  Users should revert any poll_interval_ms mitigation back to 16 (now cheap).
- Terminal mouse-copy: `pub term_sel: Option<TermSel{tid,ox,oy,vw,vh,anchor,end}>`. handle_mouse:
  Down(Left) on a terminal pane starts a selection at the clicked screen cell; Drag extends end;
  Up copies via `selection_text_from_screen(&screen,a,b,last_col)` (pub(crate) free fn, linear
  text-flow, trailing-space-trimmed) → clipboard + kill_ring + "Copied N chars". ui.rs
  render_terminal_pane highlights selected cells (selection_bg). Wheel-scroll + Cmd+V paste
  ALREADY worked (ScrollUp/Down→scroll_view; paste_text→send_bytes w/ bracketed re-wrap).
  Selfcheck extracts a printf'd row via the free fn. Real drag = real-terminal-only per AGENTS.md.

## Undo: coalesced runs + time-travel mode (2026-07)
- ROOT BUG (fixed): insert_char_at_cursor/delete_before_cursor never checkpoint()'d → typing
  was invisible to undo. Now coalesced via App.edit_run: EditRun{None,Insert,Delete}. In
  handle_edit_primitive: capture prev_run at top, reset edit_run=None; the Char arm checkpoints
  only if prev_run != Insert (a run of typed chars = ONE undo), Backspace arm similarly for
  Delete. run_action resets edit_run=None so any command breaks the run. Enter checkpoints +
  auto_indent (copies prev line's leading whitespace). Bindings: C-/, C-_, C-x u = undo; M-/,
  C-x C-u, cmd-Z = redo; cmd-z = undo (Mac muscle memory, kitty only).
- TIME-TRAVEL MODE (user request): Mode::Undo, entered via Action::UndoMode (M-u + menu row
  "Undo history…"). handle_undo_mode: ←/↑/u = do_undo, →/↓/r = do_redo, Home = undo-all
  (while focused_buf_mut().undo()), End = redo-all, any other key = exit to Edit. Status line
  (undo_status) shows "UNDO ◂ N back · M forward". buffer.undo_depth()→(undo_len,redo_len).
  Verified live: M-u → Home rewinds to file start, End restores, Esc exits.
- GOTCHA: testing undo via screen_text() is flaky (empty-buffer render); assert on
  buffers[id].rope.to_string() directly instead.

## Speed features shipped (speed_design.md steps 1-4, 2026-07)
- STEP 1 motion: KeyModifiers::SUPER detected in handle_edit_primitive; ⌘←/→=move_token_sel
  (code-token: class-run of word/punct, whitespace skipped — token_class helper), ⌘↑/↓=page,
  ⌘⇧=extend. Structural jumps: jump_block (blank line), jump_symbol (col-0 kw heuristic),
  match_bracket — Actions JumpBlockPrev/Next, JumpSymbolPrev/Next, MatchBracket bound
  C-x [ ] { } m. ⌘ only on kitty terminals; M-f/M-b + PageUp/Down are the fallback.
- STEP 2 search-as-teleport: search_labels + search_pick fields. handle_isearch_key: Tab →
  build_search_labels (home-row asdfghjkl over search_hl in doc order) + search_pick=true;
  next key picks a label → jump+accept. Land-on-any-key: the `_` arm ends isearch + re-
  dispatches the key to handle_key. isearch_status()→(cur,total) for the n/m counter (shown
  in the Prompt label; cursor anchored to label+input len, not incl. the counter). Labels
  render as hl kind 3 (label_style chip) in the per-char highlight map.
- STEP 3 unified terminal composer: handle_terminal Ctrl+Space → open_bar(Command) (was
  Shell). handle_bar_command Enter: if items_len==0 && has_query && bar_return==Terminal →
  submit_terminal_shell() (flips to BarMode::Shell + translate_shell_query, or runs directly
  with no key). "if not a command → shell-translate" per user. No double-press.
- STEP 4 selection-aware agent + reversible refactor: refactor_target/refactor_replacement
  fields. submit_agent_query captures selection_range + appends selected_text() block to
  context (tells model: refactor→reply ONLY a ``` block). tick Answer: if refactor_target,
  extract_code_block(text)→refactor_replacement. Ask-panel Enter (empty query) → apply_refactor:
  ONE checkpoint() + rope.remove+insert → reversible via C-/. Panel shows "▶ Enter to replace
  the selection (N lines)". Cleared in close_bar + C-l.
- GOTCHA fixed: selfcheck now hermetic — clears GEMINI/GOOGLE/GROQ/MARS_LLM/ARES_LLM env at
  the top of selfcheck() (an inherited key flipped the shell composer to translate-not-run,
  failing the terminal check). 51 checks pass. Logo: render_splash is now a top-level overlay
  (Clear + centered) gated on show_splash — was editor-pane-only, so terminal-default startup
  hid it.

## Left file-tree sidebar (2026-07, REPLACED the `@` bottom picker)
- User pivoted the `@` bottom fuzzy dropdown → a LEFT sidebar file tree (Mode::Tree).
  BarMode::File + render_file_dropdown + file_matches + longest_common_prefix ALL REMOVED.
- FileTree{root, expanded:HashSet<PathBuf>, selected, filter} + App.tree_open + App.tree_rows
  (Vec<TreeRow>{path,label,depth,is_dir,expanded,updir}). compute_tree_rows(&self):
  filter empty → browse (../ row if root has parent, then push_dir_rows recursing into
  expanded dirs, read_dir_entries reads fs live + skips dotfiles/project_ignore, dirs-first);
  filter non-empty → flat fuzzy shortlist over project_index. refresh_tree_rows() recomputes
  after every mutation + clamps selected. Browse reads fs live (only expanded dirs, cheap) —
  no dir cache.
- Entry: `@` (in bar → close_bar + toggle_file_tree) OR C-x d / C-x C-f / C-x p / C-x b
  (all → Action::ToggleFileTree/FindFile/etc → toggle_file_tree). toggle is tri-state:
  closed→open+focus(Mode::Tree); open+Tree→close; open+Edit→focus. GOTCHA: C-x is an
  Edit-only prefix so you CANNOT press C-x d from inside the focused tree — close via Esc
  (handle_tree Esc: clear filter else close). Opening a file (Enter on file row) → Mode::Edit
  but tree STAYS open (persistent sidebar); re-focus with C-x d from Edit.
- Nav (handle_tree): ↑↓/C-p/C-n move; Right = tree_activate(false), Enter = tree_activate(true)
  — folders/`../` behave the same for both (expand / re-root); for a FILE, Right PREVIEWS
  (show_file_in_pane(commit=false): shows it in the pane, stays Mode::Tree, reversible) while
  Enter COMMITS (commit=true: Mode::Edit). show_file_in_pane reuses an already-open buffer
  (find by path) so repeated previews don't pile up duplicate buffers. ← collapse-or-parent;
  typing filters; Backspace pops filter. Layout: render() carves a left Constraint::Length(tree_width)
  column (knob, default 30, capped at width-20) when tree_open; render_file_tree draws a
  bordered box, folders bold+accent with ▾/▸ carets, `../` dim, indent by depth, selection
  bg only when focused. tree_width knob in tuning.rs.
- Groq/qwen setup (still current): see below block.

## Tree reset-on-close + terminal cwd (2026-07)
- Closing the tree (close_tree(): used by the toggle-hide branch AND handle_tree Esc) now
  sets file_tree=None + clears tree_rows, so reopening rebuilds fresh at the project root
  (forgets any `../` wandering). Opening a FILE keeps the tree open (not a close) so it
  doesn't reset then.
- Terminal cwd: portable-pty's CommandBuilder with NO cwd lands the shell at `/` (not the
  process cwd — confirmed the daemon cwd was correct but the shell still went to root).
  Fix: App.run_cwd = std::env::current_dir() at App::new; open_terminal passes
  startup_cwd.or(run_cwd) so a no-file session's terminal opens where `mars` was launched.
  (startup_cwd = first opened file's dir still wins when a file was opened.)

## GOTCHA: tree root MUST be absolute (2026-07)
- Bug "blank sidebar after Enter on ../": the tree root was the relative "." (from
  startup_cwd=None → project_index root "."), and "." .parent() is an empty PathBuf →
  read_dir("") fails → blank tree + header shows "/". FIX: canonicalize the root to
  absolute in toggle_file_tree when creating the FileTree
  (std::fs::canonicalize(&root).unwrap_or(root)). Now `../` (parent) navigation works.
- Also `../` was invisible: fg was Color::DarkGray. Now theme_accent_bright + a "↑ " glyph.

## GOTCHA: tree selection highlight must be full-width + high-contrast (2026-07)
- Bug report "can't move up/down in the tree, can't type, right opens the file": the tree
  was WORKING (Mode::Tree correct, keys routed) — the SELECTION HIGHLIGHT was just invisible.
  Cause: bg=Color::DarkGray applied only to the short label span (not full row width), and
  the `../` row was DarkGray-fg-on-DarkGray-bg = invisible. Fix in render_file_tree: selected
  row uses bg=theme_accent (terracotta) with fg=theme_chip_fg, and pads a trailing spaces span
  to inner.width so the band spans the WHOLE row (like render_bar_dropdown's selected row).
- DEBUG METHOD that cracked it: python pty.fork + pyte emulator to drive the REAL binary
  (headless TestBackend couldn't show it). Two pitfalls: (1) must answer the DA1/kitty query
  (`\x1b[c`/`\x1b[?u`) with `\x1b[?62;c` or crossterm's supports_keyboard_enhancement blocks
  startup forever; (2) must set pty winsize via ioctl TIOCSWINSZ (struct winsize) or ratatui
  renders to a 0x0 area (blank). pyte's screen.buffer[y][x].bg exposes cell bg to verify
  highlights. Script pattern saved mentally: fork → set winsize → drain+answer-DA → write
  keys → snapshot screen.buffer. Raw-byte grep for typed text FAILS (ratatui interleaves
  cursor moves — the AGENTS.md gotcha); use pyte/vt100.

## Rebrand (2026-07)
- Binary/crate = `mars` (repo dir still Ares/ on disk). Config ~/.config/mars/ with
  one-time auto-migration from ~/.config/ares/. Sockets $TMPDIR/mars-<uid>/. Env:
  MARS_LLM_KEY/URL/MODEL, MARS_NO_SYSTEM_CLIPBOARD, MARS_DEBUG_LOG — all fall back to
  the old ARES_* names. Tagline: "mission control for your terminal".
- Palette (theme_* knobs in tuning.json): accent #D97757 terracotta (Claude Code clay),
  bright #E9A178 sand (teaching surfaces), dark #B7410E rust (splash gradient),
  chip fg #1F1410; selection bg #4A2A1F; search bg #8A5414. Rule: brand in chrome,
  not meaning (terminal panes stay green; danger stays red-only-in-confirms).
- Splash: MARS block logo + tagline + starter hints in the empty scratch until first
  key (app.show_splash). Selfcheck asserts "mission control" appears then vanishes.

## Movement + chord rulings
- Movement rulings (2026-07, rev 2): C-t = TRAVEL MODE (one-char verbs + cheat panel;
  new tab = C-t t; creation exits, navigation stays); C-c = copy (line if no
  selection), C-v = system paste (Emacs C-c prefix and page-down gone by ruling);
  M-o/M-arrows = panes; C-{ C-} (kitty-protocol) + M-{ M-} M-1..9 C-PgUp/PgDn = tabs;
  C-| / C-- splits (kitty) with C-\ / M-- universal twins; M-g = goto-line;
  C-x x = swap pane. Shifted punctuation can't be a chord on legacy terminals —
  kitty keyboard protocol (crossterm PushKeyboardEnhancementFlags, gated on
  supports_keyboard_enhancement) unlocks it; every modern chord has an Alt twin.
- Round-3 rulings (2026-07): C-o + Ctrl+arrows = pane nav (Alt isn't Meta on stock mac
  terminals — that's why M-o felt broken); chrome layer = navigation chords work inside
  terminal panes (is_chrome_action set in app.rs), editing chords never intercepted;
  cmd-/super- parse to SUPER (cmd-c/v/s/a bound; only super-reporting terminals
  deliver them); tuning.json = all behavioral knobs as {value, description}
  (src/tuning.rs, layered like keys.json).
