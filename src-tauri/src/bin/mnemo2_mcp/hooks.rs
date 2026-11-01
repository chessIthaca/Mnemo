// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The Claude Code hook modes (`notices`, `gate-agent`) and the background
//! `wait` command.
//!
//! A hook must never break the user's session: every failure here ends in "no
//! output", and no mode starts Mnemo2. Without a run file naming the caller's
//! project the hooks return before making any network call.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::instance::{
    call, find_instance, find_instance_for_hook, root_body, Env, Instance, HOOK_TIMEOUT,
};

/// Extra read time on top of the wait's own timeout, so the server's "no events"
/// reply wins the race against our socket timeout.
const WAIT_SLACK: Duration = Duration::from_secs(30);

/// `wait` could not find (or keep talking to) Mnemo2. The lead must not start
/// another waiter: that would loop forever while Mnemo2 is down.
pub const NOT_REACHABLE: &str = "Mnemo2 is not reachable for this project. Do not restart the waiter; call team_status once Mnemo2 is running again.";
/// The server released this lead while it was waiting.
pub const DETACHED: &str = "The Mnemo lead was detached (another session closed or Mnemo2 restarted). Call team_status before restarting the waiter.";
/// Mnemo2 runs, but the project is not open in it.
pub const NO_PROJECT: &str = "Mnemo2 has no project open for this directory. Do not restart the waiter; call team_status once the project is open in Mnemo2.";

/// Which `notices` hook is running.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NoticesMode {
    /// PostToolUse: hand over (and clear) the queued notices.
    Drain,
    /// SessionStart: summary plus anything already queued.
    SessionStart,
    /// Stop: ask whether workers are running with nobody waiting.
    StopCheck,
}

#[derive(Deserialize, Default)]
struct NoticesReply {
    #[serde(default)]
    attached: bool,
    #[serde(default)]
    notices: Vec<String>,
    #[serde(default)]
    active_workers: u32,
    #[serde(default)]
    waiter_alive: bool,
    #[serde(default)]
    summary: String,
}

/// The directory a hook reports: stdin `cwd`, else `CLAUDE_PROJECT_DIR`, else
/// the process cwd.
fn hook_cwd(env: &Env, input: &Value) -> PathBuf {
    input
        .get("cwd")
        .and_then(Value::as_str)
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
        .or_else(|| env.project_dir.clone())
        .unwrap_or_else(|| env.process_cwd.clone())
}

fn parse_input(text: &str) -> Value {
    serde_json::from_str(text.trim_start_matches('\u{feff}')).unwrap_or(Value::Null)
}

/// True when the hook fired for a tool call made inside a subagent: Claude Code
/// adds a non-empty `agent_id` then. Those calls must not consume the lead's
/// notices or trigger its stop check.
fn from_subagent(input: &Value) -> bool {
    input
        .get("agent_id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.trim().is_empty())
}

/// The Claude Code session the hook fired for, when its input names one
/// (`session_id`). The server records it for the staking lead, so that
/// session's own hooks — and no other session's — can re-attach it after an
/// idle release.
fn hook_session(input: &Value) -> Option<&str> {
    input
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|session| !session.is_empty())
}

/// The helper path for the waiter command lines. A path that could break out
/// of the quotes falls back to the bare command name.
fn quoted_helper(env: &Env) -> String {
    let helper = env.helper.to_string_lossy();
    let usable = !helper.is_empty() && !helper.chars().any(|c| c == '"' || c.is_control());
    format!("\"{}\"", if usable { helper.as_ref() } else { "mnemo2-mcp" })
}

fn notice_lines(notices: &[String]) -> String {
    let lines: Vec<String> = notices.iter().map(|notice| format!("- {notice}")).collect();
    format!("Mnemo notices:\n{}", lines.join("\n"))
}

fn hook_output(event: &str, context: &str) -> String {
    json!({
        "hookSpecificOutput": {
            "hookEventName": event,
            "additionalContext": context,
        }
    })
    .to_string()
}

/// `notices --drain | --session-start | --stop-check`: the JSON to print, if any.
pub fn notices(env: &Env, mode: NoticesMode, stdin_text: &str) -> Option<String> {
    let input = parse_input(stdin_text);
    let stop_hook_active = input
        .get("stop_hook_active")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if mode == NoticesMode::StopCheck && stop_hook_active {
        return None;
    }
    // A subagent's tool calls are not the lead's: leave its notices queued.
    if mode != NoticesMode::SessionStart && from_subagent(&input) {
        return None;
    }
    let instance = find_instance_for_hook(env, &hook_cwd(env, &input))?;
    let wire_mode = if mode == NoticesMode::StopCheck {
        "peek"
    } else {
        "drain"
    };
    // Name the session when Claude Code does: the server re-attaches an
    // idle-released lead only for the session that staked it.
    let mut body = json!({ "root": instance.root, "mode": wire_mode });
    if let Some(session) = hook_session(&input) {
        body["session"] = json!(session);
    }
    let reply = call(&instance, "/lead/notices", &body, HOOK_TIMEOUT)?;
    let reply: NoticesReply = serde_json::from_value(reply).ok()?;
    match mode {
        NoticesMode::Drain => {
            if reply.notices.is_empty() {
                return None;
            }
            Some(hook_output("PostToolUse", &notice_lines(&reply.notices)))
        }
        // The server fills the summary whenever the root is its open project:
        // SessionStart can run before MCP `initialize` attaches the lead.
        NoticesMode::SessionStart => {
            let mut context = reply.summary.trim().to_string();
            if !reply.notices.is_empty() {
                if !context.is_empty() {
                    context.push_str("\n\n");
                }
                context.push_str(&notice_lines(&reply.notices));
            }
            if context.is_empty() {
                return None;
            }
            Some(hook_output("SessionStart", &context))
        }
        NoticesMode::StopCheck => {
            if !reply.attached || reply.active_workers == 0 || reply.waiter_alive {
                return None;
            }
            let helper = quoted_helper(env);
            // Name this session in the command (the Stop hook knows it; the
            // session itself does not): staking the lead with its session is
            // what lets its own hooks re-attach it after an idle release.
            let session = hook_session(&input)
                .map(|session| format!(" --session {session}"))
                .unwrap_or_default();
            Some(
                json!({
                    "decision": "block",
                    "reason": format!(
                        "{} Mnemo worker(s) are still running and no waiter is active. \
                         Start this as a background command so you are woken when one reports. \
                         Bash: {helper} wait{session} \u{2014} PowerShell: & {helper} wait{session}",
                        reply.active_workers,
                    ),
                })
                .to_string(),
            )
        }
    }
}

/// `gate-agent`: the PreToolUse JSON when the agent call needs the user's OK.
pub fn gate_agent(env: &Env, stdin_text: &str) -> Option<String> {
    let input = parse_input(stdin_text);
    let tool_name = input.get("tool_name").and_then(Value::as_str).unwrap_or("");
    let subagent_type = input
        .pointer("/tool_input/subagent_type")
        .and_then(Value::as_str)
        .filter(|kind| !kind.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| match tool_name {
            // An agent call without a type runs the general-purpose agent.
            "" | "Agent" | "Task" => "general-purpose".to_string(),
            other => other.to_string(),
        });
    let instance = find_instance_for_hook(env, &hook_cwd(env, &input))?;
    let reply = call(
        &instance,
        "/lead/gate-agent",
        &json!({ "root": instance.root, "subagent_type": subagent_type }),
        HOOK_TIMEOUT,
    )?;
    if reply.get("decision").and_then(Value::as_str) != Some("ask") {
        return None;
    }
    let reason = reply
        .get("reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
        .unwrap_or("Mnemo's workers should do this work; Claude Code's own agents need your OK.");
    Some(
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "ask",
                "permissionDecisionReason": reason,
            }
        })
        .to_string(),
    )
}

/// The instance `wait` should listen to: the process cwd first, then
/// `CLAUDE_PROJECT_DIR`.
fn wait_instance(env: &Env) -> Option<Instance> {
    find_instance(env, &env.process_cwd).or_else(|| {
        env.project_dir
            .as_ref()
            .and_then(|dir| find_instance(env, dir))
    })
}

/// The notices in a `/lead/wait` reply.
fn reply_notices(reply: &Value) -> Vec<String> {
    reply
        .get("notices")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(|notice| notice.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// What `wait` prints for a `/lead/wait` reply. Notices win; otherwise the
/// server's `status` decides, and a reply without one reads as before.
fn describe_wait_reply(reply: &Value, timeout_s: u64) -> String {
    let notices = reply_notices(reply);
    if !notices.is_empty() {
        return notices.join("\n");
    }
    match reply.get("status").and_then(Value::as_str) {
        Some("detached") => DETACHED.to_string(),
        Some("no_project") => NO_PROJECT.to_string(),
        Some("timeout") => format!("no events after {timeout_s} s"),
        _ => "no events".to_string(),
    }
}

/// `wait [--timeout <s>] [--session <id>]`: block until a notice is queued;
/// the text to print.
///
/// When no instance is found, or it stops answering, this keeps looking every
/// `wait_poll` for up to `wait_reach_timeout` (the instance may be restarting)
/// and then says plainly that Mnemo2 is unreachable, instead of exiting at
/// once and having the lead start another waiter, again and again.
///
/// `--session` names the Claude Code session the Stop hook suggested this
/// command for (the session itself cannot know it): staking the lead with
/// that name is what lets the session's own hooks re-attach it after an
/// idle release.
pub fn wait(env: &Env, timeout_s: u64, session: Option<&str>) -> String {
    let read_timeout = Duration::from_secs(timeout_s).saturating_add(WAIT_SLACK);
    let session = session.map(str::trim).filter(|session| !session.is_empty());
    let mut unreachable_since: Option<Instant> = None;
    loop {
        let reply = wait_instance(env).and_then(|instance| {
            let mut body = root_body(&instance);
            body["timeout_s"] = json!(timeout_s);
            if let Some(session) = session {
                body["session"] = json!(session);
            }
            call(&instance, "/lead/wait", &body, read_timeout)
        });
        if let Some(reply) = reply {
            return describe_wait_reply(&reply, timeout_s);
        }
        let since = *unreachable_since.get_or_insert_with(Instant::now);
        if since.elapsed() >= env.wait_reach_timeout {
            return NOT_REACHABLE.to_string();
        }
        std::thread::sleep(env.wait_poll);
    }
}
#[cfg(test)]
#[path = "hooks_tests.rs"]
mod tests;
