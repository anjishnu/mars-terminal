//! The key-never-leaves-home broker (`mars keyd`) and the remote-side proxy call.
//!
//! `mars keyd` runs on your home machine, holds the LLM key, and answers `Chat`
//! requests that arrive over the platform control channel. When you
//! `mars ssh <host>`, that channel is remote-forwarded, so the agent on the
//! remote box asks the broker instead of ever holding a key. Reuses
//! `session.rs`'s JSON-lines frame style (`write_frame` + `read_line`).

use crate::agent::{self, AgentConfig};
use crate::session::write_frame;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};
use std::time::Duration;

const BROKER_VERSION: &str = "1";
pub const BROKER_CAPABILITY_ENV: &str = "MARS_BROKER_CAPABILITY";
pub const BROKER_HANDOFF_PROTOCOL: &str = "capability-v1";
pub use crate::ssh::{
    remote_prelude_cmd, remote_session_cmd, remote_session_cmd_with_capability, INSTALL_SH,
};

#[derive(Clone)]
struct SessionBrokerRoute {
    sock: String,
    capability: Option<String>,
}

enum SessionBrokerState {
    Environment,
    Session(Option<SessionBrokerRoute>),
}

static SESSION_BROKER: OnceLock<RwLock<SessionBrokerState>> = OnceLock::new();

pub fn set_session_broker(
    sock: Option<String>,
    capability: Option<String>,
) -> Result<()> {
    let route = match sock.filter(|s| !s.is_empty()) {
        Some(sock) => {
            if sock.len() > 512 || sock.bytes().any(|b| matches!(b, 0 | b'\r' | b'\n')) {
                anyhow::bail!("invalid broker socket in session handshake");
            }
            let capability = capability.filter(|s| !s.is_empty());
            if let Some(cap) = &capability {
                validate_capability(cap)?;
            }
            if requires_capability(&sock) && capability.is_none() {
                anyhow::bail!("capability broker socket arrived without a capability");
            }
            Some(SessionBrokerRoute { sock, capability })
        }
        None => {
            if capability.as_ref().is_some_and(|s| !s.is_empty()) {
                anyhow::bail!("broker capability arrived without a socket");
            }
            None
        }
    };
    let state =
        SESSION_BROKER.get_or_init(|| RwLock::new(SessionBrokerState::Environment));
    *state
        .write()
        .map_err(|_| anyhow::anyhow!("session broker state is poisoned"))? =
        SessionBrokerState::Session(route);
    Ok(())
}

pub fn reset_session_broker() {
    if let Some(state) = SESSION_BROKER.get() {
        if let Ok(mut state) = state.write() {
            *state = SessionBrokerState::Environment;
        }
    }
}

pub(crate) fn current_session_broker_route() -> Result<(Option<String>, Option<String>)> {
    if let Some(state) = SESSION_BROKER.get() {
        let state = state
            .read()
            .map_err(|_| anyhow::anyhow!("session broker state is poisoned"))?;
        if let SessionBrokerState::Session(route) = &*state {
            return Ok(match route {
                Some(route) => (Some(route.sock.clone()), route.capability.clone()),
                None => (None, None),
            });
        }
    }
    let sock = std::env::var("MARS_AUTH_SOCK")
        .ok()
        .filter(|value| !value.is_empty());
    let capability = sock
        .as_deref()
        .and_then(broker_capability_for);
    Ok((sock, capability))
}

fn validate_capability(capability: &str) -> Result<()> {
    if capability.is_empty()
        || capability.len() > 128
        || capability.bytes().any(|b| matches!(b, b'\r' | b'\n'))
    {
        anyhow::bail!("invalid broker tunnel capability");
    }
    Ok(())
}

fn session_broker_sock() -> Option<Option<String>> {
    let state = SESSION_BROKER.get()?.read().ok()?;
    match &*state {
        SessionBrokerState::Environment => None,
        SessionBrokerState::Session(route) => {
            Some(route.as_ref().map(|route| route.sock.clone()))
        }
    }
}

pub(crate) fn broker_capability_for(sock: &str) -> Option<String> {
    if let Some(state) = SESSION_BROKER.get() {
        if let Ok(state) = state.read() {
            if let SessionBrokerState::Session(route) = &*state {
                return route
                    .as_ref()
                    .filter(|route| route.sock == sock)
                    .and_then(|route| route.capability.clone());
            }
        }
    }
    std::env::var(BROKER_CAPABILITY_ENV)
        .ok()
        .filter(|cap| validate_capability(cap).is_ok())
}

fn requires_capability(sock: &str) -> bool {
    std::path::Path::new(sock)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("mars-auth-cap-"))
}

/// Remote → home. One request per connection lifetime is enough, but the
/// connection is kept open for reuse across an agent session.
#[derive(Serialize, Deserialize)]
pub enum BrokerRequest {
    Chat {
        version: String,
        /// `None` → the broker uses its own configured model (the robust default,
        /// since the remote may not know which provider the key is for).
        model: Option<String>,
        messages: Vec<serde_json::Value>,
        max_tokens: u32,
        temperature: f64,
        /// Self-reported by the remote so the home fleet reflects live activity
        /// (`mars ls`). Optional — older remotes simply omit them.
        #[serde(default)]
        host: Option<String>,
        #[serde(default)]
        session: Option<String>,
        /// What the CALLER was doing ("auto_name", "summarize", …) and the call id it minted.
        /// Without these a brokered call lands in the home log as an anonymous "remote", so a
        /// background feature failing at the broker is untraceable from the side that asked —
        /// which is how auto-naming failed invisibly for three days. Optional: an older remote
        /// omits them and is logged as before.
        #[serde(default)]
        task: Option<String>,
        #[serde(default)]
        origin_call_id: Option<u64>,
    },
}

/// Home → remote.
#[derive(Serialize, Deserialize)]
pub enum BrokerResponse {
    Chat { text: String },
    Error { message: String },
}

/// `$HOME/.mars/auth.sock`, under a `0700` dir — the home broker's socket, and
/// the thing `ssh -R` forwards to the remote.
pub fn broker_socket_path() -> Result<PathBuf> {
    let home = crate::sys::paths::home_dir()
        .ok_or_else(|| anyhow::anyhow!("cannot locate the home directory"))?;
    let dir = home.join(".mars");
    std::fs::create_dir_all(&dir)?;
    crate::sys::fsperm::restrict_dir(&dir)?;
    Ok(dir.join("auth.sock"))
}

/// True if something is listening at `path`. A dead leftover socket file is
/// unlinked: sshd refuses to bind a `-R` forward over it (server-side
/// `StreamLocalBindUnlink` is off by default and the client-side flag only
/// covers local forwards), so sweeping here lets the next connection bind.
pub fn probe_and_sweep(path: &std::path::Path) -> bool {
    match crate::sys::control::probe(path) {
        crate::sys::control::Probe::Live => true,
        crate::sys::control::Probe::Dead => {
            if path.exists() {
                let _ = std::fs::remove_file(path);
            }
            false
        }
        crate::sys::control::Probe::Indeterminate => false,
    }
}

/// The socket the remote agent should proxy through, if any: an explicit
/// `MARS_AUTH_SOCK`, else any live forwarded socket — a dead socket (the
/// tunnel is gone) must fall through to the provider chain, not pin every
/// call to an unreachable broker.
pub fn detect_broker_sock() -> Option<String> {
    if let Ok(session) = std::env::var("MARS_SESSION") {
        if !session.is_empty() {
            let instance_id = std::env::var("MARS_SESSION_ID")
                .ok()
                .filter(|value| !value.is_empty());
            let Ok((sock, capability, _)) =
                crate::session::query_broker_route(&session, instance_id.as_deref())
            else {
                return None;
            };
            return set_session_broker(sock.clone(), capability)
                .ok()
                .and(sock);
        }
    }
    if let Some(route) = session_broker_sock() {
        return route;
    }
    if let Ok(s) = std::env::var("MARS_AUTH_SOCK") {
        if !s.is_empty() {
            if requires_capability(&s) && broker_capability_for(&s).is_none() {
                return None;
            }
            return Some(s);
        }
    }
    // A locally auto-started broker (`mars keyd`, incl. the one `ensure_broker` spawns)
    // binds ~/.mars/auth.sock — a plain (non-capability) socket. Discover it directly;
    // find_live_auth_sock below only scans /tmp for ssh-forwarded sockets.
    if let Some(home) = crate::sys::paths::home_dir() {
        let p = home.join(".mars").join("auth.sock");
        if matches!(crate::sys::control::probe(&p), crate::sys::control::Probe::Live) {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    #[cfg(unix)]
    {
        find_live_auth_sock(std::path::Path::new("/tmp"))
    }
    #[cfg(windows)]
    {
        None
    }
}

/// Auto-start the key broker (`mars keyd`) in the background when this machine has a
/// direct LLM key but no broker is up yet — so EVERY session daemon (including ones
/// already running that don't have the key in their own env) can reach the LLM via
/// ~/.mars/auth.sock. No-op if a key is already brokered, none is configured, or one is
/// listening. Called on each session launch; idempotent.
pub fn ensure_broker() {
    let cfg = AgentConfig::from_env();
    if cfg.provider == "broker" || !cfg.is_configured() {
        return; // already going through a broker, or no key to serve
    }
    if detect_broker_sock().is_some() {
        return; // one is already listening
    }
    let Ok(exe) = std::env::current_exe() else { return };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("keyd");
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    crate::sys::daemon::detach(&mut cmd); // survive this TTY
    let _ = cmd.spawn();
}

/// The forwarded socket's name carries the HOME machine's uid, which rarely
/// matches this box's (a Mac's 501 vs Linux's 1000) — so scan for any live
/// `mars-auth-*.sock` instead of guessing by uid. Own-uid first (the
/// same-uid case stays deterministic), then lexicographic. Dead leftovers
/// are swept along the way, where permissions allow.
pub fn find_live_auth_sock(dir: &std::path::Path) -> Option<String> {
    #[cfg(windows)]
    {
        let _ = dir;
        return None;
    }
    #[cfg(unix)]
    {
        let own = dir.join(format!("mars-auth-{}.sock", crate::sys::proc::uid_tag()));
        if probe_and_sweep(&own) {
            return Some(own.to_string_lossy().into_owned());
        }
        let mut candidates: Vec<_> = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| {
                        n.starts_with("mars-auth-")
                            && !n.starts_with("mars-auth-cap-")
                            && n.ends_with(".sock")
                    })
            })
            .collect();
        candidates.sort();
        candidates
            .into_iter()
            .find(|p| probe_and_sweep(p))
            .map(|p| p.to_string_lossy().into_owned())
    }
}

/// The home broker daemon. Loads the key once (from env today), binds the
/// socket, and answers `Chat` by running the real LLM call — the only process
/// that ever constructs an `Authorization` header.
pub fn keyd_main() -> Result<()> {
    // The broker resolves its own credentials DIRECTLY. Without this, `AgentConfig::from_env()`
    // inside the request handler discovers this very socket and hands the call back to us —
    // latent before, but a live loop now that a brokered failure falls through to other
    // candidates instead of returning.
    std::env::set_var("MARS_NO_BROKER", "1");
    let cfg = AgentConfig::from_env();
    if !cfg.is_configured() {
        anyhow::bail!(
            "mars keyd: no API key found. Set GROQ_API_KEY / GEMINI_API_KEY / MARS_LLM_KEY \
             on this machine first."
        );
    }
    let path = broker_socket_path()?;
    // Clear a stale socket from a previous run (nothing listening).
    if path.exists() {
        match crate::sys::control::probe(&path) {
            crate::sys::control::Probe::Dead => {
                let _ = std::fs::remove_file(&path);
            }
            crate::sys::control::Probe::Indeterminate => {
                anyhow::bail!(
                    "mars keyd: existing endpoint cannot be authenticated; \
                     stop the old keyd or run `mars killall`"
                );
            }
            crate::sys::control::Probe::Live => {}
        }
    }
    let listener = crate::sys::control::bind(&path)
        .map_err(|e| anyhow::anyhow!("mars keyd: cannot bind {}: {e}", path.display()))?;
    crate::sys::fsperm::restrict_file(&path)?;
    println!(
        "mars keyd: broker listening (provider: {}) at {}",
        cfg.provider,
        path.display()
    );
    println!("  now run:  mars ssh <host>   — the agent works there, no key on the box.");
    for conn in listener.incoming() {
        match conn {
            Ok(stream) => {
                std::thread::spawn(move || {
                    let _ = handle_conn(stream);
                });
            }
            Err(_) => continue,
        }
    }
    Ok(())
}

fn handle_conn(stream: crate::sys::control::Stream) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut w = stream;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break; // client hung up
        }
        let req: BrokerRequest = match serde_json::from_str(line.trim()) {
            Ok(r) => r,
            Err(e) => {
                write_frame(
                    &mut w,
                    &BrokerResponse::Error {
                        message: format!("malformed broker request: {e}"),
                    },
                )?;
                continue;
            }
        };
        let BrokerRequest::Chat {
            version,
            model,
            messages,
            max_tokens,
            temperature,
            host,
            session,
            task,
            origin_call_id,
        } = req;
        // Status push: a brokered call is proof the remote's agent is alive —
        // refresh the fleet so `mars ls` shows it as current, not stale.
        if let Some(h) = &host {
            crate::fleet::fleet_status(h, session.clone(), "agent active");
        }
        let resp = if version != BROKER_VERSION {
            BrokerResponse::Error {
                message: format!(
                    "broker version mismatch (home {BROKER_VERSION}, remote {version})"
                ),
            }
        } else {
            // Fresh config each request → the key is read here, at home, and a
            // provider/key change is picked up without restarting the daemon.
            let mut c = AgentConfig::from_env();
            if let Some(m) = model {
                c.model = m;
            }
            c.max_tokens = max_tokens;
            c.temperature = temperature;
            // Log under the CALLER's task, not a blanket "remote", and record the link so a
            // brokered call can be joined to the record on the side that asked for it.
            let tag = task.clone().unwrap_or_else(|| "remote".to_string());
            crate::llm_log::event(
                "broker_call",
                serde_json::json!({ "task": tag, "origin_call_id": origin_call_id,
                                    "host": host.clone(), "session": session.clone() }),
            );
            match agent::chat(&c, messages, &tag) {
                Ok(text) => BrokerResponse::Chat { text },
                Err(e) => BrokerResponse::Error {
                    message: e.to_string(),
                },
            }
        };
        write_frame(&mut w, &resp)?;
    }
    Ok(())
}

/// Remote side: send a chat request home over the forwarded socket and block for
/// the completion. No `Authorization` header, no key — ever — on this box.
pub fn chat_via_broker(
    sock: &str,
    cfg: &AgentConfig,
    messages: Vec<serde_json::Value>,
    task: &str,
    origin_call_id: u64,
) -> Result<String> {
    let stream = crate::sys::control::connect(sock).map_err(|e| {
        anyhow::anyhow!("home broker unreachable ({e}); is `mars keyd` running + the tunnel up?")
    })?;
    // A little longer than chat()'s own 30s, so the home call's timeout wins.
    stream.set_read_timeout(Some(Duration::from_secs(40)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut w = stream;
    if let Some(capability) = broker_capability_for(sock) {
        use std::io::Write as _;
        w.write_all(capability.as_bytes())?;
        w.write_all(b"\n")?;
        w.flush()?;
    }
    let model = if cfg.model.is_empty() {
        None
    } else {
        Some(cfg.model.clone())
    };
    write_frame(
        &mut w,
        &BrokerRequest::Chat {
            version: BROKER_VERSION.to_string(),
            model,
            messages,
            max_tokens: cfg.max_tokens,
            temperature: cfg.temperature,
            host: crate::sys::proc::hostname(),
            session: std::env::var("MARS_SESSION").ok(),
            task: Some(task.to_string()),
            origin_call_id: Some(origin_call_id),
        },
    )?;
    let mut line = String::new();
    reader.read_line(&mut line)?;
    match serde_json::from_str::<BrokerResponse>(line.trim())
        .map_err(|e| anyhow::anyhow!("broker sent a malformed reply: {e}"))?
    {
        BrokerResponse::Chat { text } => Ok(text),
        BrokerResponse::Error { message } => anyhow::bail!("{message}"),
    }
}

/// Make sure the home broker is running, auto-starting it (detached) if not —
/// so `mars ssh` is one command, not two. The spawned `mars keyd` inherits THIS
/// shell's env, which is exactly where the API key lives. Best-effort: ssh
/// proceeds either way (a keyless box just won't have an agent).
pub(crate) fn ensure_keyd(home_sock: &std::path::Path) -> bool {
    match crate::sys::control::probe(home_sock) {
        crate::sys::control::Probe::Live => return true,
        crate::sys::control::Probe::Indeterminate => {
            eprintln!(
                "mars ssh: the home broker endpoint exists but cannot be authenticated; \
                 stop the old keyd or run `mars killall`."
            );
            return false;
        }
        crate::sys::control::Probe::Dead => {}
    }
    // Starting the broker needs a key in this environment.
    if !AgentConfig::from_env().is_configured() {
        eprintln!(
            "mars ssh: no API key in this shell, so the remote agent won't have one.\n  \
             set GROQ_API_KEY / GEMINI_API_KEY here (then it auto-starts), or run `mars keyd` \
             where your key lives."
        );
        return false;
    }
    let _ = std::fs::remove_file(home_sock); // clear a stale socket
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(_) => return false,
    };
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("keyd");
    cmd.env_remove("MARS_AUTH_SOCK"); // the broker must never run in proxy mode
    cmd.env_remove(BROKER_CAPABILITY_ENV);
    // Log to ~/.mars/keyd.log; never spill the daemon's output onto this TTY.
    let log = home_sock.with_file_name("keyd.log");
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
    {
        Ok(f) => {
            let f2 = f.try_clone().ok();
            cmd.stdout(f);
            match f2 {
                Some(f2) => {
                    cmd.stderr(f2);
                }
                None => {
                    cmd.stderr(std::process::Stdio::null());
                }
            }
        }
        Err(_) => {
            cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }
    }
    cmd.stdin(std::process::Stdio::null());
    crate::sys::daemon::detach(&mut cmd);
    if cmd.spawn().is_err() {
        return false;
    }
    for _ in 0..40 {
        std::thread::sleep(Duration::from_millis(50));
        if crate::sys::control::probe(home_sock) == crate::sys::control::Probe::Live {
            eprintln!("mars ssh: started the home broker (mars keyd) automatically.");
            return true;
        }
    }
    eprintln!("mars ssh: could not start the home broker (see ~/.mars/keyd.log).");
    false
}

pub fn ssh_main(host: String, extra: Vec<String>) -> Result<()> {
    crate::ssh::ssh_main(host, extra)
}
