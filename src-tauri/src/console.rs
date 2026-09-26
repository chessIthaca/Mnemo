// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Console mode — the `-console` terminal frontend and its pure logic.
//!
//! Split into two layers so a future headless daemon can reuse everything
//! except the REPL loop:
//!
//! - **Pure logic (this module's free functions)** — input parsing, event
//!   rendering, approval/question answer parsing, and status formatting.
//!   No I/O, no locks; unit-tested directly.
//! - **The runtime + REPL** — [`crate::console`] also holds the
//!   `ConsoleRuntime` (brain + manager + agent loops, the AppHandle-free
//!   headless core) and `run_console` (the interactive stdin/stdout loop).
//!   These live further down in this file and use the pure helpers here.
//!
//! The REPL replicates exactly two UI affordances — the status-bar model
//! picker (`/model`) and its reasoning-effort dropdown (`/reasoning`) — via
//! the same shared swap path as the IPC `set_model` command's legacy global
//! switch ([`crate::ipc::config_io::resolve_model_provider`] +
//! [`crate::ipc::config_io::swap_provider_into_loops`]). One deliberate
//! divergence (user decision 2026-08-22, models are agent-specific): the GUI
//! picker switches only the active agent, while the console `/model` is a
//! REPL surface with no agent focus, so it keeps the global semantics
//! (factory + every live loop). Resolution/validation/effort mapping stay
//! shared, so they can never drift.

use std::sync::Arc;

use mnemo::agent::factory::AgentLoopFactory;
use mnemo::config::{Config, ModelRef};
use mnemo::provider::trace::LlmRequestLog;
use mnemo::runtime::channels::{AgentEvent, Approval, QuestionOption, UserAnswer};
use mnemo::runtime::{
    AgentCommand, AgentId, AgentManager, AgentSpawner, DescendantTracker, ParentAwareSpawner,
};
use mnemo::tool::ToolResult;

use crate::ipc::state::AgentLoopMap;

/// The reasoning-effort values the status-bar picker offers; `/reasoning`
/// validates its argument against this list before swapping.
pub(crate) const REASONING_EFFORTS: &[&str] = &["max", "high", "medium", "low", "minimal", "off"];

/// Which streaming channel a fragment belongs to — the REPL prints a
/// separator when the channel switches (reasoning vs. assistant text), like
/// the GUI's dimmed reasoning block above the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamChannel {
    /// Assistant text.
    Text,
    /// Model reasoning (reasoning models).
    Reasoning,
}

/// How one agent event renders in the console — pure data; the REPL owns the
/// actual printing (and any channel-boundary state).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Rendered {
    /// Print nothing (events the console does not surface).
    Silent,
    /// A streaming fragment printed WITHOUT a trailing newline, on the given
    /// channel (assistant text or reasoning).
    Fragment {
        /// Which channel the fragment belongs to.
        channel: StreamChannel,
        /// The fragment text.
        text: String,
    },
    /// One or more complete lines (each printed with a newline).
    Lines(Vec<String>),
}

/// A parsed REPL input line. Plain text is a prompt; the slash surface
/// deliberately mirrors only the status-bar picker affordances (`/model`,
/// `/reasoning`) plus REPL mechanics (`/help`, `/exit`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConsoleCommand {
    /// Plain text — send to the agent as a prompt (empty when the line was
    /// blank; the REPL skips empty prompts).
    Prompt(String),
    /// `/model` — list endpoints + models when no query, swap when one is
    /// given (`endpoint/model` or a bare `model`).
    Model {
        /// The raw query (`endpoint/model` or `model`), if any.
        query: Option<String>,
    },
    /// `/reasoning` — show the current effort when no value, swap when one is
    /// given (`max|high|medium|low|minimal|off`).
    Reasoning {
        /// The raw effort value, if any.
        value: Option<String>,
    },
    /// `/help` — print the command list.
    Help,
    /// `/exit` (alias `/quit`) — shut down and leave.
    Exit,
    /// An unrecognized slash command — the REPL prints an error.
    Unknown(String),
}

/// Parse one line of REPL input into a [`ConsoleCommand`].
///
/// Leading/trailing whitespace is trimmed. A line not starting with `/` is a
/// prompt (possibly empty — the REPL ignores blank lines). Slash commands are
/// split on the first space; everything after it is the argument (trimmed).
pub(crate) fn parse_command(line: &str) -> ConsoleCommand {
    let line = line.trim();
    if !line.starts_with('/') {
        return ConsoleCommand::Prompt(line.to_string());
    }
    let (word, rest) = match line.split_once(' ') {
        Some((w, r)) => (w, r.trim()),
        None => (line, ""),
    };
    let arg = (!rest.is_empty()).then(|| rest.to_string());
    match word {
        "/model" => ConsoleCommand::Model { query: arg },
        "/reasoning" => ConsoleCommand::Reasoning { value: arg },
        "/help" | "/?" => ConsoleCommand::Help,
        "/exit" | "/quit" => ConsoleCommand::Exit,
        other => ConsoleCommand::Unknown(other.to_string()),
    }
}

/// Split a `/model` query into its endpoint + model parts.
///
/// Accepts `endpoint/model` (both non-empty) or a bare `model`. Anything else
/// (empty halves, extra slashes) is an error whose text is shown to the user.
pub(crate) fn parse_model_query(query: &str) -> Result<(Option<String>, String), String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("empty model query".to_string());
    }
    match query.split_once('/') {
        None => Ok((None, query.to_string())),
        Some((endpoint, model)) => {
            if endpoint.is_empty() {
                return Err(format!("empty endpoint in '{query}'"));
            }
            if model.is_empty() {
                return Err(format!("empty model in '{query}'"));
            }
            if model.contains('/') {
                return Err(format!(
                    "malformed model query '{query}' — expected 'endpoint/model' or 'model'"
                ));
            }
            Ok((Some(endpoint.to_string()), model.to_string()))
        }
    }
}

/// Validate a `/reasoning` argument against the picker's value list.
pub(crate) fn validate_effort(value: &str) -> Result<(), String> {
    let value = value.trim();
    if REASONING_EFFORTS.contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "unknown reasoning effort '{value}' — expected one of: {}",
            REASONING_EFFORTS.join(", ")
        ))
    }
}

/// Parse the user's answer to an approval prompt. `y`/`yes` approves, `n`/`no`
/// denies, `a`/`all` denies this and the rest of the turn; anything else is
/// `None` (the REPL re-prompts without consuming the pending approval).
pub(crate) fn parse_approval_answer(line: &str) -> Option<Approval> {
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(Approval::Approve),
        "n" | "no" => Some(Approval::Deny),
        "a" | "all" => Some(Approval::DenyAll),
        _ => None,
    }
}

/// Parse the user's answer to an `ask_user` question. A 1-based option number
/// selects that option; any other non-empty text is a freeform answer (the
/// "Let's talk about it" fallback). Empty input is `None` (re-prompt).
pub(crate) fn parse_question_answer(line: &str, options: &[QuestionOption]) -> Option<UserAnswer> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    if let Ok(n) = line.parse::<usize>() {
        if n >= 1 && n <= options.len() {
            return Some(UserAnswer::Choice { index: n - 1 });
        }
    }
    Some(UserAnswer::Freeform {
        text: line.to_string(),
    })
}

/// Truncate a string to at most `max` CHARS (never splitting mid-UTF8),
/// appending an ellipsis when something was cut. Used to keep rendered tool
/// output + args on one terminal line.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Compact JSON-args summary for one-line rendering: a key:value form like
/// `{path:x, mode:edit}` (values trimmed of newlines), truncated to one
/// terminal line. Falls back to the JSON form for non-object values.
pub(crate) fn summarize_args(args: &serde_json::Value) -> String {
    let compact = match args {
        serde_json::Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{k}:{}", summarize_value(v)))
                .collect();
            format!("{{{}}}", parts.join(", "))
        }
        other => other.to_string(),
    }
    .replace(['\n', '\r'], " ");
    truncate(compact.trim(), 160)
}

/// Render one JSON value compactly inside an args summary (strings unquoted,
/// everything else via its JSON form).
fn summarize_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => truncate(s, 60),
        other => other.to_string(),
    }
}

/// Render an agent event to console output — pure data; the REPL does the
/// printing. Approval/question events render their prompt lines here too (the
/// oneshot responders stay untouched — only their serializable sibling fields
/// are read).
pub(crate) fn render_event(event: &AgentEvent) -> Rendered {
    match event {
        AgentEvent::Started => Rendered::Silent,
        AgentEvent::TextDelta(text) => Rendered::Fragment {
            channel: StreamChannel::Text,
            text: text.clone(),
        },
        AgentEvent::ReasoningDelta(text) => Rendered::Fragment {
            channel: StreamChannel::Reasoning,
            text: text.clone(),
        },
        AgentEvent::ToolCallStart { name, .. } => Rendered::Lines(vec![format!("→ {name}")]),
        AgentEvent::ToolCallArgDelta { .. } => Rendered::Silent,
        // Live partial output is a UI affordance, not console text: the console
        // prints the FULL result when the call finishes, so streaming chunks
        // here would duplicate every line (and interleave two calls' output).
        AgentEvent::ToolOutputDelta { .. } => Rendered::Silent,
        AgentEvent::ToolResult { result, .. } => Rendered::Lines(render_tool_result(result)),
        AgentEvent::Usage {
            prompt_tokens,
            completion_tokens,
            reasoning_tokens,
            cached_tokens,
            ..
        } => Rendered::Lines(vec![format!(
            "  [tokens in {prompt_tokens} (cached {cached_tokens}) · out {completion_tokens} · reasoning {reasoning_tokens}]"
        )]),
        AgentEvent::ContextUsage { .. } => Rendered::Silent,
        AgentEvent::WorkflowStateChanged { state, .. } => {
            Rendered::Lines(vec![format!("— workflow: {state:?}")])
        }
        AgentEvent::StepCompleted { step_index } => {
            Rendered::Lines(vec![format!("✓ step {step_index} complete")])
        }
        AgentEvent::SuggestionInjected { text, .. } => {
            Rendered::Lines(vec![format!("· steer landed: {text}")])
        }
        AgentEvent::Parked {
            reason,
            workflow_state,
            descendants_running,
            auto_continue_streak,
        } => Rendered::Lines(vec![format!(
            "⏸ parked: {reason:?} (wf={workflow_state}, descendants={descendants_running}, streak={auto_continue_streak})"
        )]),
        AgentEvent::PromptDispatched { .. } => Rendered::Silent,
        AgentEvent::SkillStarted { name, prompt } => Rendered::Lines(vec![format!(
            "▶ skill {name}: {}",
            truncate(prompt, 80)
        )]),
        AgentEvent::ModelChanged { model, provider, .. } => {
            match provider.as_deref() {
                Some(p) if !p.is_empty() => {
                    Rendered::Lines(vec![format!("— model changed: {model} ({p})")])
                }
                _ => Rendered::Lines(vec![format!("— model changed: {model}")]),
            }
        }
        AgentEvent::MemoryRecalled { hits } => {
            let parts: Vec<String> = hits
                .iter()
                .map(|h| format!("{}: {}", h.tier, h.title))
                .collect();
            Rendered::Lines(vec![format!(
                "⟡ memory recalled ({}): {}",
                hits.len(),
                if parts.is_empty() {
                    "-".to_string()
                } else {
                    parts.join("; ")
                }
            )])
        }
        AgentEvent::UserQuestion {
            question,
            options,
            ..
        } => {
            let mut lines = vec![format!("? {question}")];
            for (i, opt) in options.iter().enumerate() {
                match &opt.description {
                    Some(d) => lines.push(format!("  {}. {} — {}", i + 1, opt.label, truncate(d, 80))),
                    None => lines.push(format!("  {}. {}", i + 1, opt.label)),
                }
            }
            lines.push("  answer with a number or free text".to_string());
            Rendered::Lines(lines)
        }
        AgentEvent::ApprovalRequest {
            tool_name,
            args,
            core_operation,
            ..
        } => {
            let mut lines = vec![format!(
                "⚠ approval needed: {tool_name}({})",
                summarize_args(args)
            )];
            if *core_operation {
                lines.push("  core operation (merge/push always prompts)".to_string());
            }
            lines.push("  approve? [y]es / [n]o / [a]ll-no".to_string());
            Rendered::Lines(lines)
        }
        AgentEvent::Finished { .. } => Rendered::Lines(vec![String::new()]),
        // Phase transitions are UI state, not console text — silent (the
        // console REPL has no inflight bar to update).
        AgentEvent::Phase { .. } => Rendered::Silent,
        AgentEvent::Exited => Rendered::Lines(vec!["— agent exited".to_string()]),
        AgentEvent::ChildFinished {
            child_id,
            name,
            success,
        } => Rendered::Lines(vec![format!(
            "— background agent {name} ({child_id}) finished ({})",
            if *success { "ok" } else { "failed" }
        )]),
        AgentEvent::Error { error, retrying } => Rendered::Lines(vec![format!(
            "! {}{}",
            if *retrying { "(retrying) " } else { "" },
            truncate(error, 300)
        )]),
        AgentEvent::Compacted { before, after } => Rendered::Lines(vec![format!(
            "— context compacted: {before} → {after}"
        )]),
        AgentEvent::CompactStarted => Rendered::Lines(vec!["— compacting context…".to_string()]),
        AgentEvent::VisionDescribe {
            index,
            total,
            query,
        } => Rendered::Lines(vec![format!(
            "— describing image {index}/{total} via vision model… ({})",
            truncate(query, 120)
        )]),
        AgentEvent::VisionDescribed {
            index,
            total,
            success,
            description,
        } => {
            let mark = if *success {
                "described"
            } else {
                "description failed"
            };
            Rendered::Lines(vec![format!(
                "— image {index}/{total} {mark}: {}",
                truncate(description, 160)
            )])
        }
    }
}

/// Render a tool result: the first line of the output (success keeps it
/// informational; failure shows the text so the user sees WHY).
fn render_tool_result(result: &ToolResult) -> Vec<String> {
    let first_line = result
        .output
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("");
    let mark = if result.success { "·" } else { "✗" };
    vec![format!("  {mark} {}", truncate(first_line, 140))]
}

/// The console's view of the current model selection — what `/model` and
/// `/reasoning` print when called without arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelStatus {
    /// The endpoint currently serving the live provider.
    pub endpoint: String,
    /// The model id currently serving the live provider.
    pub model: String,
    /// The effective reasoning effort (`None` = the request field is omitted).
    pub effective_effort: Option<String>,
    /// Whether the endpoint accepts `reasoning_effort` at all.
    pub supports_reasoning_effort: bool,
}

/// Render the `/model` listing: every configured endpoint with its models,
/// marking the global default and the current selection, plus a usage hint.
pub(crate) fn render_model_list(
    endpoints: &[(String, Vec<String>)],
    current: Option<&ModelStatus>,
    default_model: Option<&str>,
) -> Vec<String> {
    let mut lines = vec!["endpoints:".to_string()];
    for (name, models) in endpoints {
        lines.push(format!("  {name}"));
        if models.is_empty() {
            lines.push("    (no models configured)".to_string());
        }
        for m in models {
            let mut tags = Vec::new();
            if Some(m.as_str()) == default_model {
                tags.push("default");
            }
            if let Some(cur) = current {
                if cur.endpoint == *name && cur.model == *m {
                    tags.push("current");
                }
            }
            let tag = if tags.is_empty() {
                String::new()
            } else {
                format!("  ← {}", tags.join(", "))
            };
            lines.push(format!("    {m}{tag}"));
        }
    }
    lines.push("usage: /model <endpoint>/<model>  (or /model <model>)".to_string());
    lines
}

/// Render the `/reasoning` status: the current effective effort, the value
/// list, and a note when the endpoint rejects the field entirely.
pub(crate) fn render_reasoning_status(status: &ModelStatus) -> Vec<String> {
    if !status.supports_reasoning_effort {
        return vec![
            format!(
                "reasoning: not supported by endpoint '{}' — the effort field is never sent",
                status.endpoint
            ),
            "the /reasoning command cannot change this".to_string(),
        ];
    }
    vec![
        format!(
            "reasoning: {} ({})",
            status
                .effective_effort
                .as_deref()
                .unwrap_or("off — field omitted"),
            if status.effective_effort.is_some() {
                "sent with every request"
            } else {
                "no reasoning_effort field"
            }
        ),
        format!("values: {}", REASONING_EFFORTS.join(", ")),
        "usage: /reasoning <value>".to_string(),
    ]
}

/// The `/help` text — mirrors the REPL's actual surface (only the picker
/// affordances + REPL mechanics).
pub(crate) fn help_text() -> Vec<String> {
    vec![
        "commands:".to_string(),
        "  /model                list endpoints + models".to_string(),
        "  /model <ep>/<model>   switch model (also /model <model>)".to_string(),
        "  /reasoning            show the current reasoning effort".to_string(),
        "  /reasoning <value>    set effort: max|high|medium|low|minimal|off".to_string(),
        "  /help                 this text".to_string(),
        "  /exit                 quit (also /quit)".to_string(),
        "anything else is sent to the agent as a prompt.".to_string(),
    ]
}

// ---------------------------------------------------------------------------
// Headless core — ConsoleRuntime + ConsoleSpawner
// ---------------------------------------------------------------------------

/// The console's tracked model selection — the endpoint + model the live
/// provider was last built from, plus the reasoning effort baked into it.
/// Initialized from the startup build (exactly mirroring `build_brain`'s
/// provider construction in main.rs) and updated by each `/model` +
/// `/reasoning` swap.
///
/// The provider itself doesn't expose its reasoning effort through the
/// `LlmClient` trait, so the console tracks the selection here — the only
/// mutation paths are startup and `ConsoleRuntime::swap_model`, which keeps
/// the tracked value in sync with the live provider by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelSelection {
    /// The endpoint the provider was built from.
    endpoint: String,
    /// The model id the provider was built from.
    model: String,
    /// The effective reasoning effort (`None` = the request field is omitted).
    effort: Option<String>,
    /// Whether the endpoint accepts `reasoning_effort` at all.
    supports: bool,
    /// The raw picker value the user last requested (`max`/`high`/`medium`/
    /// `low`/`minimal`/`off`), carried through model swaps like the status
    /// bar's toolbar value. `None` = no explicit override (use the target
    /// endpoint's configured default).
    requested_effort: Option<String>,
}

/// Derive the startup [`ModelSelection`] from config — the endpoint/model
/// fallback chain delegates to
/// [`crate::startup::resolve_startup_provider`] (the same tested helper
/// `build_brain` uses), so the console's initial selection matches the
/// app-startup provider by construction; this adds the console's effort +
/// capability tracking on top. `None` ONLY when no endpoint exists at all
/// (the one case where `build_brain` also falls back to the dummy
/// provider).
fn initial_selection(config: &Config) -> Option<ModelSelection> {
    let (ep, model) = crate::startup::resolve_startup_provider(config);
    let ep = ep?;
    let effort = ep.effective_reasoning_effort_for(Some(&model));
    Some(ModelSelection {
        endpoint: ep.name.clone(),
        model,
        effort,
        supports: ep.supports_reasoning_effort,
        requested_effort: None,
    })
}

/// Map the current selection to the reasoning-effort value sent on a model
/// swap — the status-bar picker's exact rules (StatusBar.tsx `selectModel`):
///
/// - target endpoint unsupported → `"off"` (the field is never sent);
/// - leaving an unsupported endpoint → `None` (adopt the target's
///   configured default — "off" must not stick forever);
/// - supported → supported → keep the current requested value (`None` = the
///   target's configured default; `"off"` passes through and omits the
///   field, mirroring the toolbar).
fn model_swap_effort(sel: Option<&ModelSelection>, target_supports: bool) -> Option<String> {
    if !target_supports {
        return Some("off".to_string());
    }
    let sel = sel?;
    if !sel.supports {
        return None;
    }
    sel.requested_effort.clone()
}

/// The console's [`AgentSpawner`] — the AppHandle-free twin of
/// [`crate::ipc::spawn::IpcSpawner`]. Delegates to the same shared spawn
/// path (`spawn_agent_shared`) so a console-spawned background agent is
/// indistinguishable from a GUI one (own workflow, manager registration,
/// loop-map tracking). Two GUI-only niceties are skipped: no
/// `PromptDispatched` event (it exists purely to render the task as the
/// first message in a UI transcript) and no `ChildFinished` emit (a
/// sidebar-only marker) — the parent completion Suggestion IS sent, by the
/// console's [`LifecycleBookkeeping`], with the same text the GUI uses.
pub(crate) struct ConsoleSpawner {
    factory: Arc<AgentLoopFactory>,
    manager: Arc<tokio::sync::Mutex<AgentManager>>,
    agent_loops: AgentLoopMap,
}

impl ConsoleSpawner {
    /// Create a spawner over the shared runtime pieces.
    pub(crate) fn new(
        factory: Arc<AgentLoopFactory>,
        manager: Arc<tokio::sync::Mutex<AgentManager>>,
        agent_loops: AgentLoopMap,
    ) -> Self {
        Self {
            factory,
            manager,
            agent_loops,
        }
    }
}

#[async_trait::async_trait]
impl AgentSpawner for ConsoleSpawner {
    /// Spawn a background agent with no parent and no forced model —
    /// delegates to `spawn_with_parent` with both as `None` (mirrors
    /// `IpcSpawner`).
    async fn spawn(&self, name: &str, task: &str, role: Option<String>) -> Result<AgentId, String> {
        self.spawn_with_parent(name, task, None, None, role).await
    }

    /// This spawner is parent-aware (tool-spawned agents record their
    /// parent, closing the completion-notification loop).
    fn parent_aware(&self) -> Option<&dyn ParentAwareSpawner> {
        Some(self)
    }
}

#[async_trait::async_trait]
impl ParentAwareSpawner for ConsoleSpawner {
    /// Spawn a background agent on behalf of the given parent (the agent
    /// whose `spawn_agent` tool call triggered this). Same semantics as
    /// `IpcSpawner::spawn_with_parent` minus the UI-only event emit.
    async fn spawn_with_parent(
        &self,
        name: &str,
        task: &str,
        parent_id: Option<AgentId>,
        model: Option<ModelRef>,
        role: Option<String>,
    ) -> Result<AgentId, String> {
        let (id, _) = crate::ipc::spawn::spawn_agent_shared(
            self.factory.clone(),
            self.manager.clone(),
            self.agent_loops.clone(),
            name.to_string(),
            Some(task.to_string()),
            parent_id,
            model,
            role,
            false, // own_plans_dir — subagents share the main dir (can't mutate)
            None, // no worktree binding — subagents inherit the parent's root
        )
        .await?;
        Ok(id)
    }
}

#[async_trait::async_trait]
impl DescendantTracker for ConsoleSpawner {
    /// Whether the agent has any running spawned descendants — delegates to
    /// the manager's `parent_id`-chain walk, same as the IPC twin.
    async fn has_running_descendants(&self, agent_id: AgentId) -> bool {
        let mgr = self.manager.lock().await;
        mgr.has_running_descendants(agent_id)
    }
}

/// Console-side lifecycle bookkeeping — the console's equivalent of the GUI
/// event forwarder's manager maintenance, minus the UI emits. The REPL calls
/// one of these for EVERY agent event, exactly where the forwarder would:
///
/// - [`on_started`](Self::on_started) / [`on_turn_ended`](Self::on_turn_ended)
///   keep `AgentHandle.running` current. Those flags drive two security-
///   relevant behaviors that silently break without maintenance: the
///   workflow state-transition gate ("no plan mutations while subagents
///   run", via [`DescendantTracker`]) and
///   `cleanup_inactive_subagents` (which must never Cancel a RUNNING child).
/// - [`on_turn_ended`](Self::on_turn_ended) also sends the child's parent the
///   completion-notification `Suggestion` (once per child, with the child's
///   `last_review_report` path — the same text the GUI forwarder composes),
///   closing the orchestrator feedback loop.
/// - [`on_exited`](Self::on_exited) removes the dead agent from the manager
///   and the per-agent loop map, so both stay bounded over a long session.
struct LifecycleBookkeeping {
    manager: Arc<tokio::sync::Mutex<AgentManager>>,
    agent_loops: AgentLoopMap,
    /// Children already notified (first terminal event wins — a multi-turn
    /// child must not re-notify its parent).
    notified_children: std::sync::Mutex<std::collections::HashSet<AgentId>>,
}

impl LifecycleBookkeeping {
    /// Create bookkeeping over the shared manager + loop map.
    fn new(manager: Arc<tokio::sync::Mutex<AgentManager>>, agent_loops: AgentLoopMap) -> Self {
        Self {
            manager,
            agent_loops,
            notified_children: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// The agent started a turn — mark it running.
    pub(crate) async fn on_started(&self, id: AgentId) {
        self.manager.lock().await.set_running(id, true);
    }

    /// The agent's turn ended (a `Finished` or a final non-retrying `Error`)
    /// — mark it idle and, for a tool-spawned child, notify its parent once.
    /// `success` mirrors the GUI forwarder: `true` for `Finished`, `false`
    /// for a final error (drives the failed-reviewer protocol).
    pub(crate) async fn on_turn_ended(&self, id: AgentId, success: bool) {
        {
            let mgr = self.manager.lock().await;
            mgr.set_running(id, false);
        }
        // Notify at most once per child (mirrors the forwarder's
        // `notified_children` set — the insert decides, before the
        // parent-existence check, exactly like `events.rs`).
        if self
            .notified_children
            .lock()
            .expect("notified_children lock poisoned")
            .insert(id)
        {
            self.notify_parent(id, success).await;
        }
    }

    /// The agent's task truly terminated — mark it idle and remove it from
    /// the manager + the per-agent loop map (and from the notified set, so
    /// neither grows without bound over a long session).
    pub(crate) async fn on_exited(&self, id: AgentId) {
        {
            let mut mgr = self.manager.lock().await;
            mgr.set_running(id, false);
            mgr.remove(id);
        }
        self.agent_loops.lock().await.remove(&id);
        self.notified_children
            .lock()
            .expect("notified_children lock poisoned")
            .remove(&id);
    }

    /// Send the child's parent the completion-notification `Suggestion`,
    /// including the child's last review-report path when it wrote one — the
    /// same composition as the GUI's `notify_parent_on_completion` (minus
    /// the sidebar-only `ChildFinished` emit), including the failed-reviewer
    /// protocol: a `role: "reviewer"` child that ended (failed OR finished
    /// cleanly) with no report sets the parent's reviewer-failure latch and
    /// gets the ask-the-user text instead of the generic no-report steer.
    /// Best-effort: a full inbox or a gone parent drops the notification
    /// silently (a nicety, not a guarantee — same as the GUI).
    async fn notify_parent(&self, id: AgentId, success: bool) {
        let (parent_id, child_name, child_role) = {
            let mgr = self.manager.lock().await;
            let Some(pid) = mgr.parent_id(id) else {
                return; // no parent (the main agent / a UI-spawned agent)
            };
            let name = mgr
                .get(id)
                .map(|h| h.name.clone())
                .unwrap_or_else(|| format!("agent-{id}"));
            let role = mgr.role(id);
            (pid, name, role)
        };
        let report_path: Option<String> = self
            .agent_loops
            .lock()
            .await
            .get(&id)
            .map(|l| l.last_review_report())
            .flatten();
        // Backlog 5b46674d: no !success gate — a reviewer that FINISHES
        // report-less is just as report-less as a failed one (the review did
        // not happen either way).
        let reviewer_failed_no_report =
            child_role.as_deref() == Some("reviewer") && report_path.is_none();
        if reviewer_failed_no_report {
            if let Some(parent_loop) = {
                let loops = self.agent_loops.lock().await;
                loops.get(&parent_id).cloned()
            } {
                parent_loop.set_reviewer_failure_pending(true);
            }
        }
        let text = if reviewer_failed_no_report {
            mnemo::runtime::turn_resolve::reviewer_failure_suggestion_text(&child_name)
        } else {
            let outcome = if success { "finished" } else { "failed" };
            mnemo::runtime::turn_resolve::completion_suggestion_text(
                &child_name,
                outcome,
                report_path.as_deref(),
            )
        };
        let mgr = self.manager.lock().await;
        let _ = mgr.send(parent_id, AgentCommand::Suggestion(text.into()));
    }
}

/// The headless core of console mode: the brain + agent manager + per-agent
/// loop map + the fan-in event receiver, with typed operations.
///
/// This is the seam a future headless daemon reuses — nothing here reads
/// stdin, writes to stdout, or touches a Tauri `AppHandle`. The REPL
/// (`run_console`) is a thin interactive frontend over these operations; a
/// daemon would drive the same ops from a socket/stdin-less loop instead.
pub(crate) struct ConsoleRuntime {
    /// The whole brain, held for the process lifetime so its Drop-managed
    /// pieces (the CodeGraph file watcher) stay alive — mirroring the GUI,
    /// which keeps the same Arcs alive inside its managed `IpcState`.
    _brain: crate::Brain,
    factory: Arc<AgentLoopFactory>,
    manager: Arc<tokio::sync::Mutex<AgentManager>>,
    agent_loops: AgentLoopMap,
    config: Arc<tokio::sync::Mutex<Config>>,
    project: Arc<tokio::sync::Mutex<mnemo::project::Project>>,
    trace: Arc<LlmRequestLog>,
    browser: Arc<mnemo::browser::BrowserManager>,
    fanin_rx: tokio::sync::mpsc::Receiver<(AgentId, AgentEvent)>,
    main_id: AgentId,
    /// The tracked model selection (see [`ModelSelection`]). Std mutex —
    /// never held across an await.
    selection: std::sync::Mutex<Option<ModelSelection>>,
    /// Lifecycle bookkeeping (running flags, parent notifications, dead-agent
    /// cleanup) — the console's forwarder-equivalent. See
    /// [`LifecycleBookkeeping`].
    bookkeeping: LifecycleBookkeeping,
}

impl ConsoleRuntime {
    /// Build the runtime from a ready brain: create the manager, wire the
    /// [`ConsoleSpawner`] into the factory, and spawn the main agent via the
    /// same shared path as the GUI's main agent (id allocation, channel
    /// setup, loop-map insert, and registration are identical).
    pub(crate) async fn start(brain: crate::Brain) -> anyhow::Result<Self> {
        let mut mgr = AgentManager::new(256);
        // Take the fan-in receiver before wrapping the manager (the runtime
        // owns it, like the GUI's event forwarder does).
        let fanin_rx = mgr.take_fanin_rx().expect("fan-in receiver already taken");
        let manager = Arc::new(tokio::sync::Mutex::new(mgr));
        let agent_loops: AgentLoopMap =
            Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

        // Wire the spawn_agent tool's spawner BEFORE building the main agent
        // (the tool is registered per build) — same ordering as the GUI
        // setup. One ConsoleSpawner serves as both AgentSpawner and
        // DescendantTracker.
        let spawner = Arc::new(ConsoleSpawner::new(
            brain.factory.clone(),
            manager.clone(),
            agent_loops.clone(),
        ));
        brain.factory.set_spawner(spawner.clone());
        brain.factory.set_descendant_tracker(spawner);

        // The main agent — identical args to the GUI's main-agent spawn in
        // main.rs setup: no initial prompt, no parent, no forced model, no
        // role, and the shared main plans dir.
        let (main_id, _) = crate::ipc::spawn::spawn_agent_shared(
            brain.factory.clone(),
            manager.clone(),
            agent_loops.clone(),
            "main".to_string(),
            None,
            None,
            None,
            None,
            false,
            None, // no worktree binding — the main agent works the main tree
        )
        .await
        .map_err(|e| anyhow::anyhow!("main agent spawn failed: {e}"))?;

        let selection = {
            let config = brain.config.lock().await;
            initial_selection(&config)
        };

        // Clone the shared handles out of the brain BEFORE moving the brain
        // itself into `_brain` (its Drop-managed pieces must stay alive for
        // the process lifetime).
        let factory = brain.factory.clone();
        let config = brain.config.clone();
        let project = brain.project.clone();
        let trace = brain.trace.clone();
        let browser = brain.browser.clone();

        // The bookkeeping struct shares the manager + loop map (cloned before
        // they move into the runtime's own fields).
        let bookkeeping = LifecycleBookkeeping::new(manager.clone(), agent_loops.clone());

        Ok(Self {
            _brain: brain,
            factory,
            manager,
            agent_loops,
            config,
            project,
            trace,
            browser,
            fanin_rx,
            main_id,
            selection: std::sync::Mutex::new(selection),
            bookkeeping,
        })
    }

    /// The resolved project root (shown in the console banner).
    pub(crate) async fn project_root(&self) -> std::path::PathBuf {
        self.project.lock().await.root.clone()
    }

    /// Read the next fanned-in agent event (`None` when all senders closed).
    pub(crate) async fn next_event(&mut self) -> Option<(AgentId, AgentEvent)> {
        self.fanin_rx.recv().await
    }

    /// The main agent's id (the agent prompts are sent to).
    pub(crate) fn main_id(&self) -> AgentId {
        self.main_id
    }

    /// The shared headless browser (for bounded teardown on exit).
    pub(crate) fn browser(&self) -> Arc<mnemo::browser::BrowserManager> {
        self.browser.clone()
    }

    /// The shared LLM request trace log (flushed on exit).
    pub(crate) fn trace(&self) -> Arc<LlmRequestLog> {
        self.trace.clone()
    }

    /// Send a prompt to the main agent.
    pub(crate) async fn send_prompt(&self, text: String) -> Result<(), String> {
        let mgr = self.manager.lock().await;
        mgr.send(
            self.main_id,
            AgentCommand::Prompt {
                text,
                images: vec![],
            },
        )
        .map_err(|_| "failed to send prompt — agent not running".to_string())
    }

    /// The tracked selection as a display status for `/model` + `/reasoning`.
    pub(crate) fn status(&self) -> Option<ModelStatus> {
        self.selection
            .lock()
            .expect("selection lock poisoned")
            .as_ref()
            .map(|s| ModelStatus {
                endpoint: s.endpoint.clone(),
                model: s.model.clone(),
                effective_effort: s.effort.clone(),
                supports_reasoning_effort: s.supports,
            })
    }

    /// The `/model` listing inputs: `(endpoint, models)` pairs in config
    /// order plus the global default model id.
    pub(crate) async fn endpoint_listing(&self) -> (Vec<(String, Vec<String>)>, Option<String>) {
        let config = self.config.lock().await;
        let endpoints = config
            .endpoints
            .iter()
            .map(|e| (e.name.clone(), e.model_ids()))
            .collect();
        (endpoints, config.general.general.default_model.clone())
    }

    /// Swap the live model and/or reasoning effort — the exact semantics of
    /// the IPC `set_model` command (status-bar picker): validate + build via
    /// the shared resolver, swap into the factory + every live loop via the
    /// shared swap, and update the tracked selection. Live swap only — the
    /// on-disk config is NOT written (same as the picker).
    ///
    /// Returns the new [`ModelStatus`] for display.
    pub(crate) async fn swap_model(
        &self,
        endpoint_name: &str,
        model: &str,
        effort: Option<&str>,
    ) -> Result<ModelStatus, String> {
        let resolved = {
            let config = self.config.lock().await;
            let resolved = crate::ipc::config_io::resolve_model_provider(
                &config,
                endpoint_name,
                model,
                effort,
                &self.trace,
            )
            .map_err(|e| e.message)?;
            let supports = config
                .endpoint(endpoint_name)
                .map(|e| e.supports_reasoning_effort)
                .unwrap_or(true);
            // Track the swap BEFORE releasing the config lock so the
            // selection and the resolver read agree on the endpoint. The
            // raw requested value is recorded too (it is carried through
            // model swaps like the toolbar's value).
            *self.selection.lock().expect("selection lock poisoned") = Some(ModelSelection {
                endpoint: endpoint_name.to_string(),
                model: model.to_string(),
                effort: resolved.effective_effort.clone(),
                supports,
                requested_effort: effort.map(str::to_string),
            });
            resolved
        };
        // Rebuild the context manager from the new provider's window (same
        // as set_model) and swap factory + every live loop. The resolved
        // DISPLAY effort rides along so the loops' status-bar mirrors track
        // the new default (backlog 51dab4da).
        let context_manager = self.factory.context_manager_for(resolved.provider.as_ref());
        crate::ipc::config_io::swap_provider_into_loops(
            &self.factory,
            &self.agent_loops,
            resolved.provider,
            context_manager,
            Some(resolved.display_effort),
        )
        .await;
        Ok(self.status().expect("selection was just set"))
    }

    /// Resolve a bare model id to the endpoint that serves it — the `/model
    /// <model>` shorthand. Errors when the model is listed nowhere (with a
    /// pointer to the listing) or at several endpoints (ambiguous — the
    /// `endpoint/model` form is required).
    pub(crate) async fn find_endpoint_for_model(&self, model: &str) -> Result<String, String> {
        let config = self.config.lock().await;
        let matches: Vec<&str> = config
            .endpoints
            .iter()
            .filter(|e| e.has_model(model))
            .map(|e| e.name.as_str())
            .collect();
        match matches.as_slice() {
            [] => Err(format!(
                "model '{model}' is not listed at any endpoint — /model shows what's available"
            )),
            [only] => Ok((*only).to_string()),
            many => Err(format!(
                "model '{model}' is listed at multiple endpoints ({}) — use /model <endpoint>/<model>",
                many.join(", ")
            )),
        }
    }

    /// Cancel every registered agent so their tasks exit (the REPL then
    /// waits, bounded, for the main agent's `Exited` event). Errors are
    /// ignored — an agent that is already gone needs no cancel.
    pub(crate) async fn request_shutdown(&self) {
        let mgr = self.manager.lock().await;
        for handle in mgr.list() {
            let _ = mgr.send(handle.id, AgentCommand::Cancel);
        }
    }

    /// Lifecycle bookkeeping: the agent started a turn.
    pub(crate) async fn mark_started(&self, id: AgentId) {
        self.bookkeeping.on_started(id).await;
    }

    /// Lifecycle bookkeeping: the agent's turn ended (Finished / final
    /// Error) — also notifies a child's parent once. `success` is `true`
    /// for a clean `Finished`, `false` for a final error.
    pub(crate) async fn mark_turn_ended(&self, id: AgentId, success: bool) {
        self.bookkeeping.on_turn_ended(id, success).await;
    }

    /// Lifecycle bookkeeping: the agent's task terminated — removes it from
    /// the manager + loop map.
    pub(crate) async fn mark_exited(&self, id: AgentId) {
        self.bookkeeping.on_exited(id).await;
    }

    /// Cancel every INACTIVE subagent (a fresh plan from Complete) — the
    /// forwarder's Complete→Executing cleanup, shared via the same helper.
    pub(crate) async fn cleanup_inactive_subagents(&self, trigger: AgentId) {
        crate::ipc::events::cleanup_inactive_subagents(&self.manager, trigger).await;
    }

    /// The reasoning-effort value a `/model <endpoint>/<model>` swap should
    /// send, mirroring the status-bar picker's rules against the current
    /// selection and the target endpoint's capability flag (see
    /// [`model_swap_effort`]).
    pub(crate) async fn effort_for_model_swap(&self, target_endpoint: &str) -> Option<String> {
        let target_supports = {
            let config = self.config.lock().await;
            config
                .endpoint(target_endpoint)
                .map(|e| e.supports_reasoning_effort)
                .unwrap_or(true)
        };
        let sel = self.selection.lock().expect("selection lock poisoned");
        model_swap_effort(sel.as_ref(), target_supports)
    }
}

// ---------------------------------------------------------------------------
// Interactive frontend — the REPL
// ---------------------------------------------------------------------------

/// Mutable REPL loop state — one pending interaction at a time (matching the
/// GUI's one-question-at-a-time contract), streaming-channel tracking for
/// fragment/line boundaries, and the exit handshake.
#[derive(Default)]
struct ReplState {
    /// The unresolved approval's oneshot (the agent blocks on it).
    pending_approval: Option<tokio::sync::oneshot::Sender<Approval>>,
    /// The unresolved question's oneshot + its options (for answer parsing).
    pending_question: Option<(
        tokio::sync::oneshot::Sender<UserAnswer>,
        Vec<QuestionOption>,
    )>,
    /// The channel of the last printed fragment (`Some` = mid-line).
    last_channel: Option<StreamChannel>,
    /// Whether the main agent is mid-turn (suppresses the prompt marker).
    running: bool,
    /// The main agent's task exited (the exit handshake can finish).
    main_exited: bool,
    /// `/exit` or stdin-EOF was seen — cancelling + draining, then leave.
    exiting: bool,
    /// Hard bound on the exit drain (the agent may be wedged mid-LLM-call).
    exit_deadline: Option<tokio::time::Instant>,
    /// stdin hit EOF — stop polling the input branch entirely (a closed
    /// mpsc is ALWAYS ready, so re-polling it would busy-spin a core).
    stdin_closed: bool,
    /// Each agent's last observed workflow state, for the Complete→Executing
    /// transition that triggers inactive-subagent cleanup (mirrors the GUI
    /// forwarder's `prev_workflow_state`).
    prev_workflow_state: std::collections::HashMap<AgentId, mnemo::workflow::WorkflowState>,
}

/// Print the idle prompt marker (`> `) and flush — called at startup and
/// after each main-agent turn ends.
fn print_prompt() {
    use std::io::Write;
    print!("> ");
    let _ = std::io::stdout().flush();
}

/// Print a [`Rendered`] event with fragment/line boundary handling: a line
/// after fragments first terminates the in-flight fragment line, and a
/// fragment on a NEW channel first breaks the previous channel's line (so
/// reasoning and assistant text never concatenate).
fn print_rendered(rendered: Rendered, state: &mut ReplState) {
    use std::io::Write;
    match rendered {
        Rendered::Silent => {}
        Rendered::Fragment { channel, text } => {
            if state.last_channel.is_some() && state.last_channel != Some(channel) {
                println!();
            }
            state.last_channel = Some(channel);
            print!("{text}");
            let _ = std::io::stdout().flush();
        }
        Rendered::Lines(lines) => {
            if state.last_channel.is_some() {
                println!();
                state.last_channel = None;
            }
            for line in lines {
                println!("{line}");
            }
        }
    }
}

/// Print the outcome of a `/model` + `/reasoning` swap.
fn print_swap_result(result: Result<ModelStatus, String>) {
    match result {
        Ok(s) => {
            let effort = s.effective_effort.as_deref().unwrap_or("off");
            let note = if s.supports_reasoning_effort {
                ""
            } else {
                " (endpoint rejects the field — never sent)"
            };
            println!(
                "— model set: {}/{} (reasoning: {effort}{note})",
                s.endpoint, s.model
            );
        }
        Err(e) => println!("! {e}"),
    }
}

/// Handle one fanned-in agent event: render it, extract the interactive
/// oneshots (approval/question), and drive the lifecycle bookkeeping the GUI
/// forwarder performs (running flags, parent completion notifications,
/// dead-agent cleanup, Complete→Executing inactive-subagent cleanup).
async fn handle_event(
    id: AgentId,
    event: AgentEvent,
    state: &mut ReplState,
    runtime: &ConsoleRuntime,
) {
    print_rendered(render_event(&event), state);
    match event {
        AgentEvent::ApprovalRequest { responder, .. } => {
            if state.pending_approval.is_some() {
                // Sequential REPL: a second approval while one is pending is
                // denied immediately (fail-closed, like the GUI's auto-deny
                // paths) rather than silently queued behind the first.
                let _ = responder.send(Approval::Deny);
                println!("⚠ second approval arrived while one is pending — denied");
            } else {
                state.pending_approval = Some(responder);
            }
        }
        AgentEvent::UserQuestion {
            options, responder, ..
        } => {
            if state.pending_question.is_some() {
                // Can't normally happen (each agent blocks on its own
                // question); if a subagent races one in, answer it with a
                // clear deferral instead of dropping the oneshot silently.
                let _ = responder.send(UserAnswer::Freeform {
                    text: "(console: superseded by another question)".to_string(),
                });
                println!("⚠ second question arrived while one is pending — deferred");
            } else {
                state.pending_question = Some((responder, options));
            }
        }
        AgentEvent::WorkflowStateChanged {
            state: new_state, ..
        } => {
            // Complete → Executing = a fresh plan started — cancel INACTIVE
            // subagents from the previous plan (running ones are left
            // alone), mirroring the GUI forwarder.
            let was_complete = state.prev_workflow_state.get(&id)
                == Some(&mnemo::workflow::WorkflowState::Complete);
            state.prev_workflow_state.insert(id, new_state);
            if was_complete && new_state == mnemo::workflow::WorkflowState::Executing {
                runtime.cleanup_inactive_subagents(id).await;
            }
        }
        AgentEvent::Started => {
            runtime.mark_started(id).await;
            if id == runtime.main_id() {
                state.running = true;
            }
        }
        AgentEvent::Finished { .. } => {
            runtime.mark_turn_ended(id, true).await;
            if id == runtime.main_id() && state.running {
                // Guarded: a final Error already ended the turn (the agent
                // then also emits a bookkeeping Finished) — one prompt only.
                state.running = false;
                if !state.exiting {
                    print_prompt();
                }
            }
        }
        AgentEvent::Error {
            retrying: false, ..
        } => {
            runtime.mark_turn_ended(id, false).await;
            if id == runtime.main_id() && state.running {
                state.running = false;
                if !state.exiting {
                    print_prompt();
                }
            }
        }
        AgentEvent::Exited => {
            runtime.mark_exited(id).await;
            if id == runtime.main_id() {
                state.main_exited = true;
            }
        }
        // Retrying errors keep the agent running; delta events need no
        // bookkeeping at all.
        _ => {}
    }
}

/// Handle one line of stdin — a pending approval/question answer, a slash
/// command, or a prompt for the agent.
async fn handle_input(line: String, state: &mut ReplState, runtime: &ConsoleRuntime) {
    // `/exit` + `/help` always work, even while an approval/question is
    // pending: the agent's approval gate honors the exit Cancel (see the
    // approval-gate select! in the lib), and help is read-only. Everything
    // else typed while an interaction is pending is treated as the answer —
    // sequential resolution, matching the GUI's one-at-a-time contract.
    let cmd = parse_command(&line);
    match cmd {
        ConsoleCommand::Exit => {
            begin_shutdown(state, runtime).await;
            return;
        }
        ConsoleCommand::Help => {
            for l in help_text() {
                println!("{l}");
            }
            return;
        }
        _ => {}
    }

    // Pending interactions take precedence: the next line answers them.
    if let Some(sender) = state.pending_approval.take() {
        match parse_approval_answer(&line) {
            Some(approval) => {
                let label = match approval {
                    Approval::Approve => "approved",
                    Approval::Deny => "denied",
                    Approval::DenyAll => "denied (this and the rest of the turn)",
                };
                let _ = sender.send(approval);
                println!("— {label}");
            }
            None => {
                state.pending_approval = Some(sender);
                println!("  answer with y / n / a");
            }
        }
        return;
    }
    if let Some((sender, options)) = state.pending_question.take() {
        match parse_question_answer(&line, &options) {
            Some(answer) => {
                let _ = sender.send(answer);
            }
            None => {
                state.pending_question = Some((sender, options));
                println!("  answer with a number or free text");
            }
        }
        return;
    }

    match cmd {
        ConsoleCommand::Prompt(text) => {
            if text.is_empty() {
                return;
            }
            if let Err(e) = runtime.send_prompt(text).await {
                println!("! {e}");
            }
        }
        ConsoleCommand::Model { query: None } => {
            let (endpoints, default) = runtime.endpoint_listing().await;
            for line in render_model_list(&endpoints, runtime.status().as_ref(), default.as_deref())
            {
                println!("{line}");
            }
        }
        ConsoleCommand::Model { query: Some(q) } => {
            // The effort sent on a model swap mirrors the status-bar
            // picker's rules (keep the current override, except when
            // crossing to/from an unsupported endpoint).
            match parse_model_query(&q) {
                Err(e) => println!("! {e}"),
                Ok((None, model)) => match runtime.find_endpoint_for_model(&model).await {
                    Err(e) => println!("! {e}"),
                    Ok(endpoint) => {
                        let effort = runtime.effort_for_model_swap(&endpoint).await;
                        print_swap_result(
                            runtime
                                .swap_model(&endpoint, &model, effort.as_deref())
                                .await,
                        )
                    }
                },
                Ok((Some(endpoint), model)) => {
                    let effort = runtime.effort_for_model_swap(&endpoint).await;
                    print_swap_result(
                        runtime
                            .swap_model(&endpoint, &model, effort.as_deref())
                            .await,
                    )
                }
            }
        }
        ConsoleCommand::Reasoning { value: None } => match runtime.status() {
            Some(status) => {
                for line in render_reasoning_status(&status) {
                    println!("{line}");
                }
            }
            None => println!("! no model selected — use /model first"),
        },
        ConsoleCommand::Reasoning { value: Some(v) } => {
            if let Err(e) = validate_effort(&v) {
                println!("! {e}");
                return;
            }
            let Some(sel) = runtime.status() else {
                println!("! no model selected — use /model first");
                return;
            };
            // "off" maps to None (omit the field) at the shared resolver;
            // every other value passes through verbatim — the same mapping
            // the IPC set_model command applies to the dropdown value.
            let effort = if v == "off" { None } else { Some(v.as_str()) };
            print_swap_result(runtime.swap_model(&sel.endpoint, &sel.model, effort).await);
        }
        ConsoleCommand::Help => unreachable!("handled above"),
        ConsoleCommand::Exit => unreachable!("handled above"),
        ConsoleCommand::Unknown(cmd) => {
            println!("! unknown command {cmd} — /help lists commands");
        }
    }
}

/// Start the exit handshake: cancel every agent, set the drain deadline.
async fn begin_shutdown(state: &mut ReplState, runtime: &ConsoleRuntime) {
    state.exiting = true;
    state.exit_deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_secs(5));
    runtime.request_shutdown().await;
    println!("— shutting down…");
}

/// Run the console REPL over the shared brain. Returns the process exit code
/// (0 on a clean `/exit`, 1 when the brain fails to start).
///
/// The REPL is deliberately thin: every operation goes through
/// [`ConsoleRuntime`], and every rendering decision through the pure
/// helpers above — a future headless daemon swaps this function for a
/// non-interactive loop over the same runtime.
pub(crate) async fn run_console() -> i32 {
    let brain = match crate::build_brain(None) {
        Ok(crate::BrainOutcome::Ready(brain)) => brain,
        Ok(crate::BrainOutcome::NeedsProject(_)) => {
            eprintln!(
                "mnemo console: no project resolved — run from a project directory, \
                 pass --project <path>, or open the GUI once to register one."
            );
            return 1;
        }
        Err(e) => {
            eprintln!("mnemo console: failed to start: {e:#}");
            return 1;
        }
    };

    // Installed bundled embedding model: build_brain deferred its ~110 MB
    // ONNX load off the main thread (AppHangB1 diagnostics) and left the
    // store on the hash embedder. Load + swap in the background — same path
    // the GUI's setup hook runs (review finding 1, 2026-08-20: without this
    // the console silently degraded to hash for the whole session).
    if let (Some(model), Some(store)) = (&brain.pending_model_load, &brain.memory_store) {
        crate::ipc::embeddings::spawn_startup_load_console(
            store.clone(),
            brain.embedder_status.clone(),
            model.clone(),
        );
    }

    // Managed Laya (the console twin of the GUI setup hook): start the
    // sidecar on the port the classifier client was built against. No
    // Tauri events in console mode — the status transitions still land in
    // the shared status for any GUI process reading it.
    if let Some((checkpoint_id, port)) = &brain.pending_laya_start {
        let manager = brain.laya.clone();
        let checkpoint = crate::ipc::laya::find_checkpoint(checkpoint_id);
        let port = *port;
        let no_app: Option<tauri::AppHandle> = None;
        tokio::spawn(async move {
            crate::ipc::laya::start_managed_server(&no_app, manager, checkpoint, port).await;
        });
    }

    let mut runtime = match ConsoleRuntime::start(brain).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("mnemo console: failed to start the agent: {e:#}");
            return 1;
        }
    };

    println!("mnemo console — {}", runtime.project_root().await.display());
    println!("type a prompt, or /help for commands. /exit quits.");
    print_prompt();

    // One stdin reader on the blocking pool → an mpsc the select loop reads.
    // EOF closes the channel (treated like /exit).
    let (input_tx, mut input_rx) = tokio::sync::mpsc::channel::<String>(8);
    tokio::task::spawn_blocking(move || {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            match line {
                Ok(l) => {
                    if input_tx.blocking_send(l).is_err() {
                        break; // receiver gone — the REPL is shutting down
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut state = ReplState::default();
    loop {
        // Exit handshake done: the main agent exited, or the drain deadline
        // passed (a wedged LLM call must not hang the console).
        if state.exiting
            && (state.main_exited
                || state
                    .exit_deadline
                    .is_some_and(|d| d <= tokio::time::Instant::now()))
        {
            break;
        }
        tokio::select! {
            maybe_ev = runtime.next_event() => {
                match maybe_ev {
                    None => break, // every agent gone
                    Some((id, event)) => handle_event(id, event, &mut state, &runtime).await,
                }
            }
            maybe_line = input_rx.recv(), if !state.stdin_closed => {
                match maybe_line {
                    None => {
                        // stdin closed (Ctrl+Z / EOF) — same as /exit. Mark
                        // it so the always-ready closed channel is never
                        // re-polled (that would busy-spin a core during the
                        // exit drain).
                        state.stdin_closed = true;
                        if !state.exiting {
                            begin_shutdown(&mut state, &runtime).await;
                            println!("— stdin closed");
                        }
                    }
                    Some(line) => handle_input(line, &mut state, &runtime).await,
                }
            }
            // While exiting, also wake on the deadline itself (covers the
            // case where no further events arrive at all).
            _ = tokio::time::sleep_until(
                state.exit_deadline
                    .unwrap_or_else(|| tokio::time::Instant::now() + std::time::Duration::from_secs(3600)),
            ), if state.exiting => {
                break;
            }
        }
    }

    // Bounded browser teardown, mirroring the GUI's RunEvent::Exit: a wedged
    // close must not hang the console; 10s covers the normal worst case
    // (Arc-unwrap retries + child kill), leaving only genuine wedges cut off.
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        runtime.browser().close(),
    )
    .await;
    // Drain the asynchronous trace mirror so the session's last requests land.
    runtime.trace().flush_file_writes();
    println!("bye.");
    0
}

/// Give this process a working console before the first output.
///
/// Release builds are GUI-subsystem binaries (`windows_subsystem = "windows"`
/// in main.rs) — launched from PowerShell they get no console of their own
/// and NULL std handles. An earlier approach used
/// `AttachConsole(ATTACH_PARENT_PROCESS)` to share the launching terminal,
/// then rewired stdio — that made prints visible, but because PowerShell does
/// not wait on GUI-subsystem children it kept owning the shared console and
/// stole keystrokes (typed input only partially echoed; the REPL was
/// unusable). Launched without any parent terminal (double-click / shortcut)
/// there is not even a console to attach to.
///
/// This runs FIRST in `main()`, before any print (Rust resolves its stdio
/// handles on first use and caches them):
///
/// 1. Skip when a console is already attached (debug builds are
///    console-subsystem binaries — they already own the launching terminal).
/// 2. Skip when all three standard handles are already valid — a fully
///    redirected launch (`mnemo --console > out.txt < in.txt 2> err.txt`, or
///    piped) has no console window and does not need one; opening
///    `AllocConsole` would flash a spurious empty window while output still
///    correctly goes to the redirect targets.
/// 3. `AllocConsole` — open a **fresh console window of our own**. Never
///    `AttachConsole` the parent: the REPL must own input so PowerShell/cmd
///    cannot split keystrokes with it. The cost is a separate console window
///    when launched from a terminal; the gain is a usable prompt.
/// 4. Rewire ABSENT standard handles to the console devices: open `CONIN$` /
///    `CONOUT$` and `SetStdHandle` them as stdin/stdout/stderr, so the REPL
///    both displays output and accepts typed input. Only handles that are
///    missing (NULL/invalid) are filled — a redirected launch keeps its
///    valid file handles (and step 2 already skipped the window when every
///    handle was already present).
///
/// `AllocConsole` failing leaves the handles untouched — prints stay dropped
/// (same as the pre-fix behavior), never a crash. Dependency-free raw FFI;
/// no winapi crate.
#[cfg(windows)]
pub(crate) fn attach_console() {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;

    #[link(name = "kernel32")]
    extern "system" {
        fn AllocConsole() -> i32;
        fn GetConsoleWindow() -> *mut c_void;
        fn GetStdHandle(n_std_handle: u32) -> *mut c_void;
        fn SetStdHandle(n_std_handle: u32, h_handle: *mut c_void) -> i32;
        fn CreateFileW(
            lp_file_name: *const u16,
            dw_desired_access: u32,
            dw_share_mode: u32,
            lp_security_attributes: *mut SECURITY_ATTRIBUTES,
            dw_creation_disposition: u32,
            dw_flags_and_attributes: u32,
            h_template_file: *mut c_void,
        ) -> *mut c_void;
    }

    #[repr(C)]
    struct SECURITY_ATTRIBUTES {
        n_length: u32,
        lp_security_descriptor: *mut c_void,
        b_inherit_handle: i32,
    }

    const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6; // (DWORD)-10
    const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // (DWORD)-11
    const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4; // (DWORD)-12
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const OPEN_EXISTING: u32 = 3;
    const INVALID_HANDLE_VALUE: *mut c_void = (-1isize) as *mut c_void;

    unsafe {
        // 1. Already attached to a console (debug builds, or a launcher that
        //    handed us a real one): the standard handles are already valid
        //    and we already own input — leave it alone.
        if GetConsoleWindow() != std::ptr::null_mut() {
            return;
        }
        let handle_is_absent = |id: u32| -> bool {
            let current = GetStdHandle(id);
            current.is_null() || current == INVALID_HANDLE_VALUE
        };
        // 2. Fully redirected / piped launch: every std handle already points
        //    at a real file/pipe/NUL. No console is needed and AllocConsole
        //    would only open a spurious empty window (LOW-1, 2026-09-04).
        if !handle_is_absent(STD_INPUT_HANDLE)
            && !handle_is_absent(STD_OUTPUT_HANDLE)
            && !handle_is_absent(STD_ERROR_HANDLE)
        {
            return;
        }
        // 3. Own console only. Do NOT AttachConsole(parent): a GUI-subsystem
        //    release binary launched from PowerShell/cmd shares that console
        //    while the shell keeps reading keystrokes, so typed input is
        //    split and the REPL is unusable. AllocConsole opens a window the
        //    REPL fully owns (shortcut launch already needed this path).
        if AllocConsole() == 0 {
            return; // could not open a console — leave the handles as-is
        }
        // 4. Rewire the standard handles to the console devices. Rust's
        //    stdio has not been resolved yet (this runs before any print),
        //    so the first stdin/stdout/stderr use picks these handles up.
        //    Only ABSENT handles are filled: a redirected launch
        //    (`mnemo --console > out.txt`) starts with valid file/pipe
        //    handles that must keep pointing at the redirect targets (the
        //    C-runtime convention). The interactive bug condition is the
        //    NULL-handle launch state PowerShell gives GUI-subsystem
        //    children, so the rewiring fires exactly where it is needed.
        let mut inheritable = SECURITY_ATTRIBUTES {
            n_length: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lp_security_descriptor: std::ptr::null_mut(),
            b_inherit_handle: 1, // children (the shell tool etc.) may inherit
        };
        let mut open_console_device = |name: &str, access: u32| -> *mut c_void {
            let wide: Vec<u16> = OsStr::new(name).encode_wide().chain([0]).collect();
            CreateFileW(
                wide.as_ptr(),
                access,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                &mut inheritable,
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        // CONIN$ is the input buffer; CONOUT$ the screen buffer. There is no
        // CONERR$ device — stderr shares CONOUT$ with stdout (the same
        // screen, what the C runtime does), so one handle serves both.
        let conin = open_console_device("CONIN$", GENERIC_READ | GENERIC_WRITE);
        let conout = open_console_device("CONOUT$", GENERIC_READ | GENERIC_WRITE);
        if !conin.is_null() && conin != INVALID_HANDLE_VALUE && handle_is_absent(STD_INPUT_HANDLE) {
            SetStdHandle(STD_INPUT_HANDLE, conin);
        }
        if !conout.is_null() && conout != INVALID_HANDLE_VALUE {
            if handle_is_absent(STD_OUTPUT_HANDLE) {
                SetStdHandle(STD_OUTPUT_HANDLE, conout);
            }
            if handle_is_absent(STD_ERROR_HANDLE) {
                SetStdHandle(STD_ERROR_HANDLE, conout);
            }
        }
    }
}

/// Non-Windows: console attachment is a no-op (processes always inherit the
/// terminal).
#[cfg(not(windows))]
pub(crate) fn attach_console() {}

#[cfg(test)]
mod tests {
    use super::*;

    use mnemo::runtime::channels::QuestionOption;

    // --- parse_command -------------------------------------------------

    #[test]
    fn parse_command_plain_text_is_prompt() {
        assert_eq!(
            parse_command("fix the login bug"),
            ConsoleCommand::Prompt("fix the login bug".to_string())
        );
        // Trimmed.
        assert_eq!(
            parse_command("  hi there  "),
            ConsoleCommand::Prompt("hi there".to_string())
        );
        // Empty line → empty prompt (the REPL skips it).
        assert_eq!(parse_command("   "), ConsoleCommand::Prompt(String::new()));
    }

    #[test]
    fn parse_command_slash_surface() {
        assert_eq!(
            parse_command("/model"),
            ConsoleCommand::Model { query: None }
        );
        assert_eq!(
            parse_command("/model openai/o3"),
            ConsoleCommand::Model {
                query: Some("openai/o3".to_string())
            }
        );
        assert_eq!(
            parse_command("/reasoning"),
            ConsoleCommand::Reasoning { value: None }
        );
        assert_eq!(
            parse_command("/reasoning off"),
            ConsoleCommand::Reasoning {
                value: Some("off".to_string())
            }
        );
        assert_eq!(parse_command("/help"), ConsoleCommand::Help);
        assert_eq!(parse_command("/?"), ConsoleCommand::Help);
        assert_eq!(parse_command("/exit"), ConsoleCommand::Exit);
        assert_eq!(parse_command("/quit"), ConsoleCommand::Exit);
    }

    #[test]
    fn parse_command_unknown_slash_is_unknown() {
        assert_eq!(
            parse_command("/compact"),
            ConsoleCommand::Unknown("/compact".to_string())
        );
        // With args the command WORD is reported, not the whole line.
        assert_eq!(
            parse_command("/nonsense arg here"),
            ConsoleCommand::Unknown("/nonsense".to_string())
        );
    }

    // --- parse_model_query ----------------------------------------------

    #[test]
    fn parse_model_query_endpoint_form() {
        assert_eq!(
            parse_model_query("openai/o3").unwrap(),
            (Some("openai".to_string()), "o3".to_string())
        );
    }

    #[test]
    fn parse_model_query_bare_model() {
        assert_eq!(
            parse_model_query("gpt-4o").unwrap(),
            (None, "gpt-4o".to_string())
        );
    }

    #[test]
    fn parse_model_query_malformed() {
        assert!(parse_model_query("").is_err());
        assert!(parse_model_query("   ").is_err());
        assert!(parse_model_query("/model").is_err()); // empty endpoint
        assert!(parse_model_query("openai/").is_err()); // empty model
        assert!(parse_model_query("a/b/c").is_err()); // extra slash
    }

    // --- effort validation ----------------------------------------------

    #[test]
    fn validate_effort_accepts_picker_values() {
        for v in REASONING_EFFORTS {
            assert!(validate_effort(v).is_ok(), "{v} must be valid");
        }
    }

    #[test]
    fn validate_effort_rejects_others() {
        assert!(validate_effort("ultra").is_err());
        assert!(validate_effort("").is_err());
    }

    // --- answers ---------------------------------------------------------

    #[test]
    fn parse_approval_answer_mappings() {
        assert_eq!(parse_approval_answer("y"), Some(Approval::Approve));
        assert_eq!(parse_approval_answer("YES"), Some(Approval::Approve));
        assert_eq!(parse_approval_answer(" n "), Some(Approval::Deny));
        assert_eq!(parse_approval_answer("no"), Some(Approval::Deny));
        assert_eq!(parse_approval_answer("a"), Some(Approval::DenyAll));
        assert_eq!(parse_approval_answer("all"), Some(Approval::DenyAll));
        assert_eq!(parse_approval_answer("maybe"), None);
        assert_eq!(parse_approval_answer(""), None);
    }

    #[test]
    fn parse_question_answer_number_is_choice() {
        let options = vec![
            QuestionOption {
                label: "A".into(),
                description: None,
            },
            QuestionOption {
                label: "B".into(),
                description: None,
            },
        ];
        assert_eq!(
            parse_question_answer("1", &options),
            Some(UserAnswer::Choice { index: 0 })
        );
        assert_eq!(
            parse_question_answer(" 2 ", &options),
            Some(UserAnswer::Choice { index: 1 })
        );
        // Out-of-range numbers fall through to freeform (not a silent None —
        // the user typed SOMETHING).
        assert_eq!(
            parse_question_answer("3", &options),
            Some(UserAnswer::Freeform { text: "3".into() })
        );
    }

    #[test]
    fn parse_question_answer_text_is_freeform_and_empty_is_none() {
        let options = vec![QuestionOption {
            label: "A".into(),
            description: None,
        }];
        assert_eq!(
            parse_question_answer("let's do B instead", &options),
            Some(UserAnswer::Freeform {
                text: "let's do B instead".into()
            })
        );
        assert_eq!(parse_question_answer("", &options), None);
        assert_eq!(parse_question_answer("   ", &options), None);
    }

    // --- truncate / summarize_args ----------------------------------------

    #[test]
    fn truncate_is_char_safe_and_marks_cuts() {
        assert_eq!(truncate("abc", 5), "abc");
        assert_eq!(truncate("abcde", 5), "abcde");
        assert_eq!(truncate("abcdef", 5), "abcd…");
        // Multi-byte chars never split.
        assert_eq!(truncate("日本語テスト", 3), "日本…");
    }

    #[test]
    fn summarize_args_compacts_and_truncates() {
        let args =
            serde_json::json!({"path": "src/very/long/path/to/some/file.rs", "mode": "edit"});
        let out = summarize_args(&args);
        // serde_json's json! macro uses a BTreeMap, so key order is
        // alphabetical — assert presence, not order.
        assert!(out.starts_with('{') && out.ends_with('}'), "{out}");
        assert!(out.contains("path:"), "{out}");
        assert!(out.contains("mode:edit"), "{out}");
        assert!(out.chars().count() <= 160, "{out}");
    }

    // --- render_event -----------------------------------------------------

    #[test]
    fn render_event_text_and_reasoning_are_fragments_with_channel() {
        assert_eq!(
            render_event(&AgentEvent::TextDelta("hello".into())),
            Rendered::Fragment {
                channel: StreamChannel::Text,
                text: "hello".to_string()
            }
        );
        assert_eq!(
            render_event(&AgentEvent::ReasoningDelta("thinking…".into())),
            Rendered::Fragment {
                channel: StreamChannel::Reasoning,
                text: "thinking…".to_string()
            }
        );
    }

    #[test]
    fn render_event_quiet_events_are_silent() {
        assert_eq!(render_event(&AgentEvent::Started), Rendered::Silent);
        assert_eq!(
            render_event(&AgentEvent::ToolCallArgDelta {
                index: 0,
                fragment: "{\"a\":".into()
            }),
            Rendered::Silent
        );
        assert_eq!(
            render_event(&AgentEvent::PromptDispatched {
                text: "x".into(),
                images: vec![]
            }),
            Rendered::Silent
        );
    }

    #[test]
    fn render_event_error_carries_retry_marker() {
        let retrying = render_event(&AgentEvent::Error {
            error: "boom".into(),
            retrying: true,
        });
        match retrying {
            Rendered::Lines(lines) => {
                assert_eq!(lines, vec!["! (retrying) boom".to_string()]);
            }
            other => panic!("expected Lines, got {other:?}"),
        }
        let final_err = render_event(&AgentEvent::Error {
            error: "dead".into(),
            retrying: false,
        });
        match final_err {
            Rendered::Lines(lines) => assert_eq!(lines, vec!["! dead".to_string()]),
            other => panic!("expected Lines, got {other:?}"),
        }
    }

    #[test]
    fn render_event_approval_lists_prompt_without_consuming_responder() {
        // Construct with a live oneshot — rendering must only READ the
        // serializable fields (tool_name/args/core_operation), never touch
        // the responder.
        let (_tx, mut rx) = tokio::sync::oneshot::channel::<Approval>();
        let event = AgentEvent::ApprovalRequest {
            tool_call_id: "c1".into(),
            tool_name: "shell".into(),
            args: serde_json::json!({"command": "cargo test"}),
            preview: None,
            core_operation: false,
            responder: _tx,
        };
        match render_event(&event) {
            Rendered::Lines(lines) => {
                assert!(
                    lines[0].starts_with("⚠ approval needed: shell("),
                    "{lines:?}"
                );
                assert!(lines[0].contains("command:cargo test"), "{lines:?}");
                assert!(lines.last().unwrap().contains("[y]es"), "{lines:?}");
            }
            other => panic!("expected Lines, got {other:?}"),
        }
        // The responder is still pending (not consumed/cancelled by render).
        drop(event);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn render_event_user_question_numbers_options() {
        let (_tx, mut rx) = tokio::sync::oneshot::channel::<UserAnswer>();
        let event = AgentEvent::UserQuestion {
            question_id: "q1".into(),
            question: "Which approach?".into(),
            options: vec![
                QuestionOption {
                    label: "Ship it".into(),
                    description: Some("merge now".into()),
                },
                QuestionOption {
                    label: "Wait".into(),
                    description: None,
                },
            ],
            responder: _tx,
        };
        match render_event(&event) {
            Rendered::Lines(lines) => {
                assert_eq!(lines[0], "? Which approach?");
                assert_eq!(lines[1], "  1. Ship it — merge now");
                assert_eq!(lines[2], "  2. Wait");
            }
            other => panic!("expected Lines, got {other:?}"),
        }
        drop(event);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn render_event_compact_started_announces() {
        // Regression: CompactStarted was added to AgentEvent in the lib crate
        // without a render_event arm, which broke the bin crate build
        // (non-exhaustive match) — start.bat / `tauri build` died on E0004
        // while root cargo build (lib only) stayed green.
        match render_event(&AgentEvent::CompactStarted) {
            Rendered::Lines(lines) => {
                assert_eq!(lines, vec!["— compacting context…".to_string()]);
            }
            other => panic!("expected Lines, got {other:?}"),
        }
    }

    #[test]
    fn render_event_vision_announcements_render() {
        // The image-parsing announcements (VisionDescribe/VisionDescribed)
        // must render explicit console lines — a missing arm here broke the
        // bin crate build for CompactStarted (non-exhaustive match), so new
        // AgentEvent variants always get arms + a test.
        let describe = render_event(&AgentEvent::VisionDescribe {
            index: 1,
            total: 2,
            query: "Describe this image in detail.".into(),
        });
        match describe {
            Rendered::Lines(lines) => {
                assert_eq!(lines.len(), 1);
                assert!(lines[0].contains("describing image 1/2"), "{lines:?}");
                assert!(lines[0].contains("Describe this image"), "{lines:?}");
            }
            other => panic!("expected Lines, got {other:?}"),
        }
        let ok = render_event(&AgentEvent::VisionDescribed {
            index: 2,
            total: 2,
            success: true,
            description: "a photo of a cat".into(),
        });
        match ok {
            Rendered::Lines(lines) => {
                assert!(lines[0].contains("image 2/2 described"), "{lines:?}");
                assert!(lines[0].contains("a photo of a cat"), "{lines:?}");
            }
            other => panic!("expected Lines, got {other:?}"),
        }
        let failed = render_event(&AgentEvent::VisionDescribed {
            index: 1,
            total: 1,
            success: false,
            description: "vision is down".into(),
        });
        match failed {
            Rendered::Lines(lines) => {
                assert!(
                    lines[0].contains("image 1/1 description failed"),
                    "{lines:?}"
                );
                assert!(lines[0].contains("vision is down"), "{lines:?}");
            }
            other => panic!("expected Lines, got {other:?}"),
        }
    }

    // --- status rendering ---------------------------------------------------

    fn status() -> ModelStatus {
        ModelStatus {
            endpoint: "openai".into(),
            model: "o3".into(),
            effective_effort: Some("high".into()),
            supports_reasoning_effort: true,
        }
    }

    #[test]
    fn render_model_list_marks_default_and_current() {
        let endpoints = vec![
            (
                "openai".to_string(),
                vec!["gpt-4o".to_string(), "o3".to_string()],
            ),
            ("local".to_string(), vec!["qwen2.5:7b".to_string()]),
        ];
        let lines = render_model_list(&endpoints, Some(&status()), Some("gpt-4o"));
        assert_eq!(lines[0], "endpoints:");
        assert!(lines.contains(&"  openai".to_string()), "{lines:?}");
        assert!(
            lines.iter().any(|l| l == "    gpt-4o  ← default"),
            "{lines:?}"
        );
        assert!(lines.iter().any(|l| l == "    o3  ← current"), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("qwen2.5:7b")), "{lines:?}");
    }

    #[test]
    fn render_reasoning_status_supported_and_not() {
        let lines = render_reasoning_status(&status());
        assert_eq!(lines[0], "reasoning: high (sent with every request)");
        assert!(lines[1].contains("max"), "{lines:?}");

        let mut off = status();
        off.effective_effort = None;
        let lines = render_reasoning_status(&off);
        assert_eq!(
            lines[0],
            "reasoning: off — field omitted (no reasoning_effort field)"
        );

        let mut unsupported = status();
        unsupported.supports_reasoning_effort = false;
        let lines = render_reasoning_status(&unsupported);
        assert!(lines[0].contains("not supported"), "{lines:?}");
    }

    #[test]
    fn help_text_lists_the_full_surface() {
        let text = help_text().join("\n");
        for word in ["/model", "/reasoning", "/help", "/exit"] {
            assert!(text.contains(word), "help must mention {word}");
        }
    }

    // --- initial_selection ----------------------------------------------

    fn cfg_endpoint(name: &str, models: &[&str], supports: bool) -> mnemo::config::Endpoint {
        mnemo::config::Endpoint {
            name: name.into(),
            models: models
                .iter()
                .map(|s| mnemo::config::ModelSpec::from(s.to_string()))
                .collect(),
            supports_reasoning_effort: supports,
            ..mnemo::config::Endpoint::test_default()
        }
    }

    fn cfg_with(
        endpoints: Vec<mnemo::config::Endpoint>,
        default_model: Option<&str>,
        default_provider: Option<&str>,
    ) -> Config {
        Config {
            general: mnemo::config::GeneralConfig {
                general: mnemo::config::GeneralSection {
                    default_model: default_model.map(str::to_string),
                    default_provider: default_provider.map(str::to_string),
                    ..Default::default()
                },
                ..Default::default()
            },
            endpoints,
            ..Config::default()
        }
    }

    #[test]
    fn initial_selection_uses_named_default_provider_and_model() {
        // build_brain's chain: named default_provider wins over list order;
        // default_model is used verbatim; effort = the endpoint's effective
        // default ("max" when unset).
        let config = cfg_with(
            vec![
                cfg_endpoint("local", &["qwen:7b"], true),
                cfg_endpoint("openai", &["gpt-4o", "o3"], true),
            ],
            Some("o3"),
            Some("openai"),
        );
        let sel = initial_selection(&config).expect("a default endpoint exists");
        assert_eq!(sel.endpoint, "openai");
        assert_eq!(sel.model, "o3");
        assert_eq!(sel.effort.as_deref(), Some("max"));
        assert_eq!(sel.requested_effort, None, "startup has no live override");
        assert!(sel.supports);
    }

    #[test]
    fn initial_selection_falls_back_to_first_endpoint_and_model_chain() {
        // No default_provider → the FIRST endpoint; no default_model → that
        // endpoint's first model; empty models → the "gpt-4o" sentinel.
        let config = cfg_with(
            vec![
                cfg_endpoint("local", &["qwen:7b"], true),
                cfg_endpoint("openai", &["gpt-4o"], true),
            ],
            None,
            None,
        );
        let sel = initial_selection(&config).expect("a default endpoint exists");
        assert_eq!(sel.endpoint, "local");
        assert_eq!(sel.model, "qwen:7b");

        let config = cfg_with(vec![cfg_endpoint("empty", &[], true)], None, None);
        let sel = initial_selection(&config).expect("an endpoint exists");
        assert_eq!(sel.endpoint, "empty");
        assert_eq!(sel.model, "gpt-4o");
    }

    #[test]
    fn initial_selection_uses_dangling_default_model_verbatim() {
        // build_brain uses default_model even when no endpoint lists it
        // (the provider is built with the startup endpoint + that model).
        let config = cfg_with(
            vec![cfg_endpoint("openai", &["gpt-4o"], true)],
            Some("ghost-model"),
            None,
        );
        let sel = initial_selection(&config).expect("an endpoint exists");
        assert_eq!(sel.endpoint, "openai");
        assert_eq!(sel.model, "ghost-model");
    }

    #[test]
    fn initial_selection_honors_configured_effort_and_capability() {
        // A configured effort passes through; a non-reasoning endpoint
        // tracks effort None (never sent).
        let mut ep = cfg_endpoint("chat", &["chat-x"], false);
        ep.reasoning_effort = Some("high".to_string());
        let config = cfg_with(vec![ep], Some("chat-x"), None);
        let sel = initial_selection(&config).expect("resolves");
        assert_eq!(sel.effort, None, "unsupported endpoint never sends effort");
        assert!(!sel.supports);

        let mut ep = cfg_endpoint("reasoning", &["r1"], true);
        ep.reasoning_effort = Some("low".to_string());
        let config = cfg_with(vec![ep], Some("r1"), None);
        let sel = initial_selection(&config).expect("resolves");
        assert_eq!(sel.effort.as_deref(), Some("low"));
    }

    #[test]
    fn initial_selection_none_only_without_endpoints() {
        // The ONLY None case mirrors build_brain's dummy-provider fallback:
        // no endpoint configured at all.
        assert!(initial_selection(&cfg_with(vec![], Some("gpt-4o"), None)).is_none());
        assert!(initial_selection(&cfg_with(vec![], None, None)).is_none());
    }

    // --- model_swap_effort (status-bar picker rules) ----------------------

    fn sel_with(supports: bool, requested: Option<&str>) -> ModelSelection {
        ModelSelection {
            endpoint: "e".into(),
            model: "m".into(),
            effort: requested.map(str::to_string),
            supports,
            requested_effort: requested.map(str::to_string),
        }
    }

    #[test]
    fn model_swap_effort_mirrors_picker_rules() {
        // Unsupported target → force "off" (the field is never sent).
        assert_eq!(
            model_swap_effort(Some(&sel_with(true, Some("high"))), false),
            Some("off".to_string())
        );
        // Leaving an unsupported endpoint → None (adopt the target default;
        // "off" must not stick forever).
        assert_eq!(
            model_swap_effort(Some(&sel_with(false, Some("off"))), true),
            None
        );
        // Supported → supported keeps the live override.
        assert_eq!(
            model_swap_effort(Some(&sel_with(true, Some("high"))), true),
            Some("high".to_string())
        );
        // Supported → supported with "off" passes through (field omitted).
        assert_eq!(
            model_swap_effort(Some(&sel_with(true, Some("off"))), true),
            Some("off".to_string())
        );
        // No override, and no selection at all → None (target default).
        assert_eq!(model_swap_effort(Some(&sel_with(true, None)), true), None);
        assert_eq!(model_swap_effort(None, true), None);
    }

    // --- ConsoleSpawner ---------------------------------------------------

    use mnemo::agent::context::ContextManager;
    use mnemo::project::ConstitutionSource;
    use mnemo::provider::{Capabilities, LlmEvent, ProviderKind, ToolSchema};
    use mnemo::tool::agent::sandbox::Sandbox;

    /// A provider whose `complete` always errors — never called in these
    /// tests (the spawned agent is only registered, then cancelled).
    struct NeverProvider;
    #[async_trait::async_trait]
    impl mnemo::provider::LlmClient for NeverProvider {
        fn capabilities(&self) -> &Capabilities {
            use std::sync::OnceLock;
            static CAPS: OnceLock<Capabilities> = OnceLock::new();
            CAPS.get_or_init(Capabilities::openai)
        }
        fn kind(&self) -> ProviderKind {
            ProviderKind::OpenAI
        }
        fn model(&self) -> &str {
            "never"
        }
        async fn complete(
            &self,
            _: &[mnemo::provider::Message],
            _: &[ToolSchema],
            _: Option<mnemo::provider::ToolChoice>,
        ) -> mnemo::error::Result<futures::stream::BoxStream<'_, LlmEvent>> {
            Err(mnemo::error::Error::Provider(
                "never provider never completes".into(),
            ))
        }
    }

    fn make_factory(dir: &std::path::Path) -> Arc<AgentLoopFactory> {
        let provider: Arc<dyn mnemo::provider::LlmClient> = Arc::new(NeverProvider);
        let sandbox = Arc::new(Sandbox::new(dir).unwrap());
        let gpath = dir.join("global.md");
        let ppath = dir.join("project.md");
        std::fs::write(&gpath, "").unwrap();
        std::fs::write(&ppath, "").unwrap();
        Arc::new(AgentLoopFactory::new(
            provider,
            ConstitutionSource::new(&gpath, &ppath).unwrap(),
            None,
            sandbox,
            dir.to_path_buf(),
            None,
            Arc::new(std::sync::RwLock::new(
                mnemo::config::SafetyMode::Autonomous,
            )),
            ContextManager::new(128_000, 0.5),
            dir.join("plans"),
            None,
        ))
    }

    #[tokio::test]
    async fn console_spawner_registers_child_with_parent() {
        let dir = tempfile::tempdir().unwrap();
        let factory = make_factory(dir.path());
        let mut mgr = AgentManager::new(64);
        // Drop the fan-in receiver — sends are best-effort and swallowed.
        drop(mgr.take_fanin_rx());
        let manager = Arc::new(tokio::sync::Mutex::new(mgr));
        let agent_loops: AgentLoopMap =
            Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));

        let spawner = ConsoleSpawner::new(factory, manager.clone(), agent_loops.clone());
        let id = spawner
            .spawn_with_parent("reviewer", "review the diff", Some(7), None, None)
            .await
            .expect("spawn must succeed");

        // The child is registered with its parent + tracked in the loop map —
        // identical bookkeeping to the GUI's IpcSpawner path.
        {
            let m = manager.lock().await;
            let handle = m.get(id).expect("spawned agent registered");
            assert_eq!(handle.name, "reviewer");
            assert_eq!(handle.parent_id, Some(7));
        }
        assert!(
            agent_loops.lock().await.contains_key(&id),
            "spawned agent's loop must be tracked"
        );

        // Clean up: cancel so the background task exits promptly.
        let m = manager.lock().await;
        let _ = m.send(id, AgentCommand::Cancel);
    }

    // --- LifecycleBookkeeping (console forwarder-equivalent) ---------------

    fn empty_loops() -> AgentLoopMap {
        Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()))
    }

    #[tokio::test]
    async fn lifecycle_running_flags_drive_descendant_gate() {
        // H1(1): without running-flag maintenance, has_running_descendants
        // always returns false and the workflow state-transition gate never
        // fires. The bookkeeping must mark Started/Finished so a running
        // child gates its parent.
        let mut mgr = AgentManager::new(64);
        drop(mgr.take_fanin_rx());
        let (parent_tx, _parent_rx) = tokio::sync::mpsc::channel(8);
        let (child_tx, _child_rx) = tokio::sync::mpsc::channel(8);
        mgr.register(mnemo::runtime::AgentHandle::new(
            7,
            "main".to_string(),
            parent_tx,
        ));
        mgr.register(
            mnemo::runtime::AgentHandle::new(9, "kid".to_string(), child_tx).with_parent(7),
        );
        let manager = Arc::new(tokio::sync::Mutex::new(mgr));
        let book = LifecycleBookkeeping::new(manager.clone(), empty_loops());

        assert!(
            !manager.lock().await.has_running_descendants(7),
            "an idle child must not gate the parent"
        );
        book.on_started(9).await;
        assert!(
            manager.lock().await.has_running_descendants(7),
            "a running child MUST gate the parent's workflow transitions"
        );
        book.on_turn_ended(9, true).await;
        assert!(!manager.lock().await.has_running_descendants(7));
    }

    #[tokio::test]
    async fn lifecycle_notifies_parent_once_with_report_path() {
        // M1: a tool-spawned child's turn end must notify its parent with
        // the same completion text the GUI forwarder composes — including
        // the child's last review-report path — and only ONCE.
        let dir = tempfile::tempdir().unwrap();
        let factory = make_factory(dir.path());
        let child_loop = factory.build_with_id(9);
        child_loop.set_last_review_report("reviews/child-report.md".to_string());
        let loops = empty_loops();
        loops.lock().await.insert(9, child_loop);

        let mut mgr = AgentManager::new(64);
        drop(mgr.take_fanin_rx());
        let (parent_tx, mut parent_rx) = tokio::sync::mpsc::channel(8);
        let (child_tx, _child_rx) = tokio::sync::mpsc::channel(8);
        mgr.register(mnemo::runtime::AgentHandle::new(
            7,
            "main".to_string(),
            parent_tx,
        ));
        mgr.register(
            mnemo::runtime::AgentHandle::new(9, "reviewer".to_string(), child_tx).with_parent(7),
        );
        let manager = Arc::new(tokio::sync::Mutex::new(mgr));
        let book = LifecycleBookkeeping::new(manager, loops);

        book.on_turn_ended(9, true).await;
        match parent_rx.try_recv() {
            Ok(AgentCommand::Suggestion(payload)) => {
                let text = &payload.text;
                assert!(
                    text.contains("reviews/child-report.md"),
                    "the notification must carry the review-report path: {text}"
                );
                assert!(text.contains("reviewer"), "{text}");
            }
            other => panic!("expected a Suggestion to the parent, got {other:?}"),
        }
        // A multi-turn child must not re-notify.
        book.on_turn_ended(9, true).await;
        assert!(
            parent_rx.try_recv().is_err(),
            "the parent must be notified only once per child"
        );
    }

    #[tokio::test]
    async fn failed_reviewer_without_report_latches_parent_and_instructs_ask_user() {
        // Failed-reviewer protocol: a `role:"reviewer"` child that ends in a
        // final error with NO report must (a) set the parent's
        // reviewer_failure_pending latch (blocking blind respawns) and
        // (b) get the distinct ask-the-user text — NOT a generic "read its
        // report" steer that makes the parent hunt for a nonexistent report.
        let dir = tempfile::tempdir().unwrap();
        let factory = make_factory(dir.path());
        let child_loop = factory.build_with_id(9);
        let parent_loop = factory.build_with_id(7);
        let loops = empty_loops();
        loops.lock().await.insert(9, child_loop);
        loops.lock().await.insert(7, parent_loop.clone());

        let mut mgr = AgentManager::new(64);
        drop(mgr.take_fanin_rx());
        let (parent_tx, mut parent_rx) = tokio::sync::mpsc::channel(8);
        let (child_tx, _child_rx) = tokio::sync::mpsc::channel(8);
        mgr.register(mnemo::runtime::AgentHandle::new(
            7,
            "main".to_string(),
            parent_tx,
        ));
        mgr.register(
            mnemo::runtime::AgentHandle::new(9, "reviewer".to_string(), child_tx)
                .with_parent(7)
                .with_role(Some("reviewer".to_string())),
        );
        let manager = Arc::new(tokio::sync::Mutex::new(mgr));
        let book = LifecycleBookkeeping::new(manager, loops);

        assert!(
            !parent_loop.reviewer_failure_pending(),
            "latch starts clear"
        );
        book.on_turn_ended(9, false).await;
        match parent_rx.try_recv() {
            Ok(AgentCommand::Suggestion(payload)) => {
                let text = &payload.text;
                assert!(text.contains("FAILED"), "text: {text}");
                assert!(text.contains("Do NOT respawn"), "text: {text}");
                assert!(text.contains("ask_user"), "text: {text}");
                assert!(
                    !text.contains("read its report"),
                    "must not send the parent hunting for a nonexistent report: {text}"
                );
            }
            other => panic!("expected a Suggestion to the parent, got {other:?}"),
        }
        assert!(
            parent_loop.reviewer_failure_pending(),
            "a failed reviewer without a report must latch the parent"
        );
    }

    #[tokio::test]
    async fn non_reviewer_failure_uses_generic_text_and_no_latch() {
        // Failed-reviewer protocol applies ONLY to role:"reviewer" children:
        // a plain background agent that fails gets the generic text (which
        // still says "no report was written" — backlog 5b46674d made the
        // report-less case explicit) and does NOT latch its parent.
        let dir = tempfile::tempdir().unwrap();
        let factory = make_factory(dir.path());
        let child_loop = factory.build_with_id(9);
        let parent_loop = factory.build_with_id(7);
        let loops = empty_loops();
        loops.lock().await.insert(9, child_loop);
        loops.lock().await.insert(7, parent_loop.clone());

        let mut mgr = AgentManager::new(64);
        drop(mgr.take_fanin_rx());
        let (parent_tx, mut parent_rx) = tokio::sync::mpsc::channel(8);
        let (child_tx, _child_rx) = tokio::sync::mpsc::channel(8);
        mgr.register(mnemo::runtime::AgentHandle::new(
            7,
            "main".to_string(),
            parent_tx,
        ));
        mgr.register(
            mnemo::runtime::AgentHandle::new(9, "investigator".to_string(), child_tx)
                .with_parent(7),
            // no role — a plain background agent
        );
        let manager = Arc::new(tokio::sync::Mutex::new(mgr));
        let book = LifecycleBookkeeping::new(manager, loops);

        book.on_turn_ended(9, false).await;
        match parent_rx.try_recv() {
            Ok(AgentCommand::Suggestion(payload)) => {
                let text = &payload.text;
                assert!(text.contains("failed"), "text: {text}");
                assert!(text.contains("no report was written"), "text: {text}");
                assert!(!text.contains("ask_user"), "text: {text}");
            }
            other => panic!("expected a Suggestion to the parent, got {other:?}"),
        }
        assert!(
            !parent_loop.reviewer_failure_pending(),
            "a non-reviewer failure must not latch the parent"
        );
    }

    #[tokio::test]
    async fn lifecycle_exited_removes_agent_from_manager_and_loops() {
        // M2: dead agents must not accumulate (manager map + loop map stay
        // bounded, and provider swaps stop touching dead loops).
        let mut mgr = AgentManager::new(64);
        drop(mgr.take_fanin_rx());
        let (child_tx, _child_rx) = tokio::sync::mpsc::channel(8);
        mgr.register(mnemo::runtime::AgentHandle::new(
            9,
            "kid".to_string(),
            child_tx,
        ));
        let manager = Arc::new(tokio::sync::Mutex::new(mgr));
        let loops = empty_loops();
        loops.lock().await.insert(
            9,
            make_factory(tempfile::tempdir().unwrap().path()).build_with_id(9),
        );
        let book = LifecycleBookkeeping::new(manager.clone(), loops.clone());

        book.on_exited(9).await;
        assert!(
            manager.lock().await.get(9).is_none(),
            "removed from the manager"
        );
        assert!(
            loops.lock().await.get(&9).is_none(),
            "removed from the per-agent loop map"
        );
    }

    #[tokio::test]
    async fn running_flags_protect_children_from_inactive_cleanup() {
        // H1(2): cleanup_inactive_subagents filters on the running flag.
        // With bookkeeping keeping flags current, a RUNNING child survives a
        // fresh spawn's cleanup and a COMPLETED (started, then ended) one is
        // cancelled. A child that has never started is PENDING, not
        // completed, and must also survive — without that half, back-to-back
        // spawns cancel each other's not-yet-started agents (live
        // 2026-09-08, plan ccff0138; see ipc::events::cleanup_tests).
        let mut mgr = AgentManager::new(64);
        drop(mgr.take_fanin_rx());
        let (a_tx, mut a_rx) = tokio::sync::mpsc::channel(8);
        let (b_tx, mut b_rx) = tokio::sync::mpsc::channel(8);
        let (c_tx, mut c_rx) = tokio::sync::mpsc::channel(8);
        mgr.register(
            mnemo::runtime::AgentHandle::new(10, "runner".to_string(), a_tx).with_parent(7),
        );
        mgr.register(
            mnemo::runtime::AgentHandle::new(11, "idler".to_string(), b_tx).with_parent(7),
        );
        mgr.register(
            mnemo::runtime::AgentHandle::new(12, "pending".to_string(), c_tx).with_parent(7),
        );
        let manager = Arc::new(tokio::sync::Mutex::new(mgr));
        let book = LifecycleBookkeeping::new(manager.clone(), empty_loops());
        book.on_started(10).await; // "runner" is mid-turn
        // "idler" ran a turn and ended — genuinely completed.
        book.on_started(11).await;
        book.on_turn_ended(11, true).await;
        // "pending" was just registered by an earlier spawn: no Started yet.

        crate::ipc::events::cleanup_inactive_subagents(&manager, 100).await;
        assert!(
            a_rx.try_recv().is_err(),
            "a RUNNING subagent must never be cancelled by cleanup"
        );
        assert!(
            matches!(b_rx.try_recv(), Ok(AgentCommand::Cancel)),
            "a completed (started, then idle) subagent is cancelled by cleanup"
        );
        assert!(
            c_rx.try_recv().is_err(),
            "a never-started subagent is PENDING — cleanup must spare it"
        );
    }

    // --- Windows console attach (regression) ------------------------------

    /// Probe-child exit codes (stdio is deliberately not a console there,
    /// so the result is reported via the process exit code). Windows-only
    /// like their only users — ungated they would be dead code (a hard
    /// error under `deny(warnings)`) on non-Windows hosts.
    #[cfg(windows)]
    const PROBE_PASS: i32 = 0;
    #[cfg(windows)]
    const PROBE_FAIL: i32 = 3;

    /// Null the three standard handles — reproducing PowerShell's launch
    /// state for GUI-subsystem children (NULL handles). `Stdio::null()` in
    /// the parent gives NUL-DEVICE handles, which are VALID and would be
    /// preserved by `attach_console`'s redirection guard (LOW-2) — so the
    /// probe child nulls them itself before attaching. Raw FFI, mirroring
    /// the no-winapi style of `attach_console`.
    #[cfg(windows)]
    fn null_std_handles() {
        use std::ffi::c_void;

        #[link(name = "kernel32")]
        extern "system" {
            fn SetStdHandle(n_std_handle: u32, h_handle: *mut c_void) -> i32;
        }
        const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6; // (DWORD)-10
        const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // (DWORD)-11
        const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4; // (DWORD)-12
        unsafe {
            SetStdHandle(STD_INPUT_HANDLE, std::ptr::null_mut());
            SetStdHandle(STD_OUTPUT_HANDLE, std::ptr::null_mut());
            SetStdHandle(STD_ERROR_HANDLE, std::ptr::null_mut());
        }
    }

    /// Child side of [`attach_console_rewires_std_handles_in_consoleless_child`]:
    /// the caller runs `attach_console()` first, then this verifies all
    /// three standard handles are real console handles — non-NULL and
    /// accepted by `GetConsoleMode` (a NUL/pipe/file handle is rejected).
    /// Raw FFI, mirroring the no-winapi style of `attach_console` itself.
    #[cfg(windows)]
    fn probe_console_handles() -> i32 {
        use std::ffi::c_void;

        #[link(name = "kernel32")]
        extern "system" {
            fn GetStdHandle(n_std_handle: u32) -> *mut c_void;
            fn GetConsoleMode(h_console_handle: *mut c_void, lp_mode: *mut u32) -> i32;
        }
        const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6; // (DWORD)-10
        const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // (DWORD)-11
        const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4; // (DWORD)-12
        const INVALID_HANDLE_VALUE: *mut c_void = (-1isize) as *mut c_void;

        for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
            let handle = unsafe { GetStdHandle(id) };
            if handle.is_null() || handle == INVALID_HANDLE_VALUE {
                return PROBE_FAIL;
            }
            let mut mode = 0u32;
            if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
                return PROBE_FAIL; // a valid handle, but not a console device
            }
        }
        PROBE_PASS
    }

    /// Regression (2026-08-21): `mnemo.exe --console` — a GUI-subsystem
    /// release build launched from PowerShell — showed no output and no
    /// prompt. Root cause: `AttachConsole` alone does NOT populate the
    /// standard-handle table, so the process kept the launcher's NULL std
    /// handles and every print vanished. This test re-spawns the test
    /// binary DETACHED (no console at all — the launch state of a
    /// GUI-subsystem binary) and the child nulls its own std handles (the
    /// exact PowerShell launch state: NULL, not NUL-device, handles), then
    /// asserts `attach_console()` rewires all three std handles to console
    /// devices. Fails against the attach-only implementation; passes once
    /// SetStdHandle rewiring is in.
    #[test]
    #[cfg(windows)]
    fn attach_console_rewires_std_handles_in_consoleless_child() {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;

        // Child side: re-entered via the harness filter below. Null the std
        // handles first (the exact PowerShell launch state — the NUL-device
        // handles Stdio::null() provides are VALID and would be kept by the
        // redirection guard), then attach and report by exit code.
        if std::env::var_os("MNEMO_CONSOLE_PROBE").is_some() {
            null_std_handles();
            attach_console();
            std::process::exit(probe_console_handles());
        }

        let exe = std::env::current_exe().expect("test binary path");
        let status = std::process::Command::new(exe)
            // Re-run THIS test in the child (full path: with --exact the
            // filter must match the test's whole module path, or the child
            // would run zero tests and exit 0 — a false pass); the env var
            // flips it into probe mode (it exits before spawning anything —
            // no recursion).
            .arg("console::tests::attach_console_rewires_std_handles_in_consoleless_child")
            .arg("--exact")
            .env("MNEMO_CONSOLE_PROBE", "1")
            // DETACHED_PROCESS: the child gets NO console — exactly the
            // state a GUI-subsystem binary starts in.
            .creation_flags(DETACHED_PROCESS)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("spawn console-attach probe child");

        assert_eq!(
            status.code(),
            Some(PROBE_PASS),
            "console-less child: attach_console() must rewire the std handles \
             to CONIN$/CONOUT$ — AttachConsole alone never populates them"
        );
    }

    /// Return the process's console HWND as a usize (0 when none is attached).
    /// Shared by the own-console regression probe.
    #[cfg(windows)]
    fn get_console_window_usize() -> usize {
        use std::ffi::c_void;

        #[link(name = "kernel32")]
        extern "system" {
            fn GetConsoleWindow() -> *mut c_void;
        }
        unsafe { GetConsoleWindow() as usize }
    }

    /// Regression (2026-09): release GUI-subsystem `mnemo-app.exe -console`
    /// launched from PowerShell attached to the *parent* console via
    /// `AttachConsole(ATTACH_PARENT_PROCESS)`. PowerShell does not wait on
    /// GUI-subsystem children, so it kept owning the shared console and
    /// keystrokes were split/stolen — only a few typed chars echoed and the
    /// REPL was unusable. Fix: `attach_console` must `AllocConsole` its own
    /// window (never attach to the parent).
    ///
    /// Probe topology (the cargo-test host often has no console of its own,
    /// so AllocConsole in the host is unreliable under non-interactive
    /// runners):
    /// 1. Host spawns an intermediate with `CREATE_NEW_CONSOLE` — that
    ///    process owns a real console HWND (the "parent terminal").
    /// 2. Intermediate spawns a DETACHED grandchild (GUI-subsystem launch
    ///    state: no console, NULL std handles after nulling) and waits.
    /// 3. Grandchild runs `attach_console()` and exits PASS only when its
    ///    console HWND is non-null **and distinct from the intermediate's**.
    /// Fails against AttachConsole (child HWND == parent HWND); passes once
    /// AllocConsole-only is in.
    #[test]
    #[cfg(windows)]
    fn attach_console_allocates_own_console_not_parent() {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

        // Grandchild: DETACHED probe under the intermediate.
        if std::env::var_os("MNEMO_CONSOLE_OWN_PROBE").is_some() {
            null_std_handles();
            let parent_hwnd: usize = std::env::var("MNEMO_PARENT_CONSOLE_HWND")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            attach_console();
            let ours = get_console_window_usize();
            // Own window required: null = no console; equal to parent =
            // AttachConsole shared the launcher (the bug).
            if ours == 0 || (parent_hwnd != 0 && ours == parent_hwnd) {
                std::process::exit(PROBE_FAIL);
            }
            std::process::exit(probe_console_handles());
        }

        // Intermediate: owns a fresh console, spawns the DETACHED probe.
        if std::env::var_os("MNEMO_CONSOLE_OWN_INTERMEDIATE").is_some() {
            let parent_hwnd = get_console_window_usize();
            if parent_hwnd == 0 {
                // CREATE_NEW_CONSOLE failed to give us a window — cannot
                // reproduce the AttachConsole path.
                std::process::exit(PROBE_FAIL);
            }
            let exe = std::env::current_exe().expect("test binary path");
            let status = std::process::Command::new(exe)
                .arg("console::tests::attach_console_allocates_own_console_not_parent")
                .arg("--exact")
                .env("MNEMO_CONSOLE_OWN_PROBE", "1")
                .env("MNEMO_PARENT_CONSOLE_HWND", parent_hwnd.to_string())
                .env_remove("MNEMO_CONSOLE_OWN_INTERMEDIATE")
                .creation_flags(DETACHED_PROCESS)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("spawn own-console probe grandchild");
            std::process::exit(status.code().unwrap_or(PROBE_FAIL));
        }

        // Host: spawn the intermediate with its own console window.
        let exe = std::env::current_exe().expect("test binary path");
        let status = std::process::Command::new(exe)
            .arg("console::tests::attach_console_allocates_own_console_not_parent")
            .arg("--exact")
            .env("MNEMO_CONSOLE_OWN_INTERMEDIATE", "1")
            .creation_flags(CREATE_NEW_CONSOLE)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("spawn own-console intermediate");

        assert_eq!(
            status.code(),
            Some(PROBE_PASS),
            "attach_console() must AllocConsole its own window (HWND ≠ parent) — \
             AttachConsole shares the parent and splits keystrokes with PowerShell"
        );
    }

    /// Regression (LOW-1, 2026-09-04 re-review): a fully redirected launch
    /// (`mnemo --console > out.txt < in.txt 2> err.txt`, or piped) has no
    /// console but **valid** std handles. `attach_console` must NOT
    /// `AllocConsole` a spurious empty window — it should early-return when
    /// all three handles are already present. Without the skip, AllocConsole
    /// succeeds and a window flashes for the whole session.
    ///
    /// Probe: DETACHED child with all three stdio **piped** (valid pipe
    /// handles = fully-redirected state; do NOT null them). Child runs
    /// `attach_console()` and exits PASS only when `GetConsoleWindow()` is
    /// still NULL. Fails without the skip (HWND ≠ 0); passes with it.
    #[test]
    #[cfg(windows)]
    fn attach_console_skips_alloc_when_std_handles_already_valid() {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;

        // Child side: re-entered via the harness filter below. Keep the
        // piped handles the parent gave us (do NOT null) — that is the
        // fully-redirected launch state the skip guards.
        if std::env::var_os("MNEMO_CONSOLE_REDIRECT_PROBE").is_some() {
            attach_console();
            // Skip must fire: no console window allocated.
            if get_console_window_usize() != 0 {
                std::process::exit(PROBE_FAIL);
            }
            std::process::exit(PROBE_PASS);
        }

        let exe = std::env::current_exe().expect("test binary path");
        // Hold the pipe ends open for the child's lifetime so the handles
        // stay valid while attach_console runs (dropping them early can
        // race the child).
        let mut child = std::process::Command::new(exe)
            .arg("console::tests::attach_console_skips_alloc_when_std_handles_already_valid")
            .arg("--exact")
            .env("MNEMO_CONSOLE_REDIRECT_PROBE", "1")
            // DETACHED: no console of its own. Piped stdio: valid handles
            // (the fully-redirected state) — unlike Stdio::null() which
            // also yields valid NUL-device handles, pipes match real
            // `> out.txt` redirects more closely.
            .creation_flags(DETACHED_PROCESS)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn redirect-skip probe child");
        let status = child.wait().expect("wait redirect-skip probe child");

        assert_eq!(
            status.code(),
            Some(PROBE_PASS),
            "attach_console() must not AllocConsole when all three std handles \
             are already valid (fully redirected launch) — would flash a \
             spurious empty window"
        );
    }
}
