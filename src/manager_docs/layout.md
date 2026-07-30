# Layout

```
~/.mars/manager/
├── AGENTS.md                    how to be the manager (start here)
├── docs/                        this guide
│   ├── layout.md                you are here
│   ├── cards.md                 the output format
│   ├── memory.md                what to remember, and where
│   └── tools.md                 how to look at the world
├── policy.md                    autonomy grants — HUMAN-EDITED ONLY
│
├── index.md                     status of everything, human-readable   ← generated
├── index.json                   the same, for Rover — one read         ← generated
├── timeline.md                  append-only event log, oldest first    ← generated
│
├── inbox/                       stimuli waiting to be processed
│   └── done/                    processed, kept briefly
│
├── sessions/<name>/
│   ├── mission_briefing.md      this session's summary                 ← generated
│   ├── cards/                   YOUR output for this session
│   │   └── card-*.md
│   └── snapshots/               raw stimuli, gitignored                ← generated
│
└── memory/                      YOURS. Nothing else writes here.
    ├── beliefs.md               working memory, ≤200 lines, REWRITTEN
    ├── projects.md              what each project is
    └── cursor.json              how far you have read, per workspace
```

## The hierarchy

**session → workspaces → summary.** A *session* is one Mars daemon on one host. A *workspace* is a
pane inside it — a terminal running something, or a document. A session's `mission_briefing.md` is
the summary of its workspaces; `index.md` at the root is the summary of the sessions.

Read downward when you need detail, upward when you need context.

## Reading order on a cold start

1. `index.md` — what is happening at all
2. `memory/beliefs.md` — what you already concluded, and what the engineer has ignored
3. `memory/projects.md` — what these projects are for
4. the stimulus in `inbox/` — what changed since last time

Everything else is fetched on demand. Do not read a whole session tree speculatively; it is large
and most of it is unchanged.
