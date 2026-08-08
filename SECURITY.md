# Security

## Reporting

Email anjishnu.kr@gmail.com with a description and reproduction. You will get an acknowledgement
within a few days. Please do not open public issues for unreported vulnerabilities.

## The security model, plainly

Mars is a terminal: it runs what you tell it to run, as you. There is no sandbox between you and
your own machine, and adding one is not a goal. The parts with a genuine security boundary are the
ones that let something *other than you at the keyboard* reach the machine — the phone bridge, and
the agents that read text they did not write.

Two sentences carry most of the design:

- **The pairing token is a credential for code execution as you.** Treat it like an SSH private key.
- **Text an agent reads is untrusted input. The human press is the trust boundary.**

Everything below is the detail behind those.

## What an attacker can actually do

Three positions worth thinking about, in descending order of how much they get.

### 1. They have your pairing token

**What that gets them: everything.** The bridge's intents include reading and writing files under
`$HOME`, typing keystrokes into a live pane, and ending sessions. Typing into a pane is arbitrary
code execution as your user, so a leaked token is a full compromise of the account, not a leak of
the board.

**How it realistically leaks:** somebody photographs the QR over your shoulder, or you screenshot
the pairing screen into a chat, or you send yourself the link. The token rides in the URL
*fragment*, so it never reaches the page server and never lands in a server log — but it is on your
screen and in your phone's storage.

**What protects it:** 128 bits from `/dev/urandom` (refusing to serve at all if minting fails),
compared in constant time, stored `0600`. Guessing is not a realistic attack and never will be.

**What does not:** once the token is out, nothing. There is no expiry and no per-device
revocation — `mars serve --reset` rotates the single token and drops every paired phone, which is
the blunt instrument and currently the only one.

**So:** keep the tunnel up only while you are using it, and rotate after any exposure. The planned
fix is a single-use pairing code exchanged for a per-device token, so one phone can be revoked
without re-pairing the rest, plus a visible list of connected devices.

### 2. They can reach the tunnel URL but have no token

They can knock and be refused, and they can tell that the thing answering is a Mars bridge —
open-sourcing makes the protocol shape public, and the URL is guessable in the sense that tunnel
providers share a domain space. What they cannot do is guess the token.

The residual risk is not brute force; it is a future bug in the code that runs *before* auth. That
surface is deliberately tiny: an unauthenticated connection can send `hello` and `auth` and reach
nothing else — the bridge authenticates first and only then dials the session daemon. Keeping it
tiny is the mitigation, and any change that moves work above the auth line deserves scrutiny.

Not yet built: a failed-auth counter surfaced in `manager.health`, so sustained knocking is visible
on the phone instead of silent in a log.

### 3. They control text that an agent will read

This is the subtle one, and the one open-sourcing changes most, because the exact grammar a
proposal must match is now public. Attacker-controlled text reaching an agent is *ordinary*, not
exotic: a README in a repo you cloned, a dependency's build output, an error message, the contents
of a memo.

Two agents read that text, and they have very different powers:

| | What it reads | What it can do with nobody at the keyboard |
|---|---|---|
| **Rover chat** (the conversational XO) | repo files, pane output, memos | **Nothing.** `--allowedTools Read,Grep,Glob` |
| **Manager** (the background tick) | pane output, memos, session dirs | Write files under `~/.mars` (`acceptEdits`) |

**Attack A — forge a proposal card.** Plant text that steers the chat agent into emitting a
` ```do ` block, and the parser will accept any well-formed line in it. The realistic payoff is a
card offering `run: <attacker's command>` with a plausible-sounding reason attached.

What stops it: the agent cannot run anything itself, so the card is the whole of the attack. The
command is printed on the card verbatim, `run` requires a deliberate hold rather than a tap, and
the command is echoed into a pane you can see. `close` is the only destructive verb, is red, and
ends only the session you are already looking at. Counts are capped at three and unknown verbs are
dropped.

What does not stop it: nothing prevents the card from *appearing*. This is social engineering with
the payload printed on it in full, and the defence is a human reading the command before holding
the button. That is a real defence — it is the same one that stops you pasting a stranger's shell
one-liner — but it is not a technical control, and it should not be described as one.

**Attack B — drive the manager agent to write a file.** Higher stakes, because there is no human in
this loop at all. Four targets, in the order an attacker would want them:

- `run.sh` — editing it is arbitrary shell on the next tick. **Blocked:** the runner is compared
  against the built-in copy, and a modified one runs only if its hash is blessed in
  `~/.mars/runner.approved`, which sits outside the agent's `--add-dir`. The agent cannot approve
  its own edit.
- `restore.json` — its `chat` field was interpolated into `claude --resume {id}` and *typed into a
  live shell* on the next reboot, so a carriage return in it was a command you never saw. You would
  be confirming a reboot, not a payload. **Fixed:** ids are now validated as UUID-shaped and
  refused otherwise, falling back to `--continue`. Selfchecked.
- Memos — the agent can write board content, which is what memos *are*, so this is not blocked and
  cannot be. The impact is phishing: a memo that says "run this to fix it". It funnels back into
  Attack A's defence, a human reading a command before holding a button.
- `prompt.md` / `AGENTS.md` / `docs/**` in `~/.mars/manager` — rewriting its own standing orders
  would turn one successful injection into a permanent one, since the prompt is re-read every run.
  **Blocked, three ways:** the runner denies the agent the tool to edit them; Mars compares each
  against the built-in before every tick and refuses to run on unblessed drift; and a doc claiming
  a version number that is not ours is overwritten rather than trusted, which closes the trick of
  writing `mars-doc-version: 999` to become immune to every future release.

**Attack D — aim a worker at your repository.** An assigned worker runs `acceptEdits` in your
actual source tree with the memo as its entire brief, so a forged memo is instructions to an agent
that writes code. Shell still gates, and nobody is watching that pane to approve it — which means
the realistic payload is not code that runs now but **files that execute without being run**: a
build script, a CI workflow, a git hook, a manifest whose dependencies bring their own build
scripts. Those fire on your next build, push, or commit. Mars publishes to crates.io, so a
`build.rs` edit reaches users' machines rather than only yours.

Assigned workers therefore carry an explicit deny list covering `build.rs`, `Makefile`,
`package.json`, `Cargo.toml`, `.github/workflows/**`, `.git/hooks/**`, `.envrc` and `.claude/**`
(the last because the first thing a blocked agent proposes is writing itself an allow rule). Deny
rules outrank the permission mode in every mode, and Bash cannot be used to route around them.

**Attack C — terminal output into the pickables tray.** Any program that prints to a pane can put a
URL or a backticked command into the phone's chip tray. Chips **copy to the clipboard and never
execute**; pasting it into a shell and pressing enter remains a human act. This is a convenience
surface, not an execution surface, and it should stay that way — a "run this chip" button would
convert every program's stdout into a one-tap command.

## Known gaps

Stated plainly because a gap you know about is cheaper than one a user discovers:

1. **A worker's edits land directly in your working tree.** The deny list stops the
   executes-without-being-run class, but ordinary source edits still arrive unreviewed. Running
   assigned workers in a git worktree or on a branch would turn every worker into a diff you merge,
   which is the durable answer and the next thing to build.
2. **No per-device revocation and no token expiry.** The only lever is rotating the single token.
3. **No visibility into who is connected or who has been knocking.**
4. **A memo carries no provenance.** Nothing distinguishes "the agent concluded this" from "the
   agent was told to write this by text it read," which is exactly the distinction a reader needs.
5. **A worker follows citations the reader never saw.** The trust model for assigning a worker is
   deliberate and matches the `run` card: the full memo body is displayed above the button, and a
   human who reads it and holds anyway is taken to have seen nothing obviously hostile. The render
   is now faithful enough to carry that — HTML and comments are escaped and shown as literal text
   (measured), and a link whose label differs from its URL shows the URL. What remains is that the
   worker reads the *file* rather than the render, so frontmatter is not on screen, and the brief
   tells it to "gather what it cites" — so a clean-reading memo can point at something that is
   not. Narrowing the worker to the bytes the reader saw is the remaining distance.

## Out of scope

- Anything reachable only by an attacker who already has local code execution as your user — they
  are already you, and every control here assumes the local user is trusted.
- The security of ngrok/Cloudflare tunnels, Claude Code, or your model provider's API.
- Denial of service against your own tunnel.

## Keys and secrets

Mars stores no provider API keys of its own; the key broker reads them from your environment or OS
keychain and other processes talk to it over a same-user Unix socket. The pairing token lives in
`~/.mars/serve.token` with user-only permissions, re-asserted on every write.

## Building and installing

Install with `--locked`. `cargo install` re-resolves dependencies and ignores the lockfile unless
told not to, which means the versions you get are not the versions that were tested — a broken or
hostile release of a transitive dependency is otherwise pulled in silently:

```
cargo install mars-terminal --features web --locked
```
