# Windows port

Everything platform-specific reaches the OS through `src/sys/`; these are the facts that
shaped the Windows adapter. See `WINDOWS_PORT.md` in the repo root for the design.

- Windows ConPTY lifecycle (2026-07, src/terminal.rs): the shell process can exit
  while ConPTY keeps the output pipe open, so reader EOF never arrives. Move the
  child into a thread blocked in `Child::wait`; keep `clone_killer()` in `Term` for
  pane close/drop, cache the exit code, and suppress the natural-exit event on drop.
- Windows control hardening (2026-07, src/sys/windows.rs): rendezvous tokens must
  come from `getrandom`, not `RandomState` (successive `RandomState::new()` values
  are related). The force-kill sweep matches `Win32_Process.Name == mars.exe` plus
  the argument substring; embed `std::process::id()` because PowerShell's `$PID`
  names the helper PowerShell process, not the calling Mars process.
- Windows key events (2026-07, crossterm 0.28): `KeyEvent.kind` is always populated
  and includes `Release`. Filter releases once in `App::apply_input`; otherwise
  every typed character/action runs twice. Preserve `Repeat` for held keys.
- Windows control authentication (2026-07): a one-way token leaks the token to a
  process that rebinds a stale recorded TCP port. The control PAL now uses a fresh
  client nonce plus role-separated HMAC-SHA256 proofs in both directions; keep the
  500 ms deadline absolute across the whole handshake.
- Windows process containment (portable-pty 0.8.1): `Child::as_raw_handle()` is
  available, but the ConPTY backend neither creates a Job Object nor starts the
  child suspended. Assign the Mars server itself to a kill-on-close Job Object
  before it creates `App` to contain future PTY descendants without a post-spawn
  race. Per-pane descendant containment needs a suspended-spawn hook.
- Windows OpenSSH (2026-07): stock 9.5p2 parses `ControlMaster`/`ControlPersist`
  but Microsoft's project scope excludes client multiplexing/background mode.
  Verified live against Ubuntu sshd: `-R remote-unix-socket:local-tcp` works from
  Windows. Windows-home `mars ssh` uses that shape plus a per-invocation token
  relay; no muxing.
