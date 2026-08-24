# Mars Design Doctrine & Vision (v3)

*Mars — mission control for your terminal.*

*The first-principles design doctrine for Mars — **the Emacs of the agentic era** — and
the evolution path by which it subsumes the terminal workspace (Zellij), the project
layer (dired/finder), and agentic autonomy, without ever betraying the cognitive
contracts that make it learnable.*

> **This is a vision document.** It describes what should exist and why, in dependency
> order — not implementation status. Where the present codebase already embodies a
> principle, that is evidence the principle is buildable, nothing more.

---

## 0. What Mars is

Emacs won its era on one bet: **a tiny core exposing one uniform abstraction (the
buffer, the command) and infinite extensibility on top**. Everything Emacs later
swallowed — mail, shells, directories, git — it swallowed because the abstraction was
universal and the keys, search, and help system worked identically on all of it.

The agentic era changes who operates the editor. There are now **four actors** issuing
commands:

| Actor | Retrieval mechanism | What they need |
|---|---|---|
| **Novice** | recognition (bar, menus, descriptions) | zero recall, day-one productivity |
| **Expert** | procedural memory (chords) | stability, speed, no guidance tax |
| **Agent** | semantics (registry context → `RUN:`) | a complete, typed, safe action vocabulary |
| **Automation** | composition (macros, hooks, pipelines) | replayable, parameterized, transactional actions |

Mars's bet, the analog of Emacs's: **one canonical `Action` registry, exposed to all
four actors through interfaces matched to how each one retrieves.** A capability that
exists for one actor exists for all four, for free. This is the moat. Every architectural
decision below is judged by one question: *does it keep the registry universal as the
product grows?*

The second bet is the inversion of extensibility. Emacs users extended it by writing
Elisp; most never did. Mars users extend it by **asking the agent** — natural language is
the new Elisp, and the config surfaces (keymap JSON, future hooks and macros) are data
precisely so that the agent can safely read and edit them. The extension language is now
spoken by 100% of users on day one.

---

## 1. First principles (the science spine)

These are the invariants. Features change; these do not.

1. **Working memory holds ~4 chunks** (Cowan 2001). Every choice surface stays small:
   scoped menus, prefix-scoped which-key panels, progressive disclosure (Hick's law).
2. **Recognition beats recall** (Norman). Knowledge lives in the world: descriptions on
   every action, live keybindings on every row, hints derived from the real keymap —
   *the UI is structurally incapable of lying about a binding*.
3. **Skill is procedural and follows the power law of practice** (Newell & Rosenbloom;
   Graybiel). It accumulates only on **stable mappings**. Therefore: spatial stability
   (fixed menu order, stable layouts), binding stability (movable until 1.0, add-only
   after), and Emacs compatibility as a *stability inheritance* — we adopt the deepest
   pre-trained keymap in computing rather than reset its practice curve.
4. **Guidance must withdraw itself** (expertise reversal, Kalyuga & Sweller). All
   teaching is timeout-arbitrated or event-triggered — which-key after hesitation, nudges
   after habits form — so experts never pay the novice's tax.
5. **Error costs are asymmetric** (Norman 1981 on mode errors). Plain keys are inert by
   construction (non-modal core); destruction requires explicit modification, and the
   costlier the error, the taller the gate: chord → confirm → transactional undo.
6. **Ergonomics is a budget, not a veto.** Chords are kept (recorded ruling — Emacs
   compatibility wins), but the budget rule stands: new frequent actions get thumb-side
   or single-chord routes; new *infrequent* capability lands in searchable/prefix space,
   never as fresh pinky-chord load.
7. **Frequency × recency is how memory prioritizes** (frecency). Every ranked surface —
   commands today; files, sessions, agent suggestions tomorrow — uses the same persisted
   frecency substrate.

---

## 2. The interaction doctrine

**One gesture rules everything: `Ctrl+Space`.** From any context — editor, terminal,
future file view or session view — the bar opens, and its prefix grammar routes intent:
type = search actions, `!` = shell, `?` = ask the agent, (future) `@` = files/symbols,
`>` = agent tasks. This is the Claude-Code contract generalized: *one entry point whose
first character declares the target namespace.* New subsystems must colonize a bar
prefix, never invent a parallel entry gesture.

**The graduation engine is the product's pedagogy** and must extend to every future
surface: every slow-path use passively displays the fast path (live bindings on rows,
which-key on prefixes); usage counters trigger subtle, never-blocking nudges; nudges
follow a **spaced-repetition schedule** — back off when the user adopts the binding
(bar-to-chord ratio falls), re-engage on regression. The counters to compute this
(per-action bar uses vs. total uses) are already the persisted substrate; the vision is
an editor that *quietly teaches each user their own top-ten chords and then shuts up.*

**The movement doctrine: any container in ≤2 keystrokes.** Navigation is the highest-
frequency action class and gets the flattest bindings: within-buffer (chars/words/lines/
pages/isearch/`M-g` goto-line/`M-<`/`M->`), between panes (`M-o` cycle, `M-arrows`
directional by real screen geometry), between tabs (`C-{`/`C-}` on modern terminals,
`M-{`/`M-}` everywhere, `C-PgUp/PgDn`, `M-1..9` direct jump), and into the terminal
(`` M-` ``, bar `!`). Splits are one mnemonic chord — the character *is* the split:
`C-|` right, `C--` below (`C-\`/`M--` as universal encodings). Rule: a movement that
happens many times per minute may cost one chord, never a prefix sequence — prefix
sequences are for *operations on* containers, not travel between them.

**`C-t` is the travel hub.** One chord opens a transient mode with one-char verbs and
an always-on cheat panel: ONE directional grammar walks the whole workspace — arrows
(or `hjkl`) move focus between split panes by real geometry and spill into the adjacent
tab at the pane-grid edge, so panes and tabs are two *views* of one space, not two
movement models. Then `1-9` jump to a tab · `⇧`-arrows / `H`/`L` reorder the focused
tab · `z`/`Space` zoom (toggle) · `d`/`⌫` close the focused view (the pane, or the whole
tab when it is the tab's last pane) · `t`/`T` new tab / terminal tab · `|`/`-` split ·
`x` swap pane. Rule inside the hub: *creation exits, navigation stays.* The panel is
the teacher; the direct chords above are what it graduates you to.

**Terminal-capability tiering.** Legacy byte encoding cannot express Ctrl+shifted-
punctuation (`C-[` *is* Esc) or the Cmd key (Mac terminals keep ⌘ for themselves).
Mars enables the kitty keyboard protocol where supported and tiers bindings: modern
chords (`C-{`, `C-}`, `C--`, `C-|`, `cmd-c/v/s/a`) light up there; a universally-
transmittable twin (`M-{`, `M-}`, `M--`, `C-\`, `C-c`/`C-v`, bracketed paste for ⌘V)
exists for every one. No capability is *only* reachable through a protocol-dependent
chord — and pane movement specifically gets Ctrl-tier bindings (`C-o` cycle,
`Ctrl+arrows` directional) because Alt is not Meta on stock macOS terminals.

**The chrome-layer principle: navigation chords are global; editing chords belong to
the focused surface.** Inside a terminal pane, tab/pane travel (`C-t`, `M-{`/`M-}`,
`M-1..9`, `C-o`, `Ctrl/M-arrows`, splits) means exactly what it means in the editor —
one key language across every pane. Editing chords (`C-k`, `C-c`, `C-x`…) are never
intercepted there: they keep their shell meanings (SIGINT, readline, bash prefixes).
Prefix sequences are never global — `C-x` belongs to bash inside a shell.

**The honesty invariant.** Every hint, badge, menu row, and which-key entry derives at
render time from the live keymap. Hardcoded key strings are a lint error in spirit: one
remap must update every surface. Trust in the hint system is the precondition for the
graduation engine — a user who has been lied to once stops reading hints.

---

## 3. The agent is an input device (and must be safer than one)

Treat the agent as a fifth limb on the keyboard, and apply the mode-error doctrine to it:

1. **Visibility before action** — the agent's proposed action renders as a plan the user
   can read (the `▶ Enter to run` bar today; multi-step plan previews tomorrow), exactly
   as which-key shows a chord's continuations before commitment.
2. **Inert by default** — agent output is text until explicitly fired.
3. **Gates scale with blast radius** — destructive singles confirm; future multi-step
   agent runs execute inside a **transaction** whose entire effect is one undo chunk.
4. **One-chunk recovery** — `C-g` cancels, and "undo the agent" must be as overlearned
   and reliable as `C-/`. Reversibility, not permission dialogs, is what makes autonomy
   adoptable: users grant agency in proportion to how cheaply they can revoke its output.
5. **The interruption budget.** As the agent becomes proactive (watching builds, noticing
   errors), its interjections obey attention economics: never modal, never steals focus,
   defers non-urgent output to natural task boundaries (save, pane switch, idle), one
   status line or an upward-growing panel. The cognitive cost of an interruption lands
   at the worst time by default; the editor's job is to time-shift it.
6. **The teaching loop includes the agent.** Every agent answer cites the command *and
   its binding* — the agent is the tip-of-the-tongue rescue and the vocabulary teacher,
   converting each rescue into a future direct-path invocation.

**Composition lives at the action layer, not the key layer.** v1 of this document argued
for Vim's operator·motion grammar because composability lets a bounded memory span an
unbounded command space. The grammar was rejected for keystrokes (recorded ruling), but
the argument survives one level up: **actions compose into pipelines and macros** —
recorded by users, authored by the agent ("every time tests fail, open the failing file
at the error"), replayed as single chunks, nameable, bindable, searchable. A handful of
learned actions × composition = the combinatorial vocabulary, achieved without modal keys.
This is where "Emacs power" is cashed out in the agentic era: *the agent writes your
keyboard macros.*

---

## 4. Evolution horizons

Each horizon subsumes a neighboring tool by re-expressing it in the registry + bar +
graduation doctrine. The sequencing is dependency-driven: each horizon's features assume
the substrates listed in §6.

### H1 — The project layer (subsumes: file pickers, fuzzy finders, dired)

- **Finder before tree.** Retrieval ("open the file I'm thinking of") is 10× more
  frequent than orientation ("what's around me"). So first: project-wide fuzzy file
  finding on the existing scoring machinery, ranked by **file frecency** (same persisted
  substrate, new namespace) — `C-x C-f` grows project awareness; `@` in the bar reaches
  files/symbols.
- **Directory-as-buffer, not sidebar-as-appendix.** The Emacs/dired lesson: a directory
  listing must be a first-class pane where *the same keys work* — `C-s` searches it,
  Enter opens, the bar acts on the selection. A tree bolted on with its own key language
  (the VS Code pattern) violates the one-vocabulary principle and is rejected in advance.
- **The map is shared context.** The project index that powers the human's finder is the
  *same index* handed to the agent. One index, two consumers — this is the readiness test
  for every future data structure: *can both a pane and the agent read it?*

### H2 — The workspace layer (subsumes: Zellij/tmux)

- **Sessions are spatial memory made durable.** Save/restore must reproduce layout
  *geometry* exactly — "terminal bottom-right, code left" is procedural knowledge, and a
  restore that reshuffles panes resets a practice curve (§1.3 applied to space).
  Session names rank by frecency; `C-x s` is the session prefix (reserved now, §5).
- **Detach/attach requires the client/server split.** Zellij's killer feature —
  processes survive the window — forces PTY ownership out of the UI process into a
  daemon. This is the single largest architectural fork on the roadmap and must be
  *decided before* terminal features accrete, or every terminal feature is built twice.
- **Pane verbs stay in the registry**: zoom/fullscreen, resize, floating panes, move —
  all plain Actions (bar-searchable, agent-invokable, which-key-discoverable under
  `C-x` window space). Zellij's discoverable hint bar is already Mars's native idiom;
  subsumption here is mostly *not inventing a second UI language* for it.
- **A terminal is a context surface.** The agent can read the visible terminal screen
  (with explicit user gesture): "why did this build fail?" needs no copy-paste. The
  vt100 grid is already structured data; H2 makes it part of the context bus.

**tmux/zellij parity roadmap** (audited against the code, 2026-07; detach/reattach,
`--resume`/`--list`, takeover, and shell-survival are shipped):

- *P0 — daily-driver blockers:* ~~terminal scrollback~~, ~~dead-shell lifecycle~~,
  ~~crash-safety~~, ~~pane resizing~~, ~~pane zoom~~ (all shipped 2026-07: 10k-line
  scrollback with wheel / Shift+PgUp/PgDn and snap-back-on-input; exited shells show a
  dismissal notice and recycle their pane; autosave of path-backed buffers on a timer +
  on detach with a failure notice, daemons log to `~/.local/state/mars/<name>.log`;
  layout-tree split ratios with travel `> < + =` resize; travel `z` zoom toggle).
- *P1 — comfort parity:* copy-mode-lite (keyboard select/yank from scrollback);
  ~~cwd inheritance~~ (shipped: terminals open in the first file's dir / launch dir);
  ~~session verbs inside the editor~~ (shipped: `RenameSession` + `C-t D` detach as
  actions; `mars new/attach/ls/kill/rename` CLI); `$MARS_SESSION` exported to spawned
  shells (prevents nesting); OSC-52 clipboard for SSH-remote attach.
- *P2:* multi-client mirroring; layout restore across daemon crash; ~~native Windows
  sessions~~ (shipped 2026-07 via an authenticated loopback control channel);
  status-line customization.

### H3 — The autonomy layer (subsumes: standalone agent CLIs)

- **Agent tasks are panes.** A running agent occupies a pane like a terminal does —
  visible, resizable, closeable, its transcript scrollable; `>` in the bar dispatches
  tasks. Multiple agents = multiple panes; the workspace layer (H2) is what makes agent
  parallelism legible rather than hidden.
- **Plan preview → transactional run → one-chunk undo** (§3) becomes the standard
  execution shape for anything multi-step, human- or agent-initiated.
- **Retrieval goes semantic when scale demands it** (§6 ladder), and the ask-agent
  fallback quietly absorbs whatever lexical search misses — the TOT rescue *is* the
  semantic layer until embeddings pay their complexity rent.
- **Proactivity under the interruption budget** (§3.5): build watchers, error spotters,
  "you've done this 4 times, want a macro?" — the graduation engine generalized from
  teaching keybindings to teaching *automations*.

**Shipped from this horizon (0.7.0–0.7.1).** The manager is the proactivity strand
made real: it watches the panes, writes a briefing and memos, and — critically — is
rationed by the interruption budget rather than by what it happens to notice. Rover
carries that off the machine, first to a phone and now to a second screen. The
doctrine held on contact: everything the client can do is something a person presses,
the agent behind Rover chat is read-only and every effect is a proposal, and nothing
renames, runs or ends anything on its own. What is still open from H3 is the
*transactional* half — plan preview → transactional run → one-chunk undo is still the
per-action gate described in §3, not yet a journal (§6).

---

## 5. The keymap zoning law

Prefix namespaces are urban planning: cheap to reserve on empty land, ruinous to
retrofit. **Reserved now, before 1.0 freezes the map:**

| Prefix | Namespace | Horizon |
|---|---|---|
| `C-x` (bare) | buffers, files, windows — Emacs canonical, never colonized | H0 |
| `C-x t` | tabs | H0 |
| `C-x p` | project: finder, tree, recent files | H1 |
| `C-x s` | sessions: save, restore, detach, list | H2 |
| `C-x a` | agent: task, macro, replay | H3 |
| Bar `!` `?` `@` `>` | shell · ask · files/symbols · agent tasks | H0/H1/H3 |

Routing rule for every new action, unchanged and non-negotiable: very-high frequency →
single chord (budget-checked, §1.6); Emacs-canonical → the Emacs sequence, never a
rival; grouped/medium → prefix + which-key; rare/parameterized → bar-only; destructive →
+gate. **Keymap profiles** ship as data (`emacs.json`, `vscode.json`, `mac.json`): the
arriving VS Code user gets `Ctrl+P`/`Ctrl+\``, the Emacs user gets home — same registry,
different surface, because bindings are data, and identity-import is the cheapest
adoption lever we have.

---

## 6. Readiness audit — the load-bearing substrates

*The direct answer to "are we ready from first principles?"* The interaction doctrine
(registry, bar, graduation, honesty, zoning) scales as-is. Four substrates do **not**
yet exist and everything in H1–H3 loads onto them. In dependency order:

1. **Parameterized actions (the `interactive` spec).** Today every Action is nullary;
   arguments arrive via ad-hoc prompts. Emacs commands declare their arguments — that's
   what makes them scriptable, bindable, and composable. Mars needs a declarative
   argument schema per action (name, type, completion source, default) so that: the bar
   can prompt inline, the agent can emit `RUN: FindFile("src/main.rs")`, macros can bind
   partial applications, and hooks can fill args from context. **Blocks: agent
   composition, finder, macros — nearly everything.** Build first.
2. **The transaction journal (global, multi-buffer undo).** Per-buffer snapshot undo
   cannot express "revert everything that agent run did across five files and a
   terminal." An editor-wide journal of grouped operations — every multi-step effect
   (agent run, macro, project-wide replace) is one labeled, inspectable, reversible
   chunk. **Blocks: agentic trust (§3), hence adoption of autonomy.** Build second.
3. **The context bus.** Today the agent sees only the action registry. Formalize a
   single read interface over: registry, open buffers + cursors, project index (H1),
   visible terminal screens (H2), recent actions (the user's attention trace) — with
   per-surface user consent. One bus, all context consumers (agent, finder ranking,
   proactive watchers). **Blocks: every "agent understands my situation" moment — the
   product's core differentiation.**
4. **The client/server decision.** Session persistence and detach (H2) require PTYs and
   state to outlive the UI process. Deciding this architecture *early* is cheap;
   retrofitting a daemon under an accreted single-process feature set is how tmux
   competitors die. **Blocks: Zellij subsumption.** Decide (not necessarily build) now.

Cheap reservations to make immediately, per the zoning law: the `C-x p/s/a` prefixes and
bar sigils (§5); the retrieval ladder thresholds — **subsequence scoring to ~200
actions, add n-gram at ~200, embeddings only past ~1k and only local** — so search
quality degrades by plan, not by surprise; and file-frecency namespacing in the persisted
state schema.

Everything else in this document is doctrine already validated in the running editor:
non-modal core, honesty invariant, graduation engine, gates on destruction, one-gesture
entry. The foundations are sound; the four substrates above are the difference between
an excellent editor and the platform this document describes.

---

## 7. Decision log

Rulings made in debate (user decisions — recorded so future contributors don't
relitigate silently):

- **Non-modal core; Vim operator·motion grammar rejected** for keystrokes — the
  composability argument survives at the action layer (§3).
- **Chords kept** despite the RSI case — Emacs compatibility wins; ergonomic budget rule
  (§1.6) governs additions.
- **`Ctrl+Space` = command bar**, not set-mark. Selection = Shift+arrows + mouse.
  `C-@` set-mark is physically impossible in terminals (NUL collision) — dropped.
- **Fixed empty-query menu order**; frecency ranks search results only.
- **Bindings movable until 1.0**, add-only after.
- **Nudges are one status line**, never popups, never blocking.
- **`C-v` = system-clipboard paste** (explicit ruling, movement audit 2026-07): breaks
  Emacs page-down deliberately — `M-v`, `PgDn`, and the wheel remain. Kills/copies
  (`C-k`/`C-w`/`M-w`) also write the OS clipboard; `C-y` stays kill-ring-internal.
- **`C-t` = travel mode** (revises the earlier `C-t` = new-tab ruling): new tab is
  `C-t t`; terminal toggle lives on `` M-` `` and `C-x C-t`. Movement set per the
  movement doctrine (§2): `M-o`/`M-arrows` panes, `C-{`/`C-}`+`M-{`/`M-}`/`M-1..9`/
  `C-PgUp/PgDn` tabs, `C-|`/`C--` splits, `M-g` goto-line.
- **`C-c` = copy, `C-v` = paste** (system clipboard): the Emacs `C-c` prefix is
  sacrificed — bare `C-c` copies the selection, or the whole line without one.
  Inside terminal panes both pass through to the shell (SIGINT preserved).
- **Pane movement is Ctrl-tier** (round-3 ruling: Alt-based pane nav was "ugh" on mac):
  `C-o` cycles (Emacs open-line sacrificed), `Ctrl+arrows` directional; Alt twins kept.
- **`cmd-`/`super-` bindings supported** (`cmd-c/v/s/a` defaults); tiered per §2 —
  native on super-reporting terminals, `C-c`/`C-v` + bracketed paste elsewhere.
- **Behavioral numbers live in `tuning.json`**, each knob as `{value, description}` —
  the first agent-editable config surface (a Context-Bus precursor): an agent can read
  what a knob does and adjust it on request.
- **Agent providers by env precedence**: `MARS_LLM_*` (any OpenAI-compatible endpoint,
  e.g. local Ollama; legacy `ARES_*` honored) → enterprise (Bedrock/Azure) →
  `ANTHROPIC_API_KEY` → `OPENAI_API_KEY` → `GROQ_API_KEY` →
  `GEMINI_API_KEY`/`GOOGLE_API_KEY` (Gemini's OpenAI-compatible endpoint).
- **AWS Bedrock + Azure OpenAI (2026-07): bearer/api-key only, no SigV4.** Ruled:
  MARS supports Bedrock via **Bedrock API keys** (`AWS_BEARER_TOKEN_BEDROCK`, a plain
  bearer) over the **Converse API** — provider-neutral so any Bedrock model works —
  and Azure via its OpenAI-compatible surface (`api-key` header + deployment URL).
  IAM/SigV4 is deferred because a HMAC signer would add crypto deps to a
  deliberately dependency-light single binary; Bedrock API keys fit the existing
  "one key string" model exactly. Bedrock is non-streaming for now (converse-stream
  is AWS binary event-stream framing, not SSE). No broker protocol change: the home
  daemon re-derives the provider from env, so an enterprise key at home works on
  every ssh'd box unchanged.
- **Mars rebrand + palette (2026-07):** the project is Mars — "mission control for
  your terminal." Palette anchored on Claude Code's terracotta **#D97757**
  (`theme_accent`) for all chrome, light sand **#E9A178** on teaching surfaces, rust
  **#B7410E** in the splash gradient; selection deep rust-brown. Rule: **brand lives
  in chrome, not in meaning** — green stays on live terminals; red-as-danger stays
  reserved for confirms. Theme values are described knobs in `tuning.json`. The day-0
  splash (logo + tagline + starter hints, dismissed by any key) is the graduation
  doctrine's first surface. Legacy `ARES_*` env vars and `~/.config/ares` keep
  working (env fallback + one-time config migration).
- **Agentic workflows design (2026-07, `workflows_design.md`):** the first 7 of the
  10 non-commoditized AI workflows are specced against three substrates — a
  trailing-line **directive vocabulary** (chosen over native tool-calling for model
  portability incl. local Ollama + a human-readable confirm gate; `AgentDirective` is
  the seam to add tool-calls later), **context selectors** (model-driven `NEED:`
  expansion over always-dumping), and a **trigger framework** (event-driven, a
  pull-rendered notices queue that structurally enforces the interruption budget, one
  global in-flight LLM gate). W3's shell-translate resolved to Tab-translate rendered
  as a **cursor-anchored overlay** (no eye-jump). `agentic_inline.md` is the product
  brief; `workflows_design.md` is the build spec. Ruled: `OPEN:` directive is
  line-only; watch/brief use passive one-liners (no bell) in v1.
- **Persona = VOICE tasks only (2026-07):** a user style file (`~/.mars/persona.md`)
  rides into ask/watch as the FINAL system message under a precedence preamble —
  style can color prose, never rules. FORMAT tasks (translate, naming, mission,
  shift-batch) never see it: machine-parsed output, and mission text is re-ingested
  into prompts (a persona there would feed back into itself). Default voice: mission
  control addressing the ship's captain; empty file = off.
- **Shift report + verdict triage ladder (2026-07):** reattach shows a full-screen
  save-state restore by default (`shift_report` knob: full/notice/off) — shown only
  when something happened. Verdicts escalate ONE WAY: deterministic tier-0
  (exit codes, tail heuristics — free, also what a keyless mars uses) → one batched
  low-tier call for ambiguous rows → `model_above` only on self-check failure.
  Ruled: the overlay frame is never blocked on a model — LLM output only ever
  replaces a defensible deterministic placeholder, and the streaming replacement is
  diegetic ("telemetry coming in"). Auto-watch ON by default, gated by a
  min-active-seconds floor and a focused-pane skip.
- Dropped after evaluation: n-gram scoring below ~200 actions; onboarding starter-sets
  (a small fixed root menu *is* the starter set); literal `Shift+</>`-style movement
  chords (shifted punctuation is indistinguishable from typing in a non-modal editor —
  Alt-based equivalents chosen instead).

Terminal-reality constraints (discovered, permanent): `C-/` arrives as `C-_` (bind
both); `M-<` arrives as ALT|SHIFT+`<` (normalize shift on symbols); `C-@` = NUL =
legacy `Ctrl+Space`. Headless selfchecks cannot verify raw byte encodings — real-terminal
passes remain mandatory for chord changes.

---

### References

- **Cowan (2001)** — working memory ≈ 4 chunks; the ceiling every surface respects.
- **Hick (1952) / Hyman (1953)** — choice RT ∝ log n; progressive disclosure.
- **Newell & Rosenbloom (1981)** — power law of practice; stability beats optimality.
- **Graybiel (1998)** — basal-ganglia chunking; where bindings (and macros) live.
- **Kalyuga, Sweller et al. (2003)** — expertise reversal; guidance that withdraws itself.
- **Norman (1981; 1988)** — mode/action slips, recognition over recall, recovery over
  prevention — extended here to agent actions.
- **Brown & McNeill (1966)** — tip-of-the-tongue; the ask-agent's cognitive niche.
- **Mozilla (2008)** — frecency; generalized here to files, sessions, suggestions.
- **Prior art:** Emacs (buffer universality, `interactive` spec, dired, which-key);
  Zellij/tmux (sessions, detach, discoverable hints); Claude Code / opencode (one bar,
  `!`/`?` sigils, search-first); Spacemacs/Doom (the RSI evidence); Firefox Awesome Bar.
