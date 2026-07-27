# Rover — the phone/web client and the `mars serve` bridge

Rover is the semantics-first mobile client (see `design_ideas/rover-brand.md`,
`rover-delight.md`, `mobile-pwa-mvp.md`). Two repos, one seam.

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
