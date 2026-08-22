#!/usr/bin/env bash
# Fire the design loop end to end against a real brief and capture what each stage printed.
#
# THIS IS A DELIVERABLE, NOT A TEST. `cargo test` answers "did the units behave"; this answers
# "does the loop still close", which is the only question the design is actually judged on — and
# the last run found three defects that reading had missed, including one the unit assertions
# passed straight over.
#
#   scripts/tracer.sh <brief-id> [outdir]
#
# Every stage writes a file. A stage that cannot run says so in its file rather than being skipped
# silently, because a missing file and a failing stage look identical afterwards.
set -uo pipefail
B="${1:?usage: tracer.sh <brief-id> [outdir]}"
OUT="${2:-$PWD/.tracer}"
MARS="${MARS:-./target/release/mars}"
mkdir -p "$OUT"
export RUST_BACKTRACE=0

step() { local n="$1"; shift; echo "── $n"; "$@" >"$OUT/$n.txt" 2>&1 || true; sed 's/^/   /' "$OUT/$n.txt" | head -6; }

step 03-decisions "$MARS" brief decisions "$B"
step 04-review    "$MARS" brief review    "$B"
step 10-audit     "$MARS" brief audit     "$B"
step 12-ls        "$MARS" brief ls

# The claim the design actually makes: refining one decision must not rewrite the others.
D=~/.mars/briefs/$B/brief.md
cp "$D" "$OUT/before.md"
FIRST=$("$MARS" brief decisions "$B" 2>/dev/null | grep -o '\[hld-[0-9]\]' | head -1 | tr -d '[]')
if [ -n "$FIRST" ]; then
  for K in A B C; do
    if "$MARS" brief override "$B" "$FIRST" "$K" >"$OUT/05-override.txt" 2>&1; then
      grep -q "stale now" "$OUT/05-override.txt" && break
    fi
  done
  cp "$D" "$OUT/after.md"
  python3 - "$OUT" <<'PY' > "$OUT/05-acceptance.txt"
import re,sys,pathlib
t=pathlib.Path(sys.argv[1])
def blocks(p):
    s=(t/p).read_text()
    return {b.split('\n')[0]:b for b in re.split(r'(?m)^(?=#{2,3} )',s) if b.startswith('### ')}
b,a=blocks('before.md'),blocks('after.md')
bad=[k for k in b if b[k]!=a.get(k)]
print("ACCEPTANCE · one override must not rewrite any other decision")
print(f"  byte-identical: {len(b)-len(bad)}/{len(b)}")
print(f"  rewritten: {bad or 'none'}")
raise SystemExit(1 if bad else 0)
PY
  sed 's/^/   /' "$OUT/05-acceptance.txt"
fi
echo
echo "captured → $OUT"
