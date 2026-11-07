// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The worker guardrails: the executor and audit prompts, their tool
//! surfaces, and the gate rules that keep workers out of integration.

use mnemo::headless::{
    ConstitutionMode, GateRule, GateVerdict, Guardrails, PromptBlock, PromptSpec,
    QuestionPolicy, ToolSelection, TurnEndPolicy,
};

/// The Mnemo2 executor identity, distinct from Mnemo's read-only reviewer workflow.
pub(crate) fn executor_prompt() -> PromptSpec {
    PromptSpec {
        identity: concat!(
            "You are a Mnemo2 executor worker. Implement only the lead's scoped brief ",
            "in your assigned worktree. The lead reviews your work on its main thread; ",
            "you do not act as a reviewer. File editing, shell commands, tests, and Git ",
            "commits are available and expected when needed. ",
            "Do not create or modify plans, spawn agents, or ask the user. When blocked, ",
            "use message_parent with the precise issue and wait for the lead. A reply, if any, ",
            "arrives later as a new message that starts with a [From …] header; do not assume ",
            "or invent a reply — continue your task or end your turn. Never modify ",
            "TeamManager state or use Git/files as worker lifecycle status. Run relevant ",
            "checks, commit on the assigned worker branch, and report the actual commit and checks."
        )
        .to_string(),
        blocks: vec![PromptBlock::FileToolRules],
        extra_stable: None,
        constitution: ConstitutionMode::Both,
    }
}

/// The executor surface: a real coding agent that cannot plan, finish, spawn
/// more agents, or ask the user — those are the lead's jobs.
pub(super) fn executor_guardrails() -> Guardrails {
    let mut tools = ToolSelection::all();
    tools.exclude = vec![
        "create_plan".to_string(),
        "update_plan".to_string(),
        "finish".to_string(),
        "spawn_agent".to_string(),
        "ask_user".to_string(),
    ];
    Guardrails {
        prompt: Some(executor_prompt()),
        tools,
        route: vec!["free".to_string()],
        // No session limits: no request, token, cost, or wall-clock cap.
        // Workers do the bulk of the implementation and idle for long
        // stretches while the lead reviews them; the lead or user stops
        // a worker explicitly.
        limits: None,
        gate_rules: executor_gate_rules(),
        question_policy: QuestionPolicy::Deny,
        turn_end: TurnEndPolicy::Stop,
    }
}

/// Guardrails for one worker kind: executors keep the coding surface, audit
/// workers are read-only independent reviewers.
pub(super) fn guardrails_for(kind: &str) -> Guardrails {
    if kind == "audit" {
        audit_guardrails()
    } else {
        executor_guardrails()
    }
}
/// never touches lifecycle state.
pub(crate) fn audit_prompt() -> PromptSpec {
    PromptSpec {
        identity: concat!(
            "You are a Mnemo2 independent audit worker. You did not implement the change under ",
            "review; form your own opinion from the plan, the brief, and the diff alone — never ",
            "from the implementer's claims. You are read-only by construction: you cannot edit ",
            "files, run shell commands, commit, merge, push, spawn work, ask the user, or tick ",
            "plan steps. Audit correctness, error handling, integration with neighbouring ",
            "contracts, persistence, concurrency, security, and test quality against the plan's ",
            "intent and acceptance scenarios. Report findings with file/line evidence through ",
            "message_parent, then end your turn; the lead adjudicates your audit on the ",
            "implementation worker's review. Never merge, never mutate files, and never use ",
            "Git or files as lifecycle status."
        )
        .to_string(),
        blocks: Vec::new(),
        extra_stable: None,
        constitution: ConstitutionMode::Both,
    }
}

/// The audit surface: every read tool (files, search, graph, git_read, web,
/// memory reads) with all mutation paths excluded — no file writes, no
/// shell, no git writes, no spawning, no user questions, no plan-board or
/// step ticks, and no durable-memory mutations (writes, amendments, or
/// consolidation).
fn audit_guardrails() -> Guardrails {
    let mut tools = ToolSelection::all();
    tools.exclude = vec![
        "create_plan".to_string(),
        "update_plan".to_string(),
        "finish".to_string(),
        "spawn_agent".to_string(),
        "ask_user".to_string(),
        "file_edit".to_string(),
        "file_write".to_string(),
        "multi_edit".to_string(),
        "convert_line_endings".to_string(),
        "shell".to_string(),
        "git".to_string(),
        "memory_write".to_string(),
        "memory_amend".to_string(),
        "memory_update".to_string(),
        "memory_supersede".to_string(),
        "memory_delete".to_string(),
        "memory_consolidate".to_string(),
        "complete_step".to_string(),
        "abandon_plan".to_string(),
    ];
    Guardrails {
        prompt: Some(audit_prompt()),
        tools,
        route: vec!["free".to_string()],
        limits: None,
        gate_rules: Vec::new(),
        question_policy: QuestionPolicy::Deny,
        turn_end: TurnEndPolicy::Stop,
    }
}

/// The refusal text every executor gate rule carries.
const EXECUTOR_GIT_DENY: &str = concat!(
    "TeamManager owns branches and integration: workers never push, merge, rebase, ",
    "switch or delete branches, manage worktrees, or use `gh pr`. Commit on your own ",
    "branch and report with message_parent."
);

/// Where a shell command starts inside the JSON-serialized `shell` args: the
/// `command` value, then any text up to a command boundary (start, whitespace
/// or shell punctuation, or an escaped newline/tab/quote) before the program.
const SHELL_CMD_PREFIX: &str =
    r#"(?i)"command":"(?:(?:[^"\\]|\\.)*?(?:\\[ntr"]|[^A-Za-z0-9_.\-\\"]))?"#;

/// `git` plus its global options, ending at whitespace before the subcommand
/// (`git -C dir push`, `git -c k=v merge`, `git --no-pager rebase`).
const SHELL_GIT: &str = r#"git(?:\s+(?:-C\s+\S+|-c\s+\S+|--[A-Za-z-]+(?:=\S+)?|-[pP]))*\s+"#;

/// One shell argument that is not the bare `--` path separator.
const SHELL_ARG_NOT_DASHDASH: &str =
    r#"(?:--[^\s;&|\\"]+|-[^-\s;&|\\"][^\s;&|\\"]*|-|[^-\s;&|\\"][^\s;&|\\"]*)"#;

/// The end of one shell command inside the serialized args.
const SHELL_CMD_END: &str = r#"\s*(?:[;&|)]|\\[ntr"]|")"#;

fn deny_rule(tool: &str, pattern: String) -> GateRule {
    GateRule {
        tool: tool.to_string(),
        arg_pattern: Some(pattern),
        verdict: GateVerdict::Deny {
            message: EXECUTOR_GIT_DENY.to_string(),
        },
    }
}

/// Deny rules that keep executor workers out of integration. A deny is a veto,
/// so these hold under `OverridePermissions`. Patterns match the call's
/// JSON-serialized args (see `DeclarativePolicy::gate_call`).
fn executor_gate_rules() -> Vec<GateRule> {
    // The structured `git` tool. `checkout` takes a branch, never paths
    // (paths go through `restore`), so every checkout is a branch switch.
    let mut rules = vec![deny_rule(
        "git",
        r#"(?i)"subcommand":"(?:push|merge|checkout|delete)""#.to_string(),
    )];
    rules.push(deny_rule("git", r#"(?i)"action":"delete""#.to_string()));
    // `action` alone can carry the subcommand, except for `stash` + `push`.
    rules.push(GateRule {
        tool: "git".to_string(),
        arg_pattern: Some(r#"(?i)"subcommand":"stash""#.to_string()),
        verdict: GateVerdict::Allow,
    });
    rules.push(deny_rule(
        "git",
        r#"(?i)"action":"(?:push|merge|checkout)""#.to_string(),
    ));
    // The `shell` tool: any git invocation, however chained or qualified.
    let pre = SHELL_CMD_PREFIX;
    let git = SHELL_GIT;
    for sub in [
        r"(?:push|merge|rebase|switch|worktree)\b",
        // Branch deletion: `-d`, `-D`, a combined short flag, or `--delete`.
        r#"branch\s+(?:[^;&|\\"]*\s)?(?:-[A-Za-z]*[dD][A-Za-z]*|--delete)(?:\s|[;&|\\"]|$)"#,
    ] {
        rules.push(deny_rule("shell", format!("{pre}{git}{sub}")));
    }
    // `checkout` of a branch; `checkout -- paths` and `checkout rev -- paths`
    // restore files and stay allowed.
    rules.push(deny_rule(
        "shell",
        format!("{pre}{git}checkout(?:\\s+{SHELL_ARG_NOT_DASHDASH})+{SHELL_CMD_END}"),
    ));
    rules.push(deny_rule("shell", format!(r"{pre}gh\s+pr\b")));
    rules
}
