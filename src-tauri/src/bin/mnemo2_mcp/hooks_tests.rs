// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Tests for the hook modes and `wait`, against a fake Mnemo2 endpoint.

use super::*;
use crate::testutil::{health, json_reply, FakeServer, Reply, Sandbox};
use std::sync::Arc;

/// A fake Mnemo2 with `project` open whose `path` route answers `reply`.
fn server_with(sandbox: &Sandbox, path: &'static str, reply: Value) -> FakeServer {
    let project = sandbox.project_str();
    let server = FakeServer::start("tok", move |req| {
        if req.path == "/health" {
            health(55, Some(&project))
        } else if req.path == path {
            json_reply(reply.clone())
        } else {
            Reply::Status(404, String::new())
        }
    });
    sandbox.write_run(55, server.port, "tok", Some(&sandbox.project_str()));
    server
}

fn hook_stdin(cwd: &std::path::Path, extra: Value) -> String {
    let mut input = json!({ "cwd": cwd.to_string_lossy(), "session_id": "s-1" });
    if let (Some(map), Some(more)) = (input.as_object_mut(), extra.as_object()) {
        map.extend(more.clone());
    }
    input.to_string()
}

fn parsed(output: Option<String>) -> Value {
    serde_json::from_str(&output.expect("hook output")).expect("hook output is JSON")
}

fn notices_reply(
    attached: bool,
    notices: &[&str],
    active_workers: u32,
    waiter_alive: bool,
    summary: &str,
) -> Value {
    json!({
        "attached": attached,
        "notices": notices,
        "active_workers": active_workers,
        "waiter_alive": waiter_alive,
        "summary": summary,
    })
}

// ---- notices --drain -------------------------------------------------------

#[test]
fn drain_prints_the_queued_notices_as_post_tool_use_context() {
    let sandbox = Sandbox::new();
    let server = server_with(
        &sandbox,
        "/lead/notices",
        notices_reply(true, &["worker a finished its turn", "worker b failed"], 1, false, "board"),
    );
    let env = sandbox.env();
    let output = notices(
        &env,
        NoticesMode::Drain,
        &hook_stdin(&sandbox.deep, json!({ "hook_event_name": "PostToolUse", "tool_name": "Bash" })),
    );
    let helper = env.helper.display();
    // Workers are running and no waiter: the context repeats the waiter
    // command named for this session, so a compacted session still finds it.
    assert_eq!(
        parsed(output),
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "additionalContext": format!(
                    "Mnemo notices:\n- worker a finished its turn\n- worker b failed\n\n\
                     1 Mnemo worker(s) are still running and no waiter is active. \
                     Start this as a background command so you are woken when one reports. \
                     Bash: \"{helper}\" wait --session s-1 \u{2014} PowerShell: & \"{helper}\" wait --session s-1"
                ),
            }
        })
    );
    let sent = server.requests_to("/lead/notices");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].json(), json!({ "root": sandbox.project_str(), "mode": "drain", "session": "s-1" }));
}

#[test]
fn drain_prints_nothing_without_notices() {
    let sandbox = Sandbox::new();
    let _server = server_with(&sandbox, "/lead/notices", notices_reply(true, &[], 2, false, "board"));
    let output = notices(&sandbox.env(), NoticesMode::Drain, &hook_stdin(&sandbox.deep, json!({})));
    assert!(output.is_none());
}

#[test]
fn hooks_are_silent_and_never_touch_the_network_without_a_matching_instance() {
    let sandbox = Sandbox::new();
    // An instance exists, but for another project.
    let elsewhere = sandbox.elsewhere.to_string_lossy().into_owned();
    let server = FakeServer::start("tok", |_| json_reply(json!({})));
    sandbox.write_run(66, server.port, "tok", Some(&elsewhere));
    let env = sandbox.env();
    let stdin = hook_stdin(&sandbox.deep, json!({}));
    let started = std::time::Instant::now();
    assert!(notices(&env, NoticesMode::Drain, &stdin).is_none());
    assert!(notices(&env, NoticesMode::SessionStart, &stdin).is_none());
    assert!(notices(&env, NoticesMode::StopCheck, &stdin).is_none());
    let gate_input = hook_stdin(
        &sandbox.deep,
        json!({ "tool_name": "Agent", "tool_input": { "subagent_type": "general-purpose" } }),
    );
    assert!(gate_agent(&env, &gate_input).is_none());
    assert!(started.elapsed() < Duration::from_secs(1), "{:?}", started.elapsed());
    assert!(server.requests().is_empty());
}

#[test]
fn a_failing_instance_means_no_output() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let server = FakeServer::start("tok", move |req| {
        if req.path == "/health" {
            health(55, Some(&project))
        } else {
            Reply::Status(500, "boom".to_string())
        }
    });
    sandbox.write_run(55, server.port, "tok", Some(&sandbox.project_str()));
    let env = sandbox.env();
    let stdin = hook_stdin(&sandbox.deep, json!({ "stop_hook_active": false }));
    assert!(notices(&env, NoticesMode::Drain, &stdin).is_none());
    assert!(notices(&env, NoticesMode::StopCheck, &stdin).is_none());
    assert!(gate_agent(&env, &stdin).is_none());
}

#[test]
fn the_root_comes_from_stdin_cwd_then_claude_project_dir_then_the_process_cwd() {
    let sandbox = Sandbox::new();
    let reply = notices_reply(true, &["n1"], 0, false, "");
    let _server = server_with(&sandbox, "/lead/notices", reply);
    let mut env = sandbox.env();

    // stdin cwd inside the project wins over everything else.
    env.process_cwd = sandbox.elsewhere.clone();
    assert!(notices(&env, NoticesMode::Drain, &hook_stdin(&sandbox.deep, json!({}))).is_some());
    // No cwd in the JSON: CLAUDE_PROJECT_DIR.
    env.project_dir = Some(sandbox.project.clone());
    assert!(notices(&env, NoticesMode::Drain, "{}").is_some());
    // Unparseable stdin behaves the same.
    assert!(notices(&env, NoticesMode::Drain, "not json").is_some());
    // Neither: the process cwd.
    env.project_dir = None;
    assert!(notices(&env, NoticesMode::Drain, "{}").is_none());
    env.process_cwd = sandbox.deep.clone();
    assert!(notices(&env, NoticesMode::Drain, "").is_some());
}

// ---- notices --session-start ----------------------------------------------

#[test]
fn session_start_adds_the_summary_any_queued_notices_and_the_waiter_command() {
    let sandbox = Sandbox::new();
    let server = server_with(
        &sandbox,
        "/lead/notices",
        notices_reply(true, &["worker c merged"], 1, false, "Mnemo: board 3 open, 1 worker, lead model X."),
    );
    let env = sandbox.env();
    let output = notices(
        &env,
        NoticesMode::SessionStart,
        &hook_stdin(&sandbox.deep, json!({ "hook_event_name": "SessionStart", "source": "startup" })),
    );
    let helper = env.helper.display();
    // The session is taught its own waiter command right away: it names the
    // session, so only its own hooks can re-attach it after an idle release.
    assert_eq!(
        parsed(output),
        json!({
            "hookSpecificOutput": {
                "hookEventName": "SessionStart",
                "additionalContext": format!(
                    "Mnemo: board 3 open, 1 worker, lead model X.\n\nMnemo notices:\n- worker c merged\n\n\
                     Whenever you start a Mnemo waiter, use this command (it names your Claude Code \
                     session, so your own hooks can re-attach you after an idle release). \
                     Bash: \"{helper}\" wait --session s-1 \u{2014} PowerShell: & \"{helper}\" wait --session s-1"
                ),
            }
        })
    );
    assert_eq!(server.requests_to("/lead/notices")[0].json()["mode"], "drain");
}

#[test]
fn session_start_with_only_a_summary_and_not_attached() {
    let sandbox = Sandbox::new();
    let _server = server_with(&sandbox, "/lead/notices", notices_reply(true, &[], 0, false, "Mnemo: idle."));
    let env = sandbox.env();
    let output = notices(&env, NoticesMode::SessionStart, &hook_stdin(&sandbox.deep, json!({})));
    let helper = env.helper.display();
    assert_eq!(
        parsed(output)["hookSpecificOutput"]["additionalContext"],
        format!(
            "Mnemo: idle.\n\n\
             Whenever you start a Mnemo waiter, use this command (it names your Claude Code \
             session, so your own hooks can re-attach you after an idle release). \
             Bash: \"{helper}\" wait --session s-1 \u{2014} PowerShell: & \"{helper}\" wait --session s-1"
        )
    );

    // SessionStart can run before MCP `initialize` attaches the lead, so an
    // unattached reply with a summary still reaches the session.
    let other = Sandbox::new();
    let _server = server_with(&other, "/lead/notices", notices_reply(false, &["x"], 0, false, "Mnemo: idle."));
    let output = notices(&other.env(), NoticesMode::SessionStart, &hook_stdin(&other.deep, json!({})));
    let context = parsed(output)["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("context")
        .to_string();
    assert!(context.starts_with("Mnemo: idle."), "{context}");
    assert!(context.contains("- x"), "{context}");

    // With nothing else to say, the waiter command alone still reaches the
    // session: it is the one line the session cannot reconstruct itself.
    let empty = Sandbox::new();
    let _server = server_with(&empty, "/lead/notices", notices_reply(false, &[], 0, false, ""));
    let output = notices(&empty.env(), NoticesMode::SessionStart, &hook_stdin(&empty.deep, json!({})));
    let value = parsed(output);
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("context");
    assert!(context.contains("wait --session s-1"), "{context}");
    assert!(!context.contains("Mnemo notices"), "{context}");
}

// ---- notices --stop-check --------------------------------------------------

#[test]
fn stop_check_blocks_when_workers_run_and_nobody_waits() {
    let sandbox = Sandbox::new();
    let server = server_with(&sandbox, "/lead/notices", notices_reply(true, &[], 2, false, ""));
    let env = sandbox.env();
    let output = notices(&env, NoticesMode::StopCheck, &hook_stdin(&sandbox.deep, json!({ "stop_hook_active": false })));
    let value = parsed(output);
    assert_eq!(value["decision"], "block");
    let helper = env.helper.display();
    assert_eq!(
        value["reason"],
        format!(
            "2 Mnemo worker(s) are still running and no waiter is active. Start this as a background command so you are woken when one reports. Bash: \"{helper}\" wait --session s-1 \u{2014} PowerShell: & \"{helper}\" wait --session s-1"
        )
    );
    // The check must not consume notices.
    assert_eq!(server.requests_to("/lead/notices")[0].json()["mode"], "peek");
}

#[test]
fn stop_check_lets_claude_stop_in_every_other_case() {
    let stdin = |sandbox: &Sandbox, active: bool| {
        hook_stdin(&sandbox.deep, json!({ "stop_hook_active": active }))
    };
    // No active workers.
    let sandbox = Sandbox::new();
    let _server = server_with(&sandbox, "/lead/notices", notices_reply(true, &[], 0, false, ""));
    assert!(notices(&sandbox.env(), NoticesMode::StopCheck, &stdin(&sandbox, false)).is_none());
    // A waiter is alive.
    let sandbox = Sandbox::new();
    let _server = server_with(&sandbox, "/lead/notices", notices_reply(true, &[], 3, true, ""));
    assert!(notices(&sandbox.env(), NoticesMode::StopCheck, &stdin(&sandbox, false)).is_none());
    // No external lead attached.
    let sandbox = Sandbox::new();
    let _server = server_with(&sandbox, "/lead/notices", notices_reply(false, &[], 3, false, ""));
    assert!(notices(&sandbox.env(), NoticesMode::StopCheck, &stdin(&sandbox, false)).is_none());
}

#[test]
fn stop_check_never_blocks_twice_in_a_row() {
    let sandbox = Sandbox::new();
    let server = server_with(&sandbox, "/lead/notices", notices_reply(true, &[], 2, false, ""));
    let output = notices(
        &sandbox.env(),
        NoticesMode::StopCheck,
        &hook_stdin(&sandbox.deep, json!({ "stop_hook_active": true })),
    );
    assert!(output.is_none());
    assert!(server.requests_to("/lead/notices").is_empty(), "no need to ask");
}

// ---- subagents --------------------------------------------------------------

#[test]
fn a_subagents_tool_calls_leave_the_leads_notices_alone() {
    let sandbox = Sandbox::new();
    let server = server_with(
        &sandbox,
        "/lead/notices",
        notices_reply(true, &["worker a finished its turn"], 2, false, "board"),
    );
    let env = sandbox.env();
    let subagent = hook_stdin(&sandbox.deep, json!({ "agent_id": "agent-7f3a", "agent_type": "Explore" }));
    assert!(notices(&env, NoticesMode::Drain, &subagent).is_none());
    assert!(notices(&env, NoticesMode::StopCheck, &subagent).is_none());
    assert!(server.requests().is_empty(), "a subagent hook must not even ask the server");

    // The same hook for the lead itself (no agent_id, or an empty one) drains.
    for extra in [json!({}), json!({ "agent_id": "" }), json!({ "agent_id": "  " }), json!({ "agent_id": null })] {
        let output = notices(&env, NoticesMode::Drain, &hook_stdin(&sandbox.deep, extra.clone()));
        assert!(output.is_some(), "{extra}");
    }
    assert_eq!(server.requests_to("/lead/notices").len(), 4);
}

#[test]
fn session_start_is_not_affected_by_an_agent_id() {
    let sandbox = Sandbox::new();
    let server = server_with(&sandbox, "/lead/notices", notices_reply(true, &[], 0, false, "Mnemo: idle."));
    let output = notices(
        &sandbox.env(),
        NoticesMode::SessionStart,
        &hook_stdin(&sandbox.deep, json!({ "agent_id": "agent-1" })),
    );
    // The subagent gate must not suppress the session-start context (here:
    // the summary plus the waiter command named for this session).
    let value = parsed(output);
    let context = value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("context");
    assert!(context.starts_with("Mnemo: idle."), "{context}");
    assert!(context.contains("wait --session s-1"), "{context}");
    assert_eq!(server.requests_to("/lead/notices").len(), 1);
}

// ---- gate-agent ------------------------------------------------------------

fn gate_input(sandbox: &Sandbox, tool_name: &str, tool_input: Value) -> String {
    hook_stdin(
        &sandbox.deep,
        json!({ "hook_event_name": "PreToolUse", "tool_name": tool_name, "tool_input": tool_input }),
    )
}

#[test]
fn gate_agent_asks_when_the_server_says_ask() {
    let sandbox = Sandbox::new();
    let server = server_with(
        &sandbox,
        "/lead/gate-agent",
        json!({ "decision": "ask", "reason": "Mnemo workers should do this; ask the user first." }),
    );
    let output = gate_agent(
        &sandbox.env(),
        &gate_input(&sandbox, "Agent", json!({ "subagent_type": "general-purpose", "prompt": "x" })),
    );
    assert_eq!(
        parsed(output),
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "ask",
                "permissionDecisionReason": "Mnemo workers should do this; ask the user first.",
            }
        })
    );
    let sent = server.requests_to("/lead/gate-agent");
    assert_eq!(
        sent[0].json(),
        json!({ "root": sandbox.project_str(), "subagent_type": "general-purpose" })
    );
}

#[test]
fn gate_agent_prints_nothing_on_allow() {
    let sandbox = Sandbox::new();
    let server = server_with(&sandbox, "/lead/gate-agent", json!({ "decision": "allow", "reason": "" }));
    let output = gate_agent(&sandbox.env(), &gate_input(&sandbox, "Agent", json!({ "subagent_type": "Explore" })));
    assert!(output.is_none());
    assert_eq!(server.requests_to("/lead/gate-agent")[0].json()["subagent_type"], "Explore");
}

#[test]
fn gate_agent_names_untyped_calls_for_the_server() {
    let sandbox = Sandbox::new();
    let server = server_with(&sandbox, "/lead/gate-agent", json!({ "decision": "allow", "reason": "" }));
    let env = sandbox.env();
    assert!(gate_agent(&env, &gate_input(&sandbox, "Task", json!({ "prompt": "x" }))).is_none());
    assert!(gate_agent(&env, &gate_input(&sandbox, "Workflow", json!({ "script": "x" }))).is_none());
    let sent: Vec<Value> = server
        .requests_to("/lead/gate-agent")
        .iter()
        .map(|req| req.json()["subagent_type"].clone())
        .collect();
    assert_eq!(sent, vec![json!("general-purpose"), json!("Workflow")]);
}

#[test]
fn gate_agent_gives_ask_a_default_reason() {
    let sandbox = Sandbox::new();
    let _server = server_with(&sandbox, "/lead/gate-agent", json!({ "decision": "ask" }));
    let output = gate_agent(&sandbox.env(), &gate_input(&sandbox, "Agent", json!({})));
    let reason = parsed(output)["hookSpecificOutput"]["permissionDecisionReason"].clone();
    assert!(reason.as_str().is_some_and(|text| !text.is_empty()));
}

// ---- wait ------------------------------------------------------------------

#[test]
fn wait_prints_each_notice_on_its_own_line() {
    let sandbox = Sandbox::new();
    let server = server_with(
        &sandbox,
        "/lead/wait",
        json!({ "notices": ["worker a finished its turn", "worker b needs input"] }),
    );
    let text = wait(&sandbox.env(), 90, None);
    assert_eq!(text, "worker a finished its turn\nworker b needs input");
    let sent = server.requests_to("/lead/wait");
    assert_eq!(sent[0].json(), json!({ "root": sandbox.project_str(), "timeout_s": 90 }));
}

#[test]
fn wait_says_no_events_on_an_empty_list() {
    let sandbox = Sandbox::new();
    let _server = server_with(&sandbox, "/lead/wait", json!({ "notices": [] }));
    // A reply without a status reads as it always did.
    assert_eq!(wait(&sandbox.env(), 5, None), "no events");
}

#[test]
fn wait_reports_each_server_status_distinctly() {
    let say = |reply: Value| {
        let sandbox = Sandbox::new();
        let _server = server_with(&sandbox, "/lead/wait", reply);
        wait(&sandbox.env(), 90, None)
    };
    assert_eq!(say(json!({ "notices": [], "status": "timeout" })), "no events after 90 s");
    assert_eq!(say(json!({ "notices": [], "status": "detached" })), DETACHED);
    assert_eq!(say(json!({ "notices": [], "status": "no_project" })), NO_PROJECT);
    assert_eq!(say(json!({ "notices": [] , "status": "something-new" })), "no events");
    // Notices win over any status, and are printed as before.
    assert_eq!(say(json!({ "notices": ["a", "b"], "status": "notices" })), "a\nb");
    assert_eq!(say(json!({ "notices": ["a"] })), "a");
    // The messages tell the lead what to do instead of restarting blindly.
    assert!(DETACHED.contains("team_status") && DETACHED.contains("before restarting the waiter"));
    assert!(NO_PROJECT.contains("Do not restart the waiter"));
    assert!(NOT_REACHABLE.contains("Do not restart the waiter") && NOT_REACHABLE.contains("team_status"));
    assert_eq!(
        NOT_REACHABLE,
        "Mnemo2 is not reachable for this project. Do not restart the waiter; call team_status once Mnemo2 is running again."
    );
    assert_eq!(
        DETACHED,
        "The Mnemo lead was detached (another session closed or Mnemo2 restarted). Call team_status before restarting the waiter."
    );
}

#[test]
fn wait_without_an_instance_polls_then_says_not_reachable() {
    let sandbox = Sandbox::new();
    let env = sandbox.env(); // 300 ms patience, polling every 20 ms
    let started = Instant::now();
    assert_eq!(wait(&env, 5, None), NOT_REACHABLE);
    let took = started.elapsed();
    assert!(took >= env.wait_reach_timeout, "gave up after only {took:?}");
    assert!(took < env.wait_reach_timeout + Duration::from_secs(2), "{took:?}");
}

#[test]
fn wait_finds_an_instance_that_comes_up_while_it_polls() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let server = Arc::new(FakeServer::start("tok", {
        let project = project.clone();
        move |req| match req.path.as_str() {
            "/health" => health(55, Some(&project)),
            "/lead/wait" => json_reply(json!({ "notices": ["worker a finished its turn"], "status": "notices" })),
            _ => Reply::Status(404, String::new()),
        }
    }));
    let mut env = sandbox.env();
    env.wait_reach_timeout = Duration::from_secs(5);
    let (run_dir, port) = (sandbox.run_dir.clone(), server.port);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        crate::testutil::write_run_file(&run_dir, 55, port, "tok", Some(&project), 1);
    });
    assert_eq!(wait(&env, 5, None), "worker a finished its turn");
}

#[test]
fn wait_rides_out_a_restart_of_mnemo2() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let server = FakeServer::start("tok", {
        let (project, calls) = (project.clone(), calls.clone());
        move |req| match req.path.as_str() {
            "/health" => health(55, Some(&project)),
            // The first wait dies with the old instance, the second is answered.
            "/lead/wait" if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 => {
                Reply::Status(500, String::new())
            }
            "/lead/wait" => json_reply(json!({ "notices": [], "status": "timeout" })),
            _ => Reply::Status(404, String::new()),
        }
    });
    sandbox.write_run(55, server.port, "tok", Some(&project));
    let mut env = sandbox.env();
    env.wait_reach_timeout = Duration::from_secs(5);
    assert_eq!(wait(&env, 7, None), "no events after 7 s");
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn wait_falls_back_to_claude_project_dir() {
    let sandbox = Sandbox::new();
    let _server = server_with(&sandbox, "/lead/wait", json!({ "notices": ["x"] }));
    let mut env = sandbox.env();
    env.process_cwd = sandbox.elsewhere.clone();
    assert_eq!(wait(&env, 5, None), NOT_REACHABLE);
    env.project_dir = Some(sandbox.project.clone());
    assert_eq!(wait(&env, 5, None), "x");
}

#[test]
fn wait_gives_up_on_an_instance_that_keeps_failing() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let server = FakeServer::start("tok", move |req| {
        if req.path == "/health" {
            health(55, Some(&project))
        } else {
            Reply::Status(500, String::new())
        }
    });
    sandbox.write_run(55, server.port, "tok", Some(&sandbox.project_str()));
    let env = sandbox.env();
    let started = Instant::now();
    assert_eq!(wait(&env, 5, None), NOT_REACHABLE);
    assert!(started.elapsed() >= env.wait_reach_timeout);
    // It kept trying while it waited, rather than asking once and quitting.
    assert!(server.requests_to("/lead/wait").len() > 1);
}

#[test]
fn wait_sends_the_session_it_was_named_with() {
    let sandbox = Sandbox::new();
    let server = server_with(&sandbox, "/lead/wait", json!({ "notices": ["x"] }));
    assert_eq!(wait(&sandbox.env(), 5, Some("sess-9")), "x");
    assert_eq!(
        server.requests_to("/lead/wait")[0].json(),
        json!({ "root": sandbox.project_str(), "timeout_s": 5, "session": "sess-9" })
    );
}

#[test]
fn a_hook_without_a_session_id_sends_none() {
    let sandbox = Sandbox::new();
    let server = server_with(&sandbox, "/lead/notices", notices_reply(true, &[], 0, false, ""));
    let stdin = json!({ "cwd": sandbox.deep.to_string_lossy() }).to_string();
    assert!(notices(&sandbox.env(), NoticesMode::Drain, &stdin).is_none());
    assert_eq!(
        server.requests_to("/lead/notices")[0].json(),
        json!({ "root": sandbox.project_str(), "mode": "drain" })
    );
}

#[test]
fn stop_check_without_a_session_id_suggests_the_plain_wait() {
    let sandbox = Sandbox::new();
    let _server = server_with(&sandbox, "/lead/notices", notices_reply(true, &[], 2, false, ""));
    let env = sandbox.env();
    let stdin = json!({
        "cwd": sandbox.deep.to_string_lossy(),
        "stop_hook_active": false,
    })
    .to_string();
    let value = parsed(notices(&env, NoticesMode::StopCheck, &stdin));
    let helper = env.helper.display();
    assert_eq!(
        value["reason"],
        format!(
            "2 Mnemo worker(s) are still running and no waiter is active. Start this as a background command so you are woken when one reports. Bash: \"{helper}\" wait \u{2014} PowerShell: & \"{helper}\" wait"
        )
    );
}

#[test]
fn drain_leaves_the_waiter_line_out_when_a_waiter_runs_or_none_works() {
    // A waiter is alive: nothing to teach.
    let sandbox = Sandbox::new();
    let _server = server_with(&sandbox, "/lead/notices", notices_reply(true, &["n1"], 2, true, ""));
    let output = notices(&sandbox.env(), NoticesMode::Drain, &hook_stdin(&sandbox.deep, json!({})));
    assert_eq!(
        parsed(output)["hookSpecificOutput"]["additionalContext"],
        "Mnemo notices:\n- n1"
    );

    // No workers are running: nothing to be woken for.
    let idle = Sandbox::new();
    let _server = server_with(&idle, "/lead/notices", notices_reply(true, &["n1"], 0, false, ""));
    let output = notices(&idle.env(), NoticesMode::Drain, &hook_stdin(&idle.deep, json!({})));
    assert_eq!(
        parsed(output)["hookSpecificOutput"]["additionalContext"],
        "Mnemo notices:\n- n1"
    );
}

#[test]
fn session_start_without_a_session_id_teaches_no_waiter() {
    let sandbox = Sandbox::new();
    let _server = server_with(&sandbox, "/lead/notices", notices_reply(true, &[], 0, false, "Mnemo: idle."));
    let stdin = json!({ "cwd": sandbox.deep.to_string_lossy() }).to_string();
    let output = notices(&sandbox.env(), NoticesMode::SessionStart, &stdin);
    assert_eq!(parsed(output)["hookSpecificOutput"]["additionalContext"], "Mnemo: idle.");
}
