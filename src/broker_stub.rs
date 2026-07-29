//! Inert stand-in for `broker.rs` — compiled when the ssh broker capability is
//! absent (`--no-default-features`). Same seam pattern as
//! `retrieval_stub.rs`: the callers never learn the capability is missing at
//! compile time; at runtime the agent simply never detects a broker socket and
//! `mars ssh` / `mars keyd` explain themselves. The portable pieces the app
//! actually shares with the broker live elsewhere (`fleet.rs`, `worklog::ago`).

use crate::agent::AgentConfig;
use anyhow::Result;
use std::path::PathBuf;

const UNAVAILABLE: &str = "the ssh broker isn't in this build — it needs the `ssh` cargo feature";

pub fn set_session_broker(
    _sock: Option<String>,
    _capability: Option<String>,
) -> Result<()> {
    Ok(())
}

pub fn reset_session_broker() {}

/// No broker without the ssh feature — auto-start is a no-op.
pub fn ensure_broker() {}

pub(crate) fn current_session_broker_route() -> Result<(Option<String>, Option<String>)> {
    Ok((None, None))
}

/// No broker socket can exist without the capability.
pub fn detect_broker_sock() -> Option<String> {
    None
}

pub(crate) fn broker_capability_for(_sock: &str) -> Option<String> {
    None
}

/// Unreachable in practice (`detect_broker_sock` never selects the broker
/// provider), but the seam keeps the caller honest if one is constructed.
pub fn chat_via_broker(
    _sock: &str,
    _cfg: &AgentConfig,
    _messages: Vec<serde_json::Value>,
    _task: &str,
    _origin_call_id: u64,
) -> Result<String> {
    anyhow::bail!("{UNAVAILABLE}")
}

pub fn keyd_main() -> Result<()> {
    anyhow::bail!("mars keyd: {UNAVAILABLE}")
}

pub fn ssh_main(_host: String, _extra: Vec<String>) -> Result<()> {
    anyhow::bail!("mars ssh: {UNAVAILABLE}")
}

/// `Err` so `killall`'s ssh-master sweep block skips itself.
pub fn broker_socket_path() -> Result<PathBuf> {
    anyhow::bail!("{UNAVAILABLE}")
}

/// No forwarded sockets to find or sweep.
pub fn find_live_auth_sock(_dir: &std::path::Path) -> Option<String> {
    None
}
