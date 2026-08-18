"""Generate the doc version lock from the current built-ins.

Format, one line per guarded doc:  <rel>  <version>  <sha256[:16] of the text>

The hash is the same FNV-1a the blessing gate uses, so any edit to a doc changes it. Pairing it
with the version marker is what makes "text changed, version did not" detectable at build time —
which is the mistake that stops the manager on every host at once.
"""
import re
import pathlib

def version_of(text: str) -> str:
    """FNV-1a over the trimmed text — byte-for-byte the same as manager.rs::version_of, so the
    lock and the blessing gate can never disagree about what a doc's identity is."""
    h = 0xCBF29CE484222325
    for b in text.strip().encode():
        h ^= b
        h = (h * 0x00000100000001B3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


MD = pathlib.Path("src/manager_docs")
MANAGER = pathlib.Path("src/manager.rs").read_text()

# Read GUARDED_DOCS straight from the source, so the lock can never list a different set.
block = MANAGER.split("pub const GUARDED_DOCS", 1)[1].split("];", 1)[0]
entries = re.findall(r'\("([^"]+)",\s*include_str!\("manager_docs/([^"]+)"\)\)', block)

lines = []
for rel, fname in entries:
    text = (MD / fname).read_text()
    m = re.search(r"mars-doc-version:\s*(\d+)", text)
    ver = m.group(1) if m else "-"
    h = version_of(text)
    lines.append(f"{rel} {ver} {h}")

# The worker's standing orders are not a manager doc — a worker reads none of the manager's — but
# they carry the same hazard: an edit without a version bump leaves every host on its old copy with
# no way to tell. Locked here so the same build-time check covers both.
for fname in ("WORKING-MODEL.md", "PLANNING-MODEL.md"):
    extra = MD / fname
    if extra.exists():
        text = extra.read_text()
        m = re.search(r"mars-doc-version:\s*(\d+)", text)
        lines.append(f"briefs/{fname} {m.group(1) if m else '-'} {version_of(text)}")

out = MD / "versions.lock"
out.write_text(
    "# Generated: the version marker and content hash of every guarded doc.\n"
    "# Regenerate with tools/doc-lock.py after bumping a doc's mars-doc-version.\n"
    "# A doc whose text changed while its version did not is the one mistake that stops the\n"
    "# manager on every host at once; the selfcheck compares against this file to catch it.\n"
    + "\n".join(lines)
    + "\n"
)
print(out.read_text())
