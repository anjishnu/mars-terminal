# SSH, the key broker, and the fleet

`mars keyd` + `mars ssh` — the agent works on a remote box that never holds a key.

## SSH broker — agent works on keyless remote boxes (2026-07, shipped a55f108/e12d844)
- `src/broker.rs` (new): `mars keyd` = home broker holding the key, binds `$HOME/.mars/auth.sock`
  (0600 under 0700 dir), reuses `session::write_frame` + `read_line`. Protocol: `BrokerRequest::Chat
  {version, model:Option, messages, max_tokens, temperature}` → `BrokerResponse::Chat{text}|Error`.
  keyd `handle_conn` runs `agent::chat` (now `pub`) with a fresh `from_env()` per request.
- Remote side: `AgentConfig` gained `broker_sock: Option<String>`. `from_env()` highest-precedence
  branch: `detect_broker_sock()` (MARS_AUTH_SOCK, else well-known `/tmp/mars-auth-<uid>.sock` if it
  exists) → provider "broker" — UNLESS an explicit MARS_LLM_KEY/ARES_LLM_KEY is set (that wins).
  `chat()` forks to `broker::chat_via_broker` in broker mode (no Authorization header on the box).
  `is_configured()` in broker mode = `UnixStream::connect(sock).is_ok()` (honest when tunnel down).
- `mars ssh <host>`: wraps system ssh — `-R remote_sock:home_sock -o StreamLocalBindUnlink=yes
  -o ControlMaster=auto/Persist=60s -t host "MARS_AUTH_SOCK=… exec $SHELL -l"`. NOT SetEnv (no
  AcceptEnv dep). Records the host in fleet cache + nudges install if mars missing on remote.
- Deferred watch: `maybe_fire_watches` peeks for a candidate first, and if provider=="broker" &&
  !is_configured() (tunnel down) RETURNS WITHOUT consuming the trigger → verdict fires on reattach.
- Fleet: `~/.mars/fleet.json` (FleetEntry{host,cwd,session,last_status,as_of}); `fleet_record` on
  `mars ssh`; `mars ls` (now `list_main(prompt: bool)`) shows local sessions + numbered RECENT HOSTS
  + interactive `→ ssh (number/name)` follow-up via `resolve_target` (ordinal/exact/unique-prefix);
  `--no-prompt` or non-TTY skips. Live status-push from remote daemons = NOT built (next).
- VERIFIED LIVE: `mars keyd` (real GROQ key) + `mars ask` with only MARS_AUTH_SOCK (no key in env)
  returned the answer. 63 selfchecks (broker detect/precedence/availability/round-trip + fleet).
- DESIGN: `design_ideas/ssh_strategy.md` §1.5 (transport: mosh rejected, OpenSSH v1 / russh v2,
  Mode P shipped / Mode E next). DEFERRED: Mode E key-push, russh, Windows TCP fallback, keychain,
  remote→home Status-frame push (needs-you-from-remotes in `mars ls`), bare-`mars` attach-or-create
  (kept tmux-like "new" default; use `mars attach`).

## Route + credential invariants
- Persistent remote broker route (2026-07): a remote session daemon outlives its
  SSH invocation, so a nonce socket/capability captured only in its spawn env dies
  after first detach. Carry the current route in `ClientFrame::Hello` and replace
  the daemon's in-memory route on every attach; never log the capability. PTY
  shells retain their spawn environment, so nested Mars processes must query the
  daemon with `ClientFrame::BrokerRoute` via `MARS_SESSION` + an immutable
  `MARS_SESSION_ID`, not trust inherited `MARS_AUTH_SOCK`; the ID survives rename.
- SSH credential boundary (2026-07): system OpenSSH can export inherited variables
  through user `SendEnv` rules. Build every ssh command through `ssh::ssh_command`,
  which removes all supported provider-key variables after keyd has inherited them.
- `ssh -o StreamLocalBindUnlink=yes` on the CLIENT is a no-op for `-R` (remote)
  unix-socket forwards — only the server's sshd_config honors it there. A stale
  /tmp/mars-auth-<uid>.sock on the remote makes the -R bind fail (and the mux
  forward failure cascades into a second password prompt). mars sweeps it in the
  ssh prelude (`remote_prelude_cmd`) and remote-side via `probe_and_sweep`.
- `command -v mars` in an ssh remote-command runs under sshd's bare non-login
  PATH (no ~/.cargo/bin) — probe install dirs explicitly, don't trust PATH.
