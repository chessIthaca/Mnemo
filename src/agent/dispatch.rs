// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Tool-call dispatch + the approval gate + provider-call retry.
//!
//! `execute_tool_call` runs one tool call end-to-end: parse arguments →
//! re-check the workflow `ToolFilter` (so a hallucinated name omitted from
//! the schema still cannot run) → check the approval gate (safety mode +
//! safety rules + sandbox) → execute → return the result alongside any
//! commands buffered while awaiting approval.
//! `complete_with_retry` wraps the provider's streaming `complete` with
//! jittered exponential backoff (equal jitter — see `retry_backoff_ms`) so a
//! transient gateway error doesn't kill the turn.

use std::path::Path;
use std::sync::Arc;

use tokio::sync::{mpsc, oneshot};

use super::approval;
use super::loop_impl::AgentLoop;
use crate::error::Result;
use crate::provider::{LlmClient, LlmEvent, Message, ToolCall};
use crate::runtime::{AgentCommand, AgentEvent, AgentId};
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::{OutputSink, ToolCall as ParsedToolCall, ToolFilter, ToolResult};
use crate::workflow::WorkflowState;

/// The file tools [`ToolFilter::ExecutingResearch`] denies wholesale — the set
/// a research plan may nevertheless use on an ARTIFACT target (`.coding/**`).
const RESEARCH_ARTIFACT_TOOLS: [&str; 4] = [
    "file_edit",
    "file_write",
    "file_append",
    "convert_line_endings",
];

/// The verdict for a file-tool call a research plan's filter denied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResearchWrite {
    /// The call IS the artifact carve-out: let it through.
    Artifact,
    /// A file tool denied because the target is not an artifact (source, docs,
    /// config) — the research boundary is the reason.
    NotAnArtifact,
    /// A file tool denied because the target is protected in EVERY state
    /// (plans/reviews/knowledge/backlog/DBs/safety.toml/`.git`). The research
    /// boundary is not the reason, and no plan kind unlocks it.
    Protected,
    /// Not a carve-out candidate: another tool name, or a call without a
    /// usable `path` argument.
    NotApplicable,
}

/// Classify a call the research filter denied: the `.coding/**` artifact
/// carve-out, a plain non-artifact target, or a protected one?
///
/// A research plan's contract is "no SOURCE-code change" — that is what makes
/// its skipped review honest. Its own analysis artifacts are the opposite of a
/// source change, and denying them had two bad outcomes: the deliverable landed
/// through the approval-gated `shell` (bypassing the file tools' diff preview
/// and path safety), or the plan got misfiled as `implementation` and paid a
/// review of a diff that does not exist.
///
/// The carve-out is deliberately narrow. The raw argument is resolved the way
/// the file tool's own ladder does — CANONICAL first when the target (or its
/// parent) already exists, so a symlink or a directory junction planted inside
/// `.coding/` cannot launder a source path into an artifact grant — then the
/// lexical creation form for a genuinely new tree, and only when no existing
/// component of that form is a link (a link that escapes the root must never be
/// granted from its spelling: the OS would follow it at write time).
/// [`Sandbox::is_artifact_write_target`] judges the resolved path, fails closed,
/// and excludes everything [`Sandbox::is_protected_write_target`] refuses.
/// Source, docs, config, protected targets, `..` escapes and links out of
/// `.coding/` all stay denied.
fn research_write_verdict(
    sandbox: &Sandbox,
    name: &str,
    args: &serde_json::Value,
) -> ResearchWrite {
    if !RESEARCH_ARTIFACT_TOOLS.contains(&name) {
        return ResearchWrite::NotApplicable;
    }
    let Some(raw) = args.get("path").and_then(|v| v.as_str()) else {
        return ResearchWrite::NotApplicable;
    };
    // Canonical first: `validate` follows links and junctions, so a `.coding/`
    // path that really lands on source is judged where it lands.
    if let Ok(canonical) = sandbox.validate(Path::new(raw)) {
        // …but `validate` is NOT canonical for a reparse point whose target does
        // not resolve: its parent fallback appends the RAW file name, so a
        // dangling link LEAF comes back as `<root>/.coding/dangling.md` — inside
        // the root by string prefix while the OS would follow the link at open
        // time and land the bytes outside (review HIGH-1, round 3). A genuinely
        // canonical path has no link component, so the walk is a no-op for it.
        if sandbox.lexical_path_is_link_free(&canonical) {
            return judge_research_write(sandbox, &canonical);
        }
        return ResearchWrite::NotApplicable;
    }
    // Otherwise the lexical creation form is judged — but ONLY when no link
    // component would be followed at write time. A link inside `.coding/`
    // pointing out of the root can make `validate` refuse the path outright
    // (nothing to canonicalize while the link is dangling), and granting the
    // spelling would hand the OS a write that escapes the sandbox
    // (review LOW-1). Fail closed instead.
    match sandbox.validate_for_creation(Path::new(raw)) {
        Ok(lexical) if sandbox.lexical_path_is_link_free(&lexical) => {
            judge_research_write(sandbox, &lexical)
        }
        _ => ResearchWrite::NotApplicable,
    }
}

/// The verdict for an already-resolved path: the artifact carve-out, a plain
/// non-artifact target, or a protected one. The two denying kinds can never
/// overlap with `Artifact`: the artifact predicate ends with
/// `!is_protected_write_target`, so a protected path is never an artifact.
fn judge_research_write(sandbox: &Sandbox, path: &Path) -> ResearchWrite {
    if sandbox.is_artifact_write_target(path) {
        ResearchWrite::Artifact
    } else if sandbox.is_protected_write_target(path) {
        ResearchWrite::Protected
    } else {
        ResearchWrite::NotAnArtifact
    }
}

/// The denial text for a call the workflow filter refused. A research plan
/// denied on a file tool gets the boundary named explicitly, and a target that
/// is protected in EVERY state gets THAT reason instead: the two have
/// different remedies ("push an implementation sub-plan" does not unlock a
/// protected file), so one shared message would send the model after the wrong
/// fix.
fn denial_message(name: &str, state: WorkflowState, verdict: ResearchWrite) -> String {
    match verdict {
        ResearchWrite::Protected => format!(
            "tool '{name}' is not allowed here: the target is a protected, app-owned file \
             (plans, reviews, knowledge, backlog, the memory/codegraph DBs, safety.toml and \
             .git are never writable by the file tools, in any workflow state) — use the \
             dedicated tools (plan/memory/safety/git) instead"
        ),
        ResearchWrite::NotAnArtifact => format!(
            "tool '{name}' is not allowed here: the active plan is a research plan ({state}), \
             which may write only its own artifacts under .coding/ — source, docs and other \
             app-owned files stay read-only; push an implementation sub-plan to change source"
        ),
        // The carve-out is granted, so this arm is unreachable from the gate —
        // or the call is not a carve-out candidate at all.
        ResearchWrite::Artifact | ResearchWrite::NotApplicable => format!(
            "tool '{name}' is not allowed in the current workflow state ({state})"
        ),
    }
}

impl AgentLoop {
    /// Execute a single tool call, handling approval + retries.
    ///
    /// Returns the tool result alongside any commands that were buffered
    /// while awaiting approval (e.g. a `Suggestion` that arrived mid-wait).
    /// The caller is responsible for re-injecting those into the conversation.
    ///
    /// `deny_all_latched` is a turn-level flag (Quality M1): once the user
    /// answers `DenyAll` on any approval, remaining tool calls in this turn
    /// — including later LLM iterations — skip the prompt and return a
    /// synthetic denial. The flag is set here when `DeniedAll` is received.
    pub(super) async fn execute_tool_call(
        &self,
        tc: &ToolCall,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        agent_id: AgentId,
        cmd_rx: &mut mpsc::Receiver<AgentCommand>,
        deny_all_latched: &mut bool,
        stop_signal: &mut Option<crate::agent::StopReason>,
    ) -> (ToolResult, Vec<AgentCommand>) {
        // Turn-level DenyAll latch: user already denied the rest of the turn.
        if *deny_all_latched {
            return (
                ToolResult::error(format!(
                    "user denied all remaining actions ({} call skipped)",
                    tc.name
                )),
                Vec::new(),
            );
        }

        // Parse the arguments. A malformed-JSON failure is sanitized
        // (tool::error_message): the model gets a clean, instructive
        // message naming the tool — never the raw serde text, never the
        // raw arguments blob echoed back into the conversation.
        let args: serde_json::Value = match serde_json::from_str(&tc.arguments) {
            Ok(v) => v,
            Err(e) => {
                return (
                    ToolResult::error(crate::tool::error_message::sanitize_arguments_error(
                        &tc.name,
                        &e,
                    )),
                    Vec::new(),
                );
            }
        };

        let parsed_call = ParsedToolCall {
            id: tc.id.clone(),
            name: tc.name.clone(),
            arguments: args,
        };

        // Check if the tool exists + needs approval.
        let tool = match self.tools.get(&tc.name) {
            Some(t) => t,
            None => {
                return (
                    ToolResult::error(format!(
                        "unknown tool '{}'. Available: {}",
                        tc.name,
                        self.available_tool_names()
                    )),
                    Vec::new(),
                );
            }
        };

        // Re-enforce the workflow ToolFilter at dispatch time. Schema filtering
        // only hides tools from the model; a hallucinated name (or a stale
        // call after a state transition) must still be denied here — before
        // approval or execution — so Planning/Skill gates cannot be bypassed.
        let safety = tool.safety();
        let category = tool.category();
        {
            let wf = self.workflow.lock().await;
            let filter = wf.allowed_tools();
            // The one carve-out: a RESEARCH plan may still write its own
            // artifacts under `.coding/**` with the file tools. The verdict is
            // computed once — it also picks the denial text below.
            let research_write = if filter == ToolFilter::ExecutingResearch {
                research_write_verdict(&self.sandbox, &tc.name, &parsed_call.arguments)
            } else {
                ResearchWrite::NotApplicable
            };
            if !filter.allows(category, safety, &tc.name)
                && research_write != ResearchWrite::Artifact
            {
                return (
                    ToolResult::error(denial_message(&tc.name, wf.state(), research_write)),
                    Vec::new(),
                );
            }
            // An mcp.* reveal connects to a foreign server (spawning a
            // process that lingers in the manager) — gate it like the
            // tools it would materialize (Agent + NeedsApproval), so
            // research/planning cannot open a connection whose tools are
            // unusable there (review LOW 2).
            if let Some(group) = parsed_call.arguments.get("group").and_then(|v| v.as_str()) {
                if !crate::tool::agent::load_tools::mcp_reveal_allowed(&filter, group) {
                    return (
                        ToolResult::error(format!(
                            "tool group '{group}' is not available in the current workflow \
                             state ({})",
                            wf.state()
                        )),
                        Vec::new(),
                    );
                }
            }
        }

        // Phase 4 main-agent-only plan policy gate (dispatch layer, after the
        // state ToolFilter check). Sub-agents are built with the flag false so
        // even a hallucinated call is denied here before any approval or side
        // effects. `finish` is included: closing out the plan's review is a
        // main-agent responsibility — a subagent (e.g. an unrestricted one
        // spawned during Reviewing) must never be able to mark the plan
        // reviewed and exit Reviewing on the parent's behalf.
        if !self.plan_mutations_allowed()
            && matches!(
                tc.name.as_str(),
                "create_plan" | "update_plan" | "complete_step" | "abandon_plan" | "finish"
            )
        {
            return (
                ToolResult::error(
                    "plan mutation tools and finish are restricted to the main agent; sub-agents may not call create_plan/update_plan/complete_step/abandon_plan/finish"
                ),
                Vec::new(),
            );
        }

        // Review-exit transition gate (backlog 569b5922): the agent cannot
        // END a workflow phase while spawned subagents are still running.
        // Only the phase-exit transitions are gated:
        //   - finish (Reviewing→Complete) — always: the closing reviewer
        //     must land its report before the plan completes (the
        //     non-empty-report finish gate is the second lock).
        //   - complete_step, but ONLY the final call — the one that would
        //     complete the ROOT plan and exit Executing (→Reviewing on
        //     implementation/bug-fixing plans, →Complete on research
        //     plans). Parallel coders/reviewers must finish before the tree
        //     is reviewed or the plan completes.
        // Everything else is deliberately NOT gated: a non-final
        // complete_step is a checklist tick (a 'spawn reviewer' step
        // completes at spawn, while the reviewer runs); update_plan edits
        // the remaining steps in place; create_plan pushes a (sub-)plan
        // while the parent keeps executing; abandon_plan is the designated
        // failed-review escape hatch (a hung reviewer must not block the
        // escape — the orphaned child's completion notification still
        // reaches the session); skill_* are plan-stack-like overlays, not
        // review exits.
        //
        // The workflow lock acquired above (for the ToolFilter check) has
        // already been dropped at the end of that block; the fresh
        // acquisition below for the finality query is likewise released
        // before the tracker await — no lock is held across an await. The
        // tracker consults the AgentManager (via the IPC-layer
        // DescendantTracker), which walks the parent_id chain under its own
        // lock.
        let exits_phase = match tc.name.as_str() {
            "finish" => true,
            "complete_step" => {
                // Mirror CompleteStepTool's 1-indexed step_index parsing
                // (integer or whitespace-padded numeric string — the tool's
                // StepNumber::parse trims) just enough to ask the workflow
                // whether this call would complete the root plan.
                // Unparseable/zero args fall through ungated — the tool
                // layer produces its own error for those.
                let step_number = parsed_call
                    .arguments
                    .get("step_index")
                    .and_then(|v| {
                        v.as_u64().or_else(|| {
                            v.as_str().and_then(|s| s.trim().parse::<u64>().ok())
                        })
                    });
                match step_number {
                    Some(n) if n >= 1 => self
                        .workflow
                        .lock()
                        .await
                        .completing_step_exits_executing((n - 1) as usize),
                    _ => false,
                }
            }
            _ => false,
        };
        if exits_phase {
            if let Some(tracker) = &self.descendant_tracker {
                if let Some(id) = self.agent_id() {
                    if tracker.has_running_descendants(id).await {
                        return (
                            ToolResult::error(
                                "cannot change workflow state while spawned subagents are still running — wait for them to finish first"
                            ),
                            Vec::new(),
                        );
                    }
                }
            }
        }

        // ask_user is intercepted here (not run via the tool's execute) — it
        // emits a UserQuestion event carrying a oneshot, awaits the answer,
        // and returns it as the ToolResult. This mirrors the approval path:
        // the tool trait's execute() has no access to the fan-in channel or
        // agent id, so the pause is driven from dispatch. One question at a
        // time is automatic — the turn blocks on the oneshot until the user
        // answers, so a second ask_user can't run until this one resolves.
        //
        // Asking the user also clears the failed-reviewer protocol latch: the
        // parent has consulted the user about the failed review, so further
        // reviewer spawns are allowed again (whatever the user decided). When
        // a failure WAS pending, this ask_user is the protocol's user
        // consultation — the retry it sanctions may carry the user-picked
        // explicit model on the reviewer respawn, so open the retry sanction
        // (consumed by the next reviewer spawn, whatever the user decided; an
        // ask_user with no failure pending also closes any lingering sanction
        // from an earlier cycle).
        if tc.name == "ask_user" {
            self.set_reviewer_retry_sanctioned(self.reviewer_failure_pending());
            self.set_reviewer_failure_pending(false);
            return self
                .ask_user(&parsed_call, fanin_tx, agent_id, cmd_rx, stop_signal)
                .await;
        }

        // abandon_plan is the failed-reviewer protocol's escape hatch: any
        // retry sanction open at abandon time will never be a retry — close
        // it (mirrors the ask_user interception's lingering-sanction close;
        // review L2).
        if tc.name == "abandon_plan" {
            self.set_reviewer_retry_sanctioned(false);
        }
        // Failed-reviewer protocol + reviewer-model gates (backlog c8e48f81):
        // every spawn_agent call passes through `reviewer_spawn_gate` — while
        // a reviewer child failed with no report, reviewer spawns are denied
        // (ask the user first); a reviewer spawn carrying an explicit `model`
        // is denied unless the failed-reviewer retry is sanctioned (ask_user
        // opens the sanction; any reviewer spawn consumes it). Full protocol
        // in the gate's doc.
        if tc.name == "spawn_agent" {
            let (denial, sanctioned) = reviewer_spawn_gate(
                self.reviewer_failure_pending(),
                self.reviewer_retry_sanctioned(),
                parsed_call.arguments.get("role").and_then(|r| r.as_str()),
                parsed_call.arguments.get("model").and_then(|m| m.as_str()),
            );
            self.set_reviewer_retry_sanctioned(sanctioned);
            if let Some(denial) = denial {
                return (denial, Vec::new());
            }
        }

        // C5: symbol-lookup redirect gate — when search/search_read is called
        // with a pattern that names an indexed symbol AND the agent has
        // ignored the symbol nudge >= ESCALATION_THRESHOLD times without
        // switching, intercept the call (the advisory nudge was ignored; the
        // gate has teeth). The gate lifts when the agent switches to a graph
        // tool once. The intercepted call returns a redirect result pointing
        // at graph_context(id=...) — the search does not run, so no nudge
        // fires and observe_result is never called (the fired count is not
        // inflated by the redirect's own "is an indexed symbol" text).
        // observe_call is also skipped: it unconditionally consumes the
        // per-agent last_fired note, and since "search" is not a SearchNudge
        // target, running it here would consume the note without counting a
        // switch — leaving the agent's redirect-following graph_context call
        // with no note to match, so the gate would never lift. Skipping it
        // preserves the note so that graph_context call registers the switch.
        if matches!(tc.name.as_str(), "search" | "search_read") {
            if let Some(pattern) = parsed_call
                .arguments
                .get("pattern")
                .and_then(|v| v.as_str())
            {
                if let Some(redirect) = symbol_search_redirect(
                    &self.graph,
                    &tc.name,
                    pattern,
                    crate::agent::steering_stats::SteeringStats::shared(),
                    agent_id,
                ) {
                    return (redirect, Vec::new());
                }
            }
        }

        // C5-family (backlog 714196da): the file_edit stale-read gate — after
        // a drift-class "old_string not found" failure on path P (the
        // EditStaleRead marker fires on the failed result's error text) with
        // no intervening fresh read of P or landed edit on it, intercept the
        // next file_edit call on P: the agent is retrying blind from memory,
        // and the gate forces the fresh read the nudge asked for. The state
        // is per-(agent, path), recorded by the funnel's result bookkeeping
        // (`observe_edit_freshness`), so interleaved calls cannot freeze it
        // (plan be16ea36 step 3). Like the C5 gate above, the intercepted
        // call never runs — no observe_result feedback.
        if tc.name == "file_edit" {
            if let Some(path) = parsed_call.arguments.get("path").and_then(|v| v.as_str()) {
                if let Some(redirect) = file_edit_redirect(
                    crate::agent::steering_stats::SteeringStats::shared(),
                    agent_id,
                    path,
                ) {
                    return (redirect, Vec::new());
                }
            }
        }

        let mode = *self.safety_mode.read().expect("safety_mode lock poisoned");
        // A tool whose *specific arguments* mark it as a core operation via
        // `never_auto_for()` (e.g. `git merge` / `git push`) must show the
        // interactive approval prompt unconditionally. This holds even under
        // Autonomous mode (which would otherwise skip approval for every tool)
        // and even if a safety rule would otherwise auto-approve it. This
        // enforces the hard user-sanction invariant those operations depend
        // on: landing commits on main / pushing to a remote always requires a
        // contemporaneous user approval.
        let force_prompt = tool.never_auto_for(&parsed_call.arguments);
        // Live-output sink for this call: throttled partial output from a
        // running tool is forwarded to the UI as ToolOutputDelta events tagged
        // with this call's id, so concurrent calls land in their own cards. The
        // sink is inert until the tool actually runs — a denied call emits
        // nothing — and it never affects the result.
        let output_sink = self.output_sink(fanin_tx, agent_id, &parsed_call.id);
        if force_prompt
            || approval::needs_approval(
                safety,
                mode,
                &tc.name,
                &parsed_call.arguments,
                &self.sandbox,
            )
        {
            // Safety-rules shortcut: if a rule matches this call, skip the
            // approval prompt and execute directly. The rules file is
            // mtime-checked inside `is_safe`, so edits in the Safety tab take
            // effect without restarting. Never applies to a never_auto tool.
            let auto_approved = !force_prompt
                && self
                    .safety_rules
                    .as_ref()
                    .map(|sr| sr.is_safe(&tc.name, &parsed_call.arguments))
                    .unwrap_or(false);
            if auto_approved {
                let mut buffered = Vec::new();
                let result = self
                    .dispatch_with_interrupt(
                        &parsed_call,
                        agent_id,
                        cmd_rx,
                        stop_signal,
                        &mut buffered,
                        output_sink.clone(),
                    )
                    .await;
                return (result, buffered);
            }

            // Pure preview for the approval UI (file tools: unified diff /
            // new-file content). Best-effort — failures leave preview None so
            // the FE can still fall back to args parsing.
            let preview = tool.approval_preview(&parsed_call.arguments);

            // Request approval.
            let (approval_tx, approval_rx) = oneshot::channel();
            let _ = fanin_tx
                .send((
                    agent_id,
                    AgentEvent::ApprovalRequest {
                        tool_call_id: tc.id.clone(),
                        tool_name: tc.name.clone(),
                        args: parsed_call.arguments.clone(),
                        preview,
                        core_operation: force_prompt,
                        responder: approval_tx,
                    },
                ))
                .await;

            // Wait for approval (while listening for interrupts).
            // Non-interrupt commands (e.g. Suggestion) are buffered and
            // returned to the caller for re-injection after the approval
            // resolves.
            let (outcome, mut buffered) = approval::await_approval(approval_rx, cmd_rx).await;
            match outcome {
                approval::ApprovalOutcome::Approved => { /* proceed */ }
                approval::ApprovalOutcome::Denied => {
                    return (
                        ToolResult::error(format!("user denied the {} call", tc.name)),
                        buffered,
                    );
                }
                approval::ApprovalOutcome::DeniedAll => {
                    // Latch for the rest of this turn so subsequent tool calls
                    // (same batch + later iterations) auto-deny without prompting.
                    *deny_all_latched = true;
                    return (
                        ToolResult::error(format!(
                            "user denied all remaining actions ({} call denied)",
                            tc.name
                        )),
                        buffered,
                    );
                }
                approval::ApprovalOutcome::Interrupted => {
                    *stop_signal = Some(crate::agent::StopReason::Interrupt);
                    return (
                        ToolResult::error("interrupted while awaiting approval"),
                        buffered,
                    );
                }
                approval::ApprovalOutcome::Cancelled => {
                    *stop_signal = Some(crate::agent::StopReason::Cancel);
                    return (
                        ToolResult::error("cancelled while awaiting approval"),
                        buffered,
                    );
                }
                approval::ApprovalOutcome::ChannelClosed => {
                    return (
                        ToolResult::error("approval channel closed (UI gone)"),
                        buffered,
                    );
                }
            }

            // Approved — execute the tool, returning any buffered commands.
            let result = self
                .dispatch_with_interrupt(
                    &parsed_call,
                    agent_id,
                    cmd_rx,
                    stop_signal,
                    &mut buffered,
                    output_sink.clone(),
                )
                .await;
            return (result, buffered);
        }

        // No approval needed — execute directly. The dispatch is wrapped in a
        // select! that polls cmd_rx so an Interrupt/Cancel arriving mid-
        // execution is caught (the tool future is dropped — kill_on_drop
        // kills shell/browser; spawn_blocking tasks complete in the
        // background). Errors are fed back to the model as tool messages (in
        // run_turn), so the model can retry with corrected args on the next
        // iteration. The MAX_RETRIES cap is enforced at the turn level — see
        // run_turn's retry tracking.
        let mut buffered = Vec::new();
        let result = self
            .dispatch_with_interrupt(
                &parsed_call,
                agent_id,
                cmd_rx,
                stop_signal,
                &mut buffered,
                output_sink.clone(),
            )
            .await;
        (result, buffered)
    }

    /// Build the live-output sink for one tool call: partial chunks are
    /// forwarded to the UI as [`AgentEvent::ToolOutputDelta`], tagged with the
    /// call's id so concurrent calls render in their own cards.
    ///
    /// `try_send` on purpose: a chunk that cannot enter the fan-in channel
    /// (full) is DROPPED. The live view is pixels — a tool's reader task must
    /// never block behind UI backpressure — and the call's final result carries
    /// the complete output regardless.
    fn output_sink(
        &self,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        agent_id: AgentId,
        tool_call_id: &str,
    ) -> OutputSink {
        let tx = fanin_tx.clone();
        let id = tool_call_id.to_string();
        OutputSink::new(move |stream, text| {
            let _ = tx.try_send((
                agent_id,
                AgentEvent::ToolOutputDelta {
                    tool_call_id: id.clone(),
                    stream,
                    text: text.to_string(),
                },
            ));
        })
    }

    /// Grace window (milliseconds) a hard stop (Interrupt/Cancel) grants the
    /// in-flight tool call before its future is dropped: fast bookkeeping
    /// mutations (memory_*, backlog_*, complete_step, file_write/edit) are
    /// millisecond-scale, so awaiting them briefly lets them COMPLETE with
    /// their real result instead of being dropped mid-write (2026-12-30 bug:
    /// interrupted turns silently dropped in-flight tool calls — a dropped
    /// backlog_add/complete_step left the mutation neither complete nor
    /// visibly failed). Slow calls (shell, browser, search, LLM-backed) blow
    /// the window and are cancelled with the explicit interrupted result, so
    /// Stop still stops promptly.
    const DRAIN_GRACE_MS: u64 = 2000;

    /// Execute a tool call while listening for Interrupt/Cancel on the command
    /// channel. The tool future is pinned and polled alongside `cmd_rx.recv()`
    /// in a `select!` loop:
    ///
    /// - On normal completion, the `ToolResult` is returned.
    /// - On `Interrupt`, `*stop_signal` is set to `StopReason::Interrupt` and
    ///   the in-flight call is granted a bounded grace window
    ///   ([`Self::DRAIN_GRACE_MS`]) to finish: a call completing within the
    ///   window returns its REAL result (the turn still stops — the signal is
    ///   set), so fast bookkeeping mutations complete-or-visibly-fail instead
    ///   of being dropped mid-write (2026-12-30 bug: interrupted turns
    ///   silently dropped in-flight tool calls). A call that blows the window
    ///   is dropped — `kill_on_drop(true)` (shell, git) kills the child
    ///   process; browser connections are dropped; `spawn_blocking` tasks
    ///   (file_write/edit) complete in the background (harmless — single
    ///   atomic syscall) — and a synthetic error result is returned.
    /// - On `Cancel`, same as Interrupt but with `StopReason::Cancel`.
    /// - On a `Suggestion`/`Prompt` (steer), the command is buffered and the
    ///   loop continues re-polling the pinned future (the steer is re-injected
    ///   by the caller after the tool completes).
    /// - On channel close (`None`), the command channel is no longer polled —
    ///   the tool future is awaited to completion and its result returned
    ///   (not a synthetic error; see the arm's inline comment).
    ///
    /// This is the single funnel every tool execution passes through, so it
    /// also feeds the steering metrics ([`crate::agent::steering_stats`]):
    /// the call name is checked against the previous result's fired marker,
    /// and the result output is matched for a new marker. Advisory only —
    /// neither observation aborts anything. The exceptions that touch a
    /// successful result's OUTPUT: the consolidation-due note (F11) is
    /// appended once per session when the working-memory tier crosses its
    /// threshold — see [`Self::with_consolidation_note`] — and C3
    /// escalation notes (a repeat-ignored actionable nudge) append the
    /// same way — see [`attach_escalations`].
    async fn dispatch_with_interrupt(
        &self,
        parsed_call: &ParsedToolCall,
        agent_id: AgentId,
        cmd_rx: &mut mpsc::Receiver<AgentCommand>,
        stop_signal: &mut Option<crate::agent::StopReason>,
        buffered: &mut Vec<AgentCommand>,
        sink: OutputSink,
    ) -> ToolResult {
        let steering = crate::agent::steering_stats::SteeringStats::shared();
        steering.observe_call(agent_id, &parsed_call.name);
        let tool_fut = self.tools.dispatch_streaming(parsed_call, sink);
        tokio::pin!(tool_fut);
        loop {
            tokio::select! {
                r = &mut tool_fut => {
                    let mut r = self.with_consolidation_note(r).await;
                    steering.observe_result(agent_id, &parsed_call.name, &r.output);
                    observe_edit_freshness(
                        steering,
                        agent_id,
                        &parsed_call.name,
                        &parsed_call.arguments,
                        &r,
                    );
                    observe_mutation_vehicle(
                        steering,
                        &parsed_call.name,
                        &parsed_call.arguments,
                        &r,
                    );
                    attach_escalations(&mut r, steering, agent_id);
                    return r;
                }
                cmd = cmd_rx.recv() => match cmd {
                    Some(cmd @ (AgentCommand::Interrupt | AgentCommand::Cancel)) => {
                        let (reason, interrupted_msg) = if matches!(cmd, AgentCommand::Interrupt) {
                            (
                                crate::agent::StopReason::Interrupt,
                                "interrupted during execution",
                            )
                        } else {
                            (
                                crate::agent::StopReason::Cancel,
                                "cancelled during execution",
                            )
                        };
                        *stop_signal = Some(reason);
                        // Grace-window drain: give the in-flight call a
                        // bounded chance to finish so fast bookkeeping
                        // mutations (memory_*, backlog_*, complete_step,
                        // file_write/edit) complete with their REAL result
                        // instead of being dropped mid-write (2026-12-30 bug:
                        // interrupted turns silently dropped in-flight tool
                        // calls — a dropped backlog_add/complete_step left
                        // the mutation neither complete nor visibly failed).
                        // The stop signal is already set, so the turn stops
                        // right after this call either way; a call that blows
                        // the window is dropped (kill_on_drop semantics) and
                        // gets the explicit interrupted result, so Stop still
                        // stops promptly.
                        match tokio::time::timeout(
                            std::time::Duration::from_millis(Self::DRAIN_GRACE_MS),
                            &mut tool_fut,
                        )
                        .await
                        {
                            Ok(result) => {
                                let mut result = self.with_consolidation_note(result).await;
                                steering.observe_result(
                                    agent_id,
                                    &parsed_call.name,
                                    &result.output,
                                );
                                observe_edit_freshness(
                                    steering,
                                    agent_id,
                                    &parsed_call.name,
                                    &parsed_call.arguments,
                                    &result,
                                );
                                observe_mutation_vehicle(
                                    steering,
                                    &parsed_call.name,
                                    &parsed_call.arguments,
                                    &result,
                                );
                                attach_escalations(&mut result, steering, agent_id);
                                return result;
                            }
                            Err(_elapsed) => {
                                return ToolResult::error(interrupted_msg);
                            }
                        }
                    }
                    Some(other) => {
                        buffered.push(other);
                    }
                    None => {
                        // Channel closed — no more commands will arrive.
                        // Stop polling cmd_rx and just wait for the tool to
                        // finish (avoids a busy loop on a closed channel).
                        // The completed result still feeds the steering
                        // metrics (review 2026-09-14: this arm used to skip
                        // observe_result, so a nudge in the result went
                        // uncounted and no new per-agent note was set).
                        let result = (&mut tool_fut).await;
                        let mut result = self.with_consolidation_note(result).await;
                        steering.observe_result(agent_id, &parsed_call.name, &result.output);
                        observe_edit_freshness(
                            steering,
                            agent_id,
                            &parsed_call.name,
                            &parsed_call.arguments,
                            &result,
                        );
                        observe_mutation_vehicle(
                            steering,
                            &parsed_call.name,
                            &parsed_call.arguments,
                            &result,
                        );
                        attach_escalations(&mut result, steering, agent_id);
                        return result;
                    }
                }
            }
        }
    }

    /// F11: append the consolidation-due note to a successful tool result
    /// when this session's working memory crossed the threshold. APPENDED
    /// (not prepended) so structured outputs the UI parses stay intact; the
    /// marker detects on the full output. The gate fires at most once per
    /// session; a missing store/session or a store error leaves the result
    /// untouched, and this must never delay or fail the tool call beyond
    /// one advisory count query.
    async fn with_consolidation_note(&self, mut r: ToolResult) -> ToolResult {
        if !r.success {
            return r; // denials and error results carry no steering note
        }
        let session_id = self.session_id();
        if let Some(note) = crate::tool::steering::consolidation_due_note(
            self.memory.as_ref(),
            session_id.as_deref(),
        )
        .await
        {
            r.output.push('\n');
            r.output.push_str(&note);
        }
        r
    }

    /// Handle an `ask_user` tool call: emit a `UserQuestion` event carrying a
    /// oneshot, await the user's answer, and return it as a `ToolResult`.
    ///
    /// Mirrors the approval path — the pause is driven from dispatch (not the
    /// tool's `execute`) because the tool trait has no access to the fan-in
    /// channel or agent id. While awaiting the answer, an `Interrupt`/`Cancel`
    /// aborts (returns an error result); a `Suggestion` is buffered and
    /// returned for re-injection (exactly like `await_approval`).
    ///
    /// One question at a time is automatic: the turn blocks on the oneshot
    /// until the user answers, so a second `ask_user` can't run until this one
    /// resolves.
    async fn ask_user(
        &self,
        parsed_call: &ParsedToolCall,
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        agent_id: AgentId,
        cmd_rx: &mut mpsc::Receiver<AgentCommand>,
        stop_signal: &mut Option<crate::agent::StopReason>,
    ) -> (ToolResult, Vec<AgentCommand>) {
        // Parse + validate the question + options.
        let (question, options) =
            match crate::tool::workflow::ask_user::parse_ask_user_args(&parsed_call.arguments) {
                Ok(q) => q,
                Err(e) => return (ToolResult::error(e), Vec::new()),
            };

        // Build a unique question id (the tool-call id is unique per call, so
        // reuse it — the pending-questions map keys on it, mirroring approvals
        // keying on tool_call_id).
        let question_id = parsed_call.id.clone();

        // Emit the question + hold the oneshot receiver.
        let (answer_tx, answer_rx) = oneshot::channel();
        let _ = fanin_tx
            .send((
                agent_id,
                AgentEvent::UserQuestion {
                    question_id: question_id.clone(),
                    question: question.clone(),
                    options: options.clone(),
                    responder: answer_tx,
                },
            ))
            .await;

        // Await the answer, listening for interrupts (mirror await_approval).
        let mut buffered = Vec::new();
        let mut rx = answer_rx;
        loop {
            tokio::select! {
                result = &mut rx => {
                    let answer = match result {
                        Ok(a) => a,
                        Err(_) => {
                            // The UI dropped the sender (agent exited / channel
                            // closed). Return an error so the model can react.
                            return (
                                ToolResult::error(
                                    "ask_user: answer channel closed (UI gone)"
                                ),
                                buffered,
                            );
                        }
                    };
                    let output = match answer {
                        crate::runtime::UserAnswer::Choice { index } => {
                            // Return the chosen option's label (or the index
                            // if out of range — defensive) so the model sees a
                            // human-readable answer.
                            options
                                .get(index)
                                .map(|o| o.label.clone())
                                .unwrap_or_else(|| format!("option {index}"))
                        }
                        crate::runtime::UserAnswer::Freeform { text } => text,
                    };
                    return (ToolResult::success(output), buffered);
                }
                cmd = cmd_rx.recv() => match cmd {
                    Some(AgentCommand::Interrupt) => {
                        *stop_signal = Some(crate::agent::StopReason::Interrupt);
                        return (
                            ToolResult::error("ask_user interrupted while awaiting answer"),
                            buffered,
                        );
                    }
                    Some(AgentCommand::Cancel) => {
                        *stop_signal = Some(crate::agent::StopReason::Cancel);
                        return (
                            ToolResult::error("ask_user cancelled while awaiting answer"),
                            buffered,
                        );
                    }
                    Some(other) => {
                        // Buffer non-interrupt commands (e.g. Suggestion) —
                        // the caller re-injects them after the answer resolves.
                        buffered.push(other);
                    }
                    None => {
                        return (
                            ToolResult::error("ask_user: command channel closed"),
                            buffered,
                        );
                    }
                }
            }
        }
    }

    /// A comma-separated, sorted list of available tool names — used in the
    /// "unknown tool" error message so the model can see what it can call.
    fn available_tool_names(&self) -> String {
        let mut names: Vec<String> = self.tools.iter().map(|t| t.name().to_string()).collect();
        names.sort();
        names.join(", ")
    }

    /// Call `provider.complete` with retry + jittered exponential backoff.
    /// Retries up to 3 times on provider errors, with equal jitter
    /// (~0.5–1s then ~1–2s — see [`crate::error::retry_backoff_ms`]).
    ///
    /// The provider is passed in explicitly (the per-turn snapshot from
    /// `run_turn`) so the whole turn — including these retries — talks to one
    /// provider even if the loop's provider is swapped mid-flight.
    ///
    /// Each failed attempt emits a transient `AgentEvent::Error`
    /// (`retrying: true`) before the backoff sleep: up to ~3s of jittered
    /// retry sleeps otherwise sit invisible inside the inflight bar's
    /// "sending" window (user report 2026-08-22). The frontend keeps
    /// `running = true` for retrying errors and shows the note in the
    /// activity log.
    pub(super) async fn complete_with_retry<'a>(
        &self,
        provider: &'a Arc<dyn LlmClient>,
        messages: &[Message],
        tool_schemas: &[crate::provider::ToolSchema],
        fanin_tx: &mpsc::Sender<(AgentId, AgentEvent)>,
        agent_id: AgentId,
        session_id: Option<&str>,
    ) -> Result<futures::stream::BoxStream<'a, LlmEvent>> {
        let mut last_err = None;
        for attempt in 0..3 {
            match provider.complete(messages, tool_schemas, None).await {
                Ok(stream) => return Ok(stream),
                Err(e) => {
                    // R21: every failed attempt re-sent the full prompt —
                    // record it so error→retry cycles (and a LiteLLM
                    // fallback switch, which guarantees a cold next request)
                    // are visible in the cache/latency aggregates.
                    // cached_tokens is NULL (not 0) — the provider never
                    // reported usage, so a real reported miss stays
                    // distinguishable. The prompt token count is our
                    // estimate (the same bytes/4 basis the builder uses);
                    // no timing is recorded (the TTFT anchor fix is a
                    // separate backlog item).
                    self.record_stats_row(
                        session_id,
                        provider.as_ref(),
                        super::turn::StatsRow {
                            prompt_tokens: crate::provider::estimate_prompt_tokens(
                                messages,
                                tool_schemas,
                            ) as u32,
                            completion_tokens: 0,
                            reasoning_tokens: 0,
                            cached_tokens: None,
                            ttft_ms: None,
                            generation_ms: None,
                            outcome: Some("error".into()),
                            purpose: None,
                        },
                    );
                    // Non-retryable errors (context overflow, auth, model
                    // not-found) fail immediately — retrying can never
                    // succeed because the prompt won't shrink, the key
                    // won't change, or the model won't appear. Skip the
                    // backoff sleep + retry entirely.
                    //
                    // Rate-limited errors (HTTP 429) also return immediately:
                    // the provider is out of quota, so retrying the SAME
                    // provider never helps ("stop after a single 429"). The
                    // turn-level retry layer (run_turn_attempt) handles the
                    // recovery — switching to the same model on a different
                    // endpoint — rather than burning 3×~1s/2s jittered backoff
                    // sleeps against a provider that just said "no quota left".
                    if e.is_serialization_bug() {
                        // Rule 6: serialization bugs are NOT retried — the
                        // request builder dropped or mutated a reasoning field
                        // the provider requires. Retrying the same request fails
                        // identically; stripping reasoning to "fix" it sometimes
                        // succeeds but permanently degrades quality. Surface
                        // with context (the offending request body is in the
                        // Trace tab).
                        let _ = fanin_tx
                            .send((
                                agent_id,
                                AgentEvent::Error {
                                    error: format!(
                                        "SERIALIZATION BUG (not retried): {e}\n\
                                         The request builder dropped or mutated a \
                                         reasoning field the provider requires. \
                                         The offending request body is in the \
                                         Trace tab."
                                    ),
                                    retrying: false,
                                },
                            ))
                            .await;
                        return Err(e);
                    }
                    if e.is_non_retryable() || e.is_rate_limited() {
                        return Err(e);
                    }
                    if attempt < 2 {
                        // Jittered exponential backoff (equal jitter: half
                        // fixed + half random) so concurrent agents retrying
                        // against the same recovering endpoint don't stay in
                        // lockstep — see `retry_backoff_ms`.
                        let delay_ms = crate::error::retry_backoff_ms(attempt + 1);
                        let _ = fanin_tx
                            .send((
                                agent_id,
                                AgentEvent::Error {
                                    error: format!(
                                        "request attempt {}/3 failed: {e} — retrying in {:.1}s",
                                        attempt + 1,
                                        delay_ms as f64 / 1000.0
                                    ),
                                    retrying: true,
                                },
                            ))
                            .await;
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                        // Attribute the backoff sleep (the actual jittered
                        // duration) to the NEXT attempt's trace record:
                        // parked on the provider here, stamped when the
                        // retry's record is created, so the wait between
                        // attempts shows as `backoff_ms` in the graph
                        // instead of an invisible gap between per-attempt
                        // records.
                        provider.record_backoff_ms(delay_ms as u32);
                    }
                    last_err = Some(e);
                }
            }
        }
        // All retries exhausted. The loop always sets last_err on the final
        // failed attempt, but use a fallback error if the loop bound ever
        // changes such that this is reached without one.
        Err(last_err.unwrap_or_else(|| {
            crate::error::Error::Provider(
                "complete_with_retry exhausted all retries with no error captured".into(),
            )
        }))
    }
}

/// C5: the symbol-lookup redirect gate. When `search`/`search_read` is called
/// with a pattern that names an indexed symbol AND the agent has ignored the
/// symbol nudge >= ESCALATION_THRESHOLD times without a single switch,
/// intercept the call and return a redirect result pointing at
/// `graph_context(id=...)`. The gate is the C3 escalation's teeth: after the
/// advisory NOTE is ignored, the next symbol-shaped search is blocked (not
/// just nudged). The gate lifts when the agent switches to a graph tool once
/// (agent_switched > 0 cancels escalation, same condition as the one-shot C3
/// NOTE). Returns `None` when the gate should not fire (non-symbol pattern, no
/// graph, below threshold, or already switched) — the search proceeds normally.
fn symbol_search_redirect(
    graph: &Option<Arc<crate::codegraph::CodeGraph>>,
    tool_name: &str,
    pattern: &str,
    steering: &crate::agent::steering_stats::SteeringStats,
    agent_id: AgentId,
) -> Option<ToolResult> {
    if !matches!(tool_name, "search" | "search_read") {
        return None;
    }
    let nudge = crate::tool::agent::search::symbol_nudge(graph, pattern)?;
    let fired = steering.should_gate_symbol_search(agent_id)?;
    Some(ToolResult::success(format!(
        "SEARCH INTERCEPTED: the pattern '{pattern}' names an indexed symbol. You have used \
         search for symbol lookups {fired} times this session without switching to the graph \
         tools.\n\n{nudge}\n\nUse graph_context with the id above for the definition + \
         callers/callees. If you need text occurrences (comments, string literals, config keys), \
         retry with a regex metacharacter or glob filter to disambiguate from a symbol lookup."
    )))
}

/// The shell command patterns that indicate file mutation (plan be16ea36
/// step 9): the observability counter's heuristic — PowerShell's
/// write/append cmdlets, append redirection, in-place stream editors, and
/// the common file-surgery vehicles (`tee`, any `python*` interpreter).
/// Token-wise matching for the word-shaped vehicles avoids substring false
/// positives ("committee" is not `tee`; a Python-named path is not python).
fn shell_command_mutates_files(command: &str) -> bool {
    let lower = command.to_lowercase();
    if lower.contains("set-content")
        || lower.contains("out-file")
        || lower.contains("add-content")
        || lower.contains("sed -i")
    {
        return true;
    }
    // `>>` counts unless it redirects into a null sink (review L4: the
    // session's own noise-filtering redirects are not file mutations).
    if let Some(idx) = lower.find(">>") {
        let target = lower[idx + 2..].trim_start();
        let first_word = target.split_whitespace().next().unwrap_or("");
        if first_word != "/dev/null" && first_word != "$null" && first_word != "nul" {
            return true;
        }
    }
    // Token-wise for the word-shaped vehicles: `tee`, and `python*` ONLY
    // when actually invoked with something to run (review L4: `python
    // --version`, a bare `python`, or `echo python` is not file surgery).
    // Still a heuristic — a search command naming python (`grep python f`)
    // counts; the counter is observational.
    let toks: Vec<&str> = lower.split_whitespace().collect();
    toks.iter().enumerate().any(|(i, tok)| {
        let tok = tok.trim_start_matches(|c: char| !c.is_ascii_alphanumeric());
        if tok.starts_with("tee") {
            return true;
        }
        if tok.starts_with("python") {
            return match toks.get(i + 1) {
                // An interpreter with nothing to run is not file surgery.
                None => false,
                Some(next) => {
                    // `-c "..."` runs code; a non-flag argument is a script
                    // path; flags like --version/-V are not file surgery.
                    next.trim_start_matches('-') == "c" || !next.starts_with('-')
                }
            };
        }
        false
    })
}

/// Mutation-vehicle observability (plan be16ea36 step 9): count file
/// mutations by vehicle — file tools (a successful `file_edit`/`file_write`/
/// `file_append` call) vs shell commands matching a mutation pattern (on
/// success — a command that never ran mutated nothing). Surfaced in the
/// Trace panel via the steering snapshot; purely observational (no gating,
/// no steering).
fn observe_mutation_vehicle(
    steering: &crate::agent::steering_stats::SteeringStats,
    tool_name: &str,
    args: &serde_json::Value,
    result: &ToolResult,
) {
    match tool_name {
        "file_edit" | "file_write" | "file_append" => {
            if result.success {
                steering.note_file_tool_mutation();
            }
        }
        "shell" => {
            if result.success {
                if let Some(command) = args.get("command").and_then(|c| c.as_str()) {
                    if shell_command_mutates_files(command) {
                        steering.note_shell_mutation();
                    }
                }
            }
        }
        _ => {}
    }
}

/// Per-path freshness bookkeeping for the file_edit stale-read gate (plan
/// be16ea36 step 3), run beside `observe_result` at every result arm of the
/// funnel: a drift-class `file_edit` failure arms the (agent, path) drift
/// state, a successful edit resets it, a read clears the paths it
/// touched — `read_files` precisely (the paths come from the call's args),
/// `search_read` wholesale (its matched paths are not in the args, and the
/// result is a broad current-content survey) — and a successful
/// `file_write`/`file_append` of P lifts P (the agent's knowledge of P is
/// fresh by construction; review L3). The global marker telemetry
/// (`observe_result`/`observe_call`) is untouched — this is the gate's own
/// state, immune to the `last_fired` chain's consume-on-any-call semantics.
fn observe_edit_freshness(
    steering: &crate::agent::steering_stats::SteeringStats,
    agent_id: AgentId,
    tool_name: &str,
    args: &serde_json::Value,
    result: &ToolResult,
) {
    match tool_name {
        // A successful file_write/file_append of P means the agent's
        // knowledge of P is fresh by construction — the ecosystem steers
        // drift-failure recovery to file_write full-file replacement, so the
        // gate lifts for P without a read (review L3).
        "file_write" | "file_append" => {
            if result.success {
                if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                    steering.clear_edit_drift_for(agent_id, path);
                }
            }
        }
        "file_edit" => {
            if let Some(path) = args.get("path").and_then(|v| v.as_str()) {
                if result.success {
                    steering.note_edit_success(agent_id, path);
                } else if crate::agent::steering_stats::is_edit_drift_failure(
                    tool_name,
                    &result.output,
                ) {
                    steering.note_edit_drift(agent_id, path);
                }
            }
        }
        "read_files" => steering.note_file_read(agent_id, &read_files_paths(args)),
        "search_read" => steering.clear_edit_drift(agent_id),
        _ => {}
    }
}

/// The paths a `read_files` call touches: the single `path` arg and/or the
/// batch `files[].path` entries (plan be16ea36 step 3 — a read of path P
/// clears P's drift state).
fn read_files_paths(args: &serde_json::Value) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(p) = args.get("path").and_then(|v| v.as_str()) {
        paths.push(p.to_string());
    }
    if let Some(files) = args.get("files").and_then(|v| v.as_array()) {
        for f in files {
            if let Some(p) = f.get("path").and_then(|v| v.as_str()) {
                paths.push(p.to_string());
            }
        }
    }
    paths
}

/// C5-family: the file_edit stale-read gate (backlog 714196da). When
/// `file_edit` is called on `path` after a drift-class failure on THAT path
/// (the "Re-read the file …" nudge, emitted on the tool's
/// old_string-not-found / past-EOF / empty-file failures) with no
/// intervening fresh read of the path or landed edit on it, intercept the
/// call and return an actionable error naming the path — the blind retry
/// ("don't edit without read") must not happen. The gate state is
/// per-(agent, path) ([`SteeringStats::should_gate_file_edit`], plan
/// be16ea36 step 3): a fresh read of the path or a successful edit on it
/// lifts it. The interception never executes the tool, so it never feeds
/// its own marker count — same rationale as the C5 symbol gate above.
pub(crate) fn file_edit_redirect(
    steering: &crate::agent::steering_stats::SteeringStats,
    agent_id: AgentId,
    path: &str,
) -> Option<ToolResult> {
    let fired = steering.should_gate_file_edit(agent_id, path)?;
    // H4 telemetry: the gate REALLY intercepted this attempt — count it so
    // the Trace panel can show the emission-decay signal (the interception
    // still never feeds the fired count).
    steering.note_edit_gated(agent_id);
    Some(ToolResult::error(format!(
        "EDIT INTERCEPTED: your last {fired} file_edit attempts failed because the \
         old_string did not match — the file has drifted from your last read. Do not \
         retry blind. First re-read the file (read_files path=\"{path}\"), then retry the \
         edit with the exact current text."
    )))
}

/// C3: append this agent's queued nudge-escalation notes (an actionable
/// marker fired the escalation threshold's worth of times for this agent
/// without a single switch) to a successful result — one line each, AFTER
/// `observe_result` so the appended note itself is never counted as a
/// marker. Best-effort: never fails or delays the result; failed results
/// carry nothing (their queued notes stay queued for the next success).
fn attach_escalations(
    r: &mut ToolResult,
    steering: &crate::agent::steering_stats::SteeringStats,
    agent_id: AgentId,
) {
    if !r.success {
        return;
    }
    for note in steering.take_escalations(agent_id) {
        r.output.push('\n');
        r.output.push_str(&note);
    }
}

/// The reviewer-spawn gates: the failed-reviewer protocol latch + the
/// reviewer-model rule (backlog c8e48f81). Called from `execute_tool_call`
/// for every `spawn_agent` call. Returns `(denial, retry_sanctioned)`:
/// `Some(denial)` aborts the call before the tool runs; the flag is the new
/// `reviewer_retry_sanctioned` value to store back on the loop.
///
/// Gate 1 — failed-reviewer protocol (pre-existing): while a `role:
/// "reviewer"` child of this agent failed with no report (`failure_pending`),
/// spawning ANOTHER reviewer is denied — the parent must first ask the user
/// (the ask_user interception clears the latch and opens the retry sanction)
/// whether to retry on a different model or abandon the review. Blind
/// respawns reproduce the same failure (same model, same task). The main
/// agent can NEVER self-review: review reports are authored only by spawned
/// reviewers, so "abandon the review" is the only exit when the user
/// declines a retry.
///
/// Gate 2 — reviewer-model rule (backlog c8e48f81): a reviewer spawn
/// carrying an explicit `model` is denied unless the failed-reviewer retry
/// is sanctioned. Reviewers run on the CONFIGURED reviewing model
/// (`[models.reviewing]` → executing → subagent → default); per-reviewer
/// model assignment is configured nowhere, and the 2026-12-30 session
/// (plan 72329f2c) showed the agent inventing "model diversity"
/// (glm-5.2/glm-5.3-gcp/deepseek-v4-flash on three parallel reviewers) —
/// the deepseek reviewer failed with no report, exactly what the configured
/// model exists to prevent. The sanctioned retry (failure → ask_user →
/// respawn on the user-picked model) is the ONLY path that may carry a
/// model.
///
/// Sanction lifecycle: consumed at DISPATCH time by ANY reviewer spawn —
/// with or without a model — and closed by `abandon_plan` (the escape hatch:
/// a sanction open at abandon time will never be a retry) and by an ask_user
/// with no failure pending. A lingering window therefore authorizes at most
/// ONE model-carrying reviewer spawn — the first after the consultation —
/// and never a second (the agent ignoring the user's answer and later
/// ad-hoc picking is the narrow residual: one-shot, requiring deliberate
/// misbehavior). Note the one-shot-at-dispatch semantic: the sanction burns
/// BEFORE the approval gate and before the spawn executes — a denied
/// approval or a failed spawn (e.g. unknown model id) closes the window, and
/// with the failure latch clear a fresh ask_user cannot re-open it. Recovery
/// is always clean: a no-model reviewer spawn always passes (the denial
/// text says exactly that).
fn reviewer_spawn_gate(
    failure_pending: bool,
    retry_sanctioned: bool,
    role: Option<&str>,
    model: Option<&str>,
) -> (Option<ToolResult>, bool) {
    // Trim the role for parity with the tool's own arg semantics
    // (spawn_agent trims: " reviewer " IS a reviewer — an untrimmed compare
    // here would let padded roles skip the model gate; review L5).
    if role.map(str::trim) != Some("reviewer") {
        return (None, retry_sanctioned);
    }
    if failure_pending {
        return (
            Some(ToolResult::error(
                "a reviewer sub-agent failed without writing a report — do NOT respawn it \
                 unchanged (the same task on the same model will almost certainly fail the \
                 same way). Use ask_user to ask the user how to proceed: retry the review on \
                 a different model, or abandon the review (abandon_plan returns to Planning). \
                 The main agent can never author a review itself",
            )),
            retry_sanctioned,
        );
    }
    // Gate 2 (backlog c8e48f81): an explicit model is ad-hoc unless the
    // failed-reviewer retry is sanctioned. Whitespace-only counts as absent
    // (matching the tool's own model-arg semantics), and so does the
    // literal string "null": a transport that stringifies JSON null for
    // non-nullable string properties manufactures exactly that artifact
    // (live incident 2027-01-24, backlog 3e6f7887: eight identical
    // reviewer-spawn refusals looped the main agent in Reviewing), and no
    // configured model is ever named "null" — it is never a real pick.
    let has_model = model
        .map(|m| !m.trim().is_empty() && m.trim() != "null")
        .unwrap_or(false);
    if has_model && !retry_sanctioned {
        return (
            Some(ToolResult::error(
                "invalid spawn: a role:\"reviewer\" spawn must OMIT the model parameter — \
                 reviewers run on the CONFIGURED reviewing model ([models.reviewing] → \
                 [models.executing] → [models.subagent] → default), which is authoritative. \
                 Per-reviewer model picks are configured nowhere; an explicit model is \
                 sanctioned ONLY via the failed-reviewer protocol (a reviewer failed without \
                 a report → ask_user → retry on the model the user picks). Respawn without \
                 the model parameter.",
            )),
            false,
        );
    }
    // Any reviewer spawn — with or without a model — consumes the sanction.
    (None, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    use crate::tool::agent::sandbox::link_fixture::{plant_dir_link, plant_file_link};

    #[test]
    fn shell_mutation_pattern_matches_the_mutation_vehicles() {
        assert!(shell_command_mutates_files(
            "Set-Content -Path a.txt -Value x"
        ));
        assert!(shell_command_mutates_files("Get-Process | Out-File p.txt"));
        assert!(shell_command_mutates_files("Add-Content a.txt 'x'"));
        assert!(shell_command_mutates_files("echo x >> a.txt"));
        // Null-sink redirects are not file mutations (review L4).
        assert!(!shell_command_mutates_files("echo x >> /dev/null"));
        assert!(!shell_command_mutates_files("echo x >> $null"));
        assert!(!shell_command_mutates_files("echo x >> nul"));
        // A real file named null.txt still counts.
        assert!(shell_command_mutates_files("echo x >> null.txt"));
        assert!(shell_command_mutates_files("sed -i 's/a/b/' a.txt"));
        assert!(shell_command_mutates_files("cargo test | tee out.txt"));
        assert!(shell_command_mutates_files("python script.py"));
        assert!(shell_command_mutates_files("python -c \"open('a','w')\""));
        assert!(shell_command_mutates_files("python3 -c \"open('a','w')\""));
        // python counts only when invoked with something to run (review L4).
        assert!(!shell_command_mutates_files("python --version"));
        assert!(!shell_command_mutates_files("python"));
        assert!(!shell_command_mutates_files("echo python"));
        assert!(!shell_command_mutates_files("cargo test"));
        assert!(!shell_command_mutates_files("git status"));
        // "committee" contains "tee" as a substring but is not the tee
        // command; a Python-named PATH is not a python invocation.
        assert!(!shell_command_mutates_files("echo committee"));
        assert!(!shell_command_mutates_files("C:\\Python39\\tools.exe x"));
    }

    #[test]
    fn mutation_vehicle_counters_track_file_tools_and_shell() {
        let stats = crate::agent::steering_stats::SteeringStats::new();
        let ok = ToolResult::success("done");
        let err = ToolResult::error("nope");

        // Successful file-tool calls count; failures don't.
        observe_mutation_vehicle(&stats, "file_edit", &serde_json::json!({"path": "a"}), &ok);
        observe_mutation_vehicle(&stats, "file_write", &serde_json::json!({"path": "a"}), &ok);
        observe_mutation_vehicle(&stats, "file_edit", &serde_json::json!({"path": "a"}), &err);
        // Shell commands matching a pattern count (on success); others don't.
        observe_mutation_vehicle(
            &stats,
            "shell",
            &serde_json::json!({"command": "Set-Content a.txt x"}),
            &ok,
        );
        observe_mutation_vehicle(
            &stats,
            "shell",
            &serde_json::json!({"command": "Set-Content a.txt x"}),
            &err,
        );
        observe_mutation_vehicle(
            &stats,
            "shell",
            &serde_json::json!({"command": "cargo test"}),
            &ok,
        );
        // Other tools never count.
        observe_mutation_vehicle(&stats, "search", &serde_json::json!({}), &ok);

        let snap = stats.snapshot();
        assert_eq!(snap.file_tool_mutations, 2);
        assert_eq!(snap.shell_mutations, 1);
    }

    #[test]
    fn file_write_success_clears_the_path_drift_state() {
        // Review L3: the ecosystem steers drift-failure recovery to
        // file_write full-file replacement — a successful file_write of P
        // means the agent's knowledge of P is fresh by construction, so the
        // gate lifts for P (the next targeted file_edit is not intercepted).
        let stats = crate::agent::steering_stats::SteeringStats::new();
        stats.note_edit_drift(1, "a.txt");
        assert!(file_edit_redirect(&stats, 1, "a.txt").is_some());
        // Recovering by rewriting the file lifts the gate for that path —
        // no read required.
        observe_edit_freshness(
            &stats,
            1,
            "file_write",
            &serde_json::json!({"path": "a.txt"}),
            &ToolResult::success("done"),
        );
        assert!(file_edit_redirect(&stats, 1, "a.txt").is_none());
        // A failed file_write does NOT lift the gate.
        stats.note_edit_drift(1, "a.txt");
        observe_edit_freshness(
            &stats,
            1,
            "file_write",
            &serde_json::json!({"path": "a.txt"}),
            &ToolResult::error("nope"),
        );
        assert!(file_edit_redirect(&stats, 1, "a.txt").is_some());
    }

    #[test]
    fn file_edit_redirect_intercepts_after_threshold_and_lifts_after_a_read() {
        // C5-family (backlog 714196da, threshold tightened by e8b39d72 H3):
        // after a SINGLE failed file_edit attempt carrying the stale-read
        // marker with no intervening read of the path, the next file_edit
        // call on THAT path is intercepted with the actionable re-read
        // message naming the path (the freshness contract — not
        // timeout-after-three); a fresh read of the path lifts the gate
        // (per-(agent, path) state, plan be16ea36 step 3).
        let stats = crate::agent::steering_stats::SteeringStats::new();
        let err_text = "old_string not found in file — the content has drifted from your \
             last read. Re-read the file (read_files, this exact path) and retry with the \
             exact current text; do not edit without a fresh read.";
        // What the funnel does on the drift failure: telemetry + drift state.
        stats.observe_result(1, "file_edit", err_text);
        stats.note_edit_drift(1, "a.txt");
        let interception =
            file_edit_redirect(&stats, 1, "a.txt").expect("gate arms after 1 failure");
        assert!(
            interception.output.contains("EDIT INTERCEPTED"),
            "{}",
            interception.output
        );
        assert!(interception.output.contains("a.txt"), "{}", interception.output);
        // H4 telemetry (backlog e8b39d72): the interception is counted
        // (gated 1) and never feeds the fired count.
        let snap = stats.snapshot();
        let edit = snap
            .by_marker
            .iter()
            .find(|m| m.marker == "edit-stale-read")
            .expect("edit marker present");
        assert_eq!(edit.gated, 1);
        assert_eq!(edit.fired, 1);
        // The agent re-reads → the gate lifts → the edit proceeds normally.
        stats.note_file_read(1, &["a.txt".to_string()]);
        assert!(file_edit_redirect(&stats, 1, "a.txt").is_none());
        // A lifted gate does not count interceptions.
        let snap = stats.snapshot();
        let edit = snap
            .by_marker
            .iter()
            .find(|m| m.marker == "edit-stale-read")
            .expect("edit marker present");
        assert_eq!(edit.gated, 1);
    }

    #[test]
    fn file_edit_redirect_lifts_after_a_read_despite_an_interleaved_call() {
        // Regression (plan be16ea36 step 3): the gate no longer rides the
        // `last_fired` chain — observe_call's consume-on-any-call semantics
        // used to let one interleaved non-read call eat the pending marker,
        // so the read never registered and the gate intercepted every later
        // edit (the session-long freeze). The funnel records the drift and
        // the read directly; interleaved calls are irrelevant.
        let stats = crate::agent::steering_stats::SteeringStats::new();
        let err_text = "old_string not found in file — the content has drifted from your \
             last read. Re-read the file (read_files, this exact path) and retry with the \
             exact current text; do not edit without a fresh read.";
        stats.observe_result(1, "file_edit", err_text);
        stats.note_edit_drift(1, "a.txt");
        stats.observe_call(1, "shell"); // interleaved non-read call
        stats.observe_call(1, "read_files"); // the fresh read
        stats.note_file_read(1, &["a.txt".to_string()]);
        assert!(
            file_edit_redirect(&stats, 1, "a.txt").is_none(),
            "the fresh read must lift the gate despite the interleaved call"
        );
    }

    #[test]
    fn read_files_paths_extracts_single_and_batch_paths() {
        // The per-path freshness contract keys off the paths a read_files
        // call touches — both arg shapes must extract (plan be16ea36 step 3).
        let single: serde_json::Value = serde_json::json!({"path": "a.txt"});
        assert_eq!(read_files_paths(&single), vec!["a.txt".to_string()]);
        let batch: serde_json::Value = serde_json::json!({
            "files": [{"path": "a.txt"}, {"path": "b.txt", "start_line": 2}]
        });
        assert_eq!(
            read_files_paths(&batch),
            vec!["a.txt".to_string(), "b.txt".to_string()]
        );
        assert!(read_files_paths(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn observe_edit_freshness_records_drift_success_and_reads() {
        // The funnel's per-path bookkeeping (plan be16ea36 step 3): a
        // drift-class failure arms (agent, path), a successful edit resets
        // it, a read_files call clears exactly the paths it touched, and a
        // search_read survey clears all of the agent's paths.
        let stats = crate::agent::steering_stats::SteeringStats::new();
        let err = ToolResult::error(
            "old_string not found in file — the content has drifted from your last read. \
             Re-read the file (read_files, this exact path) and retry with the exact \
             current text; do not edit without a fresh read.",
        );
        let edit_args = serde_json::json!({"path": "a.txt"});
        observe_edit_freshness(&stats, 1, "file_edit", &edit_args, &err);
        assert!(file_edit_redirect(&stats, 1, "a.txt").is_some());
        // A successful edit resets it.
        let ok = ToolResult::success("Edited 1 line(s) in a.txt");
        observe_edit_freshness(&stats, 1, "file_edit", &edit_args, &ok);
        assert!(file_edit_redirect(&stats, 1, "a.txt").is_none());
        // A non-drift failure (no fresh-read marker) arms nothing.
        let invalid = ToolResult::error("invalid: old_string and new_string are identical");
        observe_edit_freshness(&stats, 1, "file_edit", &edit_args, &invalid);
        assert!(file_edit_redirect(&stats, 1, "a.txt").is_none());
        // Drift on two paths; a read_files of one clears only that one...
        observe_edit_freshness(&stats, 1, "file_edit", &edit_args, &err);
        observe_edit_freshness(
            &stats,
            1,
            "file_edit",
            &serde_json::json!({"path": "b.txt"}),
            &err,
        );
        observe_edit_freshness(
            &stats,
            1,
            "read_files",
            &serde_json::json!({"path": "a.txt"}),
            &ToolResult::success("ok"),
        );
        assert!(file_edit_redirect(&stats, 1, "a.txt").is_none());
        assert!(file_edit_redirect(&stats, 1, "b.txt").is_some());
        // ...and a search_read survey clears the rest.
        observe_edit_freshness(
            &stats,
            1,
            "search_read",
            &serde_json::json!({"pattern": "x"}),
            &ToolResult::success("ok"),
        );
        assert!(file_edit_redirect(&stats, 1, "b.txt").is_none());
    }

    #[test]
    fn escalation_notes_attach_to_success_only() {
        // C3: drained escalation notes append to successful results one
        // line each (after observe_result — the note is never counted);
        // a drain is final; failed results carry nothing.
        let nudge_out =
            "note: 'hello' is an indexed symbol — graph_context(id=\"a.rs::hello::1\") …";
        let stats = crate::agent::steering_stats::SteeringStats::new();
        for _ in 0..2 {
            stats.observe_result(1, "search", nudge_out);
        }
        let mut ok = ToolResult::success("done".to_string());
        attach_escalations(&mut ok, &stats, 1);
        assert!(ok.output.contains("NOTE:"), "{}", ok.output);
        assert!(
            ok.output.starts_with("done"),
            "appended, not prepended: {}",
            ok.output
        );
        // Drained — a second attach adds nothing.
        let mut again = ToolResult::success("x".to_string());
        attach_escalations(&mut again, &stats, 1);
        assert_eq!(again.output, "x");

        // Failed results carry nothing.
        let stats2 = crate::agent::steering_stats::SteeringStats::new();
        for _ in 0..2 {
            stats2.observe_result(1, "search", nudge_out);
        }
        let mut err = ToolResult::error("boom");
        attach_escalations(&mut err, &stats2, 1);
        assert_eq!(err.output, "boom");
    }

    #[test]
    fn reviewer_spawn_gate_denies_ad_hoc_model_picks() {
        // Backlog c8e48f81 (2026-12-30 session, plan 72329f2c): the agent
        // invented per-reviewer model assignments ("model diversity") —
        // glm-5.2/glm-5.3-gcp/deepseek-v4-flash on three parallel reviewers;
        // the deepseek one failed with no report. Reviewers must run on the
        // CONFIGURED reviewing model; an explicit model is sanctioned only
        // via the failed-reviewer protocol (failure → ask_user → retry on
        // the user-picked model).
        let (denial, sanctioned) =
            reviewer_spawn_gate(false, false, Some("reviewer"), Some("deepseek-v4-flash"));
        let d = denial.expect("an ad-hoc explicit model on a reviewer spawn must be denied");
        assert!(!d.success);
        assert!(
            d.output.contains("CONFIGURED reviewing model"),
            "the denial explains the rule: {}",
            d.output
        );
        assert!(
            !sanctioned,
            "the (absent) sanction is consumed by the spawn attempt"
        );
        // A whitespace-padded role is still a reviewer (the tool trims) —
        // the gate must trim the same way (review L5).
        let (denial, _) =
            reviewer_spawn_gate(false, false, Some(" reviewer "), Some("deepseek-v4-flash"));
        assert!(
            denial.is_some(),
            "a padded role must not skip the model gate"
        );
    }

    #[test]
    fn reviewer_spawn_gate_allows_the_sanctioned_retry_and_consumes_it() {
        // The failed-reviewer protocol's retry: a reviewer failed without a
        // report → ask_user (opens the sanction) → respawn on the model the
        // user picked. That spawn — and only that one — may carry a model.
        let (denial, sanctioned) =
            reviewer_spawn_gate(false, true, Some("reviewer"), Some("glm-5.3-gcp"));
        assert!(
            denial.is_none(),
            "the user-sanctioned retry may carry the picked model"
        );
        assert!(
            !sanctioned,
            "the retry window closes with the spawn (sanction consumed)"
        );
        // A SECOND model-carrying spawn is ad-hoc again — denied.
        let (denial, _) =
            reviewer_spawn_gate(false, false, Some("reviewer"), Some("glm-5.3-gcp"));
        assert!(
            denial.is_some(),
            "the sanction does not persist past one spawn"
        );
    }

    #[test]
    fn reviewer_spawn_gate_no_model_spawn_consumes_sanction_and_passes() {
        // A reviewer spawn WITHOUT a model always passes (the configured
        // reviewing model applies) — and it still consumes the sanction: the
        // retry window closes at the first reviewer spawn after the
        // consultation (and at abandon_plan / an unrelated ask_user), so it
        // authorizes at most ONE model-carrying spawn, never a later one.
        let (denial, sanctioned) = reviewer_spawn_gate(false, true, Some("reviewer"), None);
        assert!(denial.is_none(), "no-model reviewer spawns always pass");
        assert!(!sanctioned, "any reviewer spawn closes the retry window");
        // Empty/whitespace model counts as absent (matches the tool's
        // semantics).
        let (denial, _) = reviewer_spawn_gate(false, false, Some("reviewer"), Some("  "));
        assert!(denial.is_none(), "whitespace-only model is no model");
    }

    #[test]
    fn reviewer_spawn_gate_failure_pending_denies_and_preserves_sanction() {
        // The pre-existing failed-reviewer latch: while a reviewer failure is
        // pending, ANY reviewer spawn is denied (ask first). The sanction is
        // NOT consumed — the retry window opens only when ask_user runs.
        let (denial, sanctioned) = reviewer_spawn_gate(true, false, Some("reviewer"), None);
        let d = denial.expect("the failure latch denies reviewer spawns");
        assert!(
            d.output.contains("ask_user"),
            "the denial instructs the protocol: {}",
            d.output
        );
        assert!(!sanctioned, "the latch denial does not touch the sanction");
        // Non-reviewer spawns pass through untouched.
        let (denial, sanctioned) = reviewer_spawn_gate(true, true, Some("worker"), Some("m"));
        assert!(denial.is_none(), "non-reviewer spawns are not gated");
        assert!(sanctioned, "non-reviewer spawns leave the sanction untouched");
    }

    /// Helper: an in-memory graph indexing `fn hello() {}` at `a.rs:1`.
    fn make_indexed_graph() -> (
        tempfile::TempDir,
        Option<std::sync::Arc<crate::codegraph::CodeGraph>>,
    ) {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn hello() {}\n").unwrap();
        let graph = crate::codegraph::CodeGraph::open_in_memory(dir.path().to_path_buf()).unwrap();
        graph.index(None).unwrap();
        (dir, Some(std::sync::Arc::new(graph)))
    }

    /// The nudge output that `observe_result` would see from a real search
    /// that fired the symbol nudge for `hello`.
    const HELLO_NUDGE: &str =
        "'hello' is an indexed symbol — graph_context(id=\"a.rs::hello::1\") gives its definition + callers in one call.";

    #[test]
    fn symbol_search_redirect_fires_after_ignored_nudges() {
        // C5: after 2 ignored symbol-nudge fires with zero switches, the next
        // symbol-shaped search is intercepted — returns a success result with
        // the redirect message embedding the symbol id.
        let (_dir, graph) = make_indexed_graph();
        let stats = crate::agent::steering_stats::SteeringStats::new();
        stats.observe_result(1, "search", HELLO_NUDGE);
        stats.observe_result(1, "search", HELLO_NUDGE);

        let redirect = symbol_search_redirect(&graph, "search", "hello", &stats, 1);
        assert!(
            redirect.is_some(),
            "gate should fire after 2 ignored nudges"
        );
        let r = redirect.unwrap();
        assert!(r.success);
        assert!(r.output.contains("SEARCH INTERCEPTED"), "{}", r.output);
        assert!(
            r.output.contains("graph_context(id=\"a.rs::hello::1\")"),
            "redirect must embed the symbol id: {}",
            r.output
        );
        assert!(r.output.contains("2 times"), "{}", r.output);
    }

    #[test]
    fn symbol_search_redirect_does_not_fire_below_threshold() {
        // 1 fire → below ESCALATION_THRESHOLD (2) → no gate.
        let (_dir, graph) = make_indexed_graph();
        let stats = crate::agent::steering_stats::SteeringStats::new();
        stats.observe_result(1, "search", HELLO_NUDGE);

        let redirect = symbol_search_redirect(&graph, "search", "hello", &stats, 1);
        assert!(redirect.is_none(), "gate should not fire below threshold");
    }

    #[test]
    fn symbol_search_redirect_does_not_fire_for_non_symbol_pattern() {
        // 'hello world' is not a bare identifier → symbol_nudge returns None →
        // no gate, even after 2 ignored nudges.
        let (_dir, graph) = make_indexed_graph();
        let stats = crate::agent::steering_stats::SteeringStats::new();
        stats.observe_result(1, "search", HELLO_NUDGE);
        stats.observe_result(1, "search", HELLO_NUDGE);

        let redirect = symbol_search_redirect(&graph, "search", "hello world", &stats, 1);
        assert!(
            redirect.is_none(),
            "gate should not fire for non-symbol patterns"
        );
    }

    #[test]
    fn symbol_search_redirect_does_not_fire_without_graph() {
        // No graph → can't check if pattern names a symbol → no gate.
        let stats = crate::agent::steering_stats::SteeringStats::new();
        stats.observe_result(1, "search", HELLO_NUDGE);
        stats.observe_result(1, "search", HELLO_NUDGE);

        let redirect = symbol_search_redirect(&None, "search", "hello", &stats, 1);
        assert!(redirect.is_none(), "gate should not fire without a graph");
    }

    #[test]
    fn symbol_search_redirect_lifts_after_switch() {
        // After a switch to graph_context, agent_switched > 0 → gate lifts.
        let (_dir, graph) = make_indexed_graph();
        let stats = crate::agent::steering_stats::SteeringStats::new();
        stats.observe_result(1, "search", HELLO_NUDGE);
        stats.observe_result(1, "search", HELLO_NUDGE);
        stats.observe_call(1, "graph_context"); // the switch

        let redirect = symbol_search_redirect(&graph, "search", "hello", &stats, 1);
        assert!(redirect.is_none(), "gate should lift after a switch");
    }

    #[test]
    fn symbol_search_redirect_ignores_non_search_tools() {
        // The gate only applies to search/search_read — not read_files etc.
        let (_dir, graph) = make_indexed_graph();
        let stats = crate::agent::steering_stats::SteeringStats::new();
        stats.observe_result(1, "search", HELLO_NUDGE);
        stats.observe_result(1, "search", HELLO_NUDGE);

        let redirect = symbol_search_redirect(&graph, "read_files", "hello", &stats, 1);
        assert!(
            redirect.is_none(),
            "gate should not fire for non-search tools"
        );
    }

    #[test]
    fn symbol_search_redirect_also_gates_search_read() {
        // search_read shares the symbol nudge — the gate fires for it too.
        let (_dir, graph) = make_indexed_graph();
        let stats = crate::agent::steering_stats::SteeringStats::new();
        stats.observe_result(1, "search", HELLO_NUDGE);
        stats.observe_result(1, "search", HELLO_NUDGE);

        let redirect = symbol_search_redirect(&graph, "search_read", "hello", &stats, 1);
        assert!(redirect.is_some(), "gate should fire for search_read too");
    }

    /// The research artifact carve-out accepts ALL FOUR file tools on a
    /// `.coding/**` target, not just the one the dispatch fixture happens to
    /// register (`file_write`) — AND every name it covers is really denied by
    /// `ToolFilter::ExecutingResearch`, so a name whose filter denial is dropped
    /// fails here instead of becoming a file tool allowed by name on EVERY path.
    /// The opposite direction (a fifth denied file tool silently getting no
    /// carve-out) fails closed and is pinned by
    /// `research_filter_hides_source_mutating_file_tools` in
    /// `src/tool/mod.rs` (review LOW-2, 2027-01-11).
    #[test]
    fn research_artifact_write_covers_the_whole_file_tool_set() {
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        for name in RESEARCH_ARTIFACT_TOOLS {
            assert!(
                !ToolFilter::ExecutingResearch.allows(
                    crate::tool::ToolCategory::Agent,
                    crate::tool::SafetyLevel::NeedsApproval,
                    name
                ),
                "{name} must be denied by the research filter for the carve-out to matter"
            );
            let args = serde_json::json!({ "path": ".coding/analysis/notes.md" });
            assert_eq!(
                research_write_verdict(&sandbox, name, &args),
                ResearchWrite::Artifact,
                "{name} on a .coding/ artifact should pass"
            );
        }
    }

    /// The carve-out is a permission GRANT, so everything it does not name
    /// explicitly stays denied: source, docs, traversal escapes, non-file
    /// tools, and calls without a path argument — while protected side-car
    /// files get their OWN verdict (they are never writable, in any state).
    #[test]
    fn research_artifact_write_fails_closed() {
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        for raw in ["src/lib.rs", "docs/FEATURES.md", ".coding/../src/lib.rs"] {
            let args = serde_json::json!({ "path": raw });
            assert_eq!(
                research_write_verdict(&sandbox, "file_write", &args),
                ResearchWrite::NotAnArtifact,
                "{raw} must not be an artifact write"
            );
        }
        // Protected side-car files are refused in EVERY state, so they are
        // reported as protected rather than as a research boundary (the
        // trailing-dot spelling is the Win32 form — see the sandbox tests).
        for raw in [
            ".coding/plans/stack.json",
            ".coding/reviews/2026-04-08-review.md",
            ".coding/knowledge/spec/x.md",
            ".coding/backlog.jsonl",
            ".coding/plans./x.md",
        ] {
            let args = serde_json::json!({ "path": raw });
            assert_eq!(
                research_write_verdict(&sandbox, "file_write", &args),
                ResearchWrite::Protected,
                "{raw} must be reported as protected"
            );
        }
        // An escaping path is not a carve-out candidate at all.
        let escaping = serde_json::json!({ "path": "../.coding/x.md" });
        assert_eq!(
            research_write_verdict(&sandbox, "file_write", &escaping),
            ResearchWrite::NotApplicable
        );
        let missing = serde_json::json!({ "content": "x" });
        assert_eq!(
            research_write_verdict(&sandbox, "file_write", &missing),
            ResearchWrite::NotApplicable,
            "a call without a path argument must fail closed"
        );
        // A non-file tool never gets the carve-out, even on an artifact path.
        let args = serde_json::json!({ "path": ".coding/analysis/notes.md" });
        assert_eq!(
            research_write_verdict(&sandbox, "shell", &args),
            ResearchWrite::NotApplicable,
            "the carve-out is limited to the file tools"
        );
    }

    /// A link planted INSIDE `.coding/` must not launder a source path into an
    /// artifact grant: when the target (or its parent) exists, the verdict is
    /// made on the CANONICAL path, not on the lexical spelling (review LOW-2a).
    #[test]
    fn research_write_verdict_canonicalizes_an_existing_target() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "// source").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        if !plant_dir_link(&dir.path().join("src"), &dir.path().join(".coding/link")) {
            // Creating a link needs Developer Mode on Windows (or a junction);
            // the code path is platform-independent, so this is a no-op only
            // where the fixture itself cannot exist.
            return;
        }
        for raw in [".coding/link/lib.rs", ".coding/link/new.rs"] {
            let args = serde_json::json!({ "path": raw });
            assert_eq!(
                research_write_verdict(&sandbox, "file_write", &args),
                ResearchWrite::NotAnArtifact,
                "{raw} resolves (canonically) to source, never to an artifact"
            );
        }
        // A genuine artifact in the same tree still passes.
        let ok = serde_json::json!({ "path": ".coding/analysis/notes.md" });
        assert_eq!(
            research_write_verdict(&sandbox, "file_write", &ok),
            ResearchWrite::Artifact
        );
    }

    /// A link planted INSIDE `.coding/` that points OUT of the root must fail
    /// CLOSED: `validate` refuses that path, and the lexical fallback is only
    /// trusted when no link component exists — otherwise the grant would hand
    /// the OS a write that escapes the sandbox (review LOW-1).
    #[test]
    fn research_write_verdict_fails_closed_on_a_link_out_of_the_root() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        if !plant_dir_link(outside.path(), &dir.path().join(".coding/link")) {
            // No privilege-free spelling worked; the guard is then verified by
            // the junction fixture on Windows / symlinks on macOS+Linux.
            eprintln!("SKIP: could not create a directory link for the escape fixture");
            return;
        }
        for raw in [".coding/link/x.md", ".coding/link/nested/x.md"] {
            let args = serde_json::json!({ "path": raw });
            assert_eq!(
                research_write_verdict(&sandbox, "file_write", &args),
                ResearchWrite::NotApplicable,
                "{raw} resolves outside the root — never a carve-out candidate"
            );
        }
        // A symlink FILE as the leaf component is refused for the same reason,
        // including while its target is dangling.
        std::fs::write(outside.path().join("victim.md"), "outside").unwrap();
        for (target, leaf) in [
            (outside.path().join("victim.md"), ".coding/f.md"),
            (outside.path().join("missing.md"), ".coding/dangling.md"),
        ] {
            if !plant_file_link(&target, &dir.path().join(leaf)) {
                eprintln!("SKIP: could not create a file link for {leaf}");
                continue;
            }
            let args = serde_json::json!({ "path": leaf });
            assert_eq!(
                research_write_verdict(&sandbox, "file_write", &args),
                ResearchWrite::NotApplicable,
                "{leaf} is a symlinked leaf — it must not be granted from its spelling"
            );
        }
        // A DANGLING directory link (its target removed) is refused as well:
        // the parent is missing, so the lexical branch judges it — and the walk
        // still sees the junction. This is the privilege-free Windows spelling
        // of the escape, so it is worth pinning where symlinks are unavailable.
        let gone = outside.path().join("gone");
        std::fs::create_dir_all(&gone).unwrap();
        if plant_dir_link(&gone, &dir.path().join(".coding/link2")) {
            std::fs::remove_dir(&gone).unwrap();
            let args = serde_json::json!({ "path": ".coding/link2/x.md" });
            assert_eq!(
                research_write_verdict(&sandbox, "file_write", &args),
                ResearchWrite::NotApplicable,
                "a dangling directory link must not be granted"
            );
        }
        // H1's EXACT shape, privilege-free: a dangling link as the LEAF. There
        // `validate` RETURNS Ok — its parent fallback appends the raw leaf name
        // while the leaf stays an unresolved reparse point — so the guard must
        // reject it in the CANONICAL branch, not only in the lexical one.
        // Pre-fix this verdict was `Artifact`.
        let gone_leaf = outside.path().join("gone-leaf");
        std::fs::create_dir_all(&gone_leaf).unwrap();
        if plant_dir_link(&gone_leaf, &dir.path().join(".coding/dangling-leaf")) {
            std::fs::remove_dir(&gone_leaf).unwrap();
            let args = serde_json::json!({ "path": ".coding/dangling-leaf" });
            assert_eq!(
                research_write_verdict(&sandbox, "file_write", &args),
                ResearchWrite::NotApplicable,
                "a dangling link LEAF must not be granted (review HIGH-1, round 3)"
            );
        }
    }

}
