<!-- mars-doc-version: 2 -->
# Layout

```
~/.mars/
├── manager/                     THE REPO — guide, memory, aggregate index
│   ├── AGENTS.md                how to be the manager (start here)
│   ├── docs/                    this guide
│   ├── policy.md                autonomy grants — HUMAN-EDITED ONLY
│   ├── timeline.md              ONLY memos raised — often absent       <- generated
│   ├── inbox/                   stimuli waiting to be processed
│   ├── archive/YYYY-MM-DD.jsonl everything you have ever written       <- generated
│   ├── events.jsonl             what the captain did about it          <- generated
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

## The event log

`events.jsonl` records what the captain did about what you said: `seen`, `dismiss`, `snooze`,
`answer`, `ask`, `jump`. Each line names a `target` (the memo), a `version` (the exact words that
were on screen), the `briefing` version above them, and `shown_secs` — how long it sat there
before anyone moved.

`seen` is the one that makes the rest mean anything. A memo nobody acted on and a memo nobody was
ever shown look identical in an action log, and they are opposite facts: the first says you were
wrong, the second says you were never read.

The `version` is the join. Follow it into `archive/` and you get the exact text — including after
a later run has overwritten the live file, which is the only case where the question is ever
interesting.

This is what makes your judgement checkable rather than merely confident. Dismissed in two
seconds is a verdict on the memo. Answered after six hours is a verdict on its timing, not its
content. Nothing yet asks you to read this; it is here so the question can be asked later.

Never write here either.

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

1. `memory/beliefs.md` — what you already concluded, and what the engineer has ignored
2. `memory/projects.md` — what these projects are for
3. the stimulus in `inbox/` — what changed since last time

There is no aggregate index file. There used to be `index.md` and `index.json`, rewritten on a
timer by every daemon; the phone computes that view on read now, so there is no copy to go stale
and nothing to read here.

Everything else is fetched on demand. Do not read a whole session tree speculatively; it is large
and most of it is unchanged.
