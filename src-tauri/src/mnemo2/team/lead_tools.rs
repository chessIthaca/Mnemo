// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The lead's host-tool surface: the tool specs a lead or worker session
//! is offered, the executor for lead tool calls, and the answer builders
//! (`team_status`, worker answers) they return.

use std::path::Path;

use mnemo::headless::HostToolSpec;
use mnemo::memory::transcript_index::{
    SyncReport, TranscriptHit, TranscriptIndex, TranscriptSource,
};
use mnemo::tool::{SafetyLevel, ToolResult};
use serde::Serialize;
use serde_json::{json, Value};

use crate::mnemo2::dispatch_routing::RoutingMode;
use crate::mnemo2::phase3::{self, ReviewEvidence};

use super::findings_ledger;
use super::record::{ChangeSource, MessageSender, WorkerRecord};
use super::routing::tier_ids;
use super::dispatch::{
    BoxFuture, DispatchRequest, DispatchSource, PlanRef, dispatch_result, stale_base_line,
};
use super::TeamRuntime;

impl TeamRuntime {
    /// The lead's team delegation, review, and repair host tools.
    pub fn lead_host_tools(&self) -> Vec<HostToolSpec> {
        let guidance = self.tier_guidance();
        // With tiers configured the lead picks among the tiers' models only.
        let lead_models = if guidance.is_some() {
            self.tiered_models()
        } else {
            self.current_models()
        };
        let routing_mode = self
            .routing
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .mode;
        let model_description = if guidance.is_some() {
            "A specific model from the tiers, only when the tier's first model does not fit; it overrides `tier`. Prefer `tier`; models outside the tiers are refused."
        } else {
            "The exact endpoint/model spec this worker runs on. Choose it per task: a strong reasoning model for design-heavy, cross-cutting, or debugging work; a fast, inexpensive model for mechanical edits, boilerplate, or test scaffolding. Omit to use the configured default."
        };
        let mut model = json!({"type": "string", "description": model_description});
        let tier_description = match (&guidance, routing_mode) {
            (Some(text), RoutingMode::Off) => format!(
                "Required: choose the cheapest tier that fits the step (a step with a complete spec fits routine or trivial, even across files); the worker runs on that tier's first available model; an empty tier folds down.\n{text}"
            ),
            (Some(text), RoutingMode::Auto) => format!(
                "Optional: choose the cheapest tier that fits the step (a step with a complete spec fits routine or trivial, even across files), or auto to let Laya choose; the worker runs on that tier's first available model; an empty tier folds down.\n{text}"
            ),
            (Some(text), _) => format!(
                "Required: choose the cheapest tier that fits the step (a step with a complete spec fits routine or trivial, even across files), or auto where offered; the worker runs on that tier's first available model; an empty tier folds down.\n{text}"
            ),
            (None, _) => "The model tier. No tiers are configured (Settings > Tiers), so omit it and the default model is used.".to_string(),
        };
        let ladder = self.tier_ladder();
        let mut tier_names = tier_ids(&ladder);
        if matches!(routing_mode, RoutingMode::Suggest | RoutingMode::Auto) {
            tier_names.push("auto");
        }
        let tier = json!({
            "type": "string",
            "enum": tier_names,
            "description": tier_description,
        });
        let audit_tier = json!({
            "type": "string",
            "enum": tier_ids(&self.tier_ladder()),
            "description": "The audit's ladder tier; defaults to high. Empty tiers fold down. Audits are never routed by Laya or kNN; an unreported audit may be switched with team_resume, a reported audit is locked.",
        });
        let specs: Vec<&str> = lead_models
            .iter()
            .map(|model| model.spec.as_str())
            .collect();
        if !specs.is_empty() {
            model["enum"] = json!(specs);
        }
        let mut efforts: Vec<&str> = Vec::new();
        for effort in lead_models
            .iter()
            .flat_map(|model| &model.reasoning_efforts)
        {
            if !efforts.contains(&effort.as_str()) {
                efforts.push(effort);
            }
        }
        let mut reasoning_effort = json!({
            "type": "string",
            "description": "The worker's reasoning effort, chosen with its model: higher for hard design or debugging, lower for mechanical work. Applies to the model `tier` or `model` picks; a value that model does not support is rejected with its allowed list. Omit to use the model's configured default."
        });
        if !efforts.is_empty() {
            reasoning_effort["enum"] = json!(efforts);
        }
        let mut dispatch_required = vec!["worker_id", "plan_id", "step", "brief"];
        if guidance.is_some() && routing_mode != RoutingMode::Auto {
            dispatch_required.push("tier");
        }
        vec![
            HostToolSpec {
                name: "team_dispatch".to_string(),
                description: "Delegate one implementation task through TeamManager to a full executor worker, under a step of a plan on the plan board (plan first: plan_create, then dispatch with its plan_id and 1-based step). The worker receives the plan and its step automatically, so your brief is only the task-specific instructions. A step is checked off when one of its tasks merges and none is still open. Workers run in their own worktree and branch with file_edit, file_write, shell, git, and tests, and have no token or cost budget: give them whole, self-contained implementation tasks (code, tests, verification, commit), never read-only, content-only, or 'send me the file text' briefs. Choose a stable unique worker_id of 8-64 ASCII letters/digits and keep it with the brief/model; after a timeout or uncertain host result retry with that same identity, never a guessed replacement. Mark shared contracts, persistence, concurrency, security, or large/cross-cutting work with review_risk='high'; high-risk dispatch requires that plan to declare observable acceptance scenarios (plan_create acceptance_scenarios or plan_update add_acceptance_scenarios). The JSON response returns the authoritative worker ID and status. Never use generic agent spawning, Git, files, or team.json to infer or mutate worker lifecycle.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string","minLength":8,"maxLength":64,"pattern":"^[A-Za-z0-9]+$"},"plan_id":{"type":"string","description":"The board plan this task belongs to (from plan_create or plan_list). Required."},"step":{"type":"integer","minimum":1,"description":"The 1-based step of that plan this task works on. Required; an unknown, closed, or abandoned plan, a missing step, or a step that is already done is refused."},"brief":{"type":"string","description":"Task-specific instructions only. The worker automatically receives the plan (title, goal, context, decisions, interfaces, acceptance scenarios, step list) and the full text of its step; do not repeat them."},"tier":tier.clone(),"model":model.clone(),"reasoning_effort":reasoning_effort.clone(),"review_risk":{"type":"string","enum":["standard","high"],"description":"Optional; omitted means standard. Pass high for shared contracts, persistence, concurrency, security, or large/cross-cutting work: the worker then cannot merge without a fresh independent audit, and the plan must declare observable acceptance scenarios."},"read_back":{"type":"boolean","description":"Optional. When true, the worker's first turn only reads its spec and the files it names, then messages the lead with numbered questions (ambiguities, contradictions with the code, open decisions) or exactly 'No questions.', and waits for the lead's reply before changing files. Use it for large or subtle specs."}},"required":dispatch_required}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_audit_dispatch".to_string(),
                description: "Dispatch a fresh read-only independent audit of one implementation worker's current branch/tip for a high-risk review. The reviewer_id is a stable unique 8-64 ASCII letters/digits ID of your choosing; retry a timed-out dispatch with that same identity for the same worker and tip, and choose a new reviewer_id after any fix commit moves the tip. The audit receives the worker's board plan, its intent brief, and the target diff — never the worker's own summary or findings — and cannot edit files, commit, merge, spawn, ask the user, or tick plan steps. When it completes, link it through team_review's evidence.independent_reviewer_id and write an audit_adjudication; never merge the audit worker itself.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"reviewer_id":{"type":"string","minLength":8,"maxLength":64,"pattern":"^[A-Za-z0-9]+$"},"tier":audit_tier,"model":{"type":"string"},"reasoning_effort":{"type":"string","enum":["max","high","medium","low","minimal","none"]},"brief":{"type":"string"}},"required":["worker_id","reviewer_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_prove_regression".to_string(),
                description: "Prove a bug-fixing worker's regression test fails without its fix, in the worker's own checkout while it is idle. The outcome arrives as the check 'regression test fails without the fix', as the finding 'regression-not-red' when the test still passes without the fix, or as 'regression-proof-inconclusive' when the run could not decide, plus a notice. Refused unless the worker is idle and its plan is bug_fixing with a regression test. Returns at once; the proof runs in the background and team_message, team_resume, team_route_fix, review_fix and team_merge are refused for that worker until it finishes.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"}},"required":["worker_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_mutation_check".to_string(),
                description: "Run the project's mutation command over one worker's change diff and report every changed line that no test catches as a low-severity 'No test catches: ...' finding. Refused with 'configure [mutation] in .coding/checks.toml' when the project configured no mutation command, and refused while the worker is busy. Returns at once; the run is expensive (minutes to hours) and finishes with a notice.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"}},"required":["worker_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_run_checks".to_string(),
                description: "Run every .coding/checks.toml command in a worker's checkout at its current tip, or in the lead checkout when worker_id is omitted, through the machine's heavy-command slots, and record each result as a check on that tip. Returns at once; a notice reports the results. Mnemo also starts it on its own after a review with no open findings whose tip lacks passing checks. Use it instead of running the full checks yourself.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string","description":"The worker whose checkout to check; omit it to check the lead checkout."}}}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_message".to_string(),
                description: "Send a message to a worker; a busy worker receives it as a steer.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"text":{"type":"string"}},"required":["worker_id","text"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_status".to_string(),
                description: "Read authoritative TeamManager state for every worker or one exact returned worker_id. This is the only worker lifecycle/ID authority; never infer status from Git, worktrees, timestamps, or files. Pass plan_id to see one plan's workers. Each worker is a compact entry by default (id, name, status, kind, tier, model, plan, branch, worktree, error, pending, open findings, checks, review summary, merge commit, cost, last activity); pass detail true for the full record with all review evidence. The answer also carries `lead`: whether Claude Code leads the team (kind external, attached true), idled out after a silent spell (attached false, idle_since_ms set), or the in-app lead leads (kind in_app). Ask when you need it; status is not pushed.".to_string(),
                parameters: json!({"type":"object","properties":{
                    "worker_id":{"type":"string"},
                    "plan_id":{"type":"string","description":"Only the workers dispatched on this board plan."},
                    "detail":{"type":"boolean","description":"Return each worker's full record instead of the compact entry."}
                }}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_stop".to_string(),
                description: "Cancel a worker's live session through TeamManager while retaining its record, branch, worktree, and commits. Use the exact ID returned by dispatch or team_status.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"}},"required":["worker_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_resume".to_string(),
                description: "Resume a stopped, interrupted, or failed worker (including an audit that has not reported) from its saved transcript through TeamManager. Returns the same worker ID with a new session generation. To move the worker to another model (it failed, or its model does not fit the work), pass `tier` or `model` (and optionally `reasoning_effort`): an idle worker is restarted on it with its transcript kept; a working one must be stopped first. The resumed worker is idle: send it a team_message to continue.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"tier":tier,"model":model,"reasoning_effort":reasoning_effort},"required":["worker_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_compact".to_string(),
                description: "Request real context compaction for a quiescent worker. A blocked or stopped session is detached and resumed once first. A returned working status means requested, not completed; use team_status and compaction events for the outcome.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"}},"required":["worker_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_review".to_string(),
                description: "Persist your review of the current worker tip and its complete finding list. Review more than the worker's summary: read the diff unanchored first, then record substantive evidence — intent, and, unless Mnemo2 finds the change light, also execution path, integration, test quality, and residual risk. Light means not high risk and either at most 60 changed lines, or only Markdown (.md) and test files changed; the answer says which rule applied. A passing test count alone is not a review. Re-review after every fix. Example finding: {\"title\":\"main.js uses a default import\",\"detail\":\"game.js exports Game as a named export, so the page fails to load\",\"file\":\"src/main.js\",\"line\":1,\"severity\":\"high\"}.".to_string(),
                parameters: json!({"type":"object","properties":{
                    "worker_id":{"type":"string"},
                    "summary":{"type":"string"},
                    "findings":{"type":"array","items":{
                        "type":"object",
                        "properties":{
                            "id":{"type":"string","description":"Stable id; derived from file+title when omitted. Reuse it when re-reviewing."},
                            "title":{"type":"string"},
                            "detail":{"type":"string","description":"Why it matters."},
                            "file":{"type":"string","description":"Repository-relative path."},
                            "line":{"type":"integer","minimum":0,"description":"1-based line; 0 or omitted for file-wide."},
                            "severity":{"type":"string","enum":crate::mnemo2::phase3::FINDING_SEVERITIES,"description":"Every open finding blocks the merge, whatever its severity."},
                            "status":{"type":"string","enum":crate::mnemo2::phase3::FINDING_STATUSES,"default":"open"},
                            "evidence":{"type":"string","description":"Required when status is fixed."},
                            "justification":{"type":"string","description":"Required when status is wontfix."},
                            "category":{"type":"string","description":"Optional kebab-case name for the kind of mistake, reused across reviews so Mnemo2 can count it (for example vacuous-test, missing-error-path, docs-out-of-sync). Reuse a category from the list in earlier team_review results when one fits."},
                            "enforce":{"type":"string","enum":["code","build","test","review"],"description":"Optional: where this kind of mistake could be stopped for good. code: a type or API that makes it impossible to write; build: a lint or policy test that fails the build; test: a test or check that catches it; review: only a person can judge it."}
                        },
                        "required":["title","detail","file","severity"]
                    }},
                    "evidence":{"type":"object","description":"Structured review evidence for the exact tip. A light change (test or documentation files only, or at most 60 changed lines) records only unanchored_pass and intent; every other change also requires execution, integration, test_quality, and residual_risk.","properties":{
                        "unanchored_pass":{"type":"boolean"},
                        "intent":{"type":"string"},
                        "execution":{"type":"string","description":"Required unless the change is light (test or documentation files only, or at most 60 changed lines)."},
                        "integration":{"type":"string","description":"Required unless the change is light (test or documentation files only, or at most 60 changed lines)."},
                        "test_quality":{"type":"string","description":"Required unless the change is light (test or documentation files only, or at most 60 changed lines)."},
                        "residual_risk":{"type":"string","description":"Required unless the change is light (test or documentation files only, or at most 60 changed lines)."},
                        "independent_reviewer_id":{"type":"string"},
                        "audit_adjudication":{"type":"string"}
                    },"required":["unanchored_pass","intent"]}
                },"required":["worker_id","summary","findings","evidence"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_check".to_string(),
                description: "Record a real completed check against the current worker tip; never record a command you did not run.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"name":{"type":"string"},"status":{"type":"string","enum":["passed","failed"]},"detail":{"type":"string"}},"required":["worker_id","name","status"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "review_fix".to_string(),
                description: "Apply one exact, tiny source replacement for an open finding, commit it on the worker branch, and invalidate review/checks.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"finding_id":{"type":"string"},"path":{"type":"string"},"old_string":{"type":"string"},"new_string":{"type":"string"}},"required":["worker_id","finding_id","path","old_string","new_string"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(30_000),
            },
            HostToolSpec {
                name: "team_route_fix".to_string(),
                description: "Route a larger finding back to its tracked worker for repair and delta review. Pass `tier` to run this fix round on a ladder tier, or `auto` to classify the finding; auto never steps down for high-severity findings or shared contracts. The prompt includes every open finding.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"finding_id":{"type":"string"},"tier":{"type":"string","enum":["auto","trivial","routine","medium","high","escalate"]}},"required":["worker_id","finding_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_resolve_finding".to_string(),
                description: "Close a finding with fixed evidence or a written wontfix justification.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"},"finding_id":{"type":"string"},"status":{"type":"string","enum":["fixed","wontfix"]},"evidence":{"type":"string"},"justification":{"type":"string"}},"required":["worker_id","finding_id","status"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_merge".to_string(),
                description: "Merge one reviewed worker only after exact-tip checks and findings close; never targets main and never pushes. After a clean merge Mnemo2 closes the worker's session, releases its checkout (legacy folders are removed; pool folders stay detached for reuse) and deletes its branch (a safe delete: a branch that is not fully merged is kept), so never delete worker branches yourself; the merge commit keeps the worker's tip as its second parent.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"}},"required":["worker_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(120_000),
            },
            HostToolSpec {
                name: "team_revert_merge".to_string(),
                description: "Revert a worker's earlier merge into the lead branch, only while that merge has not yet reached main.".to_string(),
                parameters: json!({"type":"object","properties":{"worker_id":{"type":"string"}},"required":["worker_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(120_000),
            },
            HostToolSpec {
                name: "team_agent_done".to_string(),
                description: "Finish a worker that runs as a Claude Code agent: record its final report and mark it completed (or failed, with failed=true). Call it each time your Claude Code agent for that worker finishes a run (the step, a continuation after team_message, or a fix round after team_route_fix); then team_check, team_review and team_merge work as for any worker.".to_string(),
                parameters: json!({"type":"object","properties":{
                    "worker_id":{"type":"string","description":"The agent worker to close."},
                    "report":{"type":"string","description":"The agent's final report: the commit hash, the checks it ran and their results, and anything left open."},
                    "failed":{"type":"boolean","description":"True when the agent could not finish the step."}
                },"required":["worker_id","report"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_agent_started".to_string(),
                description: "Register the Claude Code agent you launched for an agent worker, right after launching it, so Mnemo follows its transcript: team_status then shows the agent's last activity, tool calls, tokens and whether it stalled or finished, and you get a notice when it goes silent for ten minutes or ends. Pass the agent's id; pass transcript_path when you know the agent's transcript file, otherwise Mnemo looks for ~/.claude/projects/*/*/subagents/agent-<agent_id>.jsonl (and keeps looking if it is not there yet). Only for a Working or Fixing agent worker; register again when you continue the step with a new agent.".to_string(),
                parameters: json!({"type":"object","properties":{
                    "worker_id":{"type":"string","description":"The agent worker the agent runs."},
                    "agent_id":{"type":"string","description":"The id of the Claude Code agent (the Agent tool's agent id)."},
                    "transcript_path":{"type":"string","description":"Optional: the agent's transcript file (agent-<agent_id>.jsonl)."}
                },"required":["worker_id","agent_id"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_progress".to_string(),
                description: "For a worker's Claude Code agent: post a one-line note after each milestone (code read, change in, tests pass, committed). The note goes into the worker's log and reaches the lead as a notice. Only for a Working or Fixing agent worker; the note is not empty and at most 500 characters.".to_string(),
                parameters: json!({"type":"object","properties":{
                    "worker_id":{"type":"string","description":"The worker this agent runs (the worker_id in its prompt)."},
                    "note":{"type":"string","description":"One line, at most 500 characters."}
                },"required":["worker_id","note"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_integrate".to_string(),
                description: "Merge an existing local branch into a worker's branch, in the worker's checkout, done by TeamManager: use it when a step needs another branch's work, or to bring a stale worker up to the lead branch. A conflict aborts the merge, changes nothing and lists the conflicting files. Refused while the worker is busy or its checkout is dirty.".to_string(),
                parameters: json!({"type":"object","properties":{
                    "worker_id":{"type":"string","description":"The worker whose branch receives the merge."},
                    "branch":{"type":"string","description":"An existing local branch to merge in, for example the lead branch or another worker's wt/agent-* branch."}
                },"required":["worker_id","branch"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(120_000),
            },
            HostToolSpec {
                name: "team_tiers".to_string(),
                description: "The live tier setup: each tier's models in order with their health (available, unavailable until a time after a usage limit, or not configured), its effort, whether it runs as a Claude Code agent when Claude Code leads, and the tier and model a dispatch would run on now. Read it before choosing a tier or model, and whenever a result says the tiers changed.".to_string(),
                parameters: json!({"type":"object","properties":{}}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "team_quality".to_string(),
                description: "Whether merged work holds up, per tier and per model: merges, first-pass share, average fix rounds and findings, high findings, reverts, merges a later bug-fix plan changed the same files for, and cost per merge. Read-only; use it when deciding a tier or model on evidence rather than on impressions.".to_string(),
                parameters: json!({"type":"object","properties":{
                    "window_days":{"type":"integer","description":"Days back from now to count merges over (default 30)."}
                }}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(30_000),
            },
            HostToolSpec {
                name: "session_search".to_string(),
                description: "Search what actually happened in past worker and lead conversations: the ground truth behind a memory record, a plan or a merge. Use memory_search first for decisions and root causes; use this to check a claim or see what was tried. With query it returns matching messages; with worker and seq and no query it returns that one message.".to_string(),
                parameters: json!({"type":"object","properties":{
                    "query":{"type":"string","description":"Words to find in past messages. Optional when worker and seq name one message."},
                    "worker":{"type":"string","description":"A worker id, or 'lead', to narrow the search or to read one message from that transcript."},
                    "seq":{"type":"integer","description":"The message's zero-based index in the transcript, with worker and no query."},
                    "limit":{"type":"integer","description":"Maximum matches (1-30, default 10)."}
                }}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(60_000),
            },
        ]
    }

    /// The worker's host tools: report back to the lead, and re-read the plan.
    pub fn worker_host_tools(&self) -> Vec<HostToolSpec> {
        vec![
            HostToolSpec {
                name: "message_parent".to_string(),
                description: "Send a short status or question to the lead.".to_string(),
                parameters: json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"]}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
            HostToolSpec {
                name: "plan_read".to_string(),
                description: "Re-read your plan as it stands now, including decisions and interfaces the lead added after you started: call it with no arguments. Pass `capability` only to read one OpenSpec capability's current spec (the folder names under .coding/openspec/specs, e.g. tier-ladder).".to_string(),
                parameters: json!({"type":"object","properties":{"capability":{"type":"string","description":"Only to read an OpenSpec capability's spec instead of your plan; omit it to read your plan."}}}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
        ]
    }

    /// Execute a built-in lead host-tool call.
    pub async fn handle_lead_host_tool(&self, name: &str, args: Value) -> ToolResult {
        // Boxed: the future of the whole tool table is large, and callers
        // (the in-app lead, tests) hold it on their own stack.
        Box::pin(self.handle_lead_host_tool_from(name, args, MessageSender::Lead)).await
    }

    /// Execute a lead host-tool call with its caller's identity attached.
    pub async fn handle_lead_host_tool_from(
        &self,
        name: &str,
        args: Value,
        sender: MessageSender,
    ) -> ToolResult {
        if name == "team_dispatch" {
            let (Some(brief), Some(worker_id)) = (
                args.get("brief").and_then(Value::as_str),
                args.get("worker_id").and_then(Value::as_str),
            ) else {
                return ToolResult::error(
                    "team_dispatch requires 'brief' and a caller-chosen stable 'worker_id'",
                );
            };
            let model = args
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_string);
            let review_risk = args
                .get("review_risk")
                .and_then(Value::as_str)
                .map(str::to_string);
            let read_back = args
                .get("read_back")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let tier = args.get("tier").and_then(Value::as_str).map(str::to_string);
            let reasoning_effort = args
                .get("reasoning_effort")
                .and_then(Value::as_str)
                .map(str::to_string);
            let plan_id = args
                .get("plan_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim();
            let step = args
                .get("step")
                .and_then(Value::as_u64)
                .and_then(|step| usize::try_from(step).ok())
                .unwrap_or(0);
            if plan_id.is_empty() || step < 1 {
                return ToolResult::error(
                    "team_dispatch requires 'plan_id' (a board plan id) and 'step' (a 1-based integer). Create the plan first with plan_create.",
                );
            }
            let mode = self
                .routing
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .mode;
            let explicit_model = model.is_some();
            let fold_request = if model.is_none() { tier.clone() } else { None };
            // Read before the dispatch: a deferred classification may already
            // be waiting on its timer once the worker exists.
            let mut warnings = self.lead_checkout_warnings().await;
            if sender == MessageSender::ExternalLead {
                if let Some(line) = self.tier_changes().changed_line() {
                    warnings.push(line);
                }
                if let Some(line) = model
                    .as_deref()
                    .and_then(|model| self.bypass_warning(tier.as_deref(), model))
                {
                    warnings.push(line);
                }
            }
            let result = self
                .dispatch_request(DispatchRequest {
                    brief: brief.to_string(),
                    tier,
                    dispatch_routing: true,
                    source: DispatchSource::LeadTool,
                    sender,
                    model,
                    reasoning_effort,
                    worker_id: Some(worker_id.to_string()),
                    plan: Some(PlanRef {
                        plan_id: plan_id.to_string(),
                        step: step - 1,
                    }),
                    review_risk,
                    read_back,
                })
                .await;
            return dispatch_result(
                result,
                "team_dispatch",
                mode,
                fold_request,
                explicit_model,
                warnings,
            );
        }
        if name == "team_audit_dispatch" {
            let Some(worker_id) = args.get("worker_id").and_then(Value::as_str) else {
                return ToolResult::error("team_audit_dispatch requires a 'worker_id' string");
            };
            let Some(reviewer_id) = args.get("reviewer_id").and_then(Value::as_str) else {
                return ToolResult::error("team_audit_dispatch requires a 'reviewer_id' string");
            };
            let requested_tier = args.get("tier").and_then(Value::as_str).unwrap_or("high");
            let model = args.get("model").and_then(Value::as_str).map(str::to_string);
            let explicit_model = model.is_some();
            let fold_request = model.is_none().then(|| requested_tier.to_string());
            let reasoning_effort = args
                .get("reasoning_effort")
                .and_then(Value::as_str)
                .map(str::to_string);
            let brief = args
                .get("brief")
                .and_then(Value::as_str)
                .map(str::to_string);
            let result = self
                .audit_dispatch_with_tier(
                    worker_id,
                    reviewer_id,
                    Some(requested_tier),
                    model,
                    reasoning_effort,
                    brief,
                )
                .await
                // Audits are never classified: no deferred label can land.
                .map(|(worker, reused)| (worker, reused, false));
            return dispatch_result(
                result,
                "team_audit_dispatch",
                RoutingMode::Off,
                fold_request,
                explicit_model,
                Vec::new(),
            );
        }
        if name == "team_prove_regression" || name == "team_mutation_check" {
            let Some(worker_id) = args.get("worker_id").and_then(Value::as_str) else {
                return ToolResult::error(format!("{name} requires a 'worker_id' string"));
            };
            let result = if name == "team_prove_regression" {
                self.prove_regression_tool(worker_id).await
            } else {
                self.mutation_check_tool(worker_id).await
            };
            return match result {
                Ok(text) => ToolResult::success(text),
                Err(error) => ToolResult::error(error),
            };
        }
        if name == "team_run_checks" {
            let worker_id = args.get("worker_id").and_then(Value::as_str);
            return match self.run_checks_tool(worker_id).await {
                Ok(text) => ToolResult::success(text),
                Err(error) => ToolResult::error(error),
            };
        }
        if name == "team_agent_started" || name == "team_progress" {
            let Some(worker_id) = args.get("worker_id").and_then(Value::as_str) else {
                return ToolResult::error(format!("{name} requires a 'worker_id' string"));
            };
            if name == "team_progress" {
                let Some(note) = args.get("note").and_then(Value::as_str) else {
                    return team_tool_error("team_progress requires a 'note' string".to_string());
                };
                return match Box::pin(self.agent_progress(worker_id, note)).await {
                    Ok(worker) => ToolResult::success(format!("Posted the note for {worker_id}."))
                        .with_data(serde_json::to_value(&worker).unwrap_or(Value::Null)),
                    Err(error) => team_tool_error(error),
                };
            }
            let Some(agent_id) = args.get("agent_id").and_then(Value::as_str) else {
                return team_tool_error("team_agent_started requires an 'agent_id' string".to_string());
            };
            let transcript = args.get("transcript_path").and_then(Value::as_str);
            return match Box::pin(self.agent_started(worker_id, agent_id, transcript)).await {
                Ok((worker, text)) => ToolResult::success(text)
                    .with_data(serde_json::to_value(&worker).unwrap_or(Value::Null)),
                Err(error) => team_tool_error(error),
            };
        }
        if name == "team_integrate" {
            let (Some(worker_id), Some(branch)) = (
                args.get("worker_id").and_then(Value::as_str),
                args.get("branch").and_then(Value::as_str),
            ) else {
                return ToolResult::error("team_integrate requires 'worker_id' and 'branch' strings");
            };
            return match self.integrate(worker_id, branch).await {
                Ok((worker, agent_note)) => {
                    let mut answer = worker_answer(&worker, "team_integrate");
                    if let Some(merged) = worker.integrations.last() {
                        answer.output = format!(
                            "{}\nMerged {} into {worker_id} at {}; its checks and review start over on the new tip.",
                            answer.output,
                            merged.branch,
                            super::dispatch::short_sha(&merged.commit)
                        );
                    }
                    if let Some(note) = agent_note {
                        answer.output = format!(
                            "{}\nPass this to its Claude Code agent with your next message: {note}",
                            answer.output
                        );
                    }
                    answer
                }
                Err(error) => team_tool_error(error),
            };
        }
        if name == "team_tiers" {
            if sender == MessageSender::ExternalLead {
                self.tier_changes().mark_seen();
            }
            let view = self.tiers_view();
            let output = serde_json::to_string(&json!({"action": "team_tiers", "result": view}))
                .unwrap_or_else(|_| "team_tiers".to_string());
            return ToolResult::success(output).with_data(view);
        }
        if name == "team_quality" {
            let window_days = args
                .get("window_days")
                .and_then(Value::as_u64)
                .and_then(|days| u32::try_from(days).ok())
                .unwrap_or(crate::mnemo2::quality::DEFAULT_WINDOW_DAYS);
            return match crate::mnemo2::quality::report(
                self.workspace_root.clone(),
                window_days,
            )
            .await
            {
                Ok(report) => {
                    let text = crate::mnemo2::quality::summary(&report);
                    let data = serde_json::to_value(&report).unwrap_or(Value::Null);
                    ToolResult::success(text.clone()).with_data(json!({
                        "summary": text,
                        "report": data,
                    }))
                }
                Err(error) => ToolResult::error(error),
            };
        }
        if name == "team_status" {
            let plan_id = args
                .get("plan_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty());
            let in_plan = |worker: &WorkerRecord| {
                plan_id.is_none_or(|id| worker.plan_id.as_deref() == Some(id))
            };
            let result = match args.get("worker_id").and_then(Value::as_str) {
                Some(id) => self
                    .worker(id)
                    .await
                    .map(|worker| vec![worker].into_iter().filter(|w| in_plan(w)).collect()),
                None => Ok(self
                    .snapshot()
                    .await
                    .workers
                    .into_iter()
                    .filter(|w| in_plan(w))
                    .collect::<Vec<_>>()),
            };
            // The compact entry is the default because the answer lands in the
            // lead's own context; `detail` asks for the whole record.
            let detail = args.get("detail").and_then(Value::as_bool).unwrap_or(false);
            let result = match result {
                Ok(workers) => Ok(self.status_views(&workers, detail).await),
                Err(err) => Err(err),
            };
            return team_status_result(result, self.lead_status());
        }
        if name == "session_search" {
            return self.session_search(args).await;
        }
        let Some(worker_id) = args.get("worker_id").and_then(Value::as_str) else {
            return ToolResult::error(format!("{name} requires a 'worker_id' string"));
        };
        // The stale base is read before a merge, which would add the merge
        // commit to what the branch lacks.
        let stale = if matches!(name, "team_check" | "team_merge") {
            self.stale_base_of(worker_id).await
        } else {
            None
        };
        // A team_resume that handed the step to the lead's Claude Code agent
        // (plan e52e0be1 decision 5) answers with the block to launch.
        let mut handoff_block: Option<Value> = None;
        let result = match name {
            "team_message" => match args.get("text").and_then(Value::as_str) {
                Some(text) => self.message(worker_id, text.to_string(), sender).await,
                None => Err("team_message requires a 'text' string".to_string()),
            },
            "team_stop" => self.stop(worker_id).await,
            "team_agent_done" => {
                let report = args.get("report").and_then(Value::as_str);
                let failed = args.get("failed").and_then(Value::as_bool).unwrap_or(false);
                match report {
                    Some(report) if !report.trim().is_empty() => {
                        self.agent_done(worker_id, report.to_string(), failed)
                            .await
                    }
                    _ => Err("team_agent_done requires a non-empty 'report' string".to_string()),
                }
            }
            "team_resume" => {
                let tier = args.get("tier").and_then(Value::as_str).map(str::to_string);
                let model = args.get("model").and_then(Value::as_str).map(str::to_string);
                let reasoning_effort = args
                    .get("reasoning_effort")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                if tier.is_none() && model.is_none() && reasoning_effort.is_none() {
                    self.resume(worker_id).await
                } else {
                    self.resume_on_tier(
                        worker_id,
                        tier.as_deref(),
                        model,
                        reasoning_effort,
                        ChangeSource::Lead,
                    )
                    .await
                    .map(|resumed| {
                        handoff_block = resumed.claude_agent;
                        resumed.worker
                    })
                }
            }
            "team_compact" => self.compact(worker_id).await,
            "team_review" => {
                let summary = args.get("summary").and_then(Value::as_str).unwrap_or("");
                let evidence = args
                    .get("evidence")
                    .cloned()
                    .ok_or_else(|| {
                        "team_review requires a complete 'evidence' object with unanchored_pass and intent; execution, integration, test_quality, and residual_risk are also required unless the change is light (test or documentation files only, or at most 60 changed lines)"
                            .to_string()
                    })
                    .and_then(|value| {
                        serde_json::from_value::<ReviewEvidence>(value)
                            .map_err(|err| format!("invalid review evidence: {err}"))
                    });
                let findings = phase3::parse_findings(
                    &args.get("findings").cloned().unwrap_or_else(|| json!([])),
                );
                match (evidence, findings) {
                    (Ok(evidence), Ok(findings)) => {
                        self.review(worker_id, summary.to_string(), findings, evidence)
                            .await
                    }
                    (Ok(_), Err(err)) => Err(format!("invalid findings: {err}")),
                    (Err(err), _) => Err(err),
                }
            }
            "team_check" => {
                self.record_check(
                    worker_id,
                    args.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    args.get("status")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    args.get("detail")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                )
                .await
            }
            "review_fix" => {
                self.review_fix(
                    worker_id,
                    args.get("finding_id").and_then(Value::as_str).unwrap_or(""),
                    args.get("path").and_then(Value::as_str).unwrap_or(""),
                    args.get("old_string").and_then(Value::as_str).unwrap_or(""),
                    args.get("new_string").and_then(Value::as_str).unwrap_or(""),
                )
                .await
            }
            "team_route_fix" => {
                self.route_fix_with_tier(
                    worker_id,
                    args.get("finding_id").and_then(Value::as_str).unwrap_or(""),
                    args.get("tier").and_then(Value::as_str),
                )
                .await
            }
            "team_resolve_finding" => {
                self.resolve_finding(
                    worker_id,
                    args.get("finding_id").and_then(Value::as_str).unwrap_or(""),
                    args.get("status").and_then(Value::as_str).unwrap_or(""),
                    args.get("evidence")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    args.get("justification")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                )
                .await
            }
            // The lead's own turn is parked on this call, so its busy flag
            // does not mean it is editing the branch.
            "team_merge" => self.merge_as(worker_id, true).await,
            "team_revert_merge" => self.revert_merge_as(worker_id, true).await,
            _ => Err(format!("unknown team host tool '{name}'")),
        };
        let reviewed = result.as_ref().ok().cloned();
        let mut answer = match (result, handoff_block) {
            (Ok(worker), Some(block)) => agent_handoff_answer(&worker, block),
            (Ok(worker), None)
                if worker.claude_agent.is_some()
                    && matches!(name, "team_message" | "team_route_fix") =>
            {
                continue_agent_answer(&worker, name)
            }
            (result, _) => worker_result(result, name),
        };
        if let (true, Some(missing)) = (answer.success, stale) {
            answer.output = format!("{}\n{}", answer.output, stale_base_line(missing));
        }
        // Resume and route-fix results carry no `warnings` array: the tier
        // change line follows the JSON, as the stale-base line does.
        if answer.success
            && sender == MessageSender::ExternalLead
            && matches!(name, "team_resume" | "team_route_fix")
        {
            if let Some(line) = self.tier_changes().changed_line() {
                answer.output = format!("{}\n{line}", answer.output);
            }
        }
        if name == "team_review" && answer.success {
            if let Some(line) = finding_categories_line(&self.workspace_root) {
                answer.output = format!("{}\n{line}", answer.output);
            }
            // A category that keeps coming back is one line in the answer, so
            // the lead reads "this is now a proposal" instead of reviewing it
            // for the fourth time.
            if let Some(reviewed) = &reviewed {
                if let Some(line) = self.start_missing_checks(reviewed).await {
                    answer.output = format!("{}\n{line}", answer.output);
                }
                let lines =
                    findings_ledger::recurrence_lines(&self.workspace_root, &reviewed.findings);
                for line in lines {
                    answer.output.push('\n');
                    answer.output.push_str(&line);
                }
            }
        }
        answer
    }

    /// The status views `team_status` answers, each with its `stale_base`
    /// when the branch lacks lead commits.
    fn status_views<'a>(
        &'a self,
        workers: &'a [WorkerRecord],
        detail: bool,
    ) -> BoxFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let mut views = Vec::with_capacity(workers.len());
            for worker in workers {
                let mut view = worker_status_view(worker, detail);
                if let (Some(agent), Some(object)) =
                    (self.agent_view(worker).await, view.as_object_mut())
                {
                    object.insert("agent".to_string(), agent.to_json());
                }
                if let (Some(missing), Some(object)) =
                    (self.stale_base(worker).await, view.as_object_mut())
                {
                    object.insert("stale_base".to_string(), json!(missing));
                }
                views.push(view);
            }
            views
        })
    }

    /// The stale base of one worker, `None` when unknown or not worth reporting.
    fn stale_base_of<'a>(&'a self, worker_id: &'a str) -> BoxFuture<'a, Option<u64>> {
        Box::pin(async move {
            let worker = self.worker(worker_id).await.ok()?;
            self.stale_base(&worker).await
        })
    }

    /// `session_search`: search the past worker and lead transcripts, or read
    /// one message by its source and zero-based sequence number.
    async fn session_search(&self, args: Value) -> ToolResult {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|query| !query.is_empty())
            .map(str::to_string);
        let worker = args
            .get("worker")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|worker| !worker.is_empty())
            .map(str::to_string);
        let seq = args
            .get("seq")
            .and_then(Value::as_u64)
            .and_then(|seq| usize::try_from(seq).ok());
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|limit| usize::try_from(limit).ok())
            .unwrap_or(10);
        let Some(state_dir) = self.persistence.path.parent().map(std::path::Path::to_path_buf)
        else {
            return ToolResult::error("session_search cannot find the team's state directory");
        };
        let index_path = state_dir.join("transcript_index.db");
        let sources = transcript_sources(&state_dir);
        let task = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let mut index = TranscriptIndex::open(&index_path)?;
            let report = index.sync(&sources);
            if let Some(query) = query {
                // A worker with a query narrows the search; seq is ignored.
                let hits = index.search(&query, worker.as_deref(), limit)?;
                Ok(format_search_hits(&hits, &report))
            } else if let (Some(worker), Some(seq)) = (worker, seq) {
                match index.message(&worker, seq)? {
                    Some(hit) => Ok(format_one_message(&index, &hit)),
                    None => Ok(format!("No message #{seq} in {worker}.")),
                }
            } else {
                Err(session_search_usage())
            }
        })
        .await;
        match task {
            Ok(Ok(output)) => ToolResult::success(output),
            Ok(Err(error)) => ToolResult::error(error),
            Err(error) => ToolResult::error(format!("session_search task failed: {error}")),
        }
    }
}

/// How many characters of hit lines one `session_search` answer carries; the
/// header and the cut notice always show.
const SESSION_SEARCH_OUTPUT_CHARS: usize = 6_144;

/// The transcript files a team's state directory holds: every
/// `transcripts/<worker id>.json`, plus `lead.json` as the `lead` source.
fn transcript_sources(state_dir: &std::path::Path) -> Vec<TranscriptSource> {
    let mut sources = Vec::new();
    if let Ok(entries) = std::fs::read_dir(state_dir.join("transcripts")) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let Some(name) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            sources.push(TranscriptSource {
                name: name.to_string(),
                path,
            });
        }
    }
    sources.sort_by(|left, right| left.name.cmp(&right.name));
    let lead = state_dir.join("lead.json");
    if lead.is_file() {
        sources.push(TranscriptSource {
            name: "lead".to_string(),
            path: lead,
        });
    }
    sources
}

/// The `session_search` answer for a query: one line per hit under a match
/// count, with the unreadable-source notice on the first line.
fn format_search_hits(hits: &[TranscriptHit], report: &SyncReport) -> String {
    let mut out = format!("{} matches", hits.len());
    if !report.unreadable.is_empty() {
        out.push_str(&format!(
            " ({} transcripts unreadable: {})",
            report.unreadable.len(),
            report.unreadable.join(", ")
        ));
    }
    let mut used = 0;
    let mut cut = false;
    for hit in hits {
        let line = format!("\n{} #{} {}: {}", hit.source, hit.seq, hit.role, hit.text);
        let chars = line.chars().count();
        if used + chars > SESSION_SEARCH_OUTPUT_CHARS {
            cut = true;
            break;
        }
        used += chars;
        out.push_str(&line);
    }
    if cut {
        out.push_str("\n[cut: output reached 6144 characters; narrow the query or pass a worker and seq]");
    }
    out
}

/// The `session_search` answer for one message: its address and stored text,
/// then the sequence numbers that exist next to it.
fn format_one_message(index: &TranscriptIndex, hit: &TranscriptHit) -> String {
    let mut out = format!("{} #{} {}\n{}", hit.source, hit.seq, hit.role, hit.text);
    let mut neighbours = Vec::new();
    if hit.seq > 0 && matches!(index.message(&hit.source, hit.seq - 1), Ok(Some(_))) {
        neighbours.push(hit.seq - 1);
    }
    if matches!(index.message(&hit.source, hit.seq + 1), Ok(Some(_))) {
        neighbours.push(hit.seq + 1);
    }
    if !neighbours.is_empty() {
        let numbers: Vec<String> = neighbours.iter().map(|seq| format!("#{seq}")).collect();
        out.push_str(&format!("\nNeighbouring messages: {}.", numbers.join(", ")));
    }
    out
}

/// How to call `session_search` when the arguments name neither a search nor
/// one message.
fn session_search_usage() -> String {
    "session_search needs a query to search, or both worker and seq to read one message. \
     Example: {\"query\": \"flaky test\", \"worker\": \"agent-abc\"} or \
     {\"worker\": \"lead\", \"seq\": 42}."
        .to_string()
}

/// Strip every transcript, plus the transcript-revision bookkeeping only the
/// window's own sync uses, from a tool answer handed to a lead session or an
/// external client.
fn without_transcripts(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.remove("transcript");
            object.remove("transcript_revs");
            object.remove("omitted_transcripts");
            for (key, nested) in object.iter_mut() {
                // The `agent` object's `transcript` is the agent's file path,
                // not a conversation.
                if key != "agent" {
                    without_transcripts(nested);
                }
            }
        }
        Value::Array(items) => {
            for nested in items {
                without_transcripts(nested);
            }
        }
        _ => {}
    }
}

/// Who leads the team in a `team_status` answer: an external Claude Code
/// session that leads or idled out, or the in-app lead.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub(super) struct TeamLeadStatus {
    /// `external` while a Claude Code session leads or has idled out;
    /// `in_app` when no external session has taken the team.
    pub(super) kind: &'static str,
    /// Whether an external lead is attached right now.
    pub(super) attached: bool,
    /// Unix epoch milliseconds it attached, while attached.
    pub(super) since_ms: Option<u64>,
    /// Unix epoch milliseconds it was released for silence, while idle.
    pub(super) idle_since_ms: Option<u64>,
}

/// The first eight characters of a commit id: enough to tell tips apart
/// without repeating a full hash in every answer.
fn tip_prefix(tip: &str) -> String {
    tip.chars().take(8).collect()
}

/// One worker as `team_status` reports it: the compact entry the lead reads
/// by default, or the whole record on `detail`. Transcripts are dropped from
/// the text of both forms by [`without_transcripts`].
pub(super) fn worker_status_view(worker: &WorkerRecord, detail: bool) -> Value {
    if detail {
        return serde_json::to_value(worker).unwrap_or(Value::Null);
    }
    let findings = worker
        .findings
        .iter()
        .filter(|finding| finding.status == "open")
        .map(|finding| {
            json!({
                "id": finding.id,
                "title": finding.title,
                "severity": finding.severity,
                "file": finding.file,
                "line": finding.line,
            })
        })
        .collect::<Vec<_>>();
    let checks = worker
        .checks
        .iter()
        .map(|check| {
            json!({
                "name": check.name,
                "status": check.status,
                "tip": tip_prefix(&check.tip),
            })
        })
        .collect::<Vec<_>>();
    // The observed model id beats the selected spec: it is what actually ran.
    // A Claude Code agent worker shows the agent it runs as: it has no
    // session, so its selected spec never ran.
    let model = worker
        .claude_agent
        .as_ref()
        .map(|agent| format!("claude-code/{agent}"))
        .or_else(|| {
            worker
                .model_id
                .as_ref()
                .or(worker.model.as_ref())
                .cloned()
        });
    let mut view = json!({
        "id": worker.id,
        "name": worker.name,
        "status": worker.status,
        "kind": worker.kind,
        "tier": worker.tier,
        "model": model,
        "plan_id": worker.plan_id,
        "plan_step": worker.plan_step,
        "branch": worker.branch,
        "worktree": worker.worktree,
        "error": worker.error,
        "pending": worker.pending,
        "findings": findings,
        "checks": checks,
        "cost_micros": worker.cost_micros,
        "last_activity_ms": worker.last_activity_ms,
    });
    if let Some(routing) = &worker.routing {
        view["routing"] = json!(routing);
    }
    if let Some(review) = &worker.review {
        let mut review_view = json!({
            "summary": review.summary,
            "tip": tip_prefix(&review.tip),
        });
        if let Some(lane) = &review.lane {
            // Which lane accepted this review — "light review: …" or
            // "full review: …" — so the lead knows which evidence applied.
            review_view["lane"] = json!(lane);
        }
        view["review"] = review_view;
    }
    if let Some(merge) = &worker.merge {
        view["merge"] = json!({ "commit": merge.commit });
    }
    if !worker.tool_errors.is_empty() {
        view["tool_errors"] = json!(worker.tool_errors);
    }
    view
}

/// The `team_status` answer: the worker entries under `result` (an array, as
/// existing readers expect) with the lead's own state next to it.
fn team_status_result(result: Result<Vec<Value>, String>, lead: TeamLeadStatus) -> ToolResult {
    let entries = match result {
        Ok(entries) => entries,
        Err(err) => return team_tool_error(err),
    };
    let data = Value::Array(entries);
    let mut reported = data.clone();
    without_transcripts(&mut reported);
    let output = serde_json::to_string(&json!({
        "action": "team_status",
        "result": reported,
        "lead": lead,
    }))
    .unwrap_or_else(|_| "team_status".to_string());
    ToolResult::success(output).with_data(data)
}

/// The one failure every team tool answers with: the lead reads the message,
/// while `data` keeps the machine-readable kind.
pub(super) fn team_tool_error(err: String) -> ToolResult {
    ToolResult::error(err.clone()).with_data(json!({
        "error_kind": "domain",
        "message": err,
    }))
}

/// One worker as a team tool answers it: the text carries the compact entry
/// (the answer lands in the lead's own context), while `data` keeps the whole
/// record for internal callers. `team_status` with `detail: true` serves the
/// whole record in the text when the lead asks for it.
fn worker_answer(worker: &WorkerRecord, action: &str) -> ToolResult {
    let mut reported = worker_status_view(worker, false);
    without_transcripts(&mut reported);
    let output = serde_json::to_string(&json!({"action": action, "result": reported}))
        .unwrap_or_else(|_| action.to_string());
    let data = serde_json::to_value(worker).unwrap_or(Value::Null);
    ToolResult::success(output).with_data(data)
}

/// The `team_resume` answer when the move handed the worker's step to the
/// lead's Claude Code agent: the worker entry beside the `claude_agent` block,
/// in the result and in `data`, the way a dispatch answers.
fn agent_handoff_answer(worker: &WorkerRecord, block: Value) -> ToolResult {
    let mut reported = worker_status_view(worker, false);
    without_transcripts(&mut reported);
    let output = serde_json::to_string(&json!({
        "action": "team_resume",
        "result": {"worker": reported, "claude_agent": block},
    }))
    .unwrap_or_else(|_| "team_resume".to_string());
    let output = format!(
        "{output}\n{} now runs as a Claude Code agent: launch your Agent with the claude_agent block (prompt unchanged), then call team_agent_done.",
        worker.id
    );
    let data = serde_json::to_value(worker).unwrap_or(Value::Null);
    ToolResult::success(output).with_data(json!({"worker": data, "claude_agent": block}))
}

/// The `team_message` or `team_route_fix` answer for a worker that runs as a
/// Claude Code agent (plan ca7f2177 decision 5): Mnemo recorded the message
/// or the fix round, and the lead passes the text on to its agent. The
/// `continue_agent` block `{agent_id, worktree, message}` is in the result
/// and in `data`; the text says how to continue, with the message below it.
fn continue_agent_answer(worker: &WorkerRecord, action: &str) -> ToolResult {
    let message = worker
        .inbox
        .last()
        .map(|entry| entry.text.clone())
        .unwrap_or_default();
    let block = json!({
        "agent_id": worker.claude_agent_id,
        "worktree": worker.worktree,
        "message": message,
    });
    let mut reported = worker_status_view(worker, false);
    without_transcripts(&mut reported);
    let output = serde_json::to_string(&json!({
        "action": action,
        "result": {"worker": reported, "continue_agent": block},
    }))
    .unwrap_or_else(|_| action.to_string());
    let how = match worker.claude_agent_id.as_deref() {
        Some(agent_id) => format!(
            "Continue {}'s Claude Code agent with SendMessage to {agent_id}, sending the message below; call team_agent_done when it finishes.",
            worker.id
        ),
        None => format!(
            "No agent is registered for {}: start a new Agent in {} with this message, then call team_agent_started.",
            worker.id,
            worker.worktree.display()
        ),
    };
    let data = serde_json::to_value(worker).unwrap_or(Value::Null);
    ToolResult::success(format!("{output}\n{how}\n\n{message}"))
        .with_data(json!({"worker": data, "continue_agent": block}))
}

/// A team tool answer for one worker.
fn worker_result(result: Result<WorkerRecord, String>, action: &str) -> ToolResult {
    match result {
        Ok(worker) => worker_answer(&worker, action),
        Err(err) => team_tool_error(err),
    }
}

/// The finding categories in use and their worker counts, for the lead
/// reading a fresh review; `None` until some review names a category.
fn finding_categories_line(root: &Path) -> Option<String> {
    let counts = findings_ledger::category_counts(&findings_ledger::read_rows(root));
    if counts.is_empty() {
        return None;
    }
    let listed = counts
        .iter()
        .take(10)
        .map(|(category, count)| format!("{category} ({count})"))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("Finding categories in use: {listed}"))
}
