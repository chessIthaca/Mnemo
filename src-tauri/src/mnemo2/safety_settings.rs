// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Safety settings commands for Mnemo2's Settings dialog — the backend of
//! its **Safety** page (plan f1bd54e5, step 1).
//!
//! Two surfaces ride this module:
//!
//! - **Approval mode** — `[general] safety` in the shared `config.toml`,
//!   written through the one sanctioned pipeline (`Setup::patch_settings`
//!   → `validate_and_apply_settings_patch` → `Config::save_all`). On an
//!   OPEN lead that same call moves the engine's live
//!   `Arc<RwLock<SafetyMode>>` (`src/headless/setup.rs:569-573`), which the
//!   dispatch path reads per tool call (`src/agent/dispatch.rs:556`), so a
//!   save applies to the lead's very next action with no restart. The mode
//!   gates the LEAD only: worker sessions are spawned with
//!   `PermissionPolicy::OverridePermissions`
//!   (`src-tauri/src/mnemo2/team.rs:2750`), which skips the approval branch
//!   entirely (`src/agent/dispatch.rs:589-612` — an `ApprovalBypassed`
//!   audit, then the call runs), so workers never prompt. The page states
//!   that plainly instead of implying a gate the team does not have.
//! - **Safety rules** — the open project's `<project>/.coding/safety.toml`
//!   auto-approve allow-list the lead's `ApproveViaApi` policy consults at
//!   dispatch time (a live `SafetyRules` re-reads the file on its mtime
//!   check). Read and replaced through `Setup::safety_rules` /
//!   `Setup::set_safety_rules`; rules are per project, so the editor is
//!   reported closed without an open lead.
//! - **Git commands that always ask** — `[git] core_operations` in the same
//!   `config.toml`: the git subcommands that force the approval prompt even
//!   in an auto-approve mode or when a rule matches (`Tool::never_auto_for`,
//!   enforced on the `GitTool` and the shell's git guard). Saved through the
//!   same `Setup::patch_settings` call, which pushes the list into the live
//!   factory (`src/headless/setup.rs:593`), so an open lead's very next git
//!   call follows it — no restart. Machine-global, so the field shows with
//!   or without a project; team workers never prompt in any case.
//!
//! Every write is validated first: the mode by the shared settings
//! validator, each rule's literal pattern by the kernel's own
//! `Rule::new` (a typo'd regex would otherwise save fine and silently
//! never match), and each git subcommand by [`validate_git_approval`] (a
//! wrong word would save fine and never gate anything).

use std::path::Path;

use serde::Serialize;
use tauri::State;

use mnemo::config::settings_dto::SettingsSaveDto;
use mnemo::config::{Config, SafetyMode};
use mnemo::headless::{SafetyRuleDto, SafetyRulesDto, Setup};
use mnemo::safety_rules::{Rule, RuleKind};

use crate::mnemo2::ipc::{settings_setup, Mnemo2State};
use crate::mnemo2::runtime::LeadRuntime;

// ── Wire shape (the `safety_settings_get` snapshot) ─────────────────────────

/// The Safety page's snapshot: the saved `[general] safety` mode, the open
/// project's rules (empty without one), the machine-global git approval
/// list, and the two hints the page needs to stay honest about when a save
/// takes effect.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SafetySettingsWire {
    /// The saved mode's kebab-case word (`approve-each-action`, …) — exactly
    /// the strings [`mnemo::config::patch::parse_safety_mode`] accepts.
    pub mode: String,
    /// `<project>/.coding/safety.toml`, in file order. Empty when no
    /// project is open (there is no file to read).
    pub rules: Vec<SafetyRuleDto>,
    /// `[git] core_operations`: git subcommands the lead always asks about
    /// before running, even in an auto-approve mode or when a rule matches.
    /// Machine-global (config.toml), so it shows with or without a project.
    pub git_approval: Vec<String>,
    /// Whether a project is open — the rules are per project, so the
    /// editor is off without one.
    pub project_open: bool,
    /// Whether an open lead's engine is live, so a mode save lands on its
    /// next tool call. False: the save persists and applies when the next
    /// project opens.
    pub live_lead: bool,
}

/// The kebab-case wire word for a mode — the exact strings the shared
/// settings validator parses back.
fn safety_mode_word(mode: SafetyMode) -> &'static str {
    match mode {
        SafetyMode::ApproveEachAction => "approve-each-action",
        SafetyMode::AutoReadApproveWrites => "auto-read-approve-writes",
        SafetyMode::AutoApproveProject => "auto-approve-project",
        SafetyMode::Autonomous => "autonomous",
    }
}

// ── Validation ──────────────────────────────────────────────────────────────

/// Validate a rules set before it replaces the project's file. Every rule
/// needs a tool name and a pattern; a `Literal` pattern must compile under
/// the kernel's own matcher (an invalid regex skips the rule at load time,
/// so an unvalidated save would look applied while never matching). A
/// `CommandClass` pattern is a normalized class string compared by
/// equality — nothing to compile, only non-empty.
fn validate_rules(rules: &[SafetyRuleDto]) -> Result<(), String> {
    for (index, rule) in rules.iter().enumerate() {
        let number = index + 1;
        if rule.tool.trim().is_empty() {
            return Err(format!("Rule {number}: the tool name must not be empty."));
        }
        if rule.pattern.trim().is_empty() {
            return Err(format!("Rule {number}: the pattern must not be empty."));
        }
        if matches!(rule.kind, RuleKind::Literal) {
            Rule::new(rule.tool.trim(), &rule.pattern)
                .map_err(|err| format!("Rule {number}: {err}"))?;
        }
    }
    Ok(())
}

/// Validate and normalize `[git] core_operations` before it is saved. Each
/// entry must name one git subcommand (`merge`, `push`, `rebase`, …): an
/// ASCII letter or digit first, then ASCII letters, digits and `-`. Case and
/// surrounding spaces fold, empties drop, and duplicates keep their
/// first-seen order. An empty list is valid — nothing forces a prompt, and
/// the mode and rules decide.
fn validate_git_approval(ops: &[String]) -> Result<Vec<String>, String> {
    let mut clean: Vec<String> = Vec::new();
    for raw in ops {
        let entry = raw.trim();
        if entry.is_empty() {
            continue;
        }
        let folded = entry.to_lowercase();
        let mut chars = folded.chars();
        let valid = chars
            .next()
            .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
            && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
        if !valid {
            return Err(format!(
                "\"{entry}\" is not a git subcommand — use one word, like merge or push."
            ));
        }
        if !clean.contains(&folded) {
            clean.push(folded);
        }
    }
    Ok(clean)
}

/// Save `[general] safety` through the shared patch pipeline (validation +
/// persist + the live handle on an open lead).
async fn save_mode(setup: &Setup, mode: &str) -> Result<(), String> {
    let patch = SettingsSaveDto {
        safety: Some(mode.to_string()),
        ..SettingsSaveDto::default()
    };
    setup.patch_settings(patch).await.map_err(|err| err.to_string())
}

/// REPLACE the open project's `<project>/.coding/safety.toml` after
/// [`validate_rules`]; a live lead re-reads the file on its next dispatch.
async fn save_rules(
    setup: &Setup,
    project: &Path,
    rules: Vec<SafetyRuleDto>,
) -> Result<(), String> {
    validate_rules(&rules)?;
    setup
        .set_safety_rules(project, SafetyRulesDto { rules })
        .await
        .map_err(|err| err.to_string())
}

/// Save `[git] core_operations` through the shared patch pipeline
/// (validation + persist + the live factory's list on an open lead).
async fn save_git_approval_list(setup: &Setup, ops: Vec<String>) -> Result<(), String> {
    let clean = validate_git_approval(&ops)?;
    let patch = SettingsSaveDto {
        core_operations: Some(clean),
        ..SettingsSaveDto::default()
    };
    setup.patch_settings(patch).await.map_err(|err| err.to_string())
}

/// Read the page snapshot from a `Setup` on the lead's (or the machine
/// global) config dir. `runtime` is `Some` when a lead is open: its
/// workspace root is the project whose rules are shown, and its engine
/// carries the live mode the save moves.
async fn read_snapshot(
    setup: &Setup,
    runtime: Option<&LeadRuntime>,
) -> Result<SafetySettingsWire, String> {
    let config = Config::load(setup.config_dir()).map_err(|err| err.to_string())?;
    let rules = match runtime {
        Some(runtime) => setup.safety_rules(runtime.workspace_root()).await.rules,
        None => Vec::new(),
    };
    Ok(SafetySettingsWire {
        mode: safety_mode_word(config.general.general.safety).to_string(),
        rules,
        git_approval: config.general.git.core_operations.clone(),
        project_open: runtime.is_some(),
        live_lead: runtime.is_some(),
    })
}

// ── Commands ────────────────────────────────────────────────────────────────

/// The Safety page snapshot: the current approval mode and, with a project
/// open, its auto-approve rules.
#[tauri::command]
pub async fn safety_settings_get(
    state: State<'_, Mnemo2State>,
) -> Result<SafetySettingsWire, String> {
    let (setup, runtime) = settings_setup(&state).await?;
    read_snapshot(&setup, runtime.as_deref()).await
}

/// Save `[general] safety`; returns the fresh snapshot. On an open lead the
/// engine's live mode moves with the write, so the next tool call follows
/// the new mode — no restart.
#[tauri::command]
pub async fn save_safety_mode(
    state: State<'_, Mnemo2State>,
    mode: String,
) -> Result<SafetySettingsWire, String> {
    let (setup, runtime) = settings_setup(&state).await?;
    save_mode(&setup, &mode).await?;
    read_snapshot(&setup, runtime.as_deref()).await
}

/// Save `[git] core_operations` (the git commands that always ask) and
/// return the fresh snapshot. Machine-global: works without an open
/// project; on an open lead the next git call follows it — no restart.
#[tauri::command]
pub async fn save_git_approval(
    state: State<'_, Mnemo2State>,
    operations: Vec<String>,
) -> Result<SafetySettingsWire, String> {
    let (setup, runtime) = settings_setup(&state).await?;
    save_git_approval_list(&setup, operations).await?;
    read_snapshot(&setup, runtime.as_deref()).await
}

/// Replace the open project's safety rules (the whole list) and return the
/// fresh snapshot. Fails without an open project — rules are per project.
#[tauri::command]
pub async fn save_safety_rules(
    state: State<'_, Mnemo2State>,
    rules: Vec<SafetyRuleDto>,
) -> Result<SafetySettingsWire, String> {
    let (setup, runtime) = settings_setup(&state).await?;
    let Some(runtime) = runtime else {
        return Err(
            "Open a project first — safety rules live in the project's .coding folder.".into(),
        );
    };
    save_rules(&setup, runtime.workspace_root(), rules).await?;
    read_snapshot(&setup, Some(runtime.as_ref())).await
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use mnemo::config::patch::parse_safety_mode;

    /// A temp config dir holding only `[general] safety` (every other key
    /// defaults) — the shape the settings pipeline rewrites fully.
    fn fixture() -> (tempfile::TempDir, Setup) {
        let dir = tempfile::tempdir().expect("config dir");
        std::fs::write(
            dir.path().join("config.toml"),
            "[general]\nsafety = \"auto-approve-project\"\n",
        )
        .expect("write config.toml");
        let setup = Setup::open(Some(dir.path().to_path_buf())).expect("setup");
        (dir, setup)
    }

    /// Every page word must be a mode the shared validator accepts — a
    /// typo here would ship a radio the save rejects.
    #[test]
    fn every_mode_word_round_trips_through_the_shared_parser() {
        let modes = [
            SafetyMode::ApproveEachAction,
            SafetyMode::AutoReadApproveWrites,
            SafetyMode::AutoApproveProject,
            SafetyMode::Autonomous,
        ];
        for mode in modes {
            let word = safety_mode_word(mode);
            assert_eq!(
                parse_safety_mode(word).expect("the page word parses"),
                mode,
                "word '{word}' must name its mode"
            );
        }
    }

    #[tokio::test]
    async fn save_mode_persists_and_reads_back() {
        let (dir, setup) = fixture();
        save_mode(&setup, "autonomous").await.expect("save the mode");
        let config = Config::load(dir.path()).expect("reload config");
        assert_eq!(config.general.general.safety, SafetyMode::Autonomous);
        // The snapshot reports the saved mode and, with no lead open, that
        // the change lands at the next project open.
        let wire = read_snapshot(&setup, None).await.expect("snapshot");
        assert_eq!(wire.mode, "autonomous");
        assert!(!wire.live_lead);
        assert!(!wire.project_open);
        assert!(wire.rules.is_empty());
    }

    /// A config with no `[git]` section still shows the historical default
    /// list, so the page never opens on an empty field by accident.
    #[tokio::test]
    async fn snapshot_reports_the_default_git_approval_list() {
        let (_dir, setup) = fixture();
        let wire = read_snapshot(&setup, None).await.expect("snapshot");
        assert_eq!(wire.git_approval, vec!["merge", "push"]);
    }

    /// Saved entries fold (case, spaces), drop empties and duplicates, and
    /// land in `config.toml` under `[git] core_operations` — the key the
    /// factory's live list is reloaded from.
    #[tokio::test]
    async fn save_git_approval_persists_cleaned_and_reads_back() {
        let (dir, setup) = fixture();
        save_git_approval_list(
            &setup,
            vec!["merge".into(), " push ".into(), "Rebase ".into(), "push".into()],
        )
        .await
        .expect("save the list");
        let config = Config::load(dir.path()).expect("reload config");
        assert_eq!(
            config.general.git.core_operations,
            vec!["merge", "push", "rebase"]
        );
        let wire = read_snapshot(&setup, None).await.expect("snapshot");
        assert_eq!(wire.git_approval, vec!["merge", "push", "rebase"]);
    }

    /// An empty list saves: nothing forces a prompt, and the mode and rules
    /// decide.
    #[tokio::test]
    async fn save_git_approval_accepts_an_empty_list() {
        let (dir, setup) = fixture();
        save_git_approval_list(&setup, Vec::new())
            .await
            .expect("save an empty list");
        let config = Config::load(dir.path()).expect("reload config");
        assert!(config.general.git.core_operations.is_empty());
    }

    /// A malformed entry is refused before the pipeline runs, so the file on
    /// disk is untouched and the error names what to fix.
    #[tokio::test]
    async fn save_git_approval_rejects_a_non_subcommand_and_leaves_the_file_alone() {
        let (dir, setup) = fixture();
        let path = dir.path().join("config.toml");
        let before = std::fs::read_to_string(&path).expect("read config.toml");
        let err = save_git_approval_list(&setup, vec!["push --force".into()])
            .await
            .expect_err("a malformed entry is rejected");
        assert!(err.contains("push --force"), "the error names it: {err}");
        let after = std::fs::read_to_string(&path).expect("reread config.toml");
        assert_eq!(after, before, "the rejection wrote nothing");
    }

    #[tokio::test]
    async fn save_mode_rejects_an_unknown_word_and_leaves_the_file_alone() {
        let (dir, setup) = fixture();
        let err = save_mode(&setup, "yolo")
            .await
            .expect_err("an unknown mode is rejected");
        assert!(err.contains("unknown safety mode"), "the error names it: {err}");
        let config = Config::load(dir.path()).expect("reload config");
        assert_eq!(
            config.general.general.safety,
            SafetyMode::AutoApproveProject,
            "the rejection wrote nothing"
        );
    }

    #[test]
    fn validate_rules_rejects_a_bad_regex_and_blank_fields() {
        let bad_regex = [SafetyRuleDto {
            tool: "file_write".into(),
            pattern: "([unclosed".into(),
            kind: RuleKind::Literal,
        }];
        let err = validate_rules(&bad_regex).expect_err("a bad regex is rejected");
        assert!(err.contains("Rule 1"), "the error numbers the rule: {err}");
        assert!(err.contains("invalid regex"), "the error names the cause: {err}");

        let blank_tool = [SafetyRuleDto {
            tool: "  ".into(),
            pattern: ".*".into(),
            kind: RuleKind::Literal,
        }];
        assert!(validate_rules(&blank_tool)
            .expect_err("a blank tool is rejected")
            .contains("tool name"));

        let blank_pattern = [SafetyRuleDto {
            tool: "shell".into(),
            pattern: " ".into(),
            kind: RuleKind::CommandClass,
        }];
        assert!(validate_rules(&blank_pattern)
            .expect_err("a blank pattern is rejected")
            .contains("pattern"));
    }

    #[test]
    fn validate_rules_accepts_literal_and_command_class_rules() {
        let rules = [
            SafetyRuleDto {
                tool: "file_write".into(),
                pattern: "^file_write:.*".into(),
                kind: RuleKind::Literal,
            },
            SafetyRuleDto {
                tool: "shell".into(),
                pattern: "cargo test".into(),
                kind: RuleKind::CommandClass,
            },
        ];
        validate_rules(&rules).expect("both kinds are valid");
    }

    #[tokio::test]
    async fn save_rules_writes_the_projects_safety_toml_and_reads_back() {
        let (_dir, setup) = fixture();
        let project = tempfile::tempdir().expect("project root");
        let rules = vec![
            SafetyRuleDto {
                tool: "file_write".into(),
                pattern: "^file_write:.*".into(),
                kind: RuleKind::Literal,
            },
            SafetyRuleDto {
                tool: "shell".into(),
                pattern: "cargo test".into(),
                kind: RuleKind::CommandClass,
            },
        ];
        save_rules(&setup, project.path(), rules.clone())
            .await
            .expect("save the rules");
        let path = project.path().join(".coding").join("safety.toml");
        assert!(path.exists(), "the project's safety.toml is written");
        // The kernel reads exactly what was saved (kind defaults skipped
        // for literal rules, so the round-trip is the file contract).
        assert_eq!(setup.safety_rules(project.path()).await.rules, rules);
    }

    #[tokio::test]
    async fn save_rules_rejects_a_bad_regex_without_writing() {
        let (_dir, setup) = fixture();
        let project = tempfile::tempdir().expect("project root");
        let err = save_rules(
            &setup,
            project.path(),
            vec![SafetyRuleDto {
                tool: "file_write".into(),
                pattern: "([unclosed".into(),
                kind: RuleKind::Literal,
            }],
        )
        .await
        .expect_err("the bad regex is rejected");
        assert!(err.contains("invalid regex"), "{err}");
        assert!(
            !project.path().join(".coding").join("safety.toml").exists(),
            "the rejection wrote nothing"
        );
    }
}
