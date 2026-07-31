<!-- mars-doc-version: 1 -->
# Layout

```
~/.mars/
├── manager/                     THE REPO — guide, memory, aggregate index
│   ├── AGENTS.md                how to be the manager (start here)
│   ├── docs/                    this guide
│   ├── policy.md                autonomy grants — HUMAN-EDITED ONLY
│   ├── index.md                 status of everything, human-readable   <- generated
│   ├── index.json               the same, for Rover — one read         <- generated
│   ├── timeline.md              append-only event log, oldest first    <- generated
│   ├── inbox/                   stimuli waiting to be processed
│   ├── archive/YYYY-MM-DD.jsonl everything you have ever written       <- generated
│   └── memory/                  YOURS. Nothing else writes here.
│       ├── beliefs.md           working memory, <=200 lines, REWRITTEN
│       ├── projects.md          what each project is
│       └── cursor.json          how far you have read, per workspace
│
└── sessions/<session-id>/       ONE DIRECTORY PER SESSION
    ├── meta.json                id, CURRENT name, instance_id, timestamps
    ├── mission_briefing.md      this session's summary                 <- generated
    ├── workspaces/<pane>.md     one document per workspace             <- generated
    ├── cards/card-*.md          YOUR output for this session
    ├── snapshots/               raw stimuli, pruned                    <- generated
    └── timeline.md              this session's events                  <- generated
```

## The archive

`mission_briefing.md` and every `workspaces/<pane>.md` are **overwritten** each run. That is right
for the live artifacts — the phone wants the current answer — but it means each sentence you write
destroys the one before it.

`archive/<YYYY-MM-DD>.jsonl` keeps them. One append-only line per version, written by Mars when
your prose changes, holding `{at, session, session_id, kind, pane, words, text}`. Deduplicated on
**content**, so rewriting a note identically keeps nothing and a run that only touched mtimes adds
no records. Only signed prose is kept; the deterministic `*.computed.md` rendering is arithmetic
over snapshots that are still on disk.

You may read it, and it is the only way to answer questions that span time: whether a belief held
up, whether the same failure keeps coming back, what you said about this last week. Nothing yet
requires you to — treat it as available evidence rather than another step in the run.

Never write here. One writer, append-only, or the record is not a record.

## Why the directory is an id, not a name

A session can be renamed. The directory is keyed by an immutable **id** and the current name lives
in `meta.json`, so a rename rewrites one field. Keying directories by name meant four renames of one
daemon produced four directories — a real repo reached 118 of them, nearly all phantoms of the same
session. **Resolve a session by reading `meta.json`, never by trusting a directory name.**

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
