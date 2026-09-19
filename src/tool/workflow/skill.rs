// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The skill tools — `skill_start` / `skill_end` / `abandon_skill` (the
//! lifecycle) plus `skill_reload` / `skill_create` (the library).
//!
//! A skill is a named, activatable overlay on the workflow (defined as a TOML
//! file in `.coding/skills/`). `skill_start` enters the skill: it validates the
//! skill exists in the live [`SkillLibrary`] + is `available_in` the current
//! state, then calls [`Workflow::start_skill`], seeding the skill's tool
//! allow-list onto the active skill so [`Workflow::allowed_tools`] returns the
//! skill filter. `skill_end` exits to the skill's `target_state`;
//! `abandon_skill` rolls back to the pre-skill state. `skill_reload` re-reads
//! the skills dir into the live library (available in every workflow state, so
//! a file the user hand-edited never needs an app restart); `skill_create`
//! authors a new skill file and hot-adds it (Executing only).
//!
//! All five are `AutoRun` (not approval-gated) — protection is on the
//! *operations* inside the skill, not the entry or the authoring. Core
//! operations (git merge / push) are always approval-gated via
//! [`Tool::never_auto_for`] regardless of safety mode or active skill, and
//! `write_review_report` stays constructor-only — the Skill filter denies it,
//! so `skill_create` refuses to grant it.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;

use crate::provider::ToolSchema;
use crate::skill::{SkillLibrary, SkillSpec};
use crate::tool::agent::sandbox::Sandbox;
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
    library: Arc<SkillLibrary>,
}

impl SkillStartTool {
    /// Create the tool, bound to a workflow handle + the shared skill library.
    pub fn new(workflow: Arc<Mutex<Workflow>>, library: Arc<SkillLibrary>) -> Self {
        Self { workflow, library }
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
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        let spec = match self.library.read(|r| r.get(&args.skill).cloned()) {
            Some(s) => s,
            None => {
                return ToolResult::error(format!(
                    "unknown skill '{}'. Available: {}",
                    args.skill,
                    self.library.read(|r| r
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", "))
                ));
            }
        };
        let mut wf = self.workflow.lock().await;
        let current = wf.state();
        if !self.library.read(|r| r.is_available_in(&args.skill, current)) {
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

/// The lifecycle states a skill may be started from or land in.
///
/// `Skill` (an overlay, entered only through `start_skill`) and `Subagent` (a
/// spawn-path-stamped role state) are not landing states — landing in either
/// would panic `allowed_tools` on the next turn. Serde accepts any
/// `WorkflowState` string and the schema enum is only a hint, so every path
/// that takes a state from the model validates with this predicate.
fn is_lifecycle_state(state: WorkflowState) -> bool {
    matches!(
        state,
        WorkflowState::Planning | WorkflowState::Executing | WorkflowState::Complete
    )
}

/// The header comment written into every generated skill file.
///
/// TOML comments are never parsed — none of this reaches the model — so the
/// header is where the rationale lives, keeping `prompt` (which IS injected
/// into the system prompt on every turn the skill runs) terse and operative.
const SKILL_FILE_HEADER: &str = "\
# A Mnemo skill — authored with the `skill_create` tool.
#
# A skill is a named overlay on the workflow. `skill_start` enters it: `prompt`
# is injected into the system prompt EVERY turn it runs, and `tools` is the
# allow-list the agent picks from (the memory tools, ask_user, current_plan,
# skill_reload and the backlog tools are always available and need not be
# listed here).
# `skill_end` lands the workflow in `target_state`; `abandon_skill` rolls back.
#
# Keep `prompt` a terse operative checklist — rationale belongs HERE, in the
# comments, because only `prompt` costs context on every turn.
#
# Re-edit this file freely, then call `skill_reload` to pick the change up: the
# registry is otherwise loaded once, at app startup.
";

/// Render a skill's file contents (header comment + spec as TOML), verifying
/// that the rendered text parses back into the same spec.
///
/// The round-trip check runs BEFORE anything is written: a serialization that
/// does not reproduce the spec must never land on disk. The comparison is
/// field-level rather than `==` because [`SkillSpec`] carries no `PartialEq`.
fn render_skill_file(spec: &SkillSpec) -> Result<String, String> {
    let body = toml::to_string(spec).map_err(|e| format!("failed to serialize the skill: {e}"))?;
    let parsed: SkillSpec = toml::from_str(&body)
        .map_err(|e| format!("internal error: the rendered skill file does not parse: {e}"))?;
    if parsed.name != spec.name
        || parsed.prompt != spec.prompt
        || parsed.tools != spec.tools
        || parsed.available_in != spec.available_in
        || parsed.target_state != spec.target_state
    {
        return Err("internal error: the rendered skill file does not round-trip".to_string());
    }
    Ok(format!("{SKILL_FILE_HEADER}\n{body}"))
}

/// Validate a skill name.
///
/// The name is the registry key, the file stem AND part of a path, so the
/// alphabet is deliberately narrow: no separators, no dots, no uppercase, no
/// whitespace. Rejecting here is what makes the write path safe — the file path
/// is never derived from unvalidated model input.
fn validate_skill_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("skill name must not be empty".to_string());
    }
    if name.len() > 64 {
        return Err(format!(
            "skill name '{name}' is too long (max 64 characters)"
        ));
    }
    if !name
        .chars()
        .next()
        .map(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        .unwrap_or(false)
    {
        return Err(format!(
            "skill name '{name}' must start with a lowercase letter or a digit"
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
    {
        return Err(format!(
            "skill name '{name}' may only contain lowercase letters, digits, '_' and '-' \
             (it becomes .coding/skills/<name>.toml)"
        ));
    }
    Ok(())
}

/// Arguments for `skill_create`.
#[derive(Debug, Deserialize)]
struct SkillCreateArgs {
    /// The skill name — the registry key and the file stem.
    name: String,
    /// The workflow states the skill may be started from.
    available_in: Vec<WorkflowState>,
    /// Where the workflow lands after `skill_end`.
    target_state: WorkflowState,
    /// The tool allow-list active while the skill runs.
    tools: Vec<String>,
    /// The goal injected into the system prompt while the skill runs.
    prompt: String,
    /// Replace an existing `<name>.toml` (default: refuse).
    #[serde(default)]
    overwrite: bool,
}

/// The `skill_create` workflow tool — author a new skill file.
///
/// `AutoRun`: the file lands in the project's `.coding/skills/` side-car (the
/// same class as a plan or a knowledge file), the path is derived from a
/// validated name, and every tool the new skill names is still gated by its own
/// approval rule when it is finally called. Visibility is the real guard here:
/// the tool is Executing-only.
///
pub struct SkillCreateTool {
    library: Arc<SkillLibrary>,
    sandbox: Sandbox,
}

impl SkillCreateTool {
    /// Create the tool, bound to the shared skill library + the agent's sandbox.
    ///
    /// The sandbox is not decoration: the write target lives inside the project
    /// tree, so it goes through the same link-free creation ladder the file
    /// tools use ([`Sandbox::validate_for_write`]). Without it a planted
    /// hardlink at `.coding/skills/<name>.toml` would truncate the file it
    /// shares its inode with — the bypass class closed for the file tools by
    /// plan b4812291 (review HIGH 1, 2027-01-16).
    pub fn new(library: Arc<SkillLibrary>, sandbox: Sandbox) -> Self {
        Self { library, sandbox }
    }
}

#[async_trait]
impl Tool for SkillCreateTool {
    fn name(&self) -> &str {
        "skill_create"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "skill_create",
            "Author a new skill: writes .coding/skills/<name>.toml and registers it live, so it can be started with skill_start immediately. A skill is a named workflow overlay — skill_start enters it, injecting `prompt` into the system prompt EVERY turn while it runs and narrowing the visible tools to `tools`. Available in Executing only. The write is sandbox-mediated: a planted symlink or hardlink at the target is refused, and a skill shipped with the app (e.g. merge_to_main) is never rewritten — edit those with the approval-gated file tools. Keep `prompt` a terse operative checklist (rationale belongs in the file's header comment, which is never parsed). The memory tools, ask_user, current_plan, skill_reload and the backlog tools are always available inside a skill and need not be listed in `tools`.",
            json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "The skill name — the registry key and the file name (.coding/skills/<name>.toml). Lowercase letters, digits, '_' and '-'; must start with a letter or digit."
                    },
                    "available_in": {
                        "type": "array",
                        "items": {"type": "string", "enum": ["planning", "executing", "complete"]},
                        "description": "The workflow states this skill may be started from (at least one)."
                    },
                    "target_state": {
                        "type": "string",
                        "enum": ["planning", "executing", "complete"],
                        "description": "Where the workflow lands after skill_end."
                    },
                    "tools": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "The tool allow-list active while the skill runs (at least one tool name)."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The goal the agent drives toward — injected into the system prompt every turn the skill runs."
                    },
                    "overwrite": {
                        "type": "boolean",
                        "description": "Replace an existing <name>.toml (default false: refuse and leave the existing file untouched)."
                    }
                },
                "required": ["name", "available_in", "target_state", "tools", "prompt"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: SkillCreateArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
        };
        if let Err(e) = validate_skill_name(&args.name) {
            return ToolResult::error(e);
        }
        if crate::skill::is_shipped_skill(&args.name) {
            return ToolResult::error(format!(
                "'{}' is a skill shipped with the app — skill_create will not rewrite it. Edit the file with the approval-gated file tools (file_edit, or file_write when the file is missing) if you really mean to, or pick another name",
                args.name
            ));
        }
        let mut available_in: Vec<WorkflowState> = Vec::new();
        for state in args.available_in {
            if !is_lifecycle_state(state) {
                return ToolResult::error(format!(
                    "invalid available_in entry '{state}': must be one of 'planning', 'executing', 'complete'"
                ));
            }
            if !available_in.contains(&state) {
                available_in.push(state);
            }
        }
        if available_in.is_empty() {
            return ToolResult::error(
                "available_in must name at least one state: 'planning', 'executing' or 'complete'",
            );
        }
        if !is_lifecycle_state(args.target_state) {
            return ToolResult::error(format!(
                "invalid target_state '{}': must be one of 'planning', 'executing', 'complete'",
                args.target_state
            ));
        }
        let mut tools: Vec<String> = Vec::new();
        for tool in args.tools {
            let tool = tool.trim();
            if tool.is_empty() {
                return ToolResult::error("tools must not contain blank entries");
            }
            if tool == "write_review_report" {
                return ToolResult::error(
                    "write_review_report cannot be granted by a skill: a review report can only be authored by a spawned reviewer subagent (the Skill tool filter denies it regardless, so listing it would be silently useless)",
                );
            }
            if !tools.iter().any(|t| t == tool) {
                tools.push(tool.to_string());
            }
        }
        if tools.is_empty() {
            return ToolResult::error("tools must name at least one tool for the allow-list");
        }
        let prompt = args.prompt.trim();
        if prompt.is_empty() {
            return ToolResult::error(
                "prompt must not be empty — it is the goal the agent drives toward while the skill runs",
            );
        }
        let spec = SkillSpec {
            name: args.name.clone(),
            available_in,
            target_state: args.target_state,
            tools,
            prompt: prompt.to_string(),
        };
        let text = match render_skill_file(&spec) {
            Ok(t) => t,
            Err(e) => return ToolResult::error(e),
        };
        let dir = self.library.dir().to_path_buf();
        let path = dir.join(format!("{}.toml", spec.name));
        // Identity BEFORE the overwrite question: `Path::exists` FOLLOWS links,
        // so a DANGLING symlink at the target reads as "free" and the write
        // would create the file wherever it points. `symlink_metadata` does not
        // follow, so it sees the link either way. Deliberately stricter than the
        // sandbox, which writes to a canonical target that is itself a
        // legitimate project file: skill_create's contract is to create the
        // SKILL file, never to write through a link.
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
            return ToolResult::error(format!(
                "{} is a symbolic link — refusing to write through it (delete the link and retry, or edit the skill file with the approval-gated file tools)",
                path.display()
            ));
        }
        if path.exists() && !args.overwrite {
            return ToolResult::error(format!(
                "{} already exists — pass overwrite: true to replace it (the existing file is left untouched)",
                path.display()
            ));
        }
        // The skills dir must exist before `validate_for_write` runs for the
        // FILE: the sandbox's `validate` canonicalizes an EXISTING parent, and
        // that is what makes the check spelling-independent (the library dir
        // comes from the project's `skills_dir` while the sandbox root is
        // canonicalized — verbatim `\\?\` on Windows — so the lexical creation
        // fallback would refuse a valid path).
        //
        // Creating it here means the mkdir is ours to gate: `create_dir_all`
        // resolves each component as the OS does, so a link planted at a
        // NON-final component (`.coding` → elsewhere) would place this directory
        // outside the root, or plant a `skills` subtree inside a protected tree.
        // `validate_dir_creation` applies the ladder's first two gates to the
        // DIRECTORY itself — `file_write` never faces this because the ladder
        // creates its parents only AFTER the link-free gate (round-2 LOW 1).
        if let Err(e) = self.sandbox.validate_dir_creation(&dir) {
            return ToolResult::error(format!("refused to create {}: {e}", dir.display()));
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return ToolResult::error(format!("failed to create {}: {e}", dir.display()));
        }
        // Then the sandbox gate, exactly like file_write: the link-free creation
        // ladder, the `.coding/**` hardlink guard (a hardlink planted at the
        // target would otherwise truncate the file it shares its inode with),
        // and the protected-target re-check on the canonical result.
        let validated = match self.sandbox.validate_for_write(&path) {
            Ok(p) => p,
            Err(e) => return ToolResult::error(format!("refused to write {}: {e}", path.display())),
        };
        if let Err(e) = std::fs::write(&validated, &text) {
            return ToolResult::error(format!("failed to write {}: {e}", validated.display()));
        }
        let (name, avail, target, tool_count) = (
            spec.name.clone(),
            spec.available_in
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(", "),
            spec.target_state,
            spec.tools.len(),
        );
        // Hot-add: the file is now the source of truth AND the live library
        // knows it, so skill_start works without a skill_reload round-trip.
        self.library.insert(spec);
        ToolResult::success(format!(
            "created {}\nskill '{name}': available in {avail} · lands in {target} after 
             skill_end · {tool_count} tool(s)\nregistered live — start it with skill_start (from 
             {avail})",
            path.display()
        ))
    }
}

/// The `skill_reload` workflow tool — re-read the skills directory.
///
/// `AutoRun`: it reads the project's own skill files and swaps an in-memory
/// registry — no project mutation. Visible in every workflow state, because a
/// skill file can be hand-edited at any moment and the registry is otherwise
/// loaded only once, at startup.
///
pub struct SkillReloadTool {
    library: Arc<SkillLibrary>,
}

impl SkillReloadTool {
    /// Create the tool, bound to the shared skill library.
    pub fn new(library: Arc<SkillLibrary>) -> Self {
        Self { library }
    }
}

#[async_trait]
impl Tool for SkillReloadTool {
    fn name(&self) -> &str {
        "skill_reload"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Workflow
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "skill_reload",
            "Re-read .coding/skills/*.toml into the live skill registry and report what changed. \
             The registry is loaded once at app startup, so a skill file that was just created or \
             hand-edited is invisible until this runs. Available in every workflow state, \
             including while a skill is active.",
            json!({"type": "object", "properties": {}}),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::AutoRun
    }

    async fn execute(&self, _args: serde_json::Value) -> ToolResult {
        let report = self.library.reload();
        let mut out = format!(
            "reloaded {} — {} skill(s) loaded",
            self.library.dir().display(),
            report.total
        );
        if report.added.is_empty() && report.removed.is_empty() {
            out.push_str("\nno skill files added or removed");
        } else {
            if !report.added.is_empty() {
                out.push_str(&format!("\nadded: {}", report.added.join(", ")));
            }
            if !report.removed.is_empty() {
                out.push_str(&format!("\nremoved: {}", report.removed.join(", ")));
            }
        }
        if report.names.is_empty() {
            out.push_str("\nskills: (none)");
        } else {
            out.push_str(&format!("\nskills: {}", report.names.join(", ")));
        }
        ToolResult::success(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::{SkillRegistry, SkillSpec};
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
        let library = Arc::new(SkillLibrary::from_registry(dir.join("skills"), make_registry()));
        (
            SkillStartTool::new(wf.clone(), library),
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

    /// Build the library tools over a tempdir's `.coding/skills` dir.
    ///
    /// The dir mirrors production (inside `.coding/`) so the sandbox's
    /// `.coding/**` hardlink guard is really exercised, and the create tool gets
    /// a sandbox rooted at the tempdir exactly as the factory wires it.
    fn make_library_tools(
        dir: &std::path::Path,
    ) -> (SkillCreateTool, SkillReloadTool, Arc<SkillLibrary>) {
        let library = Arc::new(SkillLibrary::load(dir.join(".coding/skills")));
        let sandbox = Sandbox::new(dir).unwrap();
        (
            SkillCreateTool::new(library.clone(), sandbox),
            SkillReloadTool::new(library.clone()),
            library,
        )
    }

    /// Valid `skill_create` arguments for the skill `greet`.
    fn create_args(prompt: &str) -> serde_json::Value {
        json!({
            "name": "greet",
            "available_in": ["complete", "planning"],
            "target_state": "planning",
            "tools": ["git", "file_read"],
            "prompt": prompt,
        })
    }

    #[tokio::test]
    async fn create_skill_writes_a_loadable_file_and_hot_adds_it() {
        let dir = tempdir().unwrap();
        let (create, _reload, library) = make_library_tools(dir.path());
        let r = create.execute(create_args("Greet the user.")).await;
        assert!(r.success, "create: {}", r.output);
        assert!(
            r.output.contains("greet.toml"),
            "result names the written file: {}",
            r.output
        );
        assert!(
            r.output.contains("skill_start"),
            "result points at skill_start: {}",
            r.output
        );
        // The FILE is the source of truth: an independent load must see exactly
        // what was asked for (not the hot-added in-memory copy).
        let on_disk = SkillRegistry::load_dir(library.dir());
        let spec = on_disk.get("greet").expect("the written file must load");
        assert_eq!(
            spec.available_in,
            vec![WorkflowState::Complete, WorkflowState::Planning]
        );
        assert_eq!(spec.target_state, WorkflowState::Planning);
        assert_eq!(spec.tools, vec!["git".to_string(), "file_read".to_string()]);
        assert_eq!(spec.prompt, "Greet the user.");
        // …and the live library knows it with no reload round-trip.
        assert!(library.read(|reg| reg.get("greet").is_some()));
    }

    #[tokio::test]
    async fn create_skill_refuses_to_overwrite_without_the_flag() {
        let dir = tempdir().unwrap();
        let (create, _reload, library) = make_library_tools(dir.path());
        assert!(create.execute(create_args("First.")).await.success);

        let mut second = create_args("Second.");
        let r = create.execute(second.clone()).await;
        assert!(!r.success, "an existing file must not be replaced silently");
        assert!(
            r.output.contains("overwrite"),
            "the error names the escape hatch: {}",
            r.output
        );
        let on_disk = SkillRegistry::load_dir(library.dir());
        assert_eq!(on_disk.get("greet").unwrap().prompt, "First.");

        second["overwrite"] = json!(true);
        let r = create.execute(second).await;
        assert!(r.success, "with overwrite: {}", r.output);
        let on_disk = SkillRegistry::load_dir(library.dir());
        assert_eq!(on_disk.get("greet").unwrap().prompt, "Second.");
    }

    #[tokio::test]
    async fn create_skill_rejects_invalid_arguments_without_writing() {
        let dir = tempdir().unwrap();
        let (create, _reload, library) = make_library_tools(dir.path());
        let cases: Vec<(serde_json::Value, &str)> = vec![
            (json!({"name": ""}), "must not be empty"),
            (json!({"name": "Bad Name"}), "must start with"),
            (json!({"name": "../evil"}), "must start with"),
            (json!({"name": "-lead"}), "must start with"),
            (json!({"name": "badName"}), "may only contain"),
            (json!({"name": "bad name"}), "may only contain"),
            (json!({"name": "x".repeat(65)}), "too long"),
            (json!({"available_in": []}), "at least one state"),
            (json!({"available_in": ["subagent"]}), "invalid available_in entry"),
            (json!({"target_state": "skill"}), "invalid target_state"),
            (json!({"tools": []}), "at least one tool"),
            (json!({"tools": ["  "]}), "blank"),
            (json!({"tools": ["write_review_report"]}), "spawned reviewer"),
            (json!({"prompt": "   "}), "prompt must not be empty"),
        ];
        for (patch, expected) in cases {
            let mut args = create_args("Do it.");
            for (k, v) in patch.as_object().unwrap() {
                args[k.as_str()] = v.clone();
            }
            let r = create.execute(args).await;
            assert!(!r.success, "must reject patch {patch}: {}", r.output);
            assert!(
                r.output.contains(expected),
                "expected '{expected}' in: {}",
                r.output
            );
        }
        // Validation runs before any disk access at all: the skills dir was
        // never created. (A write ANYWHERE — that dir included — shows up here;
        // the previous read_dir form also read as empty for a missing dir, so it
        // could not tell "nothing written" from "written elsewhere".)
        assert!(
            !library.dir().exists(),
            "invalid arguments must not touch the filesystem: {} exists",
            library.dir().display()
        );
    }

    #[tokio::test]
    async fn create_skill_dedupes_states_and_tools() {
        let dir = tempdir().unwrap();
        let (create, _reload, library) = make_library_tools(dir.path());
        let mut args = create_args("Do it.");
        args["available_in"] = json!(["complete", "complete", "planning"]);
        args["tools"] = json!(["git", "git", "file_read"]);
        let r = create.execute(args).await;
        assert!(r.success, "{}", r.output);
        let on_disk = SkillRegistry::load_dir(library.dir());
        let spec = on_disk.get("greet").unwrap();
        assert_eq!(spec.tools, vec!["git".to_string(), "file_read".to_string()]);
        assert_eq!(spec.available_in.len(), 2);
    }

    #[tokio::test]
    async fn create_skill_refuses_a_linked_target() {
        // Regression for HIGH 1 (review 2027-01-16): a link planted at the
        // target must not redirect the write. Without the sandbox gate,
        // `overwrite: true` truncates the file the link points at / shares its
        // inode with — the bypass class plan b4812291 closed for the file tools.
        let dir = tempdir().unwrap();
        let (create, _reload, library) = make_library_tools(dir.path());
        std::fs::create_dir_all(library.dir()).unwrap();
        let victim = dir.path().join("victim.txt");
        std::fs::write(&victim, "do not touch").unwrap();
        let target = library.dir().join("greet.toml");

        // Hardlink — every platform, no privileges needed. The sandbox's
        // `.coding/**` link-count guard sees it.
        std::fs::hard_link(&victim, &target).unwrap();
        let mut args = create_args("Write through a link.");
        args["overwrite"] = json!(true);
        let r = create.execute(args.clone()).await;
        assert!(
            !r.success,
            "a hardlinked target must be refused: {}",
            r.output
        );
        assert_eq!(
            std::fs::read_to_string(&victim).unwrap(),
            "do not touch",
            "the file the link shares its inode with must be untouched"
        );
        std::fs::remove_file(&target).unwrap();

        // Symlink — skipped where the platform refuses to plant one (no
        // Developer Mode on Windows); the sandbox's own fixture reports that.
        if crate::tool::agent::sandbox::link_fixture::plant_file_link(&victim, &target) {
            let r = create.execute(args).await;
            assert!(
                !r.success,
                "a symlinked target must be refused: {}",
                r.output
            );
            assert_eq!(std::fs::read_to_string(&victim).unwrap(), "do not touch");
        }
    }

    #[tokio::test]
    async fn create_skill_refuses_a_linked_skills_dir() {
        // Round-2 LOW 1: `create_dir_all` resolves every component as the OS
        // does, so a link planted at the skills DIR (a non-final component of
        // the target) would place the mkdir at the link's destination. The dir
        // gate runs BEFORE the mkdir, so nothing is created through the link.
        let dir = tempdir().unwrap();
        let (create, _reload, library) = make_library_tools(dir.path());
        let outside = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        if !crate::tool::agent::sandbox::link_fixture::plant_dir_link(
            outside.path(),
            library.dir(),
        ) {
            eprintln!("SKIP: could not plant a directory link at the skills dir");
            return;
        }
        let r = create.execute(create_args("Plant a subtree outside.")).await;
        assert!(
            !r.success,
            "a linked skills dir must be refused: {}",
            r.output
        );
        // The refusal must come from the DIR gate, not merely from the file-write
        // ladder further down. Pre-fix (gate deleted) `create_dir_all` succeeds
        // through the link and `validate_for_write` refuses with its own wording,
        // so only this assertion makes the test fail without the gate — the
        // original form was vacuously green (round-3 LOW 3).
        assert!(
            r.output.contains("refused to create"),
            "the directory gate must be what refuses a linked skills dir: {}",
            r.output
        );
        assert_eq!(
            std::fs::read_dir(outside.path()).unwrap().count(),
            0,
            "the link target must stay empty — no file, no directory"
        );
    }

    #[tokio::test]
    async fn create_skill_works_under_a_symlinked_ancestor() {
        // Round-3 HIGH 1: a link ABOVE the sandbox root must not refuse the create
        // happy path. macOS spells `/tmp` and `/var` as links into `/private`, and
        // the dir gate judged the caller's spelling component by component (the
        // filesystem root included), so it refused a skills dir with no link in it.
        // Judged through the deepest EXISTING ancestor, the alias resolves away.
        let real = tempdir().unwrap();
        let holder = tempdir().unwrap();
        let alias = holder.path().join("alias");
        if !crate::tool::agent::sandbox::link_fixture::plant_dir_link(real.path(), &alias) {
            eprintln!("SKIP: could not plant a directory link for the ancestor fixture");
            return;
        }
        let (create, _reload, library) = make_library_tools(&alias);
        let r = create.execute(create_args("Greet under an alias.")).await;
        assert!(
            r.success,
            "a link above the root must not refuse the create: {}",
            r.output
        );
        assert!(
            real.path().join(".coding/skills/greet.toml").is_file(),
            "the skill file must land inside the root the sandbox named"
        );
        assert!(library.read(|reg| reg.get("greet").is_some()));
    }

    #[tokio::test]
    async fn create_skill_refuses_a_dangling_symlink_target() {
        // The insidious half of HIGH 1: a DANGLING link reads as "does not
        // exist", so the overwrite refusal is skipped and the write would
        // CREATE the file the link points at — outside the skills dir.
        let dir = tempdir().unwrap();
        let (create, _reload, library) = make_library_tools(dir.path());
        std::fs::create_dir_all(library.dir()).unwrap();
        let escaped = dir.path().join("escaped.toml");
        let missing_dir = dir.path().join("missing-dir");
        let target = library.dir().join("greet.toml");
        // The fixture's rule: try the file symlink (needs Developer Mode on
        // Windows), fall back to a privilege-free directory link, and SAY the
        // skip when neither can be planted — a silently-green regression is how
        // the dangling-leaf hole survived three review rounds (round-2 LOW 2).
        let planted = crate::tool::agent::sandbox::link_fixture::plant_file_link(&escaped, &target)
            || crate::tool::agent::sandbox::link_fixture::plant_dir_link(&missing_dir, &target);
        if !planted {
            eprintln!("SKIP: could not plant a dangling link at the skill target");
            return;
        }
        let r = create.execute(create_args("Escape the skills dir.")).await;
        assert!(!r.success, "a dangling link must be refused: {}", r.output);
        assert!(
            !escaped.exists() && !missing_dir.exists(),
            "nothing may be created through the link ({} / {})",
            escaped.display(),
            missing_dir.display()
        );
    }

    #[tokio::test]
    async fn create_skill_refuses_to_rewrite_a_shipped_skill() {
        // Regression for LOW 2 (review 2027-01-16): a shipped file is the source
        // of truth for its procedure and is re-seeded only while missing, so an
        // AutoRun rewrite would silently and permanently replace one. Editing it
        // stays possible through the approval-gated file tools.
        let dir = tempdir().unwrap();
        let (create, _reload, library) = make_library_tools(dir.path());
        let mut args = create_args("Rewrite the merge procedure.");
        args["name"] = json!("merge_to_main");
        args["overwrite"] = json!(true);
        let r = create.execute(args).await;
        assert!(
            !r.success,
            "a shipped skill must not be rewritten: {}",
            r.output
        );
        assert!(
            r.output.contains("shipped"),
            "the error explains why: {}",
            r.output
        );
        assert!(!library.dir().join("merge_to_main.toml").exists());
    }

    #[tokio::test]
    async fn reload_skill_picks_up_a_hand_edited_file_and_drops_a_deleted_one() {
        let dir = tempdir().unwrap();
        let (_create, reload, library) = make_library_tools(dir.path());
        assert!(library.read(|reg| reg.get("hand").is_none()));

        std::fs::create_dir_all(library.dir()).unwrap();
        std::fs::write(
            library.dir().join("hand.toml"),
            "name = \"hand\"\navailable_in = [\"complete\"]\ntarget_state = \"planning\"\n\
             tools = [\"git\"]\nprompt = \"Hand written.\"\n",
        )
        .unwrap();

        let r = reload.execute(json!({})).await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("added: hand"),
            "reports the add: {}",
            r.output
        );
        assert!(library.read(|reg| reg.get("hand").is_some()));
        // The live registry carries the file's CONTENT, not just its name.
        assert_eq!(
            library.read(|reg| reg.get("hand").map(|s| s.prompt.clone())),
            Some("Hand written.".to_string())
        );

        std::fs::remove_file(library.dir().join("hand.toml")).unwrap();
        let r = reload.execute(json!({})).await;
        assert!(r.success, "{}", r.output);
        assert!(
            r.output.contains("removed: hand"),
            "reports the removal: {}",
            r.output
        );
        assert!(library.read(|reg| reg.get("hand").is_none()));
    }

    #[tokio::test]
    async fn reload_skill_reports_an_empty_dir() {
        let dir = tempdir().unwrap();
        let (_create, reload, _library) = make_library_tools(dir.path());
        let r = reload.execute(json!({})).await;
        assert!(r.success, "{}", r.output);
        assert!(r.output.contains("0 skill(s)"), "{}", r.output);
        assert!(r.output.contains("no skill files added or removed"), "{}", r.output);
        assert!(r.output.contains("skills: (none)"), "{}", r.output);
    }
}
