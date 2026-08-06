# Security

## Reporting

Email anjishnu.kr@gmail.com with a description and reproduction. You will get an acknowledgement
within a few days. Please do not open public issues for unreported vulnerabilities.

## The security model, plainly

Mars is a terminal: it runs what you tell it to run, as you. The parts with a genuine security
boundary are the ones that let something *other than you at the keyboard* reach the machine:

**The Rover bridge** (`mars pair` / `mars serve`) exposes a WebSocket over a public tunnel URL.

- Authentication is a single pairing token, carried in the QR/link **fragment** (never sent to
  the page server, never in server logs) and checked with a constant-time comparison. `mars
  serve --reset` rotates it and drops every connected phone.
- The filesystem surface (`fs.*`) resolves every path through one seam that expands `~`,
  canonicalizes, and refuses anything outside `$HOME` plus a deny-list of sensitive directories
  (`.ssh` and similar). Reads are capped; writes carry a compare-and-swap mtime.
- The bridge follows the *session directory*, not a process id, and refuses loudly when the
  session is gone rather than serving an empty board.

**Model-facing surfaces** treat model output as untrusted input by construction:

- Rover's chat proposals are parsed strictly out of a fenced block: unknown verbs are dropped,
  counts are capped in code, and a `run` proposal can never choose which pane it runs in — the
  captain's device picks the target at tap time. `open` proposals are resolved and existence-
  checked before a button is ever shown.
- Hook/summary text originating in terminal output (which any program on the host can write
  into) is rendered as text, never parsed into actions.
- Anything destructive (kill, close, rm-class commands) goes through an explicit confirmation
  gate whether the request came from a human or an agent.

**Agents** spawned by Mars (the manager, assigned workers) run under Claude Code's permission
system (`acceptEdits`): file edits flow, everything else gates.

## Out of scope

- Anything reachable only by an attacker who already has local code execution as your user —
  they are already you.
- The security of ngrok/Cloudflare tunnels, Claude Code, or your model provider's API.

## Keys and secrets

Mars stores no provider API keys of its own; the key broker reads them from your environment or
OS keychain and other processes talk to it over a same-user Unix socket. The pairing token lives
in `~/.mars/serve.token` with user-only permissions.
