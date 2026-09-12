// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `skill_start` + `skill_end` + `abandon_skill` — the skill lifecycle tools.
//!
//! A skill is a named, activatable overlay on the workflow (defined as a TOML
//! file in `.coding/skills/`). `skill_start` enters the skill: it validates the
//! skill exists in the registry + is `available_in` the current state, then
//! calls [`Workflow::start_skill`], seeding the skill's tool allow-list onto
//! the active skill so [`Workflow::allowed_tools`] returns the skill filter.
//! `skill_end` exits to the skill's `target_state`; `abandon_skill` rolls back
//! to the pre-skill state.
//!
//! All three are `AutoRun` (not approval-gated) — protection is on the
//! *operations* inside the skill, not the entry. Core operations (git merge /
//! push) are always approval-gated via [`Tool::never_auto_for`] regardless of
//! safety mode or active skill.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::provider::ToolSchema;
use crate::skill::SkillRegistry;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};
use crate::workflow::{Workflow, WorkflowState};

/// Arguments for `skill_start`.
#[derive(Debug, Deserialize)]
struct SkillStartArgs {
    /// The skill name (must exist in the registry).
    skill: String,
    /// Overrides the registry's prompt. When omitted, the registry's prompt is
    /// used.
    #[serde(default)]
    skill_prompt: Option<String>,
    /// Overrides the registry's target_state. When omitted, the registry's
    /// target_state is used.
    #[serde(default)]
    target_state: Option<WorkflowState>,
}

/// The `skill_start` workflow tool — enter a skill.
///
/// Validates the skill exists + is available in the current workflow state,
/// then transitions to [`WorkflowState::Skill`] with the skill's tool allow-list
/// + prompt as the active overlay. `AutoRun` — the entry is not approval-gated;
/// the operations inside the skill are.
pub struct SkillStartTool {
    workflow: Arc<Mutex<Workflow>>,
    registry: Arc<SkillRegistry>,
}

impl SkillStartTool {
    /// Create the tool, bound to a workflow handle + the skill registry.
    pub fn new(workflow: Arc<Mutex<Workflow>>, registry: Arc<SkillRegistry>) -> Self {
        Self { workflow, registry }
    }
}

#[async_trait]
impl Tool for SkillStartTool {
    fn name(&self) -> &str {
        "skill_start"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "skill_start",
            "Start a skill — a named overlay (defined in .coding/skills/<name>.toml) that \
             narrows the visible tools to its allow-list and injects a goal prompt. \
             Available in Complete and Planning, not Executing. Drive toward the skill's \
             goal, then skill_end (exit to the target state) or abandon_skill (roll back).",
            json!({
                "type": "object",
                "properties": {
                    "skill": {
                        "type": "string",
                        "description": "The skill name (must exist in the registry)."
                    },
                    "skill_prompt": {
                        "type": "string",
                        "description": "Overrides the skill's default goal prompt."
                    },
                    "target_state": {
                        "type": "string",
                        "enum": ["planning", "executing", "complete"],
                        "description": "Overrides the skill's default target state (where to land after skill_end)."
                    }
                },
                "required": ["skill"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        // The entry is not approval-gated — protection is on the operations
        // inside the skill (e.g. git merge/push are never_auto_for). Entering
        // a skill only changes tool visibility + the prompt; it performs no
        // mutation itself.
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: SkillStartArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(format!("invalid arguments: {e}")),
        };
        let spec = match self.registry.get(&args.skill) {
            Some(s) => s,
            None => {
                return ToolResult::error(format!(
                    "unknown skill '{}'. Available: {}",
                    args.skill,
                    self.registry
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
        };
        let mut wf = self.workflow.lock().await;
        let current = wf.state();
        if !self.registry.is_available_in(&args.skill, current) {
            return ToolResult::error(format!(
                "skill '{}' is not available in the {current} state (available in: {})",
                args.skill,
                spec.available_in
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        let prompt = args.skill_prompt.unwrap_or_else(|| spec.prompt.clone());
        let target_state = args.target_state.unwrap_or(spec.target_state);
        // Validate the landing state: only lifecycle states are legal
        // skill_end targets. `Skill` is an overlay (entered via start_skill,
        // never a landing state) and `Subagent` is a spawn-path-stamped role
        // state whose invariant (allow-list always set) a main-agent workflow
        // cannot satisfy — landing in either would panic `allowed_tools` on
        // the next turn (review finding 3, 2026-01-03). The schema enum is
        // only a hint; serde accepts any WorkflowState string, so enforce
        // here — this covers both the args override and the spec's TOML.
        if !matches!(
            target_state,
            WorkflowState::Planning | WorkflowState::Executing | WorkflowState::Complete
        ) {
            return ToolResult::error(format!(
                "invalid target_state '{target_state}': must be one of \"planning\", \"executing\", \"complete\""
            ));
        }
        match wf.start_skill(&args.skill, &prompt, target_state, spec.tools.clone()) {
            // Do NOT echo the prompt here: it is injected into the system
            // prompt every turn while the skill runs (prompt.rs
            // workflow_section), so echoing it would duplicate the full
            // goal text permanently in the conversation history.
            Ok(()) => ToolResult::success(format!(
                "started skill '{}' (target: {target_state}). Goal injected \
                 into the system prompt — follow it.",
                args.skill
            )),
            Err(e) => ToolResult::error(format!("failed to start skill: {e}")),
        }
    }
}

/// The `skill_end` workflow tool — exit the active skill to its target state.
///
/// `AutoRun` — exiting a skill performs no mutation; it only restores the
/// base workflow state. Returns an error if no skill is active.
pub struct SkillEndTool {
    workflow: Arc<Mutex<Workflow>>,
}

impl SkillEndTool {
    /// Create the tool, bound to a workflow handle.
    pub fn new(workflow: Arc<Mutex<Workflow>>) -> Self {
        Self { workflow }
    }
}

#[async_trait]
impl Tool for SkillEndTool {
    fn name(&self) -> &str {
        "skill_end"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "skill_end",
            "End the active skill and transition to its target state. Call this when the skill's \
             goal is achieved. Only available while a skill is active.",
            json!({"type": "object", "properties": {}}),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, _args: serde_json::Value) -> ToolResult {
        let mut wf = self.workflow.lock().await;
        match wf.end_skill() {
            Ok(()) => ToolResult::success("skill ended"),
            Err(e) => ToolResult::error(format!("failed to end skill: {e}")),
        }
    }
}

/// The `abandon_skill` workflow tool — roll back to the pre-skill state.
///
/// `AutoRun` — abandoning a skill performs no mutation; it only restores the
/// workflow to where it was before the skill started. Returns an error if no
/// skill is active.
pub struct AbandonSkillTool {
    workflow: Arc<Mutex<Workflow>>,
}

impl AbandonSkillTool {
    /// Create the tool, bound to a workflow handle.
    pub fn new(workflow: Arc<Mutex<Workflow>>) -> Self {
        Self { workflow }
    }
}

#[async_trait]
impl Tool for AbandonSkillTool {
    fn name(&self) -> &str {
        "abandon_skill"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "abandon_skill",
            "Abandon the active skill and roll back to the workflow state you were in before the \
             skill started. Use this to bail out of a skill without completing its goal. Only \
             available while a skill is active.",
            json!({"type": "object", "properties": {}}),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, _args: serde_json::Value) -> ToolResult {
        let mut wf = self.workflow.lock().await;
        match wf.abandon_skill() {
            Ok(()) => ToolResult::success("skill abandoned — rolled back to the pre-skill state"),
            Err(e) => ToolResult::error(format!("failed to abandon skill: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::SkillSpec;
    use tempfile::tempdir;

    /// Build a registry with one skill available in Complete + Planning.
    fn make_registry() -> SkillRegistry {
        let mut reg = SkillRegistry::new();
        reg.insert(SkillSpec {
            name: "merge_to_main".into(),
            available_in: vec![WorkflowState::Complete, WorkflowState::Planning],
            target_state: WorkflowState::Planning,
            tools: vec!["git".into(), "skill_end".into(), "abandon_skill".into()],
            prompt: "Merge the branch into main.".into(),
        });
        reg
    }

    fn make_tool(
        dir: &std::path::Path,
    ) -> (
        SkillStartTool,
        SkillEndTool,
        AbandonSkillTool,
        Arc<Mutex<Workflow>>,
    ) {
        let wf = Arc::new(Mutex::new(Workflow::new(dir.join("plans"))));
        let reg = Arc::new(make_registry());
        (
            SkillStartTool::new(wf.clone(), reg),
            SkillEndTool::new(wf.clone()),
            AbandonSkillTool::new(wf.clone()),
            wf,
        )
    }

    #[tokio::test]
    async fn start_skill_validates_available_in() {
        let dir = tempdir().unwrap();
        let (start, _end, _abandon, wf) = make_tool(dir.path());

        // From Planning — allowed.
        let r = start.execute(json!({"skill": "merge_to_main"})).await;
        assert!(r.success, "start from Planning: {}", r.output);
        assert_eq!(wf.lock().await.state(), WorkflowState::Skill);

        // End it so we can test the Executing rejection.
        wf.lock().await.end_skill().unwrap();
        assert_eq!(wf.lock().await.state(), WorkflowState::Planning);

        // Create a plan → Executing. skill_start must reject.
        wf.lock()
            .await
            .create_plan("P", "G", "C", vec!["a".into()])
            .unwrap();
        assert_eq!(wf.lock().await.state(), WorkflowState::Executing);
        let r = start.execute(json!({"skill": "merge_to_main"})).await;
        assert!(!r.success, "start from Executing must fail");
        assert!(r.output.contains("not available"), "got: {}", r.output);
    }

    #[tokio::test]
    async fn start_skill_unknown_skill_errors() {
        let dir = tempdir().unwrap();
        let (start, _, _, _) = make_tool(dir.path());
        let r = start.execute(json!({"skill": "nonexistent"})).await;
        assert!(!r.success);
        assert!(r.output.contains("unknown skill"));
    }

    #[tokio::test]
    async fn start_skill_overrides_prompt_and_target() {
        let dir = tempdir().unwrap();
        let (start, _, _, wf) = make_tool(dir.path());
        let r = start
            .execute(json!({
                "skill": "merge_to_main",
                "skill_prompt": "Custom merge goal.",
                "target_state": "complete"
            }))
            .await;
        assert!(r.success, "{}", r.output);
        let skill = wf.lock().await.active_skill().unwrap().clone();
        assert_eq!(skill.prompt, "Custom merge goal.");
        assert_eq!(skill.target_state, WorkflowState::Complete);
    }

    #[tokio::test]
    async fn start_skill_result_does_not_echo_the_prompt() {
        // The goal prompt is injected into the system prompt every turn the
        // skill runs (prompt.rs workflow_section) — echoing it in the tool
        // result duplicated the full text permanently in the conversation
        // history. The result must name the skill + target, not repeat the
        // goal, while the workflow still carries the full prompt.
        let dir = tempdir().unwrap();
        let (start, _, _, wf) = make_tool(dir.path());
        let r = start.execute(json!({"skill": "merge_to_main"})).await;
        assert!(r.success, "{}", r.output);
        assert!(
            !r.output.contains("Merge the branch into main."),
            "result must not echo the skill prompt, got: {}",
            r.output
        );
        assert!(r.output.contains("started skill 'merge_to_main'"));
        let skill = wf.lock().await.active_skill().unwrap().clone();
        assert_eq!(skill.prompt, "Merge the branch into main.");
    }

    #[tokio::test]
    async fn start_skill_rejects_non_lifecycle_target_state() {
        // Review finding 3 (2026-01-03): `target_state` arrives as a raw
        // string — serde accepts any WorkflowState variant, and the schema
        // enum is only a hint. "subagent" would land a MAIN-agent workflow in
        // Subagent state with no allow-list (allowed_tools panics on the next
        // turn); "skill" is an overlay, never a landing state. Both must be
        // rejected with a tool error, not a panic.
        let dir = tempdir().unwrap();
        let (start, _, _, wf) = make_tool(dir.path());
        for bad in ["subagent", "skill"] {
            let r = start
                .execute(json!({"skill": "merge_to_main", "target_state": bad}))
                .await;
            assert!(!r.success, "target_state '{bad}' must be rejected");
            assert!(
                r.output.contains("invalid target_state"),
                "error names the problem, got: {}",
                r.output
            );
        }
        // Nothing was entered: still Planning, no active skill.
        assert_eq!(wf.lock().await.state(), WorkflowState::Planning);
        assert!(wf.lock().await.active_skill().is_none());
    }

    #[tokio::test]
    async fn end_skill_transitions_to_target() {
        let dir = tempdir().unwrap();
        let (start, end, _, wf) = make_tool(dir.path());
        start.execute(json!({"skill": "merge_to_main"})).await;
        assert_eq!(wf.lock().await.state(), WorkflowState::Skill);
        let r = end.execute(json!({})).await;
        assert!(r.success, "{}", r.output);
        assert_eq!(wf.lock().await.state(), WorkflowState::Planning);
        assert!(wf.lock().await.active_skill().is_none());
    }

    #[tokio::test]
    async fn abandon_skill_rolls_back_to_pre_skill_state() {
        let dir = tempdir().unwrap();
        let (start, _, abandon, wf) = make_tool(dir.path());
        // Enter from Planning → abandon returns to Planning.
        start.execute(json!({"skill": "merge_to_main"})).await;
        let r = abandon.execute(json!({})).await;
        assert!(r.success, "{}", r.output);
        assert_eq!(wf.lock().await.state(), WorkflowState::Planning);
    }

    #[tokio::test]
    async fn end_skill_without_active_errors() {
        let dir = tempdir().unwrap();
        let (_, end, abandon, _) = make_tool(dir.path());
        let r = end.execute(json!({})).await;
        assert!(!r.success);
        let r = abandon.execute(json!({})).await;
        assert!(!r.success);
    }
}
