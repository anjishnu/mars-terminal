//! Platform Abstraction Layer (PAL) — the ONE place the operating system leaks in.
//!
//! The rest of the codebase reaches every platform-specific capability through
//! `sys::<capability>`; no module outside `src/sys/` may name `std::os::unix`,
//! `std::os::windows`, `libc`, `windows_sys`, or another OS API (enforced by
//! `tools/check-platform-isolation.sh`). See `WINDOWS_PORT.md` for the design.
//!
//! The adapter is selected at compile time. Each adapter exposes the SAME set of
//! capability modules with the SAME signatures — that shared signature IS the
//! port. We abstract *capabilities* ("a named local channel", "where my files
//! live", "spawn a detached process"), never individual syscalls.
//!
//! Capabilities:
//!   - `paths`   — where the home / state directories are
//!   - `control` — a named, same-machine, bidirectional byte channel (IPC)
//!   - `tty`     — terminal hygiene (sane-mode restore, is-a-tty)
//!   - `daemon`  — spawn a process detached from this terminal
//!   - `proc`    — process/host identity + lifecycle (per-user tag, kill-by-pattern, termination signals)
//!   - `fsperm`  — restrict a directory/file to its owning user
//!   - `shell`   — which shell a new terminal pane runs

#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
