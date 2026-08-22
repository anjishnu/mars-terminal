# What "done" includes

Applies to both repos. Two additions to the definition of done, both learned the expensive way.

## 1. A UI-impacting change ships with screenshots of it working

Not a description of it working. A screenshot, of the real surface, against a live bridge.

**Why.** Reading found none of these; looking found all of them, in one session:

- The mirror ran past the bottom of the screen — the comment claimed the frame cropped and no
  container clipped it.
- A superseded brief offered `<the question> → …` for approval, because the template's own example
  option lines parse as real options.
- Fixing that left the legacy `forks` path leaking the same placeholder.
- The staleness gate replaced the whole press row, so a brief you could not approve was one you
  could not open.
- The brief renderer ate `*For:*` and `*Assumes:*` — literal asterisks through every option line.

Five defects, all invisible to `tsc`, `cargo build` and the unit assertions, all obvious in a
picture. The cost of a screenshot is two minutes.

**How.** Point a browser at the real surface (`scripts/` has the bridge invocation), press the
thing, and put the image in the PR. `MARS_BRIDGE_PORT=<free port> mars serve <session>` stands up a
second bridge beside a live one, so this never disturbs the session you are using.

## 2. A change to the design loop ships with a tracer run

`scripts/tracer.sh <brief-id>` fires the loop end to end and captures what each stage printed,
including the assertion the design actually makes — that refining one decision does not rewrite the
others.

**Why.** `cargo test` answers "did the units behave". The tracer answers "does the loop still
close", which is the only question this design is judged on. Its first run found three defects that
reading had missed, including one the unit assertions passed straight over.

**Where it goes.** Both go in the PR body, alongside an artifact naming what still needed a human
hand — because a stage that only works when somebody stands in for it is a stage that is not built,
whatever its tests say.
