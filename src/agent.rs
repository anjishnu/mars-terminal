/// LLM agent integration. Most providers speak the OpenAI-compatible chat API;
/// Claude uses Anthropic's own Messages API (separate branch in `chat()`).
/// Env precedence (paid-first): MARS_LLM_* (any endpoint, e.g. local Ollama;
/// legacy ARES_LLM_* still honored) → ANTHROPIC_API_KEY → OPENAI_API_KEY →
/// GROQ_API_KEY → GEMINI_API_KEY / GOOGLE_API_KEY.

use std::sync::mpsc;

/// First-run / no-key setup instructions — printed by `mars setup`, by headless
/// commands that need a key, by `install.sh`, and surfaced as a dismissible notice
/// when the editor launches unconfigured. Plain text, wrapped for an 80-col terminal.
pub const SETUP_HELP: &str = "\
Mars uses an LLM for its agent features — Ask, English→shell, watch summaries, and
away briefings. The editor and terminal multiplexer work fully without one.

Get a free API key in about 30 seconds:
  • Groq   (recommended — fast, generous free tier):  https://console.groq.com/keys
  • Gemini (Google, free tier):                       https://aistudio.google.com/apikey

Then give it to Mars either way:
  • Shell rc (~/.zshrc or ~/.bashrc), then restart your shell:
      export GROQ_API_KEY=\"gsk_your_key_here\"
  • Or ~/.mars/config.json (Mars reads this on startup):
      { \"env\": { \"GROQ_API_KEY\": \"gsk_your_key_here\" } }

Other providers also work: ANTHROPIC_API_KEY, OPENAI_API_KEY, or MARS_LLM_KEY
(+ MARS_LLM_URL for a custom or local endpoint such as Ollama).

Re-print these instructions anytime with:  mars setup";

#[cfg(feature = "ssh")]
pub const PROVIDER_CREDENTIAL_ENV_VARS: &[&str] = &[
    "MARS_LLM_KEY",
    "ARES_LLM_KEY",
    "AWS_BEARER_TOKEN_BEDROCK",
    "AZURE_OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GROQ_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    ];

/// What the model asked the editor to do (always user-confirmed before firing).
#[derive(Clone, Debug, PartialEq)]
pub enum AgentDirective {
    /// `RUN: <ActionName>` — an editor action from the registry.
    Run(String),
    /// `TYPE: <command>` — type a shell command into the terminal pane.
    Type(String),
    /// `OPEN: path:line` — open a file at a line (cited from a stack trace).
    Open(String),
    /// `NEED: <what>` — a read-side request for more context (W4/W5). Never gated;
    /// Mars re-asks once with the extra source. Not shown to the user.
    Need(NeedKind),
}

/// What extra context the model asked for.
#[derive(Clone, Debug, PartialEq)]
pub enum NeedKind {
    /// Full terminal scrollback of the focused pane (W5).
    Scrollback,
    /// Another tab's panes, by tab name (W4).
    Tab(String),
}

pub enum AgentEvent {
    Answer {
        text: String,
        directive: Option<AgentDirective>,
    },
    /// A streamed ask reply is starting — reset any partial from a prior turn
    /// (the escalation retry starts a fresh stream over the same question).
    AnswerStart,
    /// One streamed chunk of the in-progress ask reply (reasoning-stripped;
    /// the final `Answer` still carries the complete, directive-parsed text).
    AnswerDelta { text: String },
    /// Background tab-naming reply (tab id, proposed name).
    AutoName { tab_id: usize, name: String },
    /// Background session-naming reply (proposed name).
    SessionName { name: String },
    /// W6: one-line verdict on a watched terminal (term id, verdict).
    WatchSummary { term_id: usize, verdict: String },
    /// On-demand summary (workspaces-panel `s`): one line for a surface, term id +
    /// text. Sets the surface's verdict/summary WITHOUT the notice side-effects a
    /// watch fire has (the user asked for it; it's already on screen).
    SurfaceSummary { term_id: usize, text: String },
    /// Background mission inference over the work journal (one line: what the
    /// user is working on). Persisted for `mars ls`.
    Mission { text: String },
    /// A background agent thread finished — clears the `bg_busy` gate even if the
    /// call failed (so one failed request can't wedge all background work).
    BgDone,
    /// W3 shell translate: English → one shell command (fills the SH bar).
    ShellTranslation { command: String, call_id: u64 },
    /// Shift report: one streamed chunk of the plain-English situation briefing.
    ShiftDelta { text: String },
    /// Shift report: the briefing finished streaming.
    ShiftDone,
    /// Goals captured at detach — what the user was working toward.
    Goals { goals: Vec<String> },
    /// Rover MAP: one-line summary of one workspace's RAW tail, keyed by term id,
    /// with provider provenance. Cached on the app and pushed in the board frame.
    RoverSummary { term_id: usize, text: String, provider: String },
    /// Rover REDUCE: the composed mission briefing over the cached per-workspace
    /// summaries, pushed live to a subscribed phone via the Briefing frame.
    RoverBrief { text: String },
    Error(String),
}

/// Human-readable provenance for a model result — the phone's honesty invariant
/// ("via groq · llama-3.3-70b" / "via laptop"). The key never leaves home, so a
/// broker call is honestly attributed to the trusted machine, not the phone.
pub fn provenance(cfg: &AgentConfig) -> String {
    if cfg.provider == "broker" {
        "laptop".to_string()
    } else if cfg.model.is_empty() {
        cfg.provider.to_string()
    } else {
        format!("{} · {}", cfg.provider, cfg.model)
    }
}

/// Kebab-case, ≤16 chars, alnum+dash — shared by tab/session auto-naming.
fn kebab(text: &str) -> String {
    let s: String = text
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    s.chars().take(16).collect()
}

/// Match a single directive line, tolerating markdown noise (`- `, backticks,
/// bold) the model sometimes wraps it in.
fn match_directive(line: &str) -> Option<AgentDirective> {
    let l = line
        .trim()
        .trim_start_matches(['-', '*', '>', ' '])
        .trim_matches('`')
        .trim_matches('*')
        .trim();
    // RUN takes only the action token (models sometimes append prose); TYPE and
    // OPEN keep the full rest (commands and paths contain spaces).
    if let Some(rest) = l.strip_prefix("RUN:") {
        if let Some(name) = rest.trim().trim_matches('`').split_whitespace().next() {
            let name = name.trim_end_matches(['.', ',', ':']);
            if !name.is_empty() {
                return Some(AgentDirective::Run(name.to_string()));
            }
        }
    }
    // NEED: read-side context request (W4/W5).
    if let Some(rest) = l.strip_prefix("NEED:") {
        let arg = rest.trim().trim_matches('`').trim();
        let low = arg.to_lowercase();
        if low.starts_with("scrollback") || low.starts_with("history") {
            return Some(AgentDirective::Need(NeedKind::Scrollback));
        }
        if let Some(tab) = low.strip_prefix("tab") {
            let name = tab.trim().to_string();
            if !name.is_empty() {
                return Some(AgentDirective::Need(NeedKind::Tab(name)));
            }
        }
    }
    for (tag, make) in [
        ("TYPE:", AgentDirective::Type as fn(String) -> AgentDirective),
        ("OPEN:", AgentDirective::Open as fn(String) -> AgentDirective),
    ] {
        if let Some(rest) = l.strip_prefix(tag) {
            let arg = rest.trim().trim_matches('`').trim().to_string();
            if !arg.is_empty() {
                return Some(make(arg));
            }
        }
    }
    None
}

/// Split a model reply into display text and a trailing directive. Lenient: the
/// directive may sit on any of the last few non-empty lines (models sometimes
/// add a sign-off after it), and may be wrapped in backticks/list markers.
pub fn parse_directive(text: &str) -> (String, Option<AgentDirective>) {
    let lines: Vec<&str> = text.lines().collect();
    // Scan the last few non-empty lines from the bottom.
    let mut hit: Option<usize> = None;
    for (i, line) in lines.iter().enumerate().rev().take(4) {
        if line.trim().is_empty() {
            continue;
        }
        if match_directive(line).is_some() {
            hit = Some(i);
            break;
        }
    }
    match hit {
        Some(i) => {
            let directive = match_directive(lines[i]);
            let display = lines[..i].join("\n").trim_end().to_string();
            (display, directive)
        }
        None => (text.to_string(), None),
    }
}

#[derive(Clone)]
pub struct AgentConfig {
    pub url: String,
    pub key: String,
    pub model: String,
    pub provider: &'static str,
    pub max_tokens: u32,
    pub temperature: f64,
    /// Set on a remote box behind an `ssh -R` auth socket: the agent proxies the
    /// LLM call home instead of holding a key. `None` = call the provider directly.
    pub broker_sock: Option<String>,
}

/// Read `MARS_<name>`, falling back to the pre-rename `ARES_<name>`.
fn env_var(name: &str) -> Result<String, std::env::VarError> {
    std::env::var(format!("MARS_{name}")).or_else(|_| std::env::var(format!("ARES_{name}")))
}

/// A 429, typed, so the cascade can tell "this family is throttled" (rotate to
/// another one) from every other failure (don't).
#[derive(Debug)]
pub struct RateLimited(pub String);

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for RateLimited {}

/// The provider retired the model, or this key can't access it (HTTP 404, or a
/// "does not exist / no access / decommissioned" body). Like [`RateLimited`] it is
/// RECOVERABLE by trying another model — the next candidate in the tier, then
/// another provider — rather than a hard failure to surface. Distinguishing this
/// from a plain 4xx is what lets model churn self-heal instead of freezing a task.
#[derive(Debug)]
pub struct ModelUnavailable(pub String);

impl std::fmt::Display for ModelUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for ModelUnavailable {}

/// Does this HTTP status / provider message mean "that model is gone / off-limits"
/// (as opposed to a transient or auth error)? Providers phrase it many ways.
pub fn is_retired_model(code: u16, api_msg: Option<&str>) -> bool {
    code == 404
        || api_msg
            .map(|m| {
                let l = m.to_lowercase();
                l.contains("does not exist")
                    || l.contains("do not have access")
                    || l.contains("decommission")
                    || l.contains("has been deprecated")
                    || l.contains("model_not_found")
                    || l.contains("no longer supported")
            })
            .unwrap_or(false)
}

/// Every provider with a key in the ambient env, paid-first: explicit
/// MARS_LLM_KEY wins, then a set Claude/OpenAI key (you meant it), then the
/// free Groq/Gemini tiers. Position 0 is what `from_env` picks; the rest are
/// rotation targets when it rate-limits. Cheap defaults per provider
/// (right-size, don't reach for the biggest); override any with MARS_LLM_MODEL.
/// One resolved provider: (key, provider tag, base url, default model). URL and
/// model are owned Strings because enterprise providers compute them from region
/// / endpoint / deployment env vars rather than a fixed literal.
type Provider = (String, &'static str, String, String);

fn provider_chain() -> Vec<Provider> {
    let mut chain: Vec<Provider> = Vec::new();
    let p = |k: String, tag: &'static str, url: &str, model: &str| {
        (k, tag, url.to_string(), model.to_string())
    };
    if let Ok(k) = env_var("LLM_KEY") {
        chain.push(p(k, "custom", "https://api.groq.com/openai/v1", "llama-3.1-8b-instant"));
    }
    // Enterprise gateways win over consumer keys — a box deliberately configured
    // for Bedrock/Azure means to use it. Bearer/api-key auth only (no SigV4).
    if let Ok(k) = std::env::var("AWS_BEARER_TOKEN_BEDROCK") {
        // Bedrock Converse API. cfg.url holds the region base; chat_bedrock fills
        // the /model/{id}/converse path. Default: a cross-region Haiku profile.
        let region = env_var("BEDROCK_REGION")
            .or_else(|_| std::env::var("AWS_REGION"))
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".to_string());
        chain.push(p(
            k,
            "bedrock",
            &format!("https://bedrock-runtime.{region}.amazonaws.com"),
            "us.anthropic.claude-3-5-haiku-20241022-v1:0",
        ));
    }
    if let (Ok(k), Ok(endpoint)) =
        (std::env::var("AZURE_OPENAI_API_KEY"), std::env::var("AZURE_OPENAI_ENDPOINT"))
    {
        // Azure OpenAI / Foundry: OpenAI-compatible body, but api-key header and a
        // deployment+api-version URL. The "model" is the deployment name.
        let deployment = env_var("AZURE_DEPLOYMENT")
            .or_else(|_| env_var("LLM_MODEL"))
            .unwrap_or_else(|_| "gpt-4o-mini".to_string());
        let version = env_var("AZURE_API_VERSION").unwrap_or_else(|_| "2024-10-21".to_string());
        let base = endpoint.trim_end_matches('/');
        chain.push((
            k,
            "azure",
            format!("{base}/openai/deployments/{deployment}/chat/completions?api-version={version}"),
            deployment,
        ));
    }
    if let Ok(k) = std::env::var("ANTHROPIC_API_KEY") {
        // Claude — Anthropic's own Messages API (not OpenAI-compatible; handled
        // in chat()). Haiku is the cheap/fast default.
        chain.push(p(k, "anthropic", "https://api.anthropic.com", "claude-haiku-4-5"));
    }
    if let Ok(k) = std::env::var("OPENAI_API_KEY") {
        chain.push(p(k, "openai", "https://api.openai.com/v1", "gpt-4o-mini"));
    }
    if let Ok(k) = std::env::var("GROQ_API_KEY") {
        // Llama-3.3-70B-versatile: a live, non-reasoning default on Groq's fast
        // free tier. (This is the fallback for any task the ring doesn't map — so
        // it must be a currently-served model; Groq retires open models often.)
        chain.push(p(k, "groq", "https://api.groq.com/openai/v1", "llama-3.3-70b-versatile"));
    }
    if let Ok(k) = std::env::var("GEMINI_API_KEY").or_else(|_| std::env::var("GOOGLE_API_KEY")) {
        // Flash-Lite: cheapest + highest free-tier limits. (Pinned dated
        // versions age out of the free tier, so track the lite line.)
        chain.push(p(
            k,
            "gemini",
            "https://generativelanguage.googleapis.com/v1beta/openai",
            "gemini-3.1-flash-lite",
        ));
    }
    chain
}

/// Rotation-for-limits targets: the other keyed providers, paid-first. Empty
/// when the user pinned a model/URL or keyed a custom endpoint (an explicit
/// choice is never rotated away from) — and, trivially, with one key.
pub fn rotation_candidates(current_provider: &str) -> Vec<AgentConfig> {
    if env_var("LLM_MODEL").is_ok()
        || env_var("LLM_URL").is_ok()
        || current_provider == "custom"
    {
        return Vec::new();
    }
    provider_chain()
        .into_iter()
        .filter(|(_, p, _, _)| *p != current_provider)
        .map(|(key, provider, url, model)| AgentConfig {
            url: url.to_string(),
            key,
            model: model.to_string(),
            provider,
            max_tokens: 512,
            temperature: 0.3,
            broker_sock: None,
        })
        .collect()
}

impl AgentConfig {
    pub fn from_env() -> Self {
        // Highest precedence: a forwarded auth socket (we're on a remote box).
        // Proxy the call home — the key never lands here. An explicit MARS_LLM_KEY
        // still wins over this, so a box you deliberately keyed keeps working.
        if std::env::var("MARS_LLM_KEY").is_err()
            && std::env::var("ARES_LLM_KEY").is_err()
        {
            if let Some(sock) = crate::broker::detect_broker_sock() {
                return AgentConfig {
                    url: String::new(),
                    key: String::new(),
                    // None-equivalent: empty → the broker picks its own model.
                    model: env_var("LLM_MODEL").unwrap_or_default(),
                    provider: "broker",
                    max_tokens: 512,
                    temperature: 0.3,
                    broker_sock: Some(sock),
                };
            }
        }
        let (key, provider, default_url, default_model) =
            provider_chain().into_iter().next().unwrap_or((
                String::new(),
                "none",
                "https://api.groq.com/openai/v1".to_string(),
                "llama-3.1-8b-instant".to_string(),
            ));

        // Explicit URL/model overrides apply to any provider.
        let url = env_var("LLM_URL").unwrap_or_else(|_| default_url.to_string());
        let model = env_var("LLM_MODEL").unwrap_or_else(|_| default_model.to_string());
        AgentConfig { url, key, model, provider, max_tokens: 512, temperature: 0.3, broker_sock: None }
    }

    pub fn is_configured(&self) -> bool {
        if self.provider == "broker" {
            // Honest on the remote: "configured" iff the tunnel is actually up.
            return self
                .broker_sock
                .as_deref()
                .map(|s| {
                    crate::sys::control::probe(std::path::Path::new(s))
                        == crate::sys::control::Probe::Live
                })
                .unwrap_or(false);
        }
        !self.key.is_empty()
    }
}

fn system_prompt(registry: &str, screen: &str) -> String {
    // Screen content is user-derived — substitute it last (see prompts.rs).
    crate::prompts::ASK_SYSTEM.replace("{registry}", registry).replace("{screen}", screen)
}

/// Build the ask messages. The fixed system-message order is the assembly
/// contract (selfcheck-pinned): base prompt, then docs-context on a retrieval
/// hit, then the persona ALWAYS LAST — positionally under every rule it is
/// forbidden to override. Persona and docs never travel through `.replace()`.
/// Then up to the last 12 turns and the new question. Pure, so tests can
/// assert the exact shape.
pub fn build_ask_messages(
    registry: &str,
    screen: &str,
    history: &[(String, String)],
    question: &str,
) -> Vec<serde_json::Value> {
    let mut messages = vec![serde_json::json!({
        "role": "system", "content": system_prompt(registry, screen)
    })];
    if let Some(ctx) = crate::retrieval::docs_context_for(question) {
        messages.push(serde_json::json!({ "role": "system", "content": ctx }));
    }
    if let Some(p) = crate::persona::system_message() {
        messages.push(p);
    }
    let start = history.len().saturating_sub(12);
    for (role, content) in &history[start..] {
        messages.push(serde_json::json!({ "role": role, "content": content }));
    }
    messages.push(serde_json::json!({ "role": "user", "content": question }));
    messages
}

/// Spawn a background thread, POST to OpenAI-compatible chat endpoint, send result back.
pub fn ask(
    cfg: AgentConfig,
    question: String,
    registry: String,
    screen: String,
    history: Vec<(String, String)>,
    tx: mpsc::Sender<AgentEvent>,
) {
    std::thread::spawn(move || {
        // Memory (Axis B) and voice both live inside the builder — one place
        // owns the system-message order.
        let mode = crate::retrieval::MemoryMode::from_env();
        let messages = build_ask_messages(&registry, &screen, &history, &question);
        // Stream: tokens render as they arrive; the final Answer still carries
        // the complete, directive-parsed text.
        let _ = tx.send(AgentEvent::AnswerStart);
        let txd = tx.clone();
        let mut on_delta = move |d: &str| {
            let _ = txd.send(AgentEvent::AnswerDelta { text: d.to_string() });
        };
        match chat_with_id_streaming(&cfg, messages.clone(), "ask", mode.as_str(), &mut on_delta) {
            Ok((text, _call_id)) => {
                let (display, directive) = parse_directive(&text);
                // Escalation-for-quality: a RUN: naming no real action is an
                // unambiguous cheap-model failure — retry ONCE, one tier up.
                // The retry is logged as `ask_escalated`, which is unmapped in
                // the ring, so the escalated model passes through model_for
                // unclobbered. A still-bad directive surfaces honestly at the
                // confirm gate.
                if let Some(AgentDirective::Run(name)) = &directive {
                    if crate::palette::Action::from_name(name).is_none() {
                        if let Some(up) = crate::tiers::model_above(cfg.provider, "ask") {
                            let cfg_up = AgentConfig { model: up, ..cfg.clone() };
                            let _ = tx.send(AgentEvent::AnswerStart); // fresh stream
                            if let Ok((text2, _)) = chat_with_id_streaming(
                                &cfg_up,
                                messages,
                                "ask_escalated",
                                mode.as_str(),
                                &mut on_delta,
                            ) {
                                let (display2, directive2) = parse_directive(&text2);
                                let _ = tx.send(AgentEvent::Answer {
                                    text: display2,
                                    directive: directive2,
                                });
                                return;
                            }
                        }
                    }
                }
                let _ = tx.send(AgentEvent::Answer { text: display, directive });
            }
            Err(e) => {
                let _ = tx.send(AgentEvent::Error(e.to_string()));
            }
        }
    });
}

/// Background tab-naming: tiny prompt, no registry, quiet failure.
pub fn auto_name(cfg: AgentConfig, tab_id: usize, screen: String, tx: mpsc::Sender<AgentEvent>) {
    std::thread::spawn(move || {
        let messages = format_task_messages(crate::prompts::AUTO_NAME_SYSTEM, &screen);
        // Task tag matches the ring's `auto_name` key (was "auto-name", which
        // silently skipped tier routing).
        if let Ok(text) = chat(&cfg, messages, "auto_name") {
            let name = kebab(&text);
            if !name.is_empty() {
                let _ = tx.send(AgentEvent::AutoName { tab_id, name });
            }
        }
        let _ = tx.send(AgentEvent::BgDone); // release the gate even on failure
    });
}

/// W6: one-line verdict on a watched terminal that went quiet or exited. Runs on
/// a background thread (even inside the detached daemon) and delivers a Notice.
pub fn watch_summary(
    cfg: AgentConfig,
    term_id: usize,
    reason: crate::app::WatchReason,
    tail: String,
    exit: Option<i32>,
    quiet_secs: u64,
    tx: mpsc::Sender<AgentEvent>,
) {
    std::thread::spawn(move || {
        let messages = build_watch_messages(reason, &tail, exit, quiet_secs);
        match chat(&cfg, messages, "watch") {
            Ok(text) => {
                let verdict = text.trim().lines().next().unwrap_or("").trim().to_string();
                if !verdict.is_empty() {
                    let _ = tx.send(AgentEvent::WatchSummary { term_id, verdict });
                }
            }
            // Surface the failure instead of going silent (and clear the gate).
            Err(e) => {
                let _ = tx.send(AgentEvent::WatchSummary {
                    term_id,
                    verdict: format!("⚠ watch couldn't summarize — {e}"),
                });
            }
        }
        let _ = tx.send(AgentEvent::BgDone); // always release the gate
    });
}

/// On-demand summary of a surface — a LOW-tier LLM call (task "summarize"),
/// user-initiated from the workspaces panel `s`. Same classifier as the watch,
/// just triggered manually for a live/idle surface that hasn't auto-fired. Emits
/// SurfaceSummary (no notice side-effects).
pub fn summarize_surface(cfg: AgentConfig, term_id: usize, tail: String, tx: mpsc::Sender<AgentEvent>) {
    std::thread::spawn(move || {
        let messages = build_watch_messages(crate::app::WatchReason::Quiet, &tail, None, 0);
        match chat(&cfg, messages, "summarize") {
            Ok(text) => {
                let line = text.trim().lines().next().unwrap_or("").trim().to_string();
                let _ = tx.send(AgentEvent::SurfaceSummary {
                    term_id,
                    text: if line.is_empty() { "(nothing to summarize)".to_string() } else { line },
                });
            }
            Err(e) => {
                let _ = tx.send(AgentEvent::SurfaceSummary { term_id, text: format!("⚠ couldn't summarize — {e}") });
            }
        }
        let _ = tx.send(AgentEvent::BgDone); // release the gate
    });
}

/// Rover MAP: a background one-line summary of ONE workspace's raw tail, for the
/// phone board. Cheap ("summarize") tier. Interpret-don't-recite / lead-with-the-
/// action tenets live in the prompt. Quiet on failure — the board keeps its prior
/// cached line (or the phone falls back), so a stale model can't wedge the board.
pub fn rover_summarize(cfg: AgentConfig, term_id: usize, tail: String, tx: mpsc::Sender<AgentEvent>) {
    std::thread::spawn(move || {
        let summary_system = crate::prompts::resolve("rover_summary", crate::prompts::ROVER_SUMMARY);
        let mut messages = vec![serde_json::json!({ "role": "system",
            "content": summary_system.trim_end() })];
        if let Some(p) = crate::persona::system_message() {
            messages.push(p); // VOICE-ish: the line is read on a phone glance
        }
        messages.push(serde_json::json!({ "role": "user", "content": tail }));
        if let Ok(text) = chat(&cfg, messages, "summarize") {
            let line = text.trim().lines().next().unwrap_or("").trim().to_string();
            if !line.is_empty() {
                let _ = tx.send(AgentEvent::RoverSummary {
                    term_id,
                    text: line,
                    provider: provenance(&cfg),
                });
            }
        }
        let _ = tx.send(AgentEvent::BgDone); // always release the gate
    });
}

/// Rover REDUCE: compose the mission briefing as a synthesis over the N cached
/// per-workspace summaries (+ their verdicts), the mission, and the PREVIOUS
/// briefing for continuity — never re-deriving from raw panes. VOICE task (the
/// persona applies; the reply is prose, never parsed). Reuses the `shift_brief`
/// tier — it is the same kind of work. Quiet on failure: the last briefing stands.
pub fn rover_brief(
    cfg: AgentConfig,
    summaries: String,
    mission: String,
    prev: String,
    tx: mpsc::Sender<AgentEvent>,
) {
    std::thread::spawn(move || {
        // Substitute the trusted placeholders first and the screen-derived
        // summaries LAST, so injected pane text is never re-scanned for holders.
        let system = crate::prompts::resolve("rover_brief", crate::prompts::ROVER_BRIEF)
            .trim_end()
            .replace("{mission}", if mission.is_empty() { "(none inferred)" } else { &mission })
            .replace("{prev}", if prev.is_empty() { "(this is the first briefing)" } else { &prev })
            .replace("{summaries}", &summaries);
        let mut messages = vec![serde_json::json!({ "role": "system", "content": system })];
        if let Some(p) = crate::persona::system_message() {
            messages.push(p); // VOICE task: the witty mission-control voice applies
        }
        messages.push(serde_json::json!({ "role": "user", "content": "Report." }));
        if let Ok(text) = chat(&cfg, messages, "shift_brief") {
            let brief = text.trim().to_string();
            if !brief.is_empty() {
                let _ = tx.send(AgentEvent::RoverBrief { text: brief });
            }
        }
        let _ = tx.send(AgentEvent::BgDone); // always release the gate
    });
}

/// Watch is a VOICE task: the verdict is prose the user reads many times a
/// day, so the persona rides along — bounded by WATCH_SYSTEM's one-line rule,
/// which the persona preamble forbids overriding. Pure, for the selfcheck.
pub fn build_watch_messages(
    reason: crate::app::WatchReason,
    tail: &str,
    exit: Option<i32>,
    quiet_secs: u64,
) -> Vec<serde_json::Value> {
    // Hand the model the ground truth it was missing: the shell's exit code on exit
    // (0 vs non-zero decides success/failure), and how long "quiet" has actually been.
    let hint = match reason {
        crate::app::WatchReason::Exit => {
            let code = match exit {
                Some(0) => " Its exit code was 0 — a clean exit.".to_string(),
                Some(c) => format!(" Its exit code was {c} (non-zero — this usually means it failed)."),
                None => String::new(),
            };
            crate::prompts::WATCH_HINT_EXIT.trim_end().replace("{code}", &code)
        }
        crate::app::WatchReason::Quiet => {
            let secs = if quiet_secs > 0 {
                format!(" — no new output for about {quiet_secs}s")
            } else {
                String::new()
            };
            crate::prompts::WATCH_HINT_QUIET.trim_end().replace("{secs}", &secs)
        }
    };
    let mut messages = vec![serde_json::json!({ "role": "system",
        "content": crate::prompts::WATCH_SYSTEM.trim_end().replace("{hint}", hint.trim_end()) })];
    if let Some(p) = crate::persona::system_message() {
        messages.push(p);
    }
    messages.push(serde_json::json!({ "role": "user", "content": tail }));
    messages
}

/// Background mission inference: read the recent work-journal snapshots and
/// name, in one line, what the user is working on. Quiet failure — a mission
/// is a nicety, never worth a notice. FORMAT task: no persona, its output is
/// re-ingested (ls summary, briefings, prompts).
pub fn infer_mission(cfg: AgentConfig, snapshots: Vec<String>, tx: mpsc::Sender<AgentEvent>) {
    std::thread::spawn(move || {
        let messages = format_task_messages(crate::prompts::MISSION_SYSTEM, &snapshots.join("\n"));
        if let Ok(text) = chat(&cfg, messages, "mission") {
            let mission = text.trim().lines().next().unwrap_or("").trim().to_string();
            if !mission.is_empty() {
                let _ = tx.send(AgentEvent::Mission { text: mission });
            }
        }
        let _ = tx.send(AgentEvent::BgDone);
    });
}

/// Background session-naming — like tab naming but for the whole session.
pub fn name_session(cfg: AgentConfig, screen: String, tx: mpsc::Sender<AgentEvent>) {
    std::thread::spawn(move || {
        let messages = format_task_messages(crate::prompts::NAME_SESSION_SYSTEM, &screen);
        // Tag matches the ring's `name_session` key (was "session-name").
        if let Ok(text) = chat(&cfg, messages, "name_session") {
            let name = kebab(&text);
            if !name.is_empty() {
                let _ = tx.send(AgentEvent::SessionName { name });
            }
        }
        let _ = tx.send(AgentEvent::BgDone); // release the gate even on failure
    });
}

/// W3: turn an English request into ONE shell command (no prose, no fences).
/// ALWAYS sends exactly one event so the caller's spinner can never wedge.
/// Synchronous shell translation with memory retrieval + logging. Returns the
/// extracted command and its `call_id`. Shared by the async composer and the
/// headless `mars translate` primitive the Python eval harness drives.
///
/// Memory (Axis A): when the active [`crate::retrieval::MemoryMode`] includes
/// history, the user's own past `(request → command)` pairs + recent shell history
/// are retrieved and shown as few-shot examples — the "sits at the terminal"
/// advantage a standalone translator can't have. The variant is logged.
/// True for models that emit an internal `<think>` block (Qwen3, QwQ, DeepSeek-R1,
/// OpenAI o-series). Only these get the "cap your reasoning" prompt clause — see
/// [`translate_once`] for why applying it to non-reasoning models breaks them.
fn is_reasoning_model(model: &str) -> bool {
    let m = model.to_lowercase();
    ["qwen3", "qwq", "deepseek-r1", "-r1", "o1-", "o3", "o4-mini", "thinking", "reasoning"]
        .iter()
        .any(|p| m.contains(p))
}

/// Every task tag a call site sends. The selfcheck pins each to a tiers.json
/// default key so a tag rename can't silently fall through to the provider
/// default model again (deliberate non-members: `ask_escalated`, `remote`).
pub const TASKS: &[&str] = &[
    "ask", "translate", "watch", "mission", "auto_name", "name_session", "shift_brief",
    "capture_goals",
];

/// Parse a goal-capture reply into 1-3 clean goal lines (strips list markers,
/// caps the count). Pure.
pub fn parse_goals(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| {
            l.trim()
                .trim_start_matches(|c: char| c.is_ascii_digit() || matches!(c, '.' | ')' | '-' | '*' | ' '))
                .trim()
                .to_string()
        })
        .filter(|l| !l.is_empty())
        .take(3)
        .collect()
}

/// Capture the user's active goals at detach: one low-tier FORMAT call over the
/// current pane evidence. Quiet failure — goals are a nicety, and a remote
/// detach may find the tunnel already gone.
pub fn capture_goals(cfg: AgentConfig, evidence: String, tx: mpsc::Sender<AgentEvent>) {
    std::thread::spawn(move || {
        let system = crate::prompts::CAPTURE_GOALS.trim_end().replace("{evidence}", &evidence);
        let messages = format_task_messages(&system, "What am I working on?");
        match chat(&cfg, messages, "capture_goals") {
            Ok(text) => {
                let goals = parse_goals(&text);
                if !goals.is_empty() {
                    let _ = tx.send(AgentEvent::Goals { goals });
                }
            }
            // Goals are a nicety (no user-facing overlay), but a silent failure here
            // is exactly what hid a dead model for days — leave a line in the log.
            Err(e) => eprintln!(
                "[mars] goal capture failed (provider {}, model {}): {e}",
                cfg.provider, cfg.model
            ),
        }
        let _ = tx.send(AgentEvent::BgDone);
    });
}

/// The shift report's plain-English situation briefing — the star of the reattach
/// overlay. A single VOICE call over the deterministic row evidence, so the
/// persona applies (the reply is displayed as prose, never machine-parsed, so
/// there's no format to corrupt). Streams token by token into the overlay; a
/// non-streaming provider (broker) delivers it in one delta. On failure the
/// overlay keeps its deterministic templated narrative.
pub fn shift_brief(
    cfg: AgentConfig,
    away: String,
    mission: String,
    prev: String,
    evidence: String,
    tx: mpsc::Sender<AgentEvent>,
) {
    std::thread::spawn(move || {
        let system = crate::prompts::SHIFT_BRIEF
            .trim_end()
            .replace("{away}", &away)
            .replace("{mission}", if mission.is_empty() { "(none inferred)" } else { &mission })
            .replace("{prev}", if prev.is_empty() { "(this is the first briefing)" } else { &prev })
            .replace("{evidence}", &evidence);
        let mut messages = vec![serde_json::json!({ "role": "system", "content": system })];
        if let Some(p) = crate::persona::system_message() {
            messages.push(p); // VOICE task: the witty mission-control voice applies
        }
        messages.push(serde_json::json!({ "role": "user", "content": "Report." }));
        let streamed = std::cell::Cell::new(false);
        let mut on_delta = |d: &str| {
            streamed.set(true);
            let _ = tx.send(AgentEvent::ShiftDelta { text: d.to_string() });
        };
        match chat_with_id_streaming(&cfg, messages, "shift_brief", "n/a", &mut on_delta) {
            Ok((text, _)) if !streamed.get() && !text.trim().is_empty() => {
                // A non-streaming provider (broker) never fired the sink — deliver
                // the whole briefing as one delta so the overlay shows it.
                let _ = tx.send(AgentEvent::ShiftDelta { text });
            }
            Err(e) => {
                eprintln!(
                    "[mars] shift_brief enrichment failed (provider {}, model {}): {e}",
                    cfg.provider, cfg.model
                );
            }
            _ => {} // streamed already, or empty (keep the templated line)
        }
        let _ = tx.send(AgentEvent::ShiftDone);
        let _ = tx.send(AgentEvent::BgDone);
    });
}

/// FORMAT-task builder for translate: machine-parsed output, so the persona
/// NEVER appears here (selfcheck-pinned). Pure.
/// A compact environment line the model can't read off the screen but that changes
/// the correct command — the shell (bash/zsh/fish), the OS (macOS `sed -i ''` vs GNU
/// `sed -i`, brew vs apt, path flags), and the working directory.
pub fn env_context() -> String {
    let shell = std::env::var("SHELL")
        .ok()
        .and_then(|s| s.rsplit(['/', '\\']).next().map(str::to_string))
        .unwrap_or_else(|| "sh".into());
    let cwd = std::env::current_dir()
        .ok()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    format!("ENV: shell={shell} · os={} · cwd={cwd}", std::env::consts::OS)
}

pub fn build_translate_messages(
    reasoning_cap: &str,
    examples_block: &str,
    request: &str,
    screen: &str,
) -> Vec<serde_json::Value> {
    let system = crate::prompts::TRANSLATE_SYSTEM
        .trim_end()
        .replace("{reasoning_cap}", reasoning_cap)
        .replace("{examples_block}", examples_block);
    let user = format!("{}\nSCREEN:\n{screen}\n\nREQUEST: {request}", env_context());
    vec![
        serde_json::json!({ "role": "system", "content": system }),
        serde_json::json!({ "role": "user", "content": user }),
    ]
}

/// Shared shape of the remaining FORMAT tasks (naming, mission): one static
/// system prompt + one user payload — persona-free by construction.
pub fn format_task_messages(system: &str, user: &str) -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({ "role": "system", "content": system.trim_end() }),
        serde_json::json!({ "role": "user", "content": user }),
    ]
}

pub fn translate_once(cfg: &AgentConfig, request: &str, screen: &str) -> anyhow::Result<(String, u64)> {
    let mode = crate::retrieval::MemoryMode::from_env();
    let examples = crate::retrieval::fewshot_for(request);
    // Reasoning models (Qwen3, R1, o-series) burn the token budget on a <think> block,
    // so we cap their reasoning. Non-reasoning models (Gemini Flash-Lite, gpt-4o-mini,
    // Haiku) read that same instruction as a cue to reason *silently* and can return an
    // EMPTY completion — so the cap is applied ONLY to reasoning models.
    let reasoning_cap = if is_reasoning_model(&cfg.model) {
        format!(" {}", crate::prompts::TRANSLATE_REASONING_CAP.trim())
    } else {
        String::new()
    };
    let examples_block = if examples.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n{}",
            crate::prompts::TRANSLATE_EXAMPLES.trim_end().replace("{examples}", &examples)
        )
    };
    let messages = build_translate_messages(&reasoning_cap, &examples_block, request, screen);
    // Tag must match the tiers.json key ("translate") — "shell" routed to the
    // provider default model for months before anyone noticed.
    let (text, call_id) = chat_with_id(cfg, messages, "translate", mode.as_str())?;
    let command = text
        .trim()
        .trim_matches('`')
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .trim_start_matches("$ ")
        .to_string();
    Ok((command, call_id))
}

pub fn translate_shell(cfg: AgentConfig, request: String, screen: String, tx: mpsc::Sender<AgentEvent>) {
    std::thread::spawn(move || {
        let ev = match translate_once(&cfg, &request, &screen) {
            Ok((command, _)) if command.is_empty() => {
                AgentEvent::Error("couldn't translate that — rephrase and retry".into())
            }
            Ok((command, call_id)) => AgentEvent::ShellTranslation { command, call_id },
            Err(e) => AgentEvent::Error(e.to_string()),
        };
        let _ = tx.send(ev);
    });
}

/// Extract "retry in 14.89s"-style hints from a 429 message → whole seconds.
pub fn retry_secs(msg: &str) -> Option<u64> {
    let after = msg.split("retry in ").nth(1)?;
    let num: String = after.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
    num.parse::<f64>().ok().map(|s| s.ceil() as u64)
}

/// Strip `<think>…</think>` reasoning blocks (Qwen3, DeepSeek-R1, etc.) so only
/// the user-facing answer + directive remain.
fn strip_reasoning(text: &str) -> String {
    let mut out = text.to_string();
    while let (Some(a), Some(b)) = (out.find("<think>"), out.find("</think>")) {
        if a < b {
            out.replace_range(a..b + "</think>".len(), "");
        } else {
            break;
        }
    }
    // A dangling, unclosed <think> → drop everything from it.
    if let Some(a) = out.find("<think>") {
        out.truncate(a);
    }
    out.trim().to_string()
}

/// Optional streaming sink: receives each visible chunk as it arrives.
type DeltaSink<'a> = Option<&'a mut dyn FnMut(&str)>;

/// Reborrow the sink for one call without consuming it (the rotation loop may
/// hand it to several attempts in sequence).
fn reborrow<'b>(sink: &'b mut DeltaSink<'_>) -> DeltaSink<'b> {
    match sink {
        Some(s) => Some(&mut **s),
        None => None,
    }
}

/// The safely-streamable prefix of a partially-received reply: closed <think>
/// blocks removed, an unclosed one truncated, and a trailing partial "<think>"
/// tag held back — so reasoning never flashes on screen and emitted text never
/// retracts as more chunks land.
pub(crate) fn stream_visible(raw: &str) -> String {
    let s = strip_reasoning(raw);
    let tag = "<think>";
    let mut cut = s.len();
    for k in (1..tag.len()).rev() {
        if s.ends_with(&tag[..k]) {
            cut = s.len() - k;
            break;
        }
    }
    // trim_end in EVERY path: strip_reasoning trims, so an untrimmed hold-back
    // result could exceed a later trimmed one — emitted text must never retract.
    s[..cut].trim_end().to_string()
}

/// Single choke point for every LLM call. `task` tags the call; `retrieval` names
/// the memory variant that shaped the prompt ("n/a" when the path doesn't retrieve).
/// Times the call, captures real token usage, logs it under a fresh `call_id` when
/// MARS_LLM_DEBUG is on, and returns `(reasoning-stripped reply, call_id)`. The
/// call_id lets a later behavioral outcome (accept/edit/reject) be correlated.
pub fn chat_with_id(
    cfg: &AgentConfig,
    messages: Vec<serde_json::Value>,
    task: &str,
    retrieval: &str,
) -> anyhow::Result<(String, u64)> {
    chat_inner(cfg, messages, task, retrieval, None)
}

/// `chat_with_id`, streaming: `on_delta` receives each visible chunk as it
/// arrives. The returned text is the complete reply, identical to what the
/// non-streaming path produces — directive parsing still needs the whole.
pub fn chat_with_id_streaming(
    cfg: &AgentConfig,
    messages: Vec<serde_json::Value>,
    task: &str,
    retrieval: &str,
    on_delta: &mut dyn FnMut(&str),
) -> anyhow::Result<(String, u64)> {
    chat_inner(cfg, messages, task, retrieval, Some(on_delta))
}

fn chat_inner(
    cfg: &AgentConfig,
    messages: Vec<serde_json::Value>,
    task: &str,
    retrieval: &str,
    mut sink: DeltaSink,
) -> anyhow::Result<(String, u64)> {
    // Remote box: proxy the whole call home over the forwarded socket. No key,
    // no Authorization header, ever constructed here. (Logged home-side.)
    // Frame-at-a-time protocol — the remote path does not stream.
    if cfg.provider == "broker" {
        let sock = cfg
            .broker_sock
            .as_deref()
            .ok_or_else(|| anyhow::anyhow!("broker mode with no socket"))?;
        return crate::broker::chat_via_broker(sock, cfg, messages).map(|t| (t, 0));
    }

    // Ordered candidates: every model in this task's tier on the primary provider
    // (IN-TIER fallback for a retired or throttled model — the fix for a provider
    // silently dropping a model), then the same tier on each other keyed provider
    // (CROSS-PROVIDER rotation for limits/outage). A retired model (404) and a
    // rate-limit (429) both arrive as an HTTP status BEFORE any token streams, so
    // no partial output can precede the move to the next candidate.
    let mut candidates: Vec<AgentConfig> = Vec::new();
    for model in crate::tiers::models_for(cfg.provider, task, &cfg.model) {
        candidates.push(AgentConfig { model, ..cfg.clone() });
    }
    for alt in rotation_candidates(cfg.provider) {
        for model in crate::tiers::models_for(alt.provider, task, &alt.model) {
            candidates.push(AgentConfig { model, ..alt.clone() });
        }
    }
    let mut last: Option<anyhow::Error> = None;
    for c in candidates {
        match attempt(&c, &messages, task, retrieval, reborrow(&mut sink)) {
            Ok(ok) => return Ok(ok),
            Err(e) => {
                // Only churn on recoverable failures. An auth error or a malformed
                // request will fail identically on every model — surface it, don't
                // hammer the whole ring. A credit/quota exhaustion is not transient, but
                // the right move is the SAME as a throttle: fall through to the next
                // provider in the chain (a free Groq/Gemini tier) rather than dead-ending
                // on a paid provider that's out of budget.
                let recoverable = e.downcast_ref::<RateLimited>().is_some()
                    || e.downcast_ref::<ModelUnavailable>().is_some()
                    || is_provider_exhausted(&e);
                last = Some(e);
                if !recoverable {
                    break;
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| anyhow::anyhow!("no model candidates for task '{task}'")))
}

/// A credit / quota / billing exhaustion (e.g. Anthropic "credit balance too low", OpenAI
/// "insufficient_quota", a 402). The provider is configured and authenticating fine, it's just
/// out of budget — so treat it like a throttle and rotate to the next provider in the chain
/// (typically a free Groq/Gemini tier) instead of surfacing a dead end.
fn is_provider_exhausted(e: &anyhow::Error) -> bool {
    let m = e.to_string().to_ascii_lowercase();
    [
        "credit balance too low",
        "insufficient",       // "insufficient_quota", "insufficient funds/credits"
        "exceeded your current quota",
        "billing",
        "payment required",
        " 402",
    ]
    .iter()
    .any(|p| m.contains(p))
}

/// One provider attempt: tier-resolve, call, log. Split from `chat_inner` so
/// the rotation loop logs every attempt as its own call record.
fn attempt(
    cfg: &AgentConfig,
    messages: &[serde_json::Value],
    task: &str,
    retrieval: &str,
    sink: DeltaSink,
) -> anyhow::Result<(String, u64)> {
    let call_id = crate::llm_log::next_call_id();
    // The model is already tier-resolved by `chat_inner`, which owns the candidate
    // order (in-tier fallback + cross-provider rotation, MARS_LLM_MODEL honored).
    // attempt just makes this one call and logs it as its own record.
    let start = std::time::Instant::now();
    let result = {
        // Guard the caller's sink: accumulate raw chunks, re-strip, and emit
        // only the growth of the visible prefix — reasoning-model <think>
        // output never reaches the screen, even split across chunk boundaries.
        let mut raw = String::new();
        let mut emitted = 0usize;
        let mut wrapped;
        let provider_sink: DeltaSink = match sink {
            Some(on_delta) => {
                wrapped = move |d: &str| {
                    raw.push_str(d);
                    let vis = stream_visible(&raw);
                    if vis.len() > emitted {
                        on_delta(&vis[emitted..]);
                        emitted = vis.len();
                    }
                };
                Some(&mut wrapped as &mut dyn FnMut(&str))
            }
            None => None,
        };
        match cfg.provider {
            "anthropic" => chat_anthropic(cfg, messages, provider_sink),
            "bedrock" => chat_bedrock(cfg, messages, provider_sink),
            _ => chat_openai(cfg, messages, provider_sink),
        }
    };
    let latency_ms = start.elapsed().as_millis() as u64;

    match result {
        Ok((text, pt, ct)) => {
            crate::llm_log::record(&crate::llm_log::CallRecord {
                call_id, task, provider: cfg.provider, model: &cfg.model, retrieval,
                prompt_tokens: pt, completion_tokens: ct, latency_ms,
                ok: true, error: None, input: messages, output: &text,
            });
            Ok((strip_reasoning(&text), call_id))
        }
        Err(e) => {
            let msg = e.to_string();
            crate::llm_log::record(&crate::llm_log::CallRecord {
                call_id, task, provider: cfg.provider, model: &cfg.model, retrieval,
                prompt_tokens: 0, completion_tokens: 0, latency_ms,
                ok: false, error: Some(&msg), input: messages, output: "",
            });
            Err(e)
        }
    }
}

/// Convenience wrapper for callers that don't need the `call_id` correlation
/// (watch / auto-name / session-name / remote) and don't retrieve.
pub fn chat(cfg: &AgentConfig, messages: Vec<serde_json::Value>, task: &str) -> anyhow::Result<String> {
    chat_with_id(cfg, messages, task, "n/a").map(|(text, _)| text)
}

/// OpenAI-compatible providers (OpenAI, Groq, Gemini's OpenAI shim, custom,
/// Ollama). Returns (raw_text, prompt_tokens, completion_tokens). With a sink,
/// requests SSE and forwards each content delta as it arrives.
fn chat_openai(
    cfg: &AgentConfig,
    messages: &[serde_json::Value],
    sink: DeltaSink,
) -> anyhow::Result<(String, u64, u64)> {
    // Azure bakes deployment + api-version into cfg.url (already a complete
    // endpoint) and authenticates with an `api-key` header, not a bearer token.
    let azure = cfg.provider == "azure";
    let url = if azure {
        cfg.url.clone()
    } else {
        format!("{}/chat/completions", cfg.url)
    };
    let mut body = serde_json::json!({
        "model": cfg.model,
        "messages": messages,
        "max_tokens": cfg.max_tokens,
        "temperature": cfg.temperature
    });
    if sink.is_some() {
        body["stream"] = serde_json::json!(true);
        // Usage-in-final-chunk is an opt-in extension; only request it where
        // it's known-supported (other shims reject unknown fields).
        if matches!(cfg.provider, "openai" | "groq" | "azure") {
            body["stream_options"] = serde_json::json!({ "include_usage": true });
        }
    }

    // Bound the call so a stalled connection surfaces as an error instead of
    // hanging the agent (and the spinner) forever.
    let mut req = ureq::post(&url)
        .timeout(std::time::Duration::from_secs(30))
        .set("Content-Type", "application/json");
    req = if azure {
        req.set("api-key", &cfg.key)
    } else {
        req.set("Authorization", &format!("Bearer {}", cfg.key))
    };
    let resp = match req.send_json(body) {
        Ok(r) => r,
        // Pull the real message out of the error body (bad key, quota, etc.).
        // Gemini wraps it in a JSON array: [{"error":{"message": …}}].
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            let api_msg = serde_json::from_str::<serde_json::Value>(&body).ok().and_then(|j| {
                let node = if j.is_array() { j[0].clone() } else { j };
                node["error"]["message"].as_str().map(str::to_string)
            });
            let retired = is_retired_model(code, api_msg.as_deref());
            let msg = match code {
                429 => match api_msg.as_deref().and_then(retry_secs) {
                    Some(s) => format!(
                        "rate limit reached — wait ~{s}s and retry (free tier). \
                         Tip: raise limits, switch model with MARS_LLM_MODEL, or use \
                         GROQ_API_KEY / a local Ollama via MARS_LLM_URL."
                    ),
                    None => "rate limit reached (free tier) — wait ~30s and retry, or \
                             switch model/provider (MARS_LLM_MODEL / GROQ_API_KEY / \
                             MARS_LLM_URL for local Ollama)."
                        .to_string(),
                },
                401 | 403 => format!(
                    "auth failed — check your API key. ({})",
                    api_msg.as_deref().unwrap_or("invalid credentials")
                ),
                _ => api_msg.unwrap_or_else(|| format!("HTTP {code}")),
            };
            if code == 429 {
                return Err(anyhow::Error::new(RateLimited(msg)));
            }
            if retired {
                return Err(anyhow::Error::new(ModelUnavailable(msg)));
            }
            anyhow::bail!("{msg}");
        }
        Err(e) => anyhow::bail!("{e}"),
    };

    if let Some(on_delta) = sink {
        use std::io::BufRead;
        let mut text = String::new();
        let (mut pt, mut ct) = (0u64, 0u64);
        for line in std::io::BufReader::new(resp.into_reader()).lines() {
            let line = line?;
            let Some(data) = line.strip_prefix("data:") else { continue };
            let data = data.trim();
            if data == "[DONE]" {
                break;
            }
            let Ok(j) = serde_json::from_str::<serde_json::Value>(data) else { continue };
            if let Some(msg) = j["error"]["message"].as_str() {
                anyhow::bail!("{msg}");
            }
            if let Some(d) = j["choices"][0]["delta"]["content"].as_str() {
                if !d.is_empty() {
                    text.push_str(d);
                    on_delta(d);
                }
            }
            if j["usage"].is_object() {
                pt = j["usage"]["prompt_tokens"].as_u64().unwrap_or(pt);
                ct = j["usage"]["completion_tokens"].as_u64().unwrap_or(ct);
            }
        }
        return Ok((text, pt, ct));
    }

    let json: serde_json::Value = resp.into_json()?;
    if let Some(msg) = json["error"]["message"].as_str() {
        anyhow::bail!("{msg}");
    }
    let text = json["choices"][0]["message"]["content"].as_str().unwrap_or("").to_string();
    let pt = json["usage"]["prompt_tokens"].as_u64().unwrap_or(0);
    let ct = json["usage"]["completion_tokens"].as_u64().unwrap_or(0);
    Ok((text, pt, ct))
}

/// Anthropic Messages API — NOT OpenAI-compatible: system is a top-level field
/// (not a message role), auth is `x-api-key` + `anthropic-version`, the reply is
/// an array of content blocks, and usage is input/output_tokens. With a sink,
/// requests SSE (`content_block_delta` events carry the text).
fn chat_anthropic(
    cfg: &AgentConfig,
    messages: &[serde_json::Value],
    sink: DeltaSink,
) -> anyhow::Result<(String, u64, u64)> {
    // Split the system message(s) out of the OpenAI-style array.
    let mut system = String::new();
    let mut msgs: Vec<serde_json::Value> = Vec::new();
    for m in messages {
        if m["role"].as_str() == Some("system") {
            if !system.is_empty() {
                system.push('\n');
            }
            system.push_str(m["content"].as_str().unwrap_or(""));
        } else {
            msgs.push(m.clone());
        }
    }
    let url = format!("{}/v1/messages", cfg.url);
    // No `temperature`: the newest Claude models (Sonnet/Haiku 4.5+, Opus 4.x) reject it
    // as deprecated, and all models fall back to a sane default when it is omitted.
    let mut body = serde_json::json!({
        "model": cfg.model,
        "max_tokens": cfg.max_tokens,
        "system": system,
        "messages": msgs
    });
    if sink.is_some() {
        body["stream"] = serde_json::json!(true);
    }
    let resp = match ureq::post(&url)
        .timeout(std::time::Duration::from_secs(30))
        .set("x-api-key", &cfg.key)
        .set("anthropic-version", "2023-06-01")
        .set("Content-Type", "application/json")
        .send_json(body)
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            let api_msg = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|j| j["error"]["message"].as_str().map(str::to_string));
            let retired = is_retired_model(code, api_msg.as_deref());
            let msg = match code {
                429 => "rate limit reached (Anthropic) — wait and retry, or switch model \
                        with MARS_LLM_MODEL."
                    .to_string(),
                401 | 403 => format!(
                    "auth failed — check ANTHROPIC_API_KEY. ({})",
                    api_msg.as_deref().unwrap_or("invalid credentials")
                ),
                _ => api_msg.unwrap_or_else(|| format!("HTTP {code}")),
            };
            if code == 429 {
                return Err(anyhow::Error::new(RateLimited(msg)));
            }
            if retired {
                return Err(anyhow::Error::new(ModelUnavailable(msg)));
            }
            anyhow::bail!("{msg}");
        }
        Err(e) => anyhow::bail!("{e}"),
    };

    if let Some(on_delta) = sink {
        use std::io::BufRead;
        let mut text = String::new();
        let (mut pt, mut ct) = (0u64, 0u64);
        for line in std::io::BufReader::new(resp.into_reader()).lines() {
            let line = line?;
            let Some(data) = line.strip_prefix("data:") else { continue };
            let Ok(j) = serde_json::from_str::<serde_json::Value>(data.trim()) else { continue };
            match j["type"].as_str().unwrap_or("") {
                "content_block_delta" => {
                    if let Some(d) = j["delta"]["text"].as_str() {
                        if !d.is_empty() {
                            text.push_str(d);
                            on_delta(d);
                        }
                    }
                }
                "message_start" => {
                    pt = j["message"]["usage"]["input_tokens"].as_u64().unwrap_or(0);
                }
                "message_delta" => {
                    ct = j["usage"]["output_tokens"].as_u64().unwrap_or(ct);
                }
                "error" => {
                    anyhow::bail!("{}", j["error"]["message"].as_str().unwrap_or("stream error"));
                }
                _ => {}
            }
        }
        return Ok((text, pt, ct));
    }

    let json: serde_json::Value = resp.into_json()?;
    if let Some(msg) = json["error"]["message"].as_str() {
        anyhow::bail!("{msg}");
    }
    // content is an array of blocks; concatenate the text ones.
    let text = json["content"]
        .as_array()
        .map(|blocks| blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(""))
        .unwrap_or_default();
    let pt = json["usage"]["input_tokens"].as_u64().unwrap_or(0);
    let ct = json["usage"]["output_tokens"].as_u64().unwrap_or(0);
    Ok((text, pt, ct))
}

/// The Bedrock Converse request body from OpenAI-style messages: system split
/// into a top-level `system` array, each turn's content wrapped as `[{text}]`,
/// generation params under `inferenceConfig`. Pure, so the selfcheck can pin
/// the shape without a network call.
pub fn build_bedrock_body(
    messages: &[serde_json::Value],
    max_tokens: u32,
    temperature: f64,
) -> serde_json::Value {
    let mut system: Vec<serde_json::Value> = Vec::new();
    let mut msgs: Vec<serde_json::Value> = Vec::new();
    for m in messages {
        let content = m["content"].as_str().unwrap_or("");
        if m["role"].as_str() == Some("system") {
            system.push(serde_json::json!({ "text": content }));
        } else {
            msgs.push(serde_json::json!({
                "role": m["role"].as_str().unwrap_or("user"),
                "content": [{ "text": content }],
            }));
        }
    }
    serde_json::json!({
        "system": system,
        "messages": msgs,
        "inferenceConfig": { "maxTokens": max_tokens, "temperature": temperature },
    })
}

/// AWS Bedrock via the Converse API. Bearer auth (a Bedrock API key — no SigV4),
/// modelId in the URL path, a provider-neutral body shape that covers every
/// Bedrock model (Claude, Llama, Mistral, Nova). Non-streaming for now: the
/// `converse-stream` endpoint uses AWS binary event-stream framing, not SSE — so
/// the sink is accepted and ignored (the answer renders at once), exactly like
/// the broker path.
fn chat_bedrock(
    cfg: &AgentConfig,
    messages: &[serde_json::Value],
    _sink: DeltaSink,
) -> anyhow::Result<(String, u64, u64)> {
    // cfg.url is the region base; the modelId (cfg.model) goes in the path.
    let url = format!("{}/model/{}/converse", cfg.url, cfg.model);
    let body = build_bedrock_body(messages, cfg.max_tokens, cfg.temperature);
    let resp = match ureq::post(&url)
        .timeout(std::time::Duration::from_secs(30))
        .set("Authorization", &format!("Bearer {}", cfg.key))
        .set("Content-Type", "application/json")
        .send_json(body)
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let body = r.into_string().unwrap_or_default();
            let api_msg = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|j| j["message"].as_str().map(str::to_string));
            let retired = is_retired_model(code, api_msg.as_deref());
            let msg = match code {
                429 => "rate limit / throttled (Bedrock) — wait and retry, or switch model \
                        with MARS_LLM_MODEL."
                    .to_string(),
                401 | 403 => format!(
                    "auth failed — check AWS_BEARER_TOKEN_BEDROCK and the region/model access. ({})",
                    api_msg.as_deref().unwrap_or("invalid credentials")
                ),
                _ => api_msg.unwrap_or_else(|| format!("HTTP {code}")),
            };
            if code == 429 {
                return Err(anyhow::Error::new(RateLimited(msg)));
            }
            if retired {
                return Err(anyhow::Error::new(ModelUnavailable(msg)));
            }
            anyhow::bail!("{msg}");
        }
        Err(e) => anyhow::bail!("{e}"),
    };

    let json: serde_json::Value = resp.into_json()?;
    if let Some(msg) = json["message"].as_str() {
        // Converse error bodies are `{ "message": … }`.
        if json["output"].is_null() {
            anyhow::bail!("{msg}");
        }
    }
    let text = json["output"]["message"]["content"]
        .as_array()
        .map(|blocks| blocks.iter().filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join(""))
        .unwrap_or_default();
    let pt = json["usage"]["inputTokens"].as_u64().unwrap_or(0);
    let ct = json["usage"]["outputTokens"].as_u64().unwrap_or(0);
    Ok((text, pt, ct))
}
