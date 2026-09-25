// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Agent-control, safety, model/workflow/skill, and stats Tauri commands.
//!
//! Each `#[tauri::command]` function is callable from the frontend via
//! `invoke("command_name", { args })`. Commands here drive a single agent:
//! prompts/steers, interrupt/cancel, approvals, safety mode + rules, live
//! model swaps, workflow/skill queries, and per-session/project stats.

use std::sync::Arc;

use serde::Serialize;
use tauri::State;

use mnemo::runtime::channels::{AgentCommand, AgentId, SerializableAgentEvent};
use mnemo::workflow::{PlanFile, WorkflowState};

use crate::ipc::error::IpcError;
use crate::ipc::events::emit_agent_event;
use crate::ipc::settings::{parse_safety_mode, safety_mode_wire};
use crate::ipc::state::{IpcState, UserIntervention};

/// Lock the manager + send a command to an agent, mapping the error with
/// `label`. Shared by the 6 send-command Tauri commands (send_prompt,
/// send_suggestion, interrupt, cancel, compact, clear_conversation) so they
/// each collapse to a one-liner.
async fn send_cmd(
    state: &State<'_, IpcState>,
    agent_id: AgentId,
    cmd: AgentCommand,
    label: &str,
) -> Result<(), IpcError> {
    let manager = state.runtime.manager.lock().await;
    manager
        .send(agent_id, cmd)
        .map_err(|e| format!("failed to {label}: {e:?}"))
        .map_err(IpcError::from)
}

/// Read the raw `safety.toml` contents (for the Safety tab editor).
///
/// Returns an empty string if the file doesn't exist yet.
#[tauri::command]
pub async fn get_safety_rules(state: State<'_, IpcState>) -> Result<String, IpcError> {
    let rules = state.safety_rules()?;
    Ok(rules.read_raw())
}

/// Save raw `safety.toml` contents (from the Safety tab editor).
///
/// The text is validated as TOML before writing; invalid TOML returns an
/// error and the file is not written. Invalid regex patterns within rules are
/// skipped (best-effort) — the file is still written so the user can fix them.
#[tauri::command]
pub async fn save_safety_rules(
    state: State<'_, IpcState>,
    content: String,
) -> Result<(), IpcError> {
    let rules = state.safety_rules()?;
    rules
        .write_raw(&content)
        .map_err(|e| format!("failed to save safety rules: {e}"))
        .map_err(IpcError::from)
}

/// Add a safety rule for a tool call (the "Mark Safe" action).
///
/// Generates a literal-match pattern from the call's signature and appends it
/// to `safety.toml`. If an identical rule already exists, this is a no-op.
/// Returns the pattern string that was saved.
#[tauri::command]
pub async fn add_safety_rule(
    state: State<'_, IpcState>,
    tool: String,
    args: String,
) -> Result<String, IpcError> {
    let rules = state.safety_rules()?;
    // Parse the args string as JSON (the frontend sends the tool call's
    // arguments as a JSON string).
    let args_value: serde_json::Value =
        serde_json::from_str(&args).map_err(|e| format!("invalid args JSON: {e}"))?;
    rules
        .add_rule(&tool, &args_value)
        .map_err(|e| format!("failed to add safety rule: {e}"))
        .map_err(IpcError::from)
}

/// Add a *broad* safety rule that auto-approves any call of the given tool
/// (the "Allow for project" action).
///
/// Generates a prefix-match pattern (`^<tool>:`) that matches every signature
/// for the tool, regardless of arguments, and appends it to `safety.toml`.
/// Unlike `set_safety_mode`, this is additive and persistent — it does not
/// change the global safety mode, so there is no desync risk. If an identical
/// broad rule already exists, this is a no-op. Returns the pattern string
/// that was saved.
#[tauri::command]
pub async fn add_safety_rule_broad(
    state: State<'_, IpcState>,
    tool: String,
) -> Result<String, IpcError> {
    let rules = state.safety_rules()?;
    rules
        .add_rule_broad(&tool)
        .map_err(|e| format!("failed to add broad safety rule: {e}"))
        .map_err(IpcError::from)
}

/// Add a `command_class` safety rule for a shell command (the "Mark Safe (same
/// operation)" action).
///
/// Classifies the command into a normalized safety class (e.g. `cargo test`)
/// and saves a rule that auto-approves any shell call with the same class —
/// ignoring cosmetic output filtering (`Select-String`, `2>&1`) but still
/// prompting for chained (`&&`, `;`) or unknown commands. Returns the class
/// string that was saved, or an error if the command can't be classified.
#[tauri::command]
pub async fn add_safety_rule_class(
    state: State<'_, IpcState>,
    tool: String,
    args: String,
) -> Result<String, IpcError> {
    let rules = state.safety_rules()?;
    let args_value: serde_json::Value =
        serde_json::from_str(&args).map_err(|e| format!("invalid args JSON: {e}"))?;
    rules
        .add_rule_class(&tool, &args_value)
        .map_err(|e| format!("failed to add command-class safety rule: {e}"))
        .map_err(IpcError::from)
}

/// Info about an agent, returned by `list_agents`.
#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub id: AgentId,
    pub name: String,
    pub running: bool,
    /// The agent that spawned this one via the `spawn_agent` tool (`None` for
    /// UI-spawned agents and the main agent — the parentless agent with the
    /// smallest id). The frontend uses it to identify the main agent.
    pub parent_id: Option<AgentId>,
    /// The effective model id this agent is running: the per-context override
    /// (workflow-state / skill / subagent / forced model) resolved for its
    /// most recent turn, else the default provider's model. `None` only when
    /// the agent's loop has been removed (it exited); present for every live
    /// agent. The frontend shows it as a second line in the agent tab.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The `endpoints.toml` endpoint name serving `model` (the resolved
    /// override's endpoint when one ran, else the default provider's).
    /// Lets the frontend label the provider correctly when the same model id
    /// is listed under two endpoints — first-match resolution cannot
    /// disambiguate (backlog 2980ca67). `None` for mock-backed agents
    /// (empty provider name); the frontend then falls back to resolving the
    /// model id against `endpoints.toml`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    /// The DISPLAY-space effective reasoning effort of the model serving
    /// this agent's most recent turn (`"off" | "low" | "medium" | "high" |
    /// "max"`), resolved exactly as the request builder resolves it
    /// (per-context `ModelRef.reasoning_effort` override, else the model's
    /// default chain `ModelSpec.reasoning_effort` → endpoint default →
    /// `"max"`, gated + clamped). `None` when unknown (mock-backed loops) —
    /// the frontend then falls back to the toolbar echo / endpoint default
    /// (backlog 51dab4da: the status bar must show the effort of the model
    /// actually in use, never a stale value).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

/// The workflow state, returned by `get_workflow_state`.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowStateInfo {
    /// The current state, serialized via the `WorkflowState` enum's serde form
    /// (lowercase: "planning" | "executing" | "complete" | "skill") so the TS
    /// mirror can type it as the `WorkflowState` union. Previously this was the
    /// Display form (capitalized), which forced `as never` casts on the frontend.
    pub state: WorkflowState,
    /// The active plan (top of the stack).
    pub plan: Option<PlanFile>,
    /// How many plans deep the stack is (0 = Planning, 1 = a single root plan).
    pub depth: usize,
    /// The ancestor plans below the active one, root first (empty unless the
    /// active plan is a sub-plan). Each carries an `id` (so the frontend
    /// staircase can click an ancestor to view it via `get_plan`) + `title`.
    pub parents: Vec<PlanAncestorInfo>,
    /// The active skill overlay, when the workflow is in the Skill state.
    /// `None` otherwise. The frontend displays the skill name + prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill: Option<ActiveSkillInfo>,
}

/// An ancestor plan in the stack (root first), returned by `get_workflow_state`.
/// Carries the plan's on-disk `id` (so the frontend can fetch it via
/// `get_plan`) + its `title` (for the staircase display).
#[derive(Debug, Clone, Serialize)]
pub struct PlanAncestorInfo {
    /// The on-disk plan id (file stem of `.coding/plans/<id>.md`).
    pub id: String,
    /// The plan title.
    pub title: String,
}

/// The active skill overlay, returned by `get_workflow_state` when a skill is
/// running. Mirrors [`mnemo::workflow::ActiveSkill`] minus the `tools`
/// list (which is an internal detail the frontend doesn't need).
#[derive(Debug, Clone, Serialize)]
pub struct ActiveSkillInfo {
    /// The skill name (selects the registry entry).
    pub name: String,
    /// The goal the agent drives toward while the skill is active.
    pub prompt: String,
    /// Where the workflow lands after `skill_end`.
    pub target_state: WorkflowState,
}

/// Send a prompt to an agent.
///
/// `images` is a list of base64 data URLs (`data:image/png;base64,...`) for
/// pasted image attachments. When non-empty, the agent builds a multipart user
/// message (text + image_url blocks).
#[tauri::command]
pub async fn send_prompt(
    state: State<'_, IpcState>,
    agent_id: AgentId,
    text: String,
    images: Vec<String>,
) -> Result<(), IpcError> {
    send_cmd(
        &state,
        agent_id,
        AgentCommand::Prompt { text, images },
        "send prompt",
    )
    .await
}

/// Send a suggestion (steer) to an agent.
///
/// When the target is the MAIN agent during an active Run-All, halt the run
/// BEFORE sending the steer. A steer soft-stops the turn (`StopReason::Steer`
/// → `Finished`); without halting, that soft-stop `Finished` resolves the
/// in-flight item and `run_all_dispatch_next` dispatches item N+1 into the
/// main agent — interleaving the user's steer with the next backlog item.
/// Halting first sets the run's stop flag and records the intervention latch
/// (the run state is KEPT, backlog b83e891f), so the soft-stop `Finished`
/// resolves through the intervention path — the item is kept InFlight with
/// its run stopped, and no next dispatch happens. Mirrors the approval halt
/// (`halt_run_all`). No-op when Run-All is inactive, so callers can invoke
/// unconditionally.
#[tauri::command]
pub async fn send_suggestion(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
    agent_id: AgentId,
    text: String,
    images: Vec<String>,
) -> Result<(), IpcError> {
    // Halt Run-All when steering the main agent mid-run — must run BEFORE
    // send_cmd so the stop flag + intervention latch are in place before the
    // soft-stop Finished arrives.
    let (is_main, running) = {
        let mgr = state.runtime.manager.lock().await;
        (
            mgr.main_agent_id() == Some(agent_id),
            mgr.get(agent_id).map(|h| h.is_running()).unwrap_or(false),
        )
    };
    if is_main {
        // A steer is an intervention, not a task failure: record the latch
        // BEFORE halting so the resolution of the steer's soft-stop turn
        // disposes of the in-flight item non-terminally (a run-all item is
        // kept InFlight with its run stopped, backlog b83e891f) instead of
        // marking it Failed/CantResolve. Only when the agent is actually
        // running — a steer on an idle agent interrupts no turn, so the
        // latch would linger and misfire on a later resolution.
        if running {
            let mut latch = state
                .backlog
                .user_intervention
                .lock()
                .expect("user_intervention lock poisoned");
            // MERGE, never overwrite: a second intervention on the same turn
            // (steer-then-steer, steer-then-stop) must not drop an item id
            // already captured by halt_run_all — a lost id strands the item
            // (the latch path bypasses the normal resolution pointers, so
            // nothing else can resolve it).
            let iv = latch.get_or_insert_with(|| UserIntervention {
                item_id: None,
                reason: String::new(),
            });
            iv.reason = "steered by the user".into();
        }
        crate::ipc::run_all::halt_run_all(&app, "steer received during unattended run", false)
            .await;
    }
    send_cmd(
        &state,
        agent_id,
        AgentCommand::Suggestion(mnemo::runtime::SteerPayload { text, images }),
        "send suggestion",
    )
    .await
}

/// Cancel a queued suggestion (steer) for an agent — the "x" on a pending
/// steer in the backlog. Sends `AgentCommand::CancelSuggestion(text)` so the
/// backend drops the matching queued steer before it's injected (handled by
/// `StopReason::fold` and the pre-inject drain in `run_turn_with_retry`).
/// A no-op if the steer was already injected or none is queued.
#[tauri::command]
pub async fn cancel_suggestion(
    state: State<'_, IpcState>,
    agent_id: AgentId,
    text: String,
) -> Result<(), IpcError> {
    send_cmd(
        &state,
        agent_id,
        AgentCommand::CancelSuggestion(text),
        "cancel suggestion",
    )
    .await
}

/// Interrupt an agent's current generation.
///
/// An interrupt on the MAIN agent is a user intervention (like a steer), so
/// the in-flight backlog item is disposed of non-terminally at resolution
/// instead of failed (a run-all item is kept InFlight with its run stopped,
/// backlog b83e891f; a single-dispatch item is kept InFlight with its
/// pointer restored, plan cace17a6): record the
/// intervention latch first, but ONLY when the agent is actually running —
/// an interrupt on an idle agent starts no turn, so the latch would linger
/// and misfire on a later resolution. Run-All is NOT halted here (unlike a
/// steer): the interrupt's soft-stop `Finished` resolution consumes the
/// latch, which stops the run's dispatch loop (the stop flag) without
/// ending it.
#[tauri::command]
pub async fn interrupt(state: State<'_, IpcState>, agent_id: AgentId) -> Result<(), IpcError> {
    let running_main = {
        let mgr = state.runtime.manager.lock().await;
        mgr.main_agent_id() == Some(agent_id)
            && mgr.get(agent_id).map(|h| h.is_running()).unwrap_or(false)
    };
    if running_main {
        let mut latch = state
            .backlog
            .user_intervention
            .lock()
            .expect("user_intervention lock poisoned");
        // MERGE, never overwrite (see send_suggestion): an interrupt after a
        // steer must preserve the item id the halt already captured.
        let iv = latch.get_or_insert_with(|| UserIntervention {
            item_id: None,
            reason: String::new(),
        });
        iv.reason = "interrupted by the user".into();
    }
    send_cmd(&state, agent_id, AgentCommand::Interrupt, "interrupt").await
}

/// Cancel an agent entirely.
#[tauri::command]
pub async fn cancel(state: State<'_, IpcState>, agent_id: AgentId) -> Result<(), IpcError> {
    send_cmd(&state, agent_id, AgentCommand::Cancel, "cancel").await
}

/// Manually compact an agent's context — summarize old messages into a summary
/// system message, keeping the system prompt + recent messages. Backed by the
/// `/compact` slash command + the context-popup Compact button. If the agent
/// is mid-turn, the turn ends first (via `StopReason::Compact`) so the
/// compaction runs on a quiescent conversation.
#[tauri::command]
pub async fn compact(state: State<'_, IpcState>, agent_id: AgentId) -> Result<(), IpcError> {
    send_cmd(&state, agent_id, AgentCommand::Compact, "compact").await
}

/// Clear an agent's conversation history entirely — a fresh start. Backed by
/// the `/new` slash command. If the agent is mid-turn, the turn ends first
/// (via `StopReason::Clear`).
#[tauri::command]
pub async fn clear_conversation(
    state: State<'_, IpcState>,
    agent_id: AgentId,
) -> Result<(), IpcError> {
    send_cmd(&state, agent_id, AgentCommand::Clear, "clear conversation").await
}

/// Approve or deny a pending tool call.
///
/// The frontend passes only `toolCallId` + `approval` (no agent id). A
/// `tool_call_id` is globally unique per tool call, so `resolve` finds the
/// single pending entry matching it regardless of agent. Agent-scoping of the
/// pending-approvals map is enforced at *cleanup* (`cleanup_for_agent`), which
/// is what must not drop other agents' approvals — resolution is unaffected.
#[tauri::command]
pub async fn approve(
    state: State<'_, IpcState>,
    tool_call_id: String,
    approval: mnemo::runtime::Approval,
) -> Result<bool, IpcError> {
    Ok(state.approvals.resolve(&tool_call_id, approval))
}

/// Answer a pending `ask_user` question.
///
/// The frontend passes only `questionId` + `answer` (no agent id). A
/// `question_id` is globally unique per `ask_user` call, so `resolve` finds
/// the single pending entry matching it regardless of agent (mirrors
/// [`approve`]). The agent task, blocked on the oneshot since it asked, resumes
/// with the answer.
#[tauri::command]
pub async fn answer_question(
    state: State<'_, IpcState>,
    question_id: String,
    answer: mnemo::runtime::UserAnswer,
) -> Result<bool, IpcError> {
    Ok(state.questions.resolve(&question_id, answer))
}

/// Set the runtime safety mode (e.g. toggle auto-approve / autonomous).
/// Accepts the kebab-case form used by config: "approve-each-action",
/// "auto-read-approve-writes", "auto-approve-project", "autonomous".
///
/// After updating the mode, any pending approval that the new mode would
/// auto-run is resolved as Approved (so the agent proceeds without waiting
/// for the user). Core operations (git merge/push) are never auto-resolved —
/// they always require a contemporaneous user approval regardless of mode.
#[tauri::command]
pub async fn set_safety_mode(state: State<'_, IpcState>, mode: String) -> Result<(), IpcError> {
    let parsed = parse_safety_mode(&mode)?;
    state.set_safety(parsed)?;
    Ok(())
}

/// Get the current runtime safety mode (kebab-case string).
#[tauri::command]
pub async fn get_safety_mode(state: State<'_, IpcState>) -> Result<String, IpcError> {
    let mode = state
        .runtime
        .safety_mode
        .read()
        .map_err(|e| format!("safety_mode lock poisoned: {e}"))?;
    Ok(safety_mode_wire(*mode).to_string())
}

/// Get the startup error, if the brain failed to build. The frontend shows
/// this as an error screen instead of the normal UI.
#[tauri::command]
pub async fn get_startup_error(state: State<'_, IpcState>) -> Result<Option<String>, String> {
    Ok(state.runtime.startup_error.clone())
}

/// Report a diagnostic line from the webview to stderr.
///
/// The one channel that still works when Tauri EVENT delivery is broken:
/// app-defined commands travel over the custom IPC protocol, while events are
/// delivered by eval'ing JS into the webview. So the frontend (and the
/// `mnemo[probe]` script in `ipc::events`) can report what it sees even when
/// nothing emitted ever arrives. Diagnostic only — never load-bearing.
#[tauri::command]
pub async fn ui_diag(msg: String) -> Result<(), String> {
    crate::ipc::events::diag_line(&format!("mnemo[ui] {msg}"));
    Ok(())
}

/// List all agents.
#[tauri::command]
pub async fn list_agents(state: State<'_, IpcState>) -> Result<Vec<AgentInfo>, String> {
    // Snapshot agent ids + names + running + parent under the manager lock,
    // then DROP the manager lock before reading provider models under the
    // agent_loops lock. This avoids holding both locks simultaneously (the
    // nested acquisition was an undocumented lock-ordering invariant — A3/P4).
    let snapshots: Vec<(AgentId, String, bool, Option<AgentId>)> = {
        let manager = state.runtime.manager.lock().await;
        manager
            .list()
            .into_iter()
            .map(|h| (h.id, h.name.clone(), h.is_running(), h.parent_id))
            .collect()
    };
    // Now read each agent's effective model under the agent_loops lock (the
    // manager lock is already dropped). A loop removed between the snapshot
    // and here yields `None` for the model (the agent exited). Prefer the
    // per-turn resolved model (the per-context override that actually ran)
    // over the default provider slot — so the UI reflects `[models]` state/
    // skill/subagent overrides instead of always showing the default model.
    let agent_loops = state.runtime.agent_loops.lock().await;
    let agents = snapshots
        .into_iter()
        .map(|(id, name, running, parent_id)| {
            let (model, provider, reasoning_effort) = agent_loops
                .get(&id)
                .map(|l| {
                    (
                        Some(
                            l.resolved_model()
                                .unwrap_or_else(|| l.provider().model().to_string()),
                        ),
                        l.effective_provider_name(),
                        // The last turn's effort, else the factory-stamped
                        // default (no turn has run yet — backlog 51dab4da).
                        l.resolved_effort().or_else(|| l.default_display_effort()),
                    )
                })
                .unwrap_or((None, None, None));
            AgentInfo {
                id,
                name,
                running,
                parent_id,
                model,
                provider,
                reasoning_effort,
            }
        })
        .collect();
    Ok(agents)
}

/// Per-agent context-window max (in tokens), returned by `context_caps`.
///
/// The frontend seeds each agent's ctx bar with this at startup (right after
/// `list_agents`), so the bar shows "0 / <max>" before the first turn's
/// `ContextUsage` event. Only the **max** is returned — never the live `used`
/// count — so a late call can never clobber an in-flight turn's used value
/// with a stale 0.
#[tauri::command]
pub async fn context_caps(state: State<'_, IpcState>) -> Result<Vec<(AgentId, u32)>, String> {
    let agent_loops = state.runtime.agent_loops.lock().await;
    Ok(agent_loops
        .iter()
        .map(|(id, l)| (*id, l.context_manager().max_tokens() as u32))
        .collect())
}

/// Spawn a new agent with the given name.
///
/// Allocates a new agent id, creates a command channel + `AgentTask` (built
/// via the `AgentLoopFactory` so the new agent gets its own workflow + tool
/// registry while sharing the provider, memory store, sandbox, and safety
/// rules), spawns the task, and registers the handle. The new agent's loop is
/// tracked in the `agent_loops` map so `get_workflow_state(agent_id)` can read
/// its plan. Returns the new agent's id + name.

/// Switch the active model (or reasoning effort) at runtime — per agent.
///
/// Models are agent-specific (user decision 2026-08-22): the GUI model picker
/// switches ONLY the given agent's model. When `agent_id` is `Some`, the
/// provider is rebuilt for the chosen endpoint + model (resolving the API key
/// from `keys.toml`, falling back to env vars) and swapped into that agent's
/// loop only — the factory and every other live agent loop are untouched, so
/// other agents keep their models and newly spawned agents keep the
/// configured default. A `ModelChanged` event is emitted for that agent only.
/// `None` keeps the legacy global behavior (factory + every live loop +
/// per-agent events) for back-compat.
///
/// The per-agent branch PINs the new provider on that agent's loop
/// (`AgentLoop::set_explicit_provider`) so the picker's choice wins over the
/// configured `[models.*]` override chain for the workflow state it was
/// picked in — an explicit live user choice must not be silently discarded
/// by a state/skill override (bug 2026-08-22: switching kimi → deepseek,
/// then the next turn ran kimi again via `[models.planning]`). The pin is
/// STATE-SCOPED (2026-12-20): on a later workflow-state change a configured
/// `[models.*]` model takes over; with nothing configured for the new state
/// the pin keeps serving.
///
/// The context manager is rebuilt from the new provider's max context so
/// summarization triggers at the right threshold.
///
/// `reasoning_effort` is the raw dropdown value (`max`/`high`/`medium`/`low`/
/// `minimal`/`off`). `"off"` omits the field from requests entirely; `None`
/// falls back to the endpoint's configured value (default `max`). The toolbar
/// effort dropdown reuses this command with the same model to swap the effort
/// live.
///
/// When the endpoint has `supports_reasoning_effort = false`, the parameter
/// is **never** sent — the dropdown value is ignored so non-reasoning models
/// cannot receive an unsupported field.
///
/// Returns an error if the endpoint or model isn't found in `endpoints.toml`,
/// or (per-agent) if the agent id is not a live agent loop.
#[tauri::command]
pub async fn set_model(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
    agent_id: Option<AgentId>,
    endpoint_name: String,
    model: String,
    reasoning_effort: Option<String>,
) -> Result<(), IpcError> {
    // Fail fast BEFORE doing any work: without a factory there's nothing to
    // swap the provider into. Checked first so a failed-brain startup doesn't
    // waste a provider build (or read API-key env vars) before erroring.
    let factory = match &state.runtime.factory {
        Some(f) => Arc::clone(f),
        None => {
            return Err("agent factory unavailable (brain failed to start)".into());
        }
    };

    // Resolve the endpoint + validate the model + build the provider while
    // holding the config lock (short — no awaits in this block). The shared
    // resolver (config_io::resolve_model_provider) is also used by the console
    // runtime, so both surfaces behave identically — no drift on validation,
    // effort mapping, or key resolution.
    let crate::ipc::config_io::ResolvedModel {
        provider,
        kind_label,
        display_effort,
        ..
    } = {
        let config = state.project.config.lock().await;
        crate::ipc::config_io::resolve_model_provider(
            &config,
            &endpoint_name,
            &model,
            reasoning_effort.as_deref(),
            &state.trace,
        )?
    };

    // Rebuild the context manager from the new provider's max context.
    let context_manager = factory.context_manager_for(provider.as_ref());

    match agent_id {
        // Per-agent switch: models are agent-specific — swap ONLY the target
        // agent's loop (the factory + other agents are untouched) and push the
        // new model to the UI for that agent only.
        Some(id) => {
            match crate::ipc::config_io::swap_provider_into_loop(
                &state.runtime.agent_loops,
                id,
                provider,
                context_manager,
                Some(display_effort.clone()),
            )
            .await
            {
                crate::ipc::config_io::SwapOutcome::NotFound => {
                    return Err(format!("no agent loop for agent {id}").into());
                }
                crate::ipc::config_io::SwapOutcome::Swapped => {
                    crate::ipc::events::emit_agent_event(
                        &app,
                        id,
                        SerializableAgentEvent::ModelChanged {
                            model: model.clone(),
                            // The picker named the endpoint explicitly — the
                            // authoritative serving endpoint; no frontend
                            // resolution needed.
                            provider: Some(endpoint_name.clone()),
                            // The DISPLAY-space effort the new provider was
                            // built with (the requested value, or the model's
                            // default) — the status bar shows it (backlog
                            // 51dab4da).
                            reasoning_effort: Some(display_effort.clone()),
                        },
                    );
                    eprintln!("switched agent {id} to model '{model}' (provider {kind_label})");
                }
                crate::ipc::config_io::SwapOutcome::Deferred => {
                    // The new model has a smaller context window — the swap is
                    // deferred so the turn loop can summarize first. Emit a
                    // note so the user knows what happened.
                    crate::ipc::events::emit_agent_event(
                        &app,
                        id,
                        SerializableAgentEvent::Error {
                            error: format!(
                                "Switching to '{model}' (smaller context) — \
                                 conversation will be summarized on the next turn"
                            ),
                            retrying: false,
                        },
                    );
                    eprintln!(
                        "deferred switch for agent {id} to model '{model}' \
                         (provider {kind_label}) — smaller context, will summarize"
                    );
                }
            }
        }
        // Legacy global switch: factory + every live agent loop (the console
        // REPL and any pre-per-agent callers).
        None => {
            let _ = crate::ipc::config_io::swap_live_provider(
                &state,
                &app,
                provider,
                context_manager,
                model.clone(),
                Some(display_effort.clone()),
            )
            .await;
            eprintln!("switched model to '{model}' (provider {kind_label})");
        }
    }

    Ok(())
}

/// Get the workflow state + plan for a specific agent.
///
/// Each agent owns its own `Workflow` (built by the `AgentLoopFactory`), so
/// the plan is per-agent. Pass the `agent_id` of the agent whose plan you want
/// (typically the active agent in the sidebar).
///
/// Parented subagents report `WorkflowState::Subagent` (stamped by the spawn
/// path) while their plan stack is the READ-ONLY MIRROR of the main plan —
/// so a subagent tab shows the "subagent" state badge alongside the main
/// plan's staircase, and `current_plan` (available to reviewers) still
/// resolves the mirrored plan.
#[tauri::command]
pub async fn get_workflow_state(
    state: State<'_, IpcState>,
    agent_id: AgentId,
) -> Result<WorkflowStateInfo, IpcError> {
    let agent_loops = state.runtime.agent_loops.lock().await;
    let agent_loop = agent_loops
        .get(&agent_id)
        .ok_or_else(|| format!("no agent loop for agent {agent_id}"))?;
    let workflow = agent_loop.workflow_handle();
    let workflow = workflow.lock().await;
    let skill = workflow.active_skill().map(|s| ActiveSkillInfo {
        name: s.name.clone(),
        prompt: s.prompt.clone(),
        target_state: s.target_state,
    });
    Ok(WorkflowStateInfo {
        state: workflow.state(),
        plan: workflow.plan().cloned(),
        depth: workflow.plan_depth(),
        parents: workflow
            .parent_infos()
            .into_iter()
            .map(|(id, title)| PlanAncestorInfo { id, title })
            .collect(),
        skill,
    })
}

/// Read a plan by its on-disk id (the file stem of `.coding/plans/<id>.md`).
///
/// Used by the frontend's clickable plan staircase: clicking an ancestor plan
/// fetches + displays it read-only (the ancestor isn't the active plan, so it
/// can't be mutated — this is a view, not an edit). Returns the parsed
/// [`PlanFile`] or an error if the plan id isn't found / fails to parse.
///
/// **Security:** `plan_id` is validated before joining into a path — plan ids
/// are hex handles minted by `create_plan` (legacy ids are 36-char UUIDs); a
/// value containing path separators or dots (e.g. `../../Cargo`) is rejected
/// before any filesystem access. This mirrors the sandbox's path-traversal
/// defense (the plans dir is sandboxed `.coding/plans/`).
#[tauri::command]
pub async fn get_plan(
    state: State<'_, IpcState>,
    agent_id: AgentId,
    plan_id: String,
) -> Result<PlanFile, IpcError> {
    // Validate the plan_id before joining it into a path. Plan ids are hex
    // handles minted by create_plan (legacy ids are 36-char UUIDs) — reject
    // anything containing path separators or `..` (a path-traversal attempt
    // like "../../Cargo"). This is the defense-in-depth the sandbox applies
    // to file tools; get_plan bypasses the sandbox (it reads the plans dir
    // directly), so this check is the guard. A plan id has no path
    // separators + no dots.
    if plan_id.is_empty()
        || plan_id.contains('/')
        || plan_id.contains('\\')
        || plan_id.contains("..")
        || plan_id.contains('.')
    {
        return Err(format!(
            "invalid plan id '{plan_id}' (path separators / '..' / '.' are rejected)"
        )
        .into());
    }
    let agent_loops = state.runtime.agent_loops.lock().await;
    let agent_loop = agent_loops
        .get(&agent_id)
        .ok_or_else(|| format!("no agent loop for agent {agent_id}"))?;
    let workflow = agent_loop.workflow_handle();
    let workflow = workflow.lock().await;
    let plans_dir = workflow.plans_dir().to_path_buf();
    drop(workflow);
    drop(agent_loops);
    let path = plans_dir.join(format!("{plan_id}.md"));
    if !path.exists() {
        return Err(format!("plan '{plan_id}' not found in {}", plans_dir.display()).into());
    }
    PlanFile::read_from_file(&path)
        .map_err(|e| format!("failed to read plan '{plan_id}': {e}"))
        .map_err(IpcError::from)
}
/// Resolve the two prompts of a UI-initiated skill entry: the OVERLAY (what the
/// system prompt carries every turn the skill runs) and the DISPATCH message
/// (what the agent is told to do now). The overlay is ALWAYS the registry prompt
/// — the skill file owns its procedure, so a caller naming a target (the Git tab
/// picks a branch) can neither rewrite nor drop the steps. A blank argument
/// falls back to the overlay so the agent always receives the goal.
fn skill_prompts(spec_prompt: &str, dispatch: Option<String>) -> (String, String) {
    match dispatch {
        Some(text) if !text.trim().is_empty() => (spec_prompt.to_string(), text),
        _ => (spec_prompt.to_string(), spec_prompt.to_string()),
    }
}

/// Enter a skill on the given agent: lock its workflow, call `start_skill`,
/// and send an `AgentCommand::Prompt` to the agent carrying the dispatch
/// message. This is the UI-initiated entry path (the "Merge to main" button's
/// confirm dialog). The agent-driven path is the `skill_start` tool (which goes
/// through the normal approval gate). Both enter the same skill state.
///
/// `skill` selects the registry entry (tool allow-list + prompt + target_state).
/// `prompt` is the DISPATCH message sent to the agent — the Git tab names the
/// branch to merge there. It never replaces the registry prompt, which stays the
/// overlay injected into the system prompt every turn while the skill runs (see
/// [`skill_prompts`]); omitting it dispatches the registry prompt itself. The
/// skill must be `available_in` the agent's current workflow state.
///
/// After `start_skill` succeeds, a `SkillStarted` event is emitted on the
/// agent event channel so the frontend announces the skill in the transcript
/// (`▶ skill "name" start`) — mirroring how `skill_end` already appears and
/// how the agent-driven `skill_start` tool shows a ToolCard. The agent-driven
/// path does NOT emit this event (its ToolCard is the announcement); emitting
/// here too would duplicate.
#[tauri::command]
pub async fn enter_skill(
    app: tauri::AppHandle,
    state: State<'_, IpcState>,
    agent_id: AgentId,
    skill: String,
    prompt: Option<String>,
) -> Result<serde_json::Value, IpcError> {
    let factory = state
        .runtime
        .factory
        .as_ref()
        .ok_or_else(|| "agent factory unavailable (brain failed to start)".to_string())?;
    // Look up the skill in the library held by the factory.
    let library = factory
        .skills_handle()
        .ok_or_else(|| "no skill library configured".to_string())?;
    let spec = library
        .read(|r| r.get(&skill).cloned())
        .ok_or_else(|| format!("unknown skill '{skill}'"))?;

    let agent_loops = state.runtime.agent_loops.lock().await;
    let agent_loop = agent_loops
        .get(&agent_id)
        .ok_or_else(|| format!("no agent loop for agent {agent_id}"))?;
    let workflow_arc = agent_loop.workflow_handle();
    drop(agent_loops);

    let current = {
        let wf = workflow_arc.lock().await;
        wf.state()
    };
    if !library.read(|r| r.is_available_in(&skill, current)) {
        return Err(format!("skill '{skill}' is not available in the {current} state").into());
    }
    let (overlay, dispatch) = skill_prompts(&spec.prompt, prompt);
    {
        let mut wf = workflow_arc.lock().await;
        wf.start_skill(&skill, &overlay, spec.target_state, spec.tools.clone())
            .map_err(|e| format!("failed to start skill: {e}"))?;
    }

    // Announce the skill in the transcript like a tool call does — emitted
    // before the Prompt so it lands before the agent's `Started` event.
    emit_agent_event(
        &app,
        agent_id,
        SerializableAgentEvent::SkillStarted {
            name: skill.clone(),
            prompt: dispatch.clone(),
        },
    );

    // Send the dispatch message so the agent drives toward it. The skill's tool
    // allow-list + the registry prompt overlay are now active.
    let manager = state.runtime.manager.lock().await;
    manager
        .send(
            agent_id,
            AgentCommand::Prompt {
                text: dispatch,
                images: vec![],
            },
        )
        .map_err(|e| format!("failed to send skill prompt: {e:?}"))?;
    drop(manager);

    Ok(serde_json::json!({
        "success": true,
        "skill": skill,
        "target_state": spec.target_state,
    }))
}

/// Get per-session token + timing stats for the agent with the given id.
///
/// The stats are read from the memory store's `request_stats` table, joined to
/// the agent's current session id (read from its `AgentLoop`). Returns an
/// error if the agent has no loop, no memory store, or no session yet.
#[tauri::command]
pub async fn get_session_stats(
    state: State<'_, IpcState>,
    agent_id: AgentId,
) -> Result<serde_json::Value, IpcError> {
    let agent_loops = state.runtime.agent_loops.lock().await;
    let agent_loop = agent_loops
        .get(&agent_id)
        .ok_or_else(|| format!("no agent loop for agent {agent_id}"))?;
    let session_id = agent_loop
        .session_id()
        .ok_or_else(|| "agent has no session yet (no prompt sent)".to_string())?;
    drop(agent_loops);

    let factory = state.factory()?;
    let store = factory
        .memory_handle()
        .ok_or_else(|| "no memory store configured".to_string())?;

    let stats = store
        .session_stats(&session_id)
        .await
        .map_err(|e| format!("failed to read session stats: {e}"))?;
    Ok(serde_json::to_value(&stats).map_err(|e| format!("serialize: {e}"))?)
}

/// Get per-project token + timing stats (aggregated across all sessions).
///
/// Reads from the memory store's `request_stats` + `sessions` tables.
#[tauri::command]
pub async fn get_project_stats(state: State<'_, IpcState>) -> Result<serde_json::Value, IpcError> {
    let factory = state.factory()?;
    let store = factory
        .memory_handle()
        .ok_or_else(|| "no memory store configured".to_string())?;

    let stats = store
        .project_stats()
        .await
        .map_err(|e| format!("failed to read project stats: {e}"))?;
    Ok(serde_json::to_value(&stats).map_err(|e| format!("serialize: {e}"))?)
}

/// Get per-project token-SAVINGS stats for the Dashboard view (aggregated
/// across all sessions) — backlog 652ae094.
///
/// Reads the memory store's `savings_events` ledger (written by the optimizer
/// levers, backlog e4a50d22) plus the prompt-cache numbers from
/// `request_stats`.
#[tauri::command]
pub async fn get_savings_stats(state: State<'_, IpcState>) -> Result<serde_json::Value, IpcError> {
    let factory = state.factory()?;
    let store = factory
        .memory_handle()
        .ok_or_else(|| "no memory store configured".to_string())?;

    let stats = store
        .savings_stats()
        .await
        .map_err(|e| format!("failed to read savings stats: {e}"))?;
    Ok(serde_json::to_value(&stats).map_err(|e| format!("serialize: {e}"))?)
}

/// Get the list of sessions with aggregated token counts (for the Stats tab's
/// session list). Ordered newest-first.
#[tauri::command]
pub async fn get_session_list(state: State<'_, IpcState>) -> Result<serde_json::Value, IpcError> {
    let factory = state.factory()?;
    let store = factory
        .memory_handle()
        .ok_or_else(|| "no memory store configured".to_string())?;

    let list = store
        .session_list()
        .await
        .map_err(|e| format!("failed to read session list: {e}"))?;
    Ok(serde_json::to_value(&list).map_err(|e| format!("serialize: {e}"))?)
}

#[cfg(test)]
mod tests {
    use crate::ipc::contract_fixtures::normalize_lf;
    /// Regression (run-all dispatches the next backlog item into the main
    /// agent during steering, 2026-08-31): `send_suggestion` must halt an
    /// active run-all BEFORE sending the steer command. Without the halt, the
    /// steer's soft-stop `Finished` resolves the in-flight item and
    /// `run_all_dispatch_next` dispatches item N+1 into the main agent —
    /// interleaving the user's steer with the next backlog item. The halt
    /// must precede `send_cmd` so the stop flag + intervention latch are
    /// in place before the soft-stop `Finished` arrives — the intervention
    /// path then keeps the item InFlight with its run stopped (race
    /// avoidance). Source-contract test:
    /// the command needs a Tauri `AppHandle`, mirroring
    /// `backlog_retry_routes_through_guarded_requeue`.
    #[test]
    fn send_suggestion_halts_run_all_before_sending() {
        let src = normalize_lf(include_str!("agent.rs"));
        let start = src
            .find("pub async fn send_suggestion")
            .expect("send_suggestion command present");
        // The first column-0 "\n}\n" after the fn start is the function's own
        // closing brace (nested blocks are indented, so they never match).
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        // Match the call tokens WITH their opening paren so inline comments
        // mentioning the names (e.g. "must run BEFORE send_cmd") don't match.
        let halt_pos = body
            .find("halt_run_all(")
            .expect("send_suggestion must call halt_run_all (halt run-all on steer)");
        let send_pos = body
            .find("send_cmd(")
            .expect("send_suggestion must call send_cmd");
        assert!(
            halt_pos < send_pos,
            "halt_run_all must run BEFORE send_cmd so the stop flag + latch \
              are in place before the steer's soft-stop Finished arrives \
              (race avoidance)"
        );
    }

    /// Regression (user report, 2026-12): a steer on the main agent mid-item
    /// must record the intervention latch (BEFORE halting) so the soft-stop
    /// turn's resolution disposes of the item non-terminally instead of
    /// failing it — but ONLY when the agent is actually running (a steer on
    /// an idle agent interrupts no turn; a lingering latch would misfire on
    /// a later resolution). Source-contract test: the command needs a Tauri
    /// `AppHandle`.
    #[test]
    fn send_suggestion_records_the_intervention_latch_before_halting() {
        let src = normalize_lf(include_str!("agent.rs"));
        let start = src
            .find("pub async fn send_suggestion")
            .expect("send_suggestion command present");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        let latch = body
            .find("user_intervention")
            .expect("send_suggestion must record the intervention latch");
        let halt = body
            .find("halt_run_all(")
            .expect("send_suggestion must halt run-all");
        assert!(
            latch < halt,
            "the latch must be recorded BEFORE halting — the soft-stop turn's resolution consumes it to dispose of the item non-terminally"
        );
        assert!(
            body.contains("is_running()"),
            "the latch is only set when the agent is actually running — a \
             lingering latch on an idle agent would misfire later"
        );
        // Review finding HIGH-2 (2026-12-05): a second intervention on the
        // same turn must MERGE into the latch — overwriting would drop an
        // item id already captured by halt_run_all and strand the item.
        assert!(
            body.contains("get_or_insert_with(") && !body.contains("*latch = Some("),
            "the latch must be merged into (get_or_insert_with), never \
             overwritten"
        );
    }

    /// Regression (user report, 2026-12): an interrupt is a user intervention
    /// too — the in-flight item must be disposed of non-terminally, not
    /// failed. The latch is set only when the agent is RUNNING (an
    /// idle-agent interrupt starts no turn, so the latch would linger and
    /// misfire on a later resolution). Source-contract test: the command
    /// needs Tauri state.
    #[test]
    fn interrupt_records_the_latch_only_when_running() {
        let src = normalize_lf(include_str!("agent.rs"));
        let start = src
            .find("pub async fn interrupt(")
            .expect("interrupt command present");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        let running_check = body
            .find("is_running()")
            .expect("interrupt must gate the latch on the agent actually running");
        let latch = body
            .find("user_intervention")
            .expect("interrupt must record the intervention latch");
        let send = body
            .find("send_cmd(")
            .expect("interrupt must still send the Interrupt command");
        assert!(
            running_check < latch && latch < send,
            "running-check, then latch, then send_cmd"
        );
        // Review finding HIGH-2 (2026-12-05): merge, never overwrite — an
        // interrupt after a steer must preserve the captured item id.
        assert!(
            body.contains("get_or_insert_with(") && !body.contains("*latch = Some("),
            "the latch must be merged into (get_or_insert_with), never \
             overwritten"
        );
    }

    /// The UI-initiated entry path may name a TARGET but never rewrite the
    /// skill's procedure: whatever `enter_skill`'s `prompt` argument carries,
    /// the overlay injected into the system prompt stays the registry prompt.
    /// Regression against the Git tab's former behavior, which passed its own
    /// full copy of the merge steps and thereby REPLACED the skill file's prompt
    /// — together with the standing rules that prompt carries (which tool may
    /// run the merge, never stash, `.coding/` must land).
    #[test]
    fn skill_entry_keeps_the_registry_prompt_as_the_overlay() {
        let spec = "1. Commit ALL work.\n2. Merge it.";
        let (overlay, dispatch) = super::skill_prompts(spec, Some("Merge 'wt/x' into main.".into()));
        assert_eq!(overlay, spec, "the overlay is always the registry prompt");
        assert_eq!(
            dispatch, "Merge 'wt/x' into main.",
            "the argument only supplies the dispatch message"
        );

        // No argument (the status-bar button) dispatches the registry prompt.
        let (overlay, dispatch) = super::skill_prompts(spec, None);
        assert_eq!(overlay, spec);
        assert_eq!(dispatch, spec);

        // A blank argument cannot blank the turn either.
        let (overlay, dispatch) = super::skill_prompts(spec, Some("   ".into()));
        assert_eq!(overlay, spec);
        assert_eq!(dispatch, spec);

        // Body contract: the command wires the overlay resolved by `skill_prompts`
        // into `start_skill`, and never the caller's argument. Reverting to
        // `start_skill(&skill, &prompt.unwrap_or_else(...))` fails here.
        let src = normalize_lf(include_str!("agent.rs"));
        let start = src
            .find("pub async fn enter_skill(")
            .expect("enter_skill command present");
        let end = src[start..]
            .find("\n}\n")
            .map(|i| start + i)
            .unwrap_or(src.len());
        let body = &src[start..end];
        assert!(
            body.contains("let (overlay, dispatch) = skill_prompts(&spec.prompt, prompt);"),
            "enter_skill must resolve the overlay + dispatch through skill_prompts"
        );
        assert!(
            body.contains("start_skill(&skill, &overlay, spec.target_state"),
            "the overlay handed to start_skill is the registry prompt, not the argument"
        );
        assert!(
            !body.contains("prompt.unwrap_or_else"),
            "the prompt argument must never become the overlay"
        );
    }
}
