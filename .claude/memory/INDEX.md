# Memory index

Format: - `<topic>.md` — <one-sentence description> [keyword1, keyword2, keyword3]

- `build-verify.md` — how Mars is compiled, selfchecked, and published, plus the build gotchas that cost an hour each [build, cargo, selfcheck, test, verify, fingerprint, publish, install, feature, stub]
- `terminal-io.md` — byte-level terminal behavior: key encodings, PTY/vt100, scrollback, clipboard, and how to test against a real terminal [terminal, key, chord, encoding, crossterm, kitty, vt100, pty, ansi, scrollback, clipboard, pyte]
- `sessions-daemon.md` — the client/server session split, daemon lifecycle, CLI surface, TTY hygiene [session, daemon, socket, detach, attach, client, server, tty, cli, rename, nested]
- `ssh-broker.md` — `mars keyd` + `mars ssh`: the key never leaves home, the fleet registry, route and credential invariants [ssh, broker, keyd, remote, fleet, auth, capability, credential, tunnel]
- `windows-port.md` — Windows adapter facts: ConPTY, control-channel auth, key events, job objects, OpenSSH limits [windows, conpty, msvc, hmac, nonce, job, openssh]
- `ui-input.md` — keybinding rulings, mouse/hit registry, selection, undo, navigator, motion, theming [ui, mouse, click, selection, drag, undo, tree, navigator, sidebar, motion, keybinding, theme, palette, splash, render]
- `agent-llm.md` — provider precedence and routing, directives, and the shipped W1-W7 agent workflows [agent, llm, provider, anthropic, groq, gemini, model, tier, prompt, directive, watch, notice, digest]
- `design-docs.md` — what each root doc is for, the durable invariants, and which `design_ideas/` docs are unbuilt proposals [design, doc, invariant, roadmap, strategy, proposal, vision]
