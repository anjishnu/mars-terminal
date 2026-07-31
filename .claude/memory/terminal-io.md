# The terminal as a device — encodings, PTY, vt100, clipboard

Byte-level behavior that headless tests cannot see. Most of these cost real debugging.

## Terminal key-encoding gotchas (cost real debugging)
- `C-/` arrives as `C-_` (0x1f) in many terminals → Undo is bound to both.
- `C-@` IS NUL — the same byte legacy terminals send for `Ctrl+Space` → a C-@ set-mark
  alias is physically impossible; selection is Shift+arrows/mouse.
- `M-<` arrives as ALT|SHIFT+'<' → `config::chord_of` strips SHIFT from non-alphabetic
  chars so bindings parse-match; keep that invariant when touching chord code.
- Ctrl+Space may arrive as `KeyCode::Null`; both `handle_edit` and `handle_terminal`
  check for it.
- Clipboard: arboard crate; kills/copies also set OS clipboard; crossterm needs the
  "bracketed-paste" feature for Event::Paste. ARES_NO_SYSTEM_CLIPBOARD=1 disables
  clipboard init (selfcheck sets it — keeps tests off the user's real clipboard).
  (The env var is `MARS_NO_SYSTEM_CLIPBOARD` since the rebrand; the ARES_ name still
  falls back.)

## P0 tmux-parity features (2026-07)
- Terminal scrollback: vt100::Parser now created with tuning.terminal_scrollback_lines
  (10k default). Term.scroll_view(delta)/scroll_to_live()/view_offset(); wheel scrolls
  terminal panes, Shift+PgUp/PgDn page, any keystroke snaps to live; title shows
  " terminal ^N " while scrolled. GOTCHA: vt100 0.15 grid.rs has a DEBUG-ONLY integer
  underflow when scroll offset > screen rows (release wraps to correct behavior) —
  worked around with [profile.dev.package.vt100] overflow-checks=false in Cargo.toml.
  vt100 0.16 fixes it but conflicts with ratatui 0.29's pinned unicode-width =0.2.0.
- Dead-shell lifecycle: reader EOF sends TermEvent::Exited -> Term.exited flag (set in
  App::tick) -> pane border rust + "process exited — Enter closes" overlay; Enter/q
  closes pane (close_terminal_pane recycles the last pane into an editor).
- Crash safety: App::autosave() silently saves modified path-backed buffers every
  tuning.autosave_secs (0=off, ticked in App::tick) AND on session detach/disconnect
  (session.rs). Daemon stdout/stderr -> ~/.local/state/mars/<name>.log with
  RUST_BACKTRACE=1 (startup/end/crash lines from main.rs --server arm).

## Testing against a real terminal
- Raw-byte grep for typed text FAILS — ratatui interleaves cursor-repositioning escapes
  BETWEEN changed characters, so typed text is never a contiguous substring of the ANSI
  stream. Parse through vt100 (already a dep) and assert on the INTERPRETED screen.
- To drive the REAL binary headlessly: python `pty.fork` + `pyte`. Two pitfalls — (1) answer
  the DA1/kitty query (`\x1b[c`/`\x1b[?u`) with `\x1b[?62;c` or crossterm's
  `supports_keyboard_enhancement` blocks startup forever; (2) set the pty winsize via ioctl
  TIOCSWINSZ or ratatui renders into a 0x0 area (blank). `screen.buffer[y][x].bg` exposes
  cell background, which is how the invisible-tree-highlight bug was cracked
  (see [`ui-input.md`]).

- **"Is a command running in this pane?" is a kernel question, not a shell-integration one.**
  `portable_pty::MasterPty::process_group_leader()` (unix) is `tcgetpgrp(master)`; compare it to
  the spawned shell's `child.process_id()` — different means a foreground job is executing, equal
  means the shell is at its prompt. Works on any shell with no `precmd`/OSC-133 setup. Mars scans
  OSC 133 already (`osc133.rs`) but **never installs shell integration**, so anything built on
  those markers silently does nothing on a plain zsh. Returns `None` on Windows — treat that as
  "no claim", never as `false`. Used by `App::pane_stalled` for the `stalled` verdict.
