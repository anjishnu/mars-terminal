#!/bin/sh
# Mars installer — consistent rust + cargo + mars install on any Linux distro
# (and macOS). Run directly (`sh install.sh`); `mars ssh` also drops this file at
# ~/.mars/install.sh on hosts it connects to.
#
# It automates the official steps (rust-lang.org/tools/install + crates.io):
#   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
#   cargo install mars-terminal --locked
#
# Design points:
#   - This shell installer is Unix-only. Native Windows is supported, but uses
#     rustup + the MSVC build tools from PowerShell (see README.md).
#   - A distro-packaged cargo (e.g. Ubuntu's 1.75) is too old (needs >= 1.85 /
#     edition2024): detected; rustup is installed instead. Never `apt install cargo`.
#   - Minimal images lack a C linker; cargo fails mid-build with a confusing
#     error. Preflighted here with the exact per-distro fix line.
#   - `--locked` builds with the crate's shipped lockfile — the same dependency
#     versions on every machine (the "consistent build" guarantee).
set -eu

MSRV_MINOR=85                 # Mars needs Rust >= 1.85
CRATE=mars-terminal           # crates.io package name (the binary is `mars`)

say() { printf '\033[1m[mars-install]\033[0m %s\n' "$1"; }
die() { printf '\033[1;31m[mars-install]\033[0m %s\n' "$1" >&2; exit 1; }

# ── 0. Platform gate ─────────────────────────────────────────────────────────
OS=$(uname -s 2>/dev/null || echo unknown)
case "$OS" in
  MINGW*|MSYS*|CYGWIN*|Windows*)
    die "install.sh configures Unix hosts and is not the native Windows installer.
  Mars supports Windows; from PowerShell, install rustup's MSVC toolchain and
  Visual Studio Build Tools, then run:
    cargo install mars-terminal --locked
  See README.md for the Windows prerequisites."
    ;;
  Linux|Darwin) : ;;
  *) say "unrecognized OS '$OS' — proceeding as unix; expect the unexpected." ;;
esac

# ── 1. Prerequisites: a fetcher and a C linker ───────────────────────────────
# Package-manager detection, used only to print EXACT fix commands (never sudo here).
pkg_hint() { # $1 = what to install per manager: "<apt> | <dnf> | <pacman> | <zypper> | <apk>"
  if   command -v apt-get >/dev/null 2>&1; then echo "sudo apt-get update && sudo apt-get install -y $1"
  elif command -v dnf     >/dev/null 2>&1; then echo "sudo dnf install -y $2"
  elif command -v yum     >/dev/null 2>&1; then echo "sudo yum install -y $2"
  elif command -v pacman  >/dev/null 2>&1; then echo "sudo pacman -S --noconfirm $3"
  elif command -v zypper  >/dev/null 2>&1; then echo "sudo zypper install -y $2"
  elif command -v apk     >/dev/null 2>&1; then echo "sudo apk add $4"
  else echo "install '$1' with your distro's package manager"
  fi
}

FETCH=""
if command -v curl >/dev/null 2>&1; then FETCH="curl --proto '=https' --tlsv1.2 -sSf"
elif command -v wget >/dev/null 2>&1; then FETCH="wget -qO-"
else
  die "need curl or wget to fetch rustup. Fix:
    $(pkg_hint curl curl curl curl)"
fi

if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1 \
   && ! command -v clang >/dev/null 2>&1; then
  # Without a linker, cargo fails MID-BUILD with a cryptic error — stop now, clearly.
  die "no C linker (cc/gcc/clang) — Rust needs one to link binaries. Fix:
    $(pkg_hint build-essential gcc gcc build-base)
  then re-run this script."
fi

# ── 2. Rust toolchain (rustup; never the distro cargo) ──────────────────────
cargo_ok() {
  command -v cargo >/dev/null 2>&1 || return 1
  v=$(cargo --version 2>/dev/null | awk '{print $2}') || return 1
  maj=${v%%.*}; rest=${v#*.}; min=${rest%%.*}
  [ "${maj:-0}" -gt 1 ] 2>/dev/null && return 0
  [ "${maj:-0}" -eq 1 ] 2>/dev/null && [ "${min:-0}" -ge "$MSRV_MINOR" ] 2>/dev/null
}

# rustup's cargo (if present) should win over any distro cargo.
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
export PATH="$HOME/.cargo/bin:$PATH"

if cargo_ok; then
  say "toolchain OK: $(cargo --version)"
else
  if command -v cargo >/dev/null 2>&1; then
    say "found $(cargo --version) — too old (Mars needs Rust >= 1.$MSRV_MINOR). Installing rustup…"
    say "(the distro cargo can stay; rustup's toolchain will take precedence on PATH)"
  else
    say "no Rust toolchain — installing rustup (official installer)…"
  fi
  eval "$FETCH https://sh.rustup.rs" | sh -s -- -y --profile minimal \
    || die "rustup installation failed — see output above (network? proxy?)."
  . "$HOME/.cargo/env"
  cargo_ok || die "Rust is still older than 1.$MSRV_MINOR — try: rustup update stable"
fi

# ── 3. Mars ──────────────────────────────────────────────────────────────────
say "installing $CRATE from crates.io (compiles once; a minute or two)…"
cargo install "$CRATE" --locked --force \
  || die "cargo install failed — the full compiler output is above."

BIN="$(command -v mars || true)"
[ -n "$BIN" ] || die "installed, but 'mars' isn't on PATH — add:  export PATH=\"\$HOME/.cargo/bin:\$PATH\""
say "done → $BIN"
mars version 2>/dev/null | tail -1 || true
case ":$PATH:" in
  *":$HOME/.cargo/bin:"*) : ;;
  *) say "add this to your shell rc so future shells find mars:
    export PATH=\"\$HOME/.cargo/bin:\$PATH\"" ;;
esac

# Agent setup: the editor works out of the box; the AI features need a free key.
echo
if mars setup 2>/dev/null | grep -q "✓ a key is configured"; then
  say "agent features: a key is already configured — you're all set."
else
  say "agent features (Ask, English→shell, briefings) need a free API key."
  echo "    Get one in ~30s and set it up:  mars setup"
fi
