# Build, verify, publish

How this repo is compiled, tested, and shipped. Read with [`terminal-io.md`] when a
failure smells like encoding rather than logic.

## The loop
- Cargo is not on the default PATH: `source ~/.cargo/env && cargo build`.
- `./target/debug/mars --selfcheck` is THE suite (ratatui TestBackend, real PTYs, a real
  daemon over a real socket, no mocks). Extend it for new behavior rather than adding a
  harness. Synthesized KeyEvents can't catch raw terminal byte encodings, so new chords
  still need an eyeball pass in a real terminal.
- TWO SKUs must both pass since the memory feature gate: `cargo build` and
  `cargo build --no-default-features` (retrieval_stub.rs). Same for the `ssh` and `syntax`
  features — a stub swap means the stub's signatures must track the real module's.
- Selfcheck isolates config via XDG_CONFIG_HOME=temp dir (immune to user remaps, and it
  proves default-file writing for keys.json + tuning.json).

## GOTCHA: selfcheck was not hermetic against Mars's own session env (2026-07-25, FIXED)
- Running `--selfcheck` from inside a Mars terminal pane (i.e. the normal dogfooding
  workflow) failed two blocks at HEAD: `MARS_OPEN_TERMINAL=1` makes `server_main` open a
  terminal pane at startup, moving the layout the daemon block drives ("shell output not
  rendered in session"), and `MARS_SESSION`/`MARS_SESSION_ID` send `detect_broker_sock()` to
  query the PARENT session's daemon instead of this process's own global ("session daemon
  did not accept the attached client's broker route").
- Both now cleared in selfcheck's hermetic block. If a selfcheck failure looks environmental,
  check what Mars itself exported into the shell before suspecting the code.

## GOTCHA: stale cargo fingerprint masked rebuilds (2026-07)
- After `cargo publish --dry-run` (Jul 3), `cargo build` reported "Finished 0.2s" with NO
  Compiling line even after source edits + touch — target/debug/mars stayed at the Jul 3
  binary and selfchecks silently ran STALE code. Fix: `cargo clean && cargo build`.
- LESSON: if `cargo build` shows no "Compiling mars-terminal" line after an edit, or a brand-new
  selfcheck line doesn't appear in output, suspect the fingerprint cache — check
  `stat -f "%Sm" target/debug/mars` against wall clock before trusting a PASS.
- RELIABLE FIX: `cargo clean -p mars-terminal && cargo build` (faster than full `cargo clean`;
  `touch src/main.rs` alone does NOT bust it). Recurred 2026-07 during the render-loop work.

## Publish / install
- Replacing the installed binary (`~/.cargo/bin/mars`) with `cp` over the existing
  file gets the new binary SIGKILLed on launch (exit 137) — macOS AMFI caches the
  code signature by inode. `rm` the old binary first, then `cp` (or use `mv`).
- Local Cargo.toml version can lag crates.io: 0.3.0 was published out-of-band
  (2026-07-11) while the repo said 0.2.0 — check `crates.io/api/v1/crates/
  mars-terminal` max_version before bumping for publish.
