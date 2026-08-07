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
- SEQUEL (2026-07-29): the same class bit again from a different direction. Broker discovery
  ends at a FIXED `~/.mars/auth.sock`, which the isolated XDG_CONFIG_HOME/MARS_RUNTIME_DIR do
  NOT cover, and provider keys come from the ambient env. So with `mars keyd` running (a
  session auto-starts it since e86bc64) the agent read as CONFIGURED, and every `!`-bar test
  took the translate branch instead of running the command — "shell command did not attach
  terminal" at what looks like a PTY failure but is an auth one. Provider detection likewise
  saw "broker" where it had set GEMINI_API_KEY. Fix: `MARS_NO_BROKER=1` + an ambient-key scrub
  at the top of `selfcheck()`; the two blocks that TEST those mechanisms clear it around
  themselves. Symptom to recognise: selfcheck green in CI, red on a machine that uses Mars.
- `--selfcheck` IS runtime-isolated (`MARS_RUNTIME_DIR` → a per-PID temp dir) and safe to run
  while the user has a live session — verified by checking the live `mars --server` PID before
  and after. Earlier notes that it "kills all live sessions" are wrong; `mars killall` is the
  thing that does that, and it is still never ours to run.

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

## GOTCHA: a running session server keeps the OLD binary (2026-07-25)
- Reinstalling (`cargo install --path .`) updates `~/.cargo/bin/mars` on disk, but a live
  `mars --server <name>` process keeps running the binary it launched with. The SERVER
  renders (not the client), so `mars attach`/reattach shows the OLD code — a fresh on-disk
  binary changes nothing until that process restarts. Cost ~an hour chasing a "health line
  doesn't render" that was really just a stale server.
- DIAGNOSE: `ps -o lstart= -p $(pgrep -f "mars --server")` vs `stat -f %Sm ~/.cargo/bin/mars`
  — if the server started before the binary's mtime, it's stale. Note `mars --selfcheck`
  runs the fresh on-disk binary standalone, so a passing selfcheck ≠ what the live session
  shows; don't conclude "works" from selfcheck when the user is on an attached session.
- FIX: `mars kill <name> && mars <name>` — the session's goals/worklog persist to disk and
  reload. Standalone (no session): just quit and relaunch. See [[sessions-daemon]].

## GOTCHA: `cargo install` failed silently for days behind a pipe (2026-08-07)
- `cargo install ... 2>&1 | tail -N && codesign ...` masks failure twice over: the
  pipeline's exit is TAIL's (always 0), so `&&` proceeds, and codesign refreshes the
  binary's mtime — every staleness heuristic (mtime vs process start) then says "fresh"
  while the binary is old. Several "installed" claims were false before this was caught.
- Root cause of the underlying failure: `cargo install` RE-RESOLVES dependencies and
  ignores Cargo.lock unless given `--locked`; a broken upstream release (zune-jpeg
  0.5.15, macro-expansion compile error) poisoned installs while `cargo build` stayed
  green on the lockfile pin.
- RULES: always `cargo install --path . --features web --force --locked`; never pipe the
  install through tail — capture to a log file and check `$?` explicitly; verify content
  not mtime: `strings ~/.cargo/bin/mars | grep -c '<new-string>'` PLUS a control string
  that must be ≥1 (a 0 on both means strings extraction failed, not staleness).

## GOTCHA: cargo reported "Fresh" for edited sources (2026-08-07)
- Symptom: edits saved to disk, `cargo build` exits 0 with "Finished in 0.5s", and the binary does
  NOT contain the new code. `-v` shows `Fresh mars-terminal` even after `touch`ing every source —
  the fingerprint was stale, not the mtimes. Same failure FAMILY as the install-behind-a-pipe bug:
  a green build that built nothing.
- FIX: `cargo clean -p mars-terminal` then rebuild (~11s; it does not re-fetch deps).
- DETECT: after any build you intend to verify behaviour against, check CONTENT not exit code —
  `strings target/debug/mars | grep -c '<a string from the edit>'` with a control string. This is
  the same probe as the install check above; use it for both.
