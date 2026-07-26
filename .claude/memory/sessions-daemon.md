# Sessions, the daemon, and the CLI

The client/server split, its lifecycle, and the process hygiene around it.
Remote/brokered sessions live in [`ssh-broker.md`]; Windows specifics in
[`windows-port.md`].

- Session daemon (2026-07, src/session.rs): thin client, server renders — daemon runs
  the same App the selfcheck already proved works headless; input = deserialized
  crossterm events over `sys::control` (Unix-domain socket on Unix; authenticated
  loopback TCP + rendezvous file on Windows); output = ratatui's own ANSI bytes
  captured by pointing CrosstermBackend at a stream-backed Write sink (FrameWriter)
  instead of stdout. `mars new <name>` spawns `mars --server <name>` detached
  (`setsid` on Unix; detached process flags on Windows) and attaches; `mars attach`
  reattaches (most-recent address mtime if unnamed); `mars ls` pings each address,
  prunes dead ones. One client per session; new attach
  sends the old one an Exit frame (takeover). Detach (C-t D / bar row) leaves the
  session running; C-x C-c ends it (dirty-guard still applies) and removes the socket.
  `App::run` was refactored to take an `InputEvent` receiver instead of reading
  crossterm directly (`app.rs` step/tick split) — standalone mode spawns a TTY-reader
  thread feeding the same channel type the server consumes from sockets.
  GOTCHA (cost ~1hr): don't write ad-hoc test helpers that re-`try_clone()`+drop a
  socket per call — works fine, was a red herring. The REAL bug: ratatui's incremental
  cell-diffing interleaves cursor-repositioning escape codes BETWEEN individual
  changed characters (one draw per keystroke), so typed text never appears as a
  contiguous substring in the raw ANSI byte stream. Test/verify session output through
  a real ANSI parser (vt100 — already a project dep) and check the INTERPRETED screen
  contents, never raw-byte-contains() on accumulated Output frames.
  Verified manually end-to-end via `script -q /dev/null ares --session/--resume` +
  `ps`/`--list` (headless client_main can't be exercised without a real/pty TTY).
  `ARES_DEBUG_LOG=<path>` env var (session.rs `debug_log`) writes timestamped
  diagnostics for hello/parse/read errors — zero-cost when unset, useful for future
  daemon debugging since a detached daemon has no visible stderr.
- Control liveness (2026-07): authentication timeout, permission, and legacy
  rendezvous parse failures are not proof a daemon is dead. `control::Probe`
  distinguishes Live/Dead/Indeterminate; only definitive dead endpoints may be
  unlinked, otherwise surface an upgrade/restart path.
- Nested sessions (2026-07): when `mars new` runs inside a PTY, remove parent
  `MARS_SESSION`, `MARS_SESSION_ID`, `MARS_AUTH_SOCK`, and broker capability from
  the spawned daemon; its attaching client hands over the current route.

## Sessions-by-default + launch (user rev, 2026-07)
- `mars [file]` is now a SESSION by default (auto-numbered, next_auto_name = lowest free
  int). No file → server/standalone opens a terminal pane (not scratch). `mars -s/
  --standalone [file]` = old no-daemon path (also opens terminal if no file). Terminal
  open in the daemon is gated by env MARS_OPEN_TERMINAL (set by session_main when
  file.is_none()) so selfcheck's server_main(None) stays scratch.
- Session naming: numbered → AI (agent::name_session → AgentEvent::SessionName →
  rename_session_to only if still numeric) → explicit (mars rename / RenameSession wins).
  maybe_auto_name_session in tick, fires once (session_name_attempted), 2× the
  auto_name_secs cadence. Reuses the socket-rename infra.
- terminal::spawn gained a cwd param; App.startup_cwd = parent of first opened file
  (set in App::new from launch file, or open_file first-file-wins); open_terminal uses it.
- Status bar line/col fix: the position readout ("<buf>  Ln N, Col N  ⚡session" for
  editors, "terminal ⚡session" for terminals) is now a SEPARATE right-aligned Paragraph
  drawn over the status area in theme_accent_bright bold — never truncated by left hints
  or hidden by a status_msg (which now trails the hints on the left). Was: single
  right_info string that got hidden by status_msg / truncated when narrow.

## CLI surface (2026-07)
- Subcommands (with long-flag aliases): mars new/session <name> [file], attach/a/
  resume [name], ls/list, kill <name>, ask "<q>", help/-h/--help, version/-V.
  Unknown -/-- args exit 2 with help (previously they were treated as FILENAMES —
  `mars --help` opened a buffer named --help). README.md = the user instructions.
- `mars ls` shows attached/detached via ClientFrame::Status → ServerFrame::Status
  (connection thread answers from an Arc<AtomicBool> the server loop maintains — no
  server-loop round trip, so it can't hang on a busy session). `mars kill` sends
  ClientFrame::Kill → SrvEvent::Kill → autosave + forced quit (skips dirty guard).

## TTY hygiene (2026-07, user hit this in Warp)
- Killed clients (SIGKILL) can't restore termios → the shell's tty stays raw →
  staircase output (`\n` without `\r`) for everything after, incl. `mars help`.
  Fix: session::sanitize_tty() runs first thing in main() — repairs OPOST/ONLCR/
  ICANON/ECHO on stdout if it's a tty. Doubly important BEFORE enable_raw_mode:
  otherwise crossterm snapshots the broken state as "original" and faithfully
  restores brokenness on exit. session::install_panic_restore() wraps the panic
  hook for both TUI paths (standalone + client) so panics leave a readable message
  and a working shell. Verified in a real pty: `stty -opost` → run mars → `opost`.
  Never verify this with mars stdout redirected (isatty=false → sanitize skips).
