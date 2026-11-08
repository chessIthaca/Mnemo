// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The lead's host-tool surface: the tool specs a lead or worker session
//! is offered, the executor for lead tool calls, and the answer builders
//! (`team_status`, worker answers) they return.

use mnemo::headless::HostToolSpec;
use mnemo::memory::transcript_index::{
    SyncReport, TranscriptHit, TranscriptIndex, TranscriptSource,
};
use mnemo::tool::{SafetyLevel, ToolResult};
use serde::Serialize;
use serde_json::{json, Value};

use crate::mnemo2::dispatch_routing::RoutingMode;
use crate::mnemo2::phase3::{self, ReviewEvidence};

use super::record::{ChangeSource, MessageSender, WorkerRecord};
use super::routing::tier_ids;
use super::dispatch::{dispatch_result, DispatchRequest, DispatchSource, PlanRef};
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
                description: "Persist your review of the current worker tip and its complete finding list. Review more than the worker's summary: read the diff unanchored first, then record substantive evidence — intent, execution path, integration, test quality, and residual risk. A passing test count alone is not a review. Re-review after every fix. Example finding: {\"title\":\"main.js uses a default import\",\"detail\":\"game.js exports Game as a named export, so the page fails to load\",\"file\":\"src/main.js\",\"line\":1,\"severity\":\"high\"}.".to_string(),
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
                            "justification":{"type":"string","description":"Required when status is wontfix."}
                        },
                        "required":["title","detail","file","severity"]
                    }},
                    "evidence":{"type":"object","properties":{
                        "unanchored_pass":{"type":"boolean"},
                        "intent":{"type":"string"},
                        "execution":{"type":"string"},
                        "integration":{"type":"string"},
                        "test_quality":{"type":"string"},
                        "residual_risk":{"type":"string"},
                        "independent_reviewer_id":{"type":"string"},
                        "audit_adjudication":{"type":"string"}
                    },"required":["unanchored_pass","intent","execution","integration","test_quality","residual_risk"]}
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
                description: "Merge one reviewed worker only after exact-tip checks and findings close; never targets main and never pushes. After a clean merge Mnemo2 closes the worker's session, removes its checkout and deletes its branch (a safe delete: a branch that is not fully merged is kept), so never delete worker branches yourself; the merge commit keeps the worker's tip as its second parent.".to_string(),
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
                description: "Re-read your plan as it stands now (decisions and interfaces the lead added after you started), or with capability, that capability's current OpenSpec spec.".to_string(),
                parameters: json!({"type":"object","properties":{"capability":{"type":"string","description":"Optional kebab-case capability name, e.g. tier-ladder"}}}),
                safety: SafetyLevel::AutoRun,
                timeout_ms: Some(10_000),
            },
        ]
    }

    /// Execute a built-in lead host-tool call.
    pub async fn handle_lead_host_tool(&self, name: &str, args: Value) -> ToolResult {
        self.handle_lead_host_tool_from(name, args, MessageSender::Lead)
            .await
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
            let result = self
                .dispatch_request(DispatchRequest {
                    brief: brief.to_string(),
                    tier,
                    dispatch_routing: true,
                    source: DispatchSource::LeadTool,
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
            );
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
            let result = result.map(|workers| {
                workers
                    .iter()
                    .map(|worker| worker_status_view(worker, detail))
                    .collect::<Vec<_>>()
            });
            return team_status_result(result, self.lead_status());
        }
        if name == "session_search" {
            return self.session_search(args).await;
        }
        let Some(worker_id) = args.get("worker_id").and_then(Value::as_str) else {
            return ToolResult::error(format!("{name} requires a 'worker_id' string"));
        };
        let result = match name {
            "team_message" => match args.get("text").and_then(Value::as_str) {
                Some(text) => self.message(worker_id, text.to_string(), sender).await,
                None => Err("team_message requires a 'text' string".to_string()),
            },
            "team_stop" => self.stop(worker_id).await,
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
                    match self.lead_model_choice(tier.as_deref(), model) {
                        Err(err) => Err(err),
                        Ok(model) => {
                            // An effort alone keeps the worker's model.
                            let model = match model {
                                Some(model) => Some(model),
                                None => self.worker(worker_id).await.ok().and_then(|w| w.model),
                            };
                            let changed = self
                                .change_model(worker_id, model, reasoning_effort, ChangeSource::Lead)
                                .await;
                            match (changed, tier) {
                                (Ok(_), Some(tier)) => {
                                    self.record_tier(worker_id, &tier).await;
                                    self.worker(worker_id).await
                                }
                                (changed, _) => changed,
                            }
                        }
                    }
                }
            }
            "team_compact" => self.compact(worker_id).await,
            "team_review" => {
                let summary = args.get("summary").and_then(Value::as_str).unwrap_or("");
                let evidence = args
                    .get("evidence")
                    .cloned()
                    .ok_or_else(|| {
                        "team_review requires a complete 'evidence' object with unanchored_pass, intent, execution, integration, test_quality, and residual_risk"
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
        worker_result(result, name)
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
            for nested in object.values_mut() {
                without_transcripts(nested);
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
    let model = worker.model_id.as_ref().or(worker.model.as_ref());
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
        view["review"] = json!({
            "summary": review.summary,
            "tip": tip_prefix(&review.tip),
        });
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

/// A team tool answer for one worker.
fn worker_result(result: Result<WorkerRecord, String>, action: &str) -> ToolResult {
    match result {
        Ok(worker) => worker_answer(&worker, action),
        Err(err) => team_tool_error(err),
    }
}
