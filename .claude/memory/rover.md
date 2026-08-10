# Rover — the phone/web client and the `mars serve` bridge

Rover is the semantics-first mobile client (see `design_ideas/rover-brand.md`,
`rover-delight.md`, `mobile-pwa-mvp.md`). Two repos, one seam.

## Deploy topology (two GitHub remotes — don't confuse them)
- **Webapp** `mars-rover` → remote `github.com/Crossvalidated-Ventures/mars-terminal.git`,
  branch **`main`**, **Lovable-connected** (push to main → Lovable rebuilds
  `mars-terminal.lovable.app/rover`). Bidirectional sync — never rewrite pushed history.
- **Rust daemon** `mars-terminal` → remote `github.com/anjishnu/mars.git`. The `mars serve`
  bridge + pane-streaming work lives on branch **`rover-dev`**.
- Reinstall the binary with `cargo install --path . --features web --force` (validly signed;
  no codesign needed, unlike a cp'd binary). Then start a FRESH `mars serve` session — old
  sessions run the pre-reinstall binary and don't speak the new frames.

## Field-test learnings (2026-07)
- **LLM features split by process**: `ask`/`summarize`/mission-briefing run in the **bridge**
  (`mars serve`, reads its own env) — the phone's briefing embeds the board in the question so
  the keyless-context bridge can still summarize. **Auto-naming + the daemon's own model
  briefing run in the SESSION daemon** (`maybe_auto_name`, `shift_brief`), which reads the key
  from ITS env. If the session started without a key, they silently fall back to
  `deterministic_narrative()` (filler) and skip naming. Fix without restart: **`mars keyd`**
  (broker; `from_env` detects the socket and proxies). No broker was running in testing.
- **Dive input is compose-then-send**, not live keystrokes (laggy over a network + IME fights
  it). KeyBar reduced to esc/C-c/enter. Nav in the dive is a breadcrumb + bottom back button,
  NOT swipe (swipe-back was too twitchy).
- **Editor panes**: `pane_screen_json` now renders `PaneContent::Editor` (buffer-rope viewport
  with a line gutter), not just terminals — else diving into an editor showed a blank screen.
- **Reflow-to-fit IS done — active-app takeover** (per user, non-takeover glance was dropped
  here on purpose): `WatchPane` carries the phone's cols/rows. While the phone watches, it
  OWNS the pane size via `App::mobile_reflow: Option<(PaneId, rows, cols)>`; the render
  (`ui.rs` ~500) keeps that pane at the phone's width instead of the layout, so a wide TUI
  reflows narrow even with a desktop attached. `App::resize_pane_to` does the actual
  `Terminal::resize` (→ SIGWINCH). Mars reclaims the moment the desk user interacts — the
  `SrvEvent::Input` `ev` branch clears `mobile_reflow` on any redraw-forcing input, and the
  next render sizes back to the layout. Selfcheck "rover takeover / reclaim" covers both
  directions (render honours takeover; reclaim on clear).

## Current state (2026-07, post redesign)
- Route is **`/rover`** (`src/routes/rover.tsx` → `src/rover/*`), NOT `/c`. Dev/build/typecheck:
  `npx vite dev --port 8080 --strictPort`, `npx vite build`, `npx tsc --noEmit`. `bun` IS now
  installed (~/.bun) but `npx vite` works fine — the Lovable build uses the committed lockfile.
- **Dive** (formerly "peek") = the live-pane view (`ui/RawScreen.tsx`). Renders the daemon's
  screen in **colour**: the daemon serialises with vt100 **`rows_formatted`** (per-row ANSI SGR,
  not `contents()`), and the phone parses SGR into styled spans in **`src/rover/ansi.ts`** —
  **no xterm** (it broke the Lovable build via a zod v3/v4 clash; we only display, never emulate).
- **Typing into a pane** (corrected 2026-08-04 — the old "hidden `<textarea>` sink in the dive"
  note described `RawScreen.tsx` and is no longer true of the shipped surface): the ONLY
  production path is `CommandComposer` (`ui/Command.tsx`). `TerminalSurface.tsx`'s own
  `{t:"key"}` send is gated behind `import.meta.env.DEV && hash.includes("probe=1")`, so the
  dive itself is read-only in a deployed build. `ui/KeyBar.tsx` is DEAD CODE — nothing imports
  it; the real key list is `RAW_KEYS` in `Command.tsx`, frecency-sorted, so a newly added key
  sorts to zero and sits below anything you've used (it looks missing without scrolling).
- **Wire path**: `{t:"key",paneId}` → bridge parses paneId as `usize` → `ClientFrame::PaneInput`
  → `App::write_to_pane` → `send_bytes`. A paneId that fails `parse::<usize>()` falls through to
  `ClientFrame::Paste`, which goes to the **focused** pane — a silent misroute (`serve.rs` emits
  a literal `"main"` on the attach-mode rung, so this is reachable). Non-terminal panes are a
  silent no-op.
- **OPEN BUG — text into a Claude Code pane composes but never submits.** `write_to_pane` writes
  raw bytes; the desktop paste path (`App::paste_text`, `app.rs` ~2064) wraps in `ESC[200~`/
  `ESC[201~` when `t.screen().bracketed_paste()`. Unmarked, Claude Code applies its own burst
  heuristic and swallows the following CR as a literal newline. `Command.tsx: toPane()` tries to
  dodge this with a 180ms gap before the Enter — insufficient. Real fix is to bracket the text
  and send the CR as a separate key; `answer` (`y\n`) and `act` (`cmd\n`) must NOT be bracketed,
  their newline is the submit. Attempted 2026-08-04, reverted unmerged at the engineer's request.
- **Session rename**: phone sends `{t:"rename",name}`; the bridge carries `ClientFrame::Rename` on a
  FRESH connection (the subscribe writer would get closed) → daemon fs-renames → next board snapshot
  returns the new name. Registry mirror via `renameSession(daemonId,name)`.
- **Ambient health**: board json carries a `health` string (`app.health.line()`); `push_mobile` is
  now `&mut` and samples health each push (detached sessions don't run the render-loop sampler).
- **Themes** mirror the terminal: Mission Control, **Eclipse** (was Night), **Terracotta Sol**
  (was Day), **Hacker** (was Phosphor), Amber. Old ids migrated on load (`THEME_ALIASES`).
- **PWA**: `public/rover.webmanifest` (scope `/rover`) + terracotta-circle-on-black icons
  (`rover-icon-{192,512,maskable}.png`, `apple-touch-icon.png`), linked in `routes/__root.tsx`.
- **ALWAYS bump `CACHE = "rover-vNN"` in `public/sw.js` when shipping anything a phone renders.**
  The service worker re-installs only when its OWN bytes change, so without a bump it never
  re-activates and never purges: the change is live on the server and invisible on the device.
  Cost this a whole debug round on 2026-08-04 — the code was confirmed in the pushed source AND
  in the built bundle while the phone still showed the old menu. Two app launches to land (an
  update activates on the launch AFTER the one that fetches it).
- **iOS gets only half of the keyboard compensation.** `interactive-widget=resizes-content`
  (`routes/__root.tsx`) is Android/Chrome only; iOS Safari overlays the keyboard and SCROLLS the
  visual viewport instead. Track `visualViewport.offsetTop` and translate the frame by it, not
  just `.height` — `RoverApp.tsx` does both now. `offsetTop` stays 0 on Android, so it needs no
  platform sniff. The frame carries `zoom: scale`, so viewport pixels are ÷scale.
- **Routing**: 1 paired session → its briefing is home (fleet is a menu item); 0 or many → fleet hub.
  A QR scan always routes through the loading screen (RoverProvider keyed on daemonId → remount).

## Web app (separate repo)
- Lives at `/Users/anjishnukumar/Mars-Mission/mars-rover` — TanStack Start + React 19 +
  Tailwind v4 + shadcn, Lovable-connected (don't rewrite pushed history; keep branch working).
- **`bun` is NOT installed on this machine** → use `npm`. The Vite 8 / Nitro betas force
  peer conflicts, so install with `npm install --legacy-peer-deps`. Node 22 is at
  `~/.local/bin/node`.
- The Rover app is route **`/c`** (`src/routes/c.tsx` → `src/rover/*`), separate from the
  marketing landing (`/`). Verify: `npm run build` (SSR, cloudflare preset), typecheck
  `npx tsc --noEmit`, dev `npm run dev` (port **8080**). The dev route tree regenerates a
  beat after start — wait ~12s before curling `/c` or you get the stale tree.
- Demo runs against a **mock daemon** (`src/rover/mockDaemon.ts`) that speaks the real
  protocol — click "Enter demo (mock daemon)" on `/c`. The mock is NOT a stub hiding work;
  it emits/accepts the identical `ServerEvent`/`ClientAction` contract.
- Key modules: `protocol.ts` (the seam), `crypto.ts` (WebCrypto non-extractable device
  keypair — identity-keyed creds, not bearer), `connection.ts` (transport-agnostic: `mock`
  → fixtures, else reconnect-first `wss`; endpoint read from the QR fragment), `store.tsx`
  (the brain — ranks the board client-side), `ui/*` (Board, WorkspaceCard, Briefing, KeyBar,
  Peek=xterm, BootPairing, RoverApp). xterm is dynamically imported (SSR-safe).

## The bridge — `mars serve` / `mars qr` (this repo)
- `src/serve.rs`, behind an **OFF-by-default `web` cargo feature** (`web = ["dep:tungstenite",
  "dep:qrcodegen"]`). Build/verify: `cargo build --features web`. Default + `--no-default-features`
  builds are UNAFFECTED (module is `#[cfg(feature="web")]`; the `serve`/`qr` CLI arms are
  cfg-split, printing a rebuild hint when off).
- **Sync WS on purpose**: the codebase has NO async runtime (thread-per-connection). Use
  `tungstenite` 0.24 (blocking; its `Message::Text(String)`/`Binary(Vec<u8>)` API is intact
  through 0.24 — the `bytes`-backed switch is 0.26+, so don't bump past 0.24 without editing
  `serve.rs`). `qrcodegen` 1.8 renders the pairing QR (dark-on-white ANSI cells so it scans
  on any terminal theme).
- The bridge is a **second client of the session socket**: reuses `session::{socket_path,
  write_frame, ClientFrame, ServerFrame, SESSION_PROTOCOL_VERSION, list_sessions, debug_log}`
  + `sys::control::connect`. Daemon protocol unchanged. Per-conn: peek → WS-upgrade goes to
  the bridge, else static app shell (`MARS_WEB_DIR/index.html` or a notice). Daemon-reader
  thread → mpsc → single WS loop (so it never deadlocks). Port 8787.
- **Scaffold scope**: it bridges the **raw terminal** (daemon `Output` ANSI → WS `{t:"output"}`,
  inbound keystrokes → `Paste`) — powering Rover's *peek* rung end-to-end. The **structured
  meaning channel** (JSON board/verdict/summary the phone renders natively) is the next
  daemon-side step: emit `bar_workspace_rows()` (app.rs:842) and `ShiftReport` (briefing.rs)
  as JSON. Raw-key fidelity (byte→KeyEvent) and the structured action sink are also TODO.

## Locked design decisions (see the design_ideas docs)
- Daemon = event bus + session/workspace host + action sink + keyless tier-0 (rank & ring);
  Rover = the brain; LLM key stays on trust via proxy. Input = structured intents (bulk) +
  pane-targeted raw keystrokes (peek only). Transport-agnostic, identity-keyed creds; LAN
  http prototype → Cloudflare tunnel → Tailscale later behind a 3-method `Transport` seam +
  endpoint-list. TLS-to-edge now, Noise E2EE later. Hero surfaces: board · blocked-answer ·
  briefing · summaries (plans next). Single-daemon UI, registry built for many.


## Pane scrollback is a numbered document, not "the last N rows" (2026-07-29)
- The daemon captures every line that scrolls off a pane into a per-pane transcript
  (`terminal.rs`: `LineLog`, `capture()`, `Term::lines`), numbered, byte-bounded by
  `tuning.terminal_line_log_bytes` (8 MB). Wire: `{t:"lines",paneId,from,to}` →
  `{t:"lines",paneId,from,first,total,rows[]}`, half-open `[from,to)` in LINE IDS.
  `first` = oldest retained id, `total` = id the next line gets. `history_ansi` + the 512 KB
  raw byte ring still exist for older clients; retire once nothing asks for `history`.
- HOW capture works, because it is non-obvious: vt100 exposes the scrollback OFFSET but not
  its DEPTH, and the depth SATURATES at the limit, so you cannot difference it to learn what
  just scrolled. The offset CAN be differenced — vt100 bumps it once per scrolled line to keep
  a scrolled-back view pinned (grid.rs `scroll_up`). Park it at 1 before `process()`, read it
  back as `1 + scrolled`. Feed bytes in 512-byte slices so a burst can never scroll more lines
  than the parser retains. `set_size` does NOT clear scrollback (contrary to an old comment).
- Programs repainting inside a scroll region never scroll the grid, so they never enter the
  transcript — that is why commands stopped appearing three times.
- The live screen push carries `first`/`total`. Without them a client scrolled up cannot learn
  that lines left the screen since its last fetch, and a GAP opens at the seam. The client
  asks for `[highest_held + 1, total)`.
- Client keeps rows in a `Map<id, text>` (`RawScreen.tsx`), renders the contiguous run ending
  at `total` — which is exactly where the live screen starts, so the seam needs no arithmetic.
  A duplicate reply merges onto the same keys instead of appending a second copy.
- Verify with `tools/transcript-probe.mjs` (dev server on :8080, real app + mock daemon). Two
  probe traps, both cost time: a `<pre>`'s textContent runs the row `<div>`s together with NO
  newline (query `pre > div`), and `document.querySelector("pre")` can return a LOWER app
  level still mounted behind the slide — pick the pre whose text matches your fixture.

## Alt-screen panes get NO transcript — recover on the client (2026-07-29)
- vt100 builds the alternate grid as `Grid::new(size, 0)` — scrollback length ZERO. A
  full-screen TUI repaints in its viewport, nothing scrolls off, so `LineLog` stays empty and
  the daemon has nothing to serve. Symptom: "everything above this is gone" on a Claude Code
  pane, with a tug affordance that can never produce anything. The transcript works fine for
  normal shell panes; this is not a wiring bug, it is structural.
- Fix is client-side (`RawScreen.tsx` `shiftOf`). Derive it from the one equation that defines
  a scroll: `next[i] === prev[i+n]` for every row not rewritten. You don't know which rows were
  rewritten, so VOTE — each non-blank row of `next` that also occurs in `prev` at index `j`
  votes for shift `j-i`; most votes wins (ties → smaller shift, so ambiguity under-captures).
  O(H) via a row→indices Map; ~9µs at 125 rows.
- DO NOT normalise the vote. Dividing matches by rows-compared turns a rewritten row (spinner,
  token counter, streaming text, input box) from an abstention into evidence AGAINST the right
  answer, and then needs a similarity threshold that can only be fitted to one fixture. A 0.6
  constant survived 8 rows of chrome at ratio 0.62 and died at 14. Counting needs no threshold
  because we only need the BEST shift, never an absolute quality.
- A pinned header needs no special case: a static row is only explained by shift 0, so it adds
  a few votes there and none elsewhere. It wins only when more of the screen is pinned than
  moving, where "nothing scrolled" is the better reading anyway.
- THE SUBTLE BUG: a shift does not say WHICH rows left. `prev.slice(0, n)` is wrong whenever
  anything is pinned — with a 2-row header it captured header+blank+ONE output line, which
  looks like recovery working while dropping two thirds. Slice from the top of the MOVING
  block: the lowest row index voting for the winning shift (`{ n, from }`).
- Recovered rows splice at the daemon line id where capture BEGAN (`local.anchor`), not after
  all daemon rows — a pane that runs a TUI and returns to the shell has daemon history on both
  sides. An early gate of "use the daemon transcript if it is non-empty, else local" is WRONG
  and silently disables recovery, because a shell prompt always precedes the TUI.
- Only lossy for frames never pushed (>1 screenful between 40ms pushes). That shape of output
  (`cat` a big file) runs on the NORMAL screen, where the daemon records it by id — the two
  mechanisms cover each other.
- Fixture: type `alt` in the mock pane (`mockDaemon.ts`) — a 120-row pane (phone-sized: visible
  + BASE_SCROLLBACK, NOT a 24-row tty) with a pinned header and 14 rows of volatile chrome,
  scrolling 3 rows/120ms with `total` frozen. Size matters: at 24 rows a React-coalesced frame
  is an unrecoverable loss; at 120 it is just a larger shift the vote handles.
- WORKFLOW that finally cracked it: stop iterating in the browser. Copy `shiftOf` into a
  standalone `.mjs`, generate the frames, print the shift per frame. That immediately showed
  the estimator was returning a clean 3 every frame and the loss was downstream — three
  speculative "fixes" had been aimed at the wrong function.

- **`fs.read` answers with the RESOLVED path, never the `~/…` literal you sent.**
  `fs_read_json` (`serve.rs`) runs `resolve_path` then serialises `path.display()`, so a
  request for `~/.mars/manager/index.json` comes back as `/Users/<me>/.mars/manager/index.json`.
  Any client guard of the form `fsFile.path !== SOME_TILDE_CONST` is therefore false on every
  read. This silently disabled the whole manager feed (`useFeed` never left `null`) for its
  entire life; the symptom was Rover showing an LLM briefing instead of the deterministic
  narrative. Match on the expanded tail (`path.endsWith(CONST.replace(/^~/, ""))`).
- **The LLM briefing ratchets its own hallucinations.** `briefingPrompt` passes the previous
  briefing in as `prev` and asks for only what CHANGED, so once the model invents an item it
  restates it as still-true forever — a quiet board never contradicts it. `lastGreeting` is
  worse: cached in localStorage, keyed by session NAME, asserts board state, never expires.
  Clearing `rover.briefing.*` / `rover.greeting.*` is the only way to flush a poisoned cache.
- **A board row's `id` is a `tab_index`, NOT an identity — never address a workspace by it.**
  `mobile_board_json` emits `"id": s.tab_index.to_string()`, a position in `App::tabs`, so
  closing any workspace shifts the id of every one after it. An id that crossed the wire can
  name a different workspace by the time it lands, and the miss is silent. Every row also
  carries `paneId` (a real `PaneId`, stable for the pane's life) — address workspaces by that.
  **Workspace rename** (2026-08) does: phone sends `{t:"rename_workspace",paneId,name}` →
  `serve.rs` → `ClientFrame::RenameWorkspace{pane,to}` → `SrvEvent` →
  `App::rename_workspace_of_pane`, which finds the tab whose `layout.pane_ids()` contains the
  pane. Unlike the SESSION rename it rides the subscribe writer (the daemon does not close the
  stream) and needs no local registry mirror — the next board push carries the name back.
- **`ui::workspace_name` ignores a purely numeric tab name** (`tab.name.parse::<usize>()` must
  ERR for the custom name to win), so renaming a workspace to `42` from either the phone or the
  desk prompt silently falls back to `terminal N` / the filename. Pre-existing and shared by
  both rename paths; a validating UI would have to reject digit-only names on its own.
