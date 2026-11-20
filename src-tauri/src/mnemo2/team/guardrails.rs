// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The worker guardrails: the executor and audit prompts, their tool
//! surfaces, and the gate rules that keep workers out of integration and
//! off the shared git stash stack — every worker checkout is a worktree of
//! one repository, so `refs/stash` is global and one worker's stash can be
//! popped by another.

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
            "TeamManager state or use Git/files as worker lifecycle status. Never use git ",
            "stash: the stash stack is shared with other workers; park work as a WIP ",
            "commit. Run relevant checks, commit on the assigned worker branch, and report ",
            "the actual commit and checks.",
            " After a visual change (UI, rendering, animation), when the image tools are ",
            "available (load_tools image), take a screenshot with the project's own tool or a ",
            "browser screenshot, check it with image_analysis or compare before and after with ",
            "image_ui_diff, and report what you saw."
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
            "intent and acceptance scenarios. When the change is visual and the image tools are ",
            "available, look at the screenshots the worker or the project produced with ",
            "image_analysis before you judge it. Report findings with file/line evidence through ",
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
pub(super) fn audit_guardrails() -> Guardrails {
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
        // A job runs commands too, so the whole job surface is a mutation
        // path: an audit worker must not be able to start, wait on, tail or
        // kill a job (plan 5b85ab61 step 2).
        "job_start".to_string(),
        "job_wait".to_string(),
        "job_tail".to_string(),
        "job_kill".to_string(),
        // `run_checks` executes the project's commands too, so an audit worker
        // must not be able to run them either (plan 5b85ab61 step 3).
        "run_checks".to_string(),
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

/// The refusal for git stash: every worktree of the repository shares
/// `refs/stash`, so a stash parked by one worker can be popped or dropped
/// by another — workers park work as WIP commits instead.
const EXECUTOR_STASH_DENY: &str = concat!(
    "The git stash stack is shared by every worker checkout of this repository, so a ",
    "stash can be popped by another worker. Park work as a WIP commit on your branch ",
    "instead (git commit -m \"wip\"), and to prove a test red, edit the fix out ",
    "temporarily. To recover a known stash commit, use git stash apply <sha>."
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

/// A stash argument after `git stash apply` that is NOT a commit SHA: a
/// `stash@{…}` ref (or any token not starting with a hex digit), or a
/// too-short abbreviation — fewer than 7 hex characters before a non-hex
/// character. The regex crate has no look-around, so the bad forms are
/// enumerated instead of negating the SHA (a 7-40 hex run matches neither
/// branch: every one of its first six characters is followed by another
/// hex character).
const SHELL_STASH_NOT_SHA: &str =
    r#"[0-9a-fA-F]{1,6}[^0-9a-fA-F][^\s;&|\\"]*|[^0-9a-fA-F\s;&|\\"][^\s;&|\\"]*"#;

/// The swapped `stash list` carrier on the structured git tool: the
/// `subcommand` and `action` fields are interchangeable, so
/// `subcommand: "list"` + `action: "stash"` is also a read-only listing
/// (the `stash` in `action` resolves to the stash subcommand, the `list`
/// in `subcommand` to its list action). Both key orders of the
/// JSON-serialized args are covered.
const GIT_STASH_LIST_SWAPPED: &str = r#"(?i)"action":"stash"[^{}]*"subcommand":"list"|"subcommand":"list"[^{}]*"action":"stash""#;

fn deny_rule(tool: &str, pattern: String) -> GateRule {
    GateRule {
        tool: tool.to_string(),
        arg_pattern: Some(pattern),
        verdict: GateVerdict::Deny {
            message: EXECUTOR_GIT_DENY.to_string(),
        },
    }
}

/// A deny rule carrying the shared-stash refusal: the stash rule has its
/// own message, distinct from the integration one, so the worker learns
/// the WIP-commit alternative instead of a branch-policy text.
fn stash_deny_rule(tool: &str, pattern: String) -> GateRule {
    GateRule {
        tool: tool.to_string(),
        arg_pattern: Some(pattern),
        verdict: GateVerdict::Deny {
            message: EXECUTOR_STASH_DENY.to_string(),
        },
    }
}

/// Deny rules that keep executor workers out of integration and off the
/// shared stash stack. A deny is a veto, so these hold under
/// `OverridePermissions`. Patterns match the call's JSON-serialized args
/// (see `DeclarativePolicy::gate_call`); evaluation is FIRST-MATCH, so the
/// Allow rules below must precede the stash denials they exempt.
fn executor_gate_rules() -> Vec<GateRule> {
    // The structured `git` tool. `checkout` takes a branch, never paths
    // (paths go through `restore`), so every checkout is a branch switch.
    let mut rules = vec![deny_rule(
        "git",
        r#"(?i)"subcommand":"(?:push|merge|checkout|delete)""#.to_string(),
    )];
    rules.push(deny_rule("git", r#"(?i)"action":"delete""#.to_string()));
    // The shared git stash stack: every worker checkout is a worktree of
    // the same repository, so `refs/stash` is global and a worker's stash
    // can be popped or dropped by another worker (or a months-old stash
    // from the lead checkout). The structured tool has no ref parameter,
    // so no pop or drop through it is safe: every push/pop/drop carrier in
    // either field is denied. The two read-only spellings must stay
    // allowed and come FIRST: `action: "list"` only ever resolves to
    // `branch list` or `stash list` (the integration denies above still
    // fire first for their own calls), and the swapped pair
    // `subcommand: "list"` + `action: "stash"` resolves to `stash list`.
    rules.push(GateRule {
        tool: "git".to_string(),
        arg_pattern: Some(r#"(?i)"action":"list""#.to_string()),
        verdict: GateVerdict::Allow,
    });
    rules.push(GateRule {
        tool: "git".to_string(),
        arg_pattern: Some(GIT_STASH_LIST_SWAPPED.to_string()),
        verdict: GateVerdict::Allow,
    });
    // Stash action names ride either field, and `action: "stash"` with no
    // subcommand resolves to stash push (the default action), so both
    // carriers are denied.
    rules.push(stash_deny_rule(
        "git",
        r#"(?i)"subcommand":"(?:stash|pop|drop)""#.to_string(),
    ));
    rules.push(stash_deny_rule(
        "git",
        r#"(?i)"action":"(?:stash|pop|drop)""#.to_string(),
    ));
    // `action` alone can carry the subcommand.
    rules.push(deny_rule(
        "git",
        r#"(?i)"action":"(?:push|merge|checkout)""#.to_string(),
    ));
    // The `shell` tool and `job_start` — a job runs the same commands, so it
    // must not become a way around the gate: any git invocation, however
    // chained or qualified.
    let pre = SHELL_CMD_PREFIX;
    let git = SHELL_GIT;
    for tool in ["shell", "job_start"] {
        for sub in [
            r"(?:push|merge|rebase|switch|worktree)\b",
            // Branch deletion: `-d`, `-D`, a combined short flag, or `--delete`.
            r#"branch\s+(?:[^;&|\\"]*\s)?(?:-[A-Za-z]*[dD][A-Za-z]*|--delete)(?:\s|[;&|\\"]|$)"#,
        ] {
            rules.push(deny_rule(tool, format!("{pre}{git}{sub}")));
        }
        // `checkout` of a branch; `checkout -- paths` and `checkout rev -- paths`
        // restore files and stay allowed.
        rules.push(deny_rule(
            tool,
            format!("{pre}{git}checkout(?:\\s+{SHELL_ARG_NOT_DASHDASH})+{SHELL_CMD_END}"),
        ));
        rules.push(deny_rule(tool, format!(r"{pre}gh\s+pr\b")));
        // The shared stash stack again, as commands: a bare `git stash` is
        // a push ("you can omit push"), and so is a bare
        // `git stash <push-option>` (`-m`, `-u`, `-- pathspec`); the named
        // mutators follow. `stash list`, `stash show` and `stash create`
        // never touch the stack and match none of these rules, so they
        // stay allowed.
        rules.push(stash_deny_rule(
            tool,
            format!(r"{pre}{git}stash(?:{SHELL_CMD_END}|\s+-)"),
        ));
        rules.push(stash_deny_rule(
            tool,
            format!(r"{pre}{git}stash\s+(?:push|save|pop|drop|clear|store|branch)\b"),
        ));
        // `git stash apply <sha>` — the argument a full or abbreviated
        // commit SHA, the recovery path for a known dangling stash commit
        // — is the one apply that runs; no argument, a `stash@{…}` ref, a
        // too-short abbreviation or an apply option (`-q`, `--index`)
        // before the SHA would all apply the top of the shared stack, so
        // they are refused: use the plain form the refusal text names.
        rules.push(stash_deny_rule(
            tool,
            format!(r"{pre}{git}stash\s+apply(?:{SHELL_CMD_END}|\s+(?:{SHELL_STASH_NOT_SHA}))"),
        ));
    }
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Workers check visual work themselves: the executor prompt names the
    /// deferred `image` group and the two tools a visual change needs, so a
    /// screenshot the worker takes is actually looked at.
    #[test]
    fn executor_prompt_asks_for_a_visual_check_with_the_image_tools() {
        let identity = executor_prompt().identity;
        assert!(
            identity.contains("when the image tools are available (load_tools image)"),
            "executor prompt must gate the visual check on the image group: {identity}"
        );
        assert!(
            identity.contains("image_analysis") && identity.contains("image_ui_diff"),
            "executor prompt must name image_analysis and image_ui_diff: {identity}"
        );
    }

    /// The audit worker looks at the screenshots itself instead of trusting a
    /// worker's description of them.
    #[test]
    fn audit_prompt_looks_at_screenshots_with_the_image_tools() {
        let identity = audit_prompt().identity;
        assert!(
            identity.contains(
                "When the change is visual and the image tools are available, look at the \
                 screenshots"
            ),
            "audit prompt must ask for a look at the screenshots: {identity}"
        );
        assert!(
            identity.contains("image_analysis"),
            "audit prompt must name image_analysis: {identity}"
        );
    }
}
