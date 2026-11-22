// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Tests for the stdio bridge and the inert stub.

use super::*;
use crate::instance::{resolve_app, AppSearch};
use crate::testutil::{
    connect_fails, health, wait_until_connect_fails, write_run_file, FakeServer, Reply, Sandbox,
};
use std::io::Cursor;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[derive(Clone, Default)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl SharedBuf {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }

    /// Every protocol line written so far, parsed.
    fn messages(&self) -> Vec<Value> {
        self.text()
            .lines()
            .map(|line| serde_json::from_str(line).expect("stdout carries only JSON lines"))
            .collect()
    }
}

/// Feed `lines` to `run` and return what it wrote to stdout, ordered by id.
fn run_lines(env: &Env, lines: &[&str]) -> (Vec<Value>, String) {
    let mut input = lines.join("\n");
    input.push('\n');
    let out = SharedBuf::default();
    let code = run_with(env.clone(), None, Cursor::new(input.into_bytes()), Box::new(out.clone()));
    assert_eq!(code, 0);
    let mut messages = out.messages();
    messages.sort_by_key(|message| message["id"].as_i64().unwrap_or(-1));
    (messages, out.text())
}

fn request(id: i64, method: &str) -> String {
    json!({ "jsonrpc": "2.0", "id": id, "method": method }).to_string()
}

fn initialize(id: i64) -> String {
    json!({
        "jsonrpc": "2.0", "id": id, "method": "initialize",
        "params": { "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": { "name": "claude-code" } },
    })
    .to_string()
}

const INITIALIZED: &str = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;

/// True for a helper id: 32 lowercase hex characters, as
/// `Uuid::new_v4().simple()` renders them.
fn is_bridge_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// A fake Mnemo2 `/mcp`: pretty-printed replies for requests, 202 for notifications.
fn mcp_server(project: &str) -> FakeServer {
    mcp_server_as(project, 77)
}

fn mcp_server_as(project: &str, pid: u32) -> FakeServer {
    let project = project.to_string();
    FakeServer::start("tok", move |req| match req.path.as_str() {
        "/health" => health(pid, Some(&project)),
        "/lead/detach" => Reply::Status(200, "{}".to_string()),
        "/mcp" => {
            let message = req.json();
            let Some(id) = message.get("id").cloned() else {
                return Reply::Status(202, String::new());
            };
            let result = match message["method"].as_str().unwrap_or("") {
                "initialize" => json!({
                    "protocolVersion": message["params"]["protocolVersion"],
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "mnemo" },
                    "instructions": "You lead a Mnemo team.",
                }),
                "tools/list" => json!({ "tools": [{ "name": "team_status" }] }),
                "tools/call" => {
                    std::thread::sleep(Duration::from_millis(300));
                    json!({ "content": [{ "type": "text", "text": "done" }], "isError": false })
                }
                _ => json!({}),
            };
            // Pretty-printed on purpose: stdout must still get one line per message.
            let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
            Reply::Status(200, serde_json::to_string_pretty(&reply).unwrap())
        }
        _ => Reply::Status(404, String::new()),
    })
}

fn attach(sandbox: &Sandbox, server: &FakeServer) {
    sandbox.write_run(77, server.port, "tok", Some(&sandbox.project_str()));
}

// ---- bridging --------------------------------------------------------------

#[test]
fn requests_are_bridged_with_auth_root_and_helper_and_notifications_are_not_answered() {
    let sandbox = Sandbox::new();
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);
    let env = sandbox.env();

    let (messages, raw) = run_lines(&env, &[&initialize(1), INITIALIZED, &request(2, "tools/list")]);

    assert_eq!(raw.lines().count(), 2, "one stdout line per answer: {raw}");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["id"], 1);
    assert_eq!(messages[0]["result"]["instructions"], "You lead a Mnemo team.");
    assert_eq!(messages[0]["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(messages[1]["id"], 2);
    assert_eq!(messages[1]["result"]["tools"][0]["name"], "team_status");

    let mcp = server.requests_to("/mcp");
    assert_eq!(mcp.len(), 3, "the notification is forwarded too");
    for req in &mcp {
        assert_eq!(req.headers["authorization"], "Bearer tok");
        assert_eq!(req.headers["x-mnemo-root"], sandbox.project_str());
        assert_eq!(req.headers["x-mnemo-helper"], env.helper.to_string_lossy());
        assert!(is_bridge_id(&req.headers["x-mnemo-bridge"]), "{:?}", req.headers);
    }
    assert!(mcp.iter().any(|req| req.json()["method"] == "notifications/initialized"));
}

#[test]
fn a_lone_notification_writes_nothing() {
    let sandbox = Sandbox::new();
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);
    let (messages, raw) = run_lines(&sandbox.env(), &[INITIALIZED]);
    assert!(messages.is_empty());
    assert!(raw.is_empty());
    assert_eq!(server.requests_to("/mcp").len(), 1);
}

#[test]
fn stdin_eof_detaches_the_external_lead_it_attached() {
    let sandbox = Sandbox::new();
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);
    run_lines(&sandbox.env(), &[&initialize(1), &request(2, "tools/call")]);
    let detach = server.requests_to("/lead/detach");
    assert_eq!(detach.len(), 1);
    let body = detach[0].json();
    assert_eq!(body["root"], json!(sandbox.project_str()));
    assert!(is_bridge_id(body["bridge"].as_str().unwrap_or("")), "{body}");
}

#[test]
fn forwarded_calls_carry_a_stable_bridge_id() {
    let sandbox = Sandbox::new();
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);

    run_lines(&sandbox.env(), &[&request(1, "tools/call"), &request(2, "tools/call")]);

    let calls = server.requests_to("/mcp");
    assert_eq!(calls.len(), 2);
    let ids: Vec<&str> = calls
        .iter()
        .filter_map(|req| req.headers.get("x-mnemo-bridge").map(String::as_str))
        .collect();
    assert_eq!(ids.len(), 2, "every forwarded call carries the header");
    assert!(is_bridge_id(ids[0]), "{}", ids[0]);
    assert_eq!(ids[0], ids[1], "one process keeps one id for all its calls");
}

#[test]
fn detach_on_close_names_the_bridge() {
    let sandbox = Sandbox::new();
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);

    run_lines(&sandbox.env(), &[&request(1, "tools/call")]);

    let call = &server.requests_to("/mcp")[0];
    let bridge = call.headers.get("x-mnemo-bridge").expect("the call names its helper").clone();
    let detach = server.requests_to("/lead/detach");
    assert_eq!(detach.len(), 1);
    let body = detach[0].json();
    assert_eq!(body["root"], json!(sandbox.project_str()));
    assert_eq!(body["bridge"], json!(bridge), "the detach names the helper that led");
}

#[test]
fn a_session_that_never_called_a_tool_does_not_detach_anyone() {
    let sandbox = Sandbox::new();
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);
    // Handshake, listing and a ping reach the server, but none of them
    // attaches the lead, so this session must leave the lead alone.
    run_lines(
        &sandbox.env(),
        &[&initialize(1), INITIALIZED, &request(2, "tools/list"), &request(3, "ping")],
    );
    assert!(!server.requests_to("/mcp").is_empty());
    assert!(server.requests_to("/lead/detach").is_empty());
}

#[test]
fn a_registered_session_that_only_lists_tools_does_not_detach_either() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);
    run_lines(&sandbox.env(), &[&initialize(1), INITIALIZED, &request(2, "tools/list")]);
    assert!(server.requests_to("/lead/detach").is_empty());
    // ... and one that called a tool detaches exactly once.
    run_lines(&sandbox.env(), &[&initialize(1), &request(2, "tools/call")]);
    assert_eq!(server.requests_to("/lead/detach").len(), 1);
}

#[test]
fn closing_stdin_without_a_message_neither_resolves_nor_detaches() {
    let sandbox = Sandbox::new();
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);
    let (messages, _) = run_lines(&sandbox.env(), &[]);
    assert!(messages.is_empty());
    assert!(server.requests().is_empty());
}

#[test]
fn a_slow_tool_call_still_answers_when_stdin_closes_right_after() {
    let sandbox = Sandbox::new();
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);
    let (messages, _) = run_lines(&sandbox.env(), &[&request(5, "tools/call")]);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["result"]["content"][0]["text"], "done");
    // The detach goes out only after the call finished.
    assert_eq!(server.requests_to("/lead/detach").len(), 1);
}

#[test]
fn a_garbled_line_gets_a_parse_error() {
    let sandbox = Sandbox::new();
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);
    let (messages, _) = run_lines(&sandbox.env(), &["{ nope", "", "   "]);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["error"]["code"], -32700);
    assert_eq!(messages[0]["id"], Value::Null);
}

#[test]
fn json_rpc_errors_pass_through_even_with_an_http_error_status() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let server = FakeServer::start("tok", move |req| match req.path.as_str() {
        "/health" => health(77, Some(&project)),
        "/mcp" => Reply::Status(
            500,
            json!({ "jsonrpc": "2.0", "id": 4, "error": { "code": -32603, "message": "internal" } })
                .to_string(),
        ),
        _ => Reply::Status(200, "{}".to_string()),
    });
    attach(&sandbox, &server);
    let (messages, _) = run_lines(&sandbox.env(), &[&request(4, "tools/list")]);
    assert_eq!(messages[0]["error"]["message"], "internal");
}

#[test]
fn a_plain_http_failure_becomes_a_json_rpc_error_for_that_request() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let server = FakeServer::start("tok", move |req| match req.path.as_str() {
        "/health" => health(77, Some(&project)),
        "/mcp" => Reply::Status(500, "kaboom".to_string()),
        _ => Reply::Status(200, "{}".to_string()),
    });
    attach(&sandbox, &server);
    let (messages, _) = run_lines(&sandbox.env(), &[&request(9, "tools/list")]);
    assert_eq!(messages[0]["id"], 9);
    let message = messages[0]["error"]["message"].as_str().unwrap();
    assert!(message.contains("HTTP 500") && message.contains("kaboom"), "{message}");
}

#[test]
fn a_rejected_token_is_retried_once_then_reported() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let server = FakeServer::start("tok", move |req| match req.path.as_str() {
        "/health" => health(77, Some(&project)),
        "/mcp" => Reply::Status(401, String::new()),
        _ => Reply::Status(200, "{}".to_string()),
    });
    attach(&sandbox, &server);
    let (messages, _) = run_lines(&sandbox.env(), &[&request(3, "tools/list"), INITIALIZED]);
    assert_eq!(messages.len(), 1, "the notification gets no error");
    let message = messages[0]["error"]["message"].as_str().unwrap();
    assert!(message.contains("isn't reachable"), "{message}");
    assert!(message.contains("token"), "{message}");
}

// ---- reconnecting and starting --------------------------------------------

fn process(bridge: &Arc<Bridge>, line: &str) {
    bridge.process(line, serde_json::from_str(line).ok());
}

fn test_bridge(env: Env) -> (Arc<Bridge>, SharedBuf) {
    let out = SharedBuf::default();
    let bridge = Arc::new(Bridge::new(env, None, Box::new(out.clone())));
    (bridge, out)
}

#[test]
fn a_dead_connection_triggers_one_lookup_and_the_request_is_resent() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    let first = mcp_server(&project);
    attach(&sandbox, &first);

    // When the first instance is gone, "starting Mnemo2" brings up a second one.
    let second: Arc<Mutex<Option<FakeServer>>> = Arc::new(Mutex::new(None));
    let started = Arc::new(AtomicUsize::new(0));
    let mut env = sandbox.env();
    env.spawner = {
        let (second, started) = (second.clone(), started.clone());
        let (run_dir, project) = (sandbox.run_dir.clone(), project.clone());
        Arc::new(move |_| {
            started.fetch_add(1, Ordering::SeqCst);
            let server = mcp_server_as(&project, 78);
            write_run_file(&run_dir, 78, server.port, "tok", Some(&project), 9);
            *second.lock().unwrap() = Some(server);
            Ok(None)
        })
    };
    let (bridge, out) = test_bridge(env);

    process(&bridge, &request(1, "tools/list"));
    assert_eq!(out.messages().len(), 1);
    assert_eq!(first.requests_to("/mcp").len(), 1);

    let first_port = first.port;
    drop(first); // Mnemo2 quits; its port closes.
    // macOS can still complete a handshake from the closed listener's backlog
    // and reset it, and the bridge rightly treats a break after connecting as
    // a mid-request failure it never resends. Wait until a connect to the old
    // port fails, where nothing reached the dead instance.
    wait_until_connect_fails(first_port);
    process(&bridge, &request(2, "tools/list"));

    let messages = out.messages();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert_eq!(messages[1]["id"], 2);
    assert!(messages[1].get("result").is_some(), "{messages:?}");
    assert_eq!(started.load(Ordering::SeqCst), 1);
    let second = second.lock().unwrap();
    assert_eq!(second.as_ref().unwrap().requests_to("/mcp").len(), 1);
}

/// The premise of the wait above: a live fake instance accepts a probe, a
/// dropped one does not — any failed connect is the resendable path.
#[test]
fn a_dropped_fake_server_stops_accepting_probe_connections() {
    let server = FakeServer::start("tok", |_| health(78, None));
    let port = server.port;
    assert!(!connect_fails(port));
    drop(server);
    wait_until_connect_fails(port);
    assert!(connect_fails(port));
}

#[test]
fn when_mnemo2_cannot_be_reached_pending_requests_get_a_json_rpc_error() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let first = mcp_server(&sandbox.project_str());
    attach(&sandbox, &first);
    let mut env = sandbox.env();
    env.spawner = Arc::new(|_| Err("could not start the Mnemo2 app".to_string()));
    let (bridge, out) = test_bridge(env);

    process(&bridge, &request(1, "tools/list"));
    let first_port = first.port;
    drop(first);
    wait_until_connect_fails(first_port);
    process(&bridge, &request(2, "tools/list"));
    process(&bridge, INITIALIZED);

    let messages = out.messages();
    assert_eq!(messages.len(), 2, "a notification gets no error: {messages:?}");
    assert_eq!(messages[1]["id"], 2);
    assert_eq!(messages[1]["error"]["code"], -32000);
    let message = messages[1]["error"]["message"].as_str().unwrap();
    assert!(message.contains("could not start the Mnemo2 app"), "{message}");
}

#[test]
fn a_missing_mnemo2_app_is_explained_in_the_error() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let mut env = sandbox.env();
    let nowhere = AppSearch {
        env_exe: None,
        helper_dir: sandbox.elsewhere.clone(),
        path_var: None,
    };
    env.spawner = Arc::new(move |_| resolve_app(&nowhere).map(|_| None));
    let (messages, _) = run_lines(&env, &[&initialize(1), &request(2, "tools/list")]);
    // The handshake still works; the first real request carries the explanation.
    assert!(messages[0].get("result").is_some(), "{messages:?}");
    let message = messages[1]["error"]["message"].as_str().unwrap();
    assert!(message.contains("isn't running"), "{message}");
    assert!(message.contains("could not be started"), "{message}");
    assert!(message.contains("npm run build:mnemo2:native"), "{message}");
}

#[test]
fn serve_starts_mnemo2_when_the_project_is_registered_but_nothing_runs() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    let server = Arc::new(mcp_server(&project));
    let started = Arc::new(AtomicUsize::new(0));
    let mut env = sandbox.env();
    env.spawner = {
        let (server, started) = (server.clone(), started.clone());
        let (run_dir, project) = (sandbox.run_dir.clone(), project.clone());
        Arc::new(move |root: &Path| {
            started.fetch_add(1, Ordering::SeqCst);
            assert_eq!(root, Path::new(&project));
            let (run_dir, project, port) = (run_dir.clone(), project.clone(), server.port);
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(120));
                write_run_file(&run_dir, 77, port, "tok", Some(&project), 1);
            });
            Ok(None)
        })
    };
    let (messages, _) = run_lines(&env, &[&initialize(1), &request(2, "tools/list")]);
    assert_eq!(started.load(Ordering::SeqCst), 1);
    // The handshake is answered here, with the lead brief naming this helper.
    let brief = messages[0]["result"]["instructions"].as_str().expect("instructions");
    assert!(brief.contains(&*env.helper.to_string_lossy()), "{brief}");
    assert_eq!(messages[1]["result"]["tools"][0]["name"], "team_status");
}

// ---- the stub outside every project -----------------------------------------

/// The tool names of a `tools/list` answer.
fn tool_names(message: &Value) -> Vec<String> {
    message["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .map(str::to_string)
        .collect()
}

/// The text of a tool result, however the call ended.
fn result_text(message: &Value) -> &str {
    message["result"]["content"][0]["text"].as_str().expect("tool result text")
}

fn call(id: i64, tool: &str, arguments: Value) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/call",
        "params": { "name": tool, "arguments": arguments },
    })
    .to_string()
}

#[test]
fn outside_a_registered_project_the_stub_answers_the_handshake_and_offers_the_project_tools() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.elsewhere]); // a project exists, just not this one
    let env = sandbox.env(); // its spawner would fail the test if used
    let response_to_server = json!({ "jsonrpc": "2.0", "id": 99, "result": {} }).to_string();
    let (messages, raw) = run_lines(
        &env,
        &[
            &initialize(1),
            INITIALIZED,
            &request(2, "tools/list"),
            &request(3, "ping"),
            &request(4, "tools/call"),
            &request(5, "resources/list"),
            &response_to_server,
        ],
    );
    assert_eq!(raw.lines().count(), 5, "{raw}");

    let init = &messages[0];
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(init["result"]["capabilities"], json!({ "tools": { "listChanged": true } }));
    assert_eq!(init["result"]["serverInfo"]["name"], "mnemo");
    assert!(init["result"].get("instructions").is_none(), "the stub must not carry instructions");

    assert_eq!(tool_names(&messages[1]), vec!["mnemo_projects", "mnemo_init_project"]);
    assert_eq!(messages[2]["result"], json!({}));
    // A tool call the stub cannot answer is a tool result error naming the two
    // tools that exist, so Claude reads what to call instead.
    assert_eq!(messages[3]["result"]["isError"], true, "{:?}", messages[3]);
    assert!(result_text(&messages[3]).contains("mnemo_init_project"));
    assert_eq!(messages[4]["error"]["code"], -32601, "{:?}", messages[4]);
    assert!(messages[4]["error"]["message"].as_str().unwrap().contains("registered"));
}

#[test]
fn a_leading_byte_order_mark_is_ignored() {
    let sandbox = Sandbox::new();
    let first = format!("\u{feff}{}", initialize(1));
    let (messages, _) = run_lines(&sandbox.env(), &[&first]);
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["result"]["serverInfo"]["name"], "mnemo");
}

#[test]
fn the_stub_does_not_need_a_registry_at_all() {
    let sandbox = Sandbox::new();
    let (messages, _) = run_lines(
        &sandbox.env(),
        &[&request(1, "tools/list"), &call(2, "mnemo_projects", json!({}))],
    );
    assert_eq!(tool_names(&messages[0]), vec!["mnemo_projects", "mnemo_init_project"]);
    assert!(result_text(&messages[1]).contains("No Mnemo projects yet"), "{:?}", messages[1]);
}

#[test]
fn stdout_stays_protocol_only_for_every_answer() {
    let sandbox = Sandbox::new();
    let (_, raw) = run_lines(&sandbox.env(), &[&initialize(1), &request(2, "tools/list")]);
    for line in raw.lines() {
        let value: Value = serde_json::from_str(line).expect("protocol line");
        assert_eq!(value["jsonrpc"], "2.0");
    }
}

// ---- the project tools ------------------------------------------------------

/// A spawner that brings a fake Mnemo2 up for the root it is asked to open,
/// `delay` later, and keeps it alive in `keep`.
fn starting_server(
    sandbox: &Sandbox,
    delay: Duration,
    starts: Arc<AtomicUsize>,
    keep: Arc<Mutex<Option<FakeServer>>>,
) -> crate::instance::Spawner {
    let run_dir = sandbox.run_dir.clone();
    Arc::new(move |root: &Path| {
        let (run_dir, root) = (run_dir.clone(), root.to_path_buf());
        let (starts, keep) = (starts.clone(), keep.clone());
        starts.fetch_add(1, Ordering::SeqCst);
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let project = root.to_string_lossy().into_owned();
            let server = mcp_server_as(&project, 88);
            write_run_file(&run_dir, 88, server.port, "tok", Some(&project), 1);
            *keep.lock().unwrap() = Some(server);
        });
        Ok(None)
    })
}

/// An `Env` whose session folder is `elsewhere` and whose spawner brings a
/// fake Mnemo2 up for whatever root it is handed.
fn stub_env(sandbox: &Sandbox) -> (Env, Arc<AtomicUsize>, Arc<Mutex<Option<FakeServer>>>) {
    let starts = Arc::new(AtomicUsize::new(0));
    let keep = Arc::new(Mutex::new(None));
    let mut env = sandbox.env();
    env.process_cwd = sandbox.elsewhere.clone();
    env.spawner = starting_server(sandbox, Duration::from_millis(100), starts.clone(), keep.clone());
    (env, starts, keep)
}

#[test]
fn stub_lists_the_project_tools() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.elsewhere]); // a project exists, just not this one
    let (messages, _) = run_lines(&sandbox.env(), &[&initialize(1), &request(2, "tools/list")]);

    let tools = messages[1]["result"]["tools"].as_array().expect("tools");
    assert_eq!(tool_names(&messages[1]), vec!["mnemo_projects", "mnemo_init_project"]);
    // `mnemo_projects` takes no arguments; `mnemo_init_project` one optional folder.
    assert_eq!(tools[0]["inputSchema"]["properties"], json!({}));
    assert_eq!(tools[1]["inputSchema"]["properties"]["path"]["type"], "string");
    assert_eq!(tools[1]["inputSchema"]["required"], Value::Null);
    for tool in tools {
        assert_eq!(tool["inputSchema"]["type"], "object");
        let description = tool["description"].as_str().expect("a description for Claude");
        assert!(description.contains("mnemo_") || description.contains("folder"), "{description}");
    }
    // Claude reads the description before it calls: the git rule is in there.
    assert!(tools[1]["description"].as_str().unwrap().contains("git init"), "{:?}", tools[1]);
}

#[test]
fn init_registers_and_goes_live_without_reload() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(sandbox.elsewhere.join(".git")).unwrap();
    let (env, starts, keep) = stub_env(&sandbox);
    let (bridge, out) = test_bridge(env);

    process(&bridge, &initialize(1));
    process(&bridge, &request(2, "tools/list"));
    let listed = out.messages().into_iter().find(|message| message["id"] == 2).expect("answer");
    assert_eq!(tool_names(&listed), vec!["mnemo_projects", "mnemo_init_project"]);

    process(&bridge, &call(3, "mnemo_init_project", json!({})));
    let done = out.messages().into_iter().find(|message| message["id"] == 3).expect("answer");
    assert_eq!(done["result"]["isError"], false, "{done}");
    let text = result_text(&done);
    assert!(text.contains("Team tools are now available in this session"), "{text}");

    // Registered under the folder's own name, at its canonical path.
    let registered = mnemo_app::mnemo2::project::list(Some(sandbox.config_dir.clone())).unwrap();
    assert_eq!(registered.len(), 1);
    assert_eq!(registered[0].name, "elsewhere");
    assert_eq!(canonical(Path::new(&registered[0].path)), sandbox.elsewhere);
    assert_eq!(starts.load(Ordering::SeqCst), 1, "it starts through the path resolve uses");
    assert!(keep.lock().unwrap().is_some(), "the instance came up");

    // The session is told the tool list changed, without a reload ...
    wait_for_list_changed(&out, Duration::from_secs(3));

    // ... and a later call is answered by the instance, not the stub.
    process(&bridge, &call(4, "team_status", json!({})));
    let answered = out.messages().into_iter().find(|message| message["id"] == 4).expect("answer");
    assert_eq!(answered["result"]["content"][0]["text"], "done", "{answered}");
}

/// One session leads the project its own folder belongs to: registering a
/// folder elsewhere must not take this session there, because the hooks and
/// the waiter keep resolving by the session folder.
#[test]
fn init_of_another_folder_registers_without_switching() {
    let sandbox = Sandbox::new();
    let other = sandbox.dir.path().join("other");
    std::fs::create_dir_all(other.join(".git")).unwrap();
    let (env, starts, _) = stub_env(&sandbox);
    let (bridge, out) = test_bridge(env);

    process(&bridge, &initialize(1));
    let arguments = json!({ "path": other.to_string_lossy() });
    process(&bridge, &call(2, "mnemo_init_project", arguments));
    let answer = out.messages().into_iter().find(|message| message["id"] == 2).expect("answer");
    assert_eq!(answer["result"]["isError"], false, "{answer}");
    let text = result_text(&answer);
    let other_canon_str = canonical(&other).to_string_lossy().into_owned();
    assert!(text.contains(&other_canon_str), "names the folder: {text}");
    assert!(text.contains("Start a Claude Code session in that folder"), "{text}");

    // The registry gained it, but nothing started and nothing was announced ...
    let registered = mnemo_app::mnemo2::project::list(Some(sandbox.config_dir.clone())).unwrap();
    assert_eq!(registered.len(), 1);
    assert_eq!(canonical(Path::new(&registered[0].path)), canonical(&other));
    assert_eq!(starts.load(Ordering::SeqCst), 0, "another folder is not started here");
    assert!(
        out.messages().iter().all(|message| message["method"] != "notifications/tools/list_changed"),
        "{:?}",
        out.messages()
    );

    // ... and the session stays on the stub.
    process(&bridge, &request(3, "tools/list"));
    let listed = out.messages().into_iter().find(|message| message["id"] == 3).expect("answer");
    assert_eq!(tool_names(&listed), vec!["mnemo_projects", "mnemo_init_project"]);
}

#[test]
fn init_refuses_a_registered_or_nested_folder() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let (env, _, _) = stub_env(&sandbox);
    let (bridge, out) = test_bridge(env);
    process(&bridge, &initialize(1));

    // The folder itself, a folder inside it, and a folder that contains it.
    let folders = [
        sandbox.project.clone(),
        sandbox.deep.clone(),
        sandbox.dir.path().to_path_buf(),
    ];
    for (index, folder) in folders.iter().enumerate() {
        let id = index as i64 + 2;
        let arguments = json!({ "path": folder.to_string_lossy() });
        process(&bridge, &call(id, "mnemo_init_project", arguments));
        let answer = out.messages().into_iter().find(|message| message["id"] == id).expect("answer");
        assert_eq!(answer["result"]["isError"], true, "{answer}");
        let text = result_text(&answer);
        assert!(text.contains("\"p0\""), "the existing entry is named: {text}");
        assert!(text.contains(&sandbox.project_str()), "{text}");
        assert!(text.contains("Start a Claude Code session in that folder"), "{text}");
    }

    // Nothing was registered and nothing was started.
    let registered = mnemo_app::mnemo2::project::list(Some(sandbox.config_dir.clone())).unwrap();
    assert_eq!(registered.len(), 1);
    assert_eq!(registered[0].name, "p0");
}

#[test]
fn init_refuses_a_folder_without_git() {
    let sandbox = Sandbox::new();
    let (env, starts, _) = stub_env(&sandbox);
    let (bridge, out) = test_bridge(env);
    process(&bridge, &initialize(1));

    // Not a git repository: the tool never runs `git init` itself.
    process(&bridge, &call(2, "mnemo_init_project", json!({})));
    let refused = out.messages().into_iter().find(|message| message["id"] == 2).expect("answer");
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    let text = result_text(&refused);
    assert!(text.contains("not a git repository"), "{text}");
    assert!(text.contains("git init"), "{text}");

    // A folder that is not there is refused too.
    let missing = sandbox.elsewhere.join("nope");
    let arguments = json!({ "path": missing.to_string_lossy() });
    process(&bridge, &call(3, "mnemo_init_project", arguments));
    let refused = out.messages().into_iter().find(|message| message["id"] == 3).expect("answer");
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert!(result_text(&refused).contains("is not a folder"));

    assert_eq!(starts.load(Ordering::SeqCst), 0);
    assert!(mnemo_app::mnemo2::project::list(Some(sandbox.config_dir.clone())).unwrap().is_empty());
}

/// A folder registered somewhere else — by another session's init, or by the
/// Mnemo2 window — is picked up by the next tool request, not by a reload.
#[test]
fn stub_picks_up_a_project_registered_meanwhile() {
    let sandbox = Sandbox::new();
    let (env, starts, _) = stub_env(&sandbox);
    let (bridge, out) = test_bridge(env);

    process(&bridge, &initialize(1));
    process(&bridge, &request(2, "tools/list"));
    let listed = out.messages().into_iter().find(|message| message["id"] == 2).expect("answer");
    assert_eq!(tool_names(&listed), vec!["mnemo_projects", "mnemo_init_project"]);
    assert_eq!(starts.load(Ordering::SeqCst), 0, "the stub's re-check must not start Mnemo2");

    // The Mnemo2 window registers this folder while the session runs.
    mnemo_app::mnemo2::project::add(Some(sandbox.config_dir.clone()), &sandbox.elsewhere)
        .expect("register");

    // The next request re-reads the registry, starts Mnemo2 and goes live.
    process(&bridge, &request(3, "tools/list"));
    let listed = out.messages().into_iter().find(|message| message["id"] == 3).expect("answer");
    assert_eq!(tool_names(&listed), vec!["team_status"]);
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    wait_for_list_changed(&out, Duration::from_secs(3));

    process(&bridge, &call(4, "team_status", json!({})));
    let answered = out.messages().into_iter().find(|message| message["id"] == 4).expect("answer");
    assert_eq!(answered["result"]["content"][0]["text"], "done", "{answered}");
}

// ---- the fast local handshake ---------------------------------------------

/// A spawner that brings a fake Mnemo2 up `delay` after it is called, and
/// raises `up` when it has.
fn slow_start(
    sandbox: &Sandbox,
    delay: Duration,
    up: Arc<AtomicBool>,
    keep: Arc<Mutex<Option<FakeServer>>>,
) -> crate::instance::Spawner {
    let (run_dir, project) = (sandbox.run_dir.clone(), sandbox.project_str());
    Arc::new(move |_| {
        let (run_dir, project, up, keep) = (run_dir.clone(), project.clone(), up.clone(), keep.clone());
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            let server = mcp_server(&project);
            write_run_file(&run_dir, 77, server.port, "tok", Some(&project), 1);
            *keep.lock().unwrap() = Some(server);
            up.store(true, Ordering::SeqCst);
        });
        Ok(None)
    })
}

#[test]
fn initialize_is_answered_at_once_while_mnemo2_is_still_starting() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let up = Arc::new(AtomicBool::new(false));
    let keep = Arc::new(Mutex::new(None));
    let mut env = sandbox.env();
    env.spawner = slow_start(&sandbox, Duration::from_millis(600), up.clone(), keep.clone());
    let (bridge, out) = test_bridge(env.clone());

    std::thread::scope(|scope| {
        scope.spawn(|| process(&bridge, &initialize(1)));
        let started = Instant::now();
        while out.messages().is_empty() {
            assert!(started.elapsed() < Duration::from_millis(400), "no handshake answer");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!up.load(Ordering::SeqCst), "the answer must not wait for Mnemo2");

        let init = &out.messages()[0];
        assert_eq!(init["id"], 1);
        assert_eq!(init["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(init["result"]["capabilities"], json!({ "tools": { "listChanged": true } }));
        assert_eq!(init["result"]["serverInfo"]["name"], "mnemo");
        assert_eq!(init["result"]["serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
        let brief = init["result"]["instructions"].as_str().expect("instructions");
        assert_eq!(brief, lead_instructions(&env.helper.to_string_lossy()));
        assert!(brief.contains(&*env.helper.to_string_lossy()));

        // The client's follow-ups queue until Mnemo2 is up, then go through.
        process(&bridge, INITIALIZED);
        process(&bridge, &request(2, "tools/list"));
        assert!(up.load(Ordering::SeqCst), "tools/list must have waited for the instance");
    });

    let messages = out.messages();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert_eq!(messages[1]["id"], 2);
    assert_eq!(messages[1]["result"]["tools"][0]["name"], "team_status");

    // The server never saw the handshake: not initialize, not initialized.
    let keep = keep.lock().unwrap();
    let seen: Vec<String> = keep
        .as_ref()
        .expect("server")
        .requests_to("/mcp")
        .iter()
        .map(|req| req.json()["method"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(seen, vec!["tools/list"]);
}

#[test]
fn notifications_flow_once_the_instance_is_ready() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);
    let (bridge, _out) = test_bridge(sandbox.env());
    process(&bridge, &initialize(1));
    process(&bridge, &request(2, "tools/list"));
    process(&bridge, r#"{"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":2}}"#);
    process(&bridge, INITIALIZED);
    let methods: Vec<String> = server
        .requests_to("/mcp")
        .iter()
        .map(|req| req.json()["method"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(methods, vec!["tools/list", "notifications/cancelled"]);
}

#[test]
fn requests_queued_behind_a_failed_start_share_its_error_and_the_next_one_retries() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let starts = Arc::new(AtomicUsize::new(0));
    let mut env = sandbox.env();
    env.spawner = {
        let starts = starts.clone();
        Arc::new(move |_| {
            starts.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(400));
            Err("could not start the Mnemo2 app".to_string())
        })
    };
    let (bridge, out) = test_bridge(env);
    let started = Instant::now();
    std::thread::scope(|scope| {
        scope.spawn(|| process(&bridge, &initialize(1)));
        std::thread::sleep(Duration::from_millis(100));
        scope.spawn(|| process(&bridge, &request(2, "tools/list")));
        scope.spawn(|| process(&bridge, &request(3, "tools/list")));
    });
    assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
    assert_eq!(starts.load(Ordering::SeqCst), 1, "queued requests must not each start Mnemo2");
    let mut messages = out.messages();
    messages.sort_by_key(|message| message["id"].as_i64().unwrap());
    assert!(messages[0].get("result").is_some(), "the handshake still succeeded");
    for index in [1, 2] {
        let message = messages[index]["error"]["message"].as_str().expect("error");
        assert!(message.contains("could not start the Mnemo2 app"), "{message}");
    }

    // A later request tries again.
    process(&bridge, &request(4, "tools/list"));
    assert_eq!(starts.load(Ordering::SeqCst), 2);
}

/// Wait until `out` has written a `notifications/tools/list_changed`.
fn wait_for_list_changed(out: &SharedBuf, within: Duration) {
    let started = Instant::now();
    while !out
        .messages()
        .iter()
        .any(|message| message["method"] == "notifications/tools/list_changed")
    {
        assert!(started.elapsed() < within, "no list_changed: {:?}", out.messages());
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Claude Code drops a `tools/list` after 30 s and then keeps no tools for the
/// session. A start slower than `list_wait` gets an empty list in time and a
/// `list_changed` once Mnemo2 is up, after which the real list comes through.
#[test]
fn a_start_slower_than_list_wait_answers_no_tools_then_announces_them() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let up = Arc::new(AtomicBool::new(false));
    let keep = Arc::new(Mutex::new(None));
    let mut env = sandbox.env();
    env.list_wait = Duration::from_millis(150);
    env.spawner = slow_start(&sandbox, Duration::from_millis(600), up.clone(), keep.clone());
    let (bridge, out) = test_bridge(env);

    std::thread::scope(|scope| {
        scope.spawn(|| process(&bridge, &initialize(1)));
        let started = Instant::now();
        process(&bridge, &request(2, "tools/list"));
        assert!(started.elapsed() < Duration::from_millis(500), "{:?}", started.elapsed());
        assert!(!up.load(Ordering::SeqCst));
        let answer = out.messages().into_iter().find(|message| message["id"] == 2).expect("answer");
        assert_eq!(answer["result"], json!({ "tools": [] }));

        wait_for_list_changed(&out, Duration::from_secs(3));
        assert!(up.load(Ordering::SeqCst), "announced before Mnemo2 was up");
    });
    process(&bridge, &request(3, "tools/list"));
    let messages = out.messages();
    let changed = messages
        .iter()
        .filter(|message| message["method"] == "notifications/tools/list_changed")
        .count();
    assert_eq!(changed, 1, "{messages:?}");
    let refreshed = messages.iter().find(|message| message["id"] == 3).expect("answer");
    assert_eq!(refreshed["result"]["tools"][0]["name"], "team_status");
}

/// A failed start leaves Claude Code without tools; a Mnemo2 the user then
/// starts by hand is found without a reconnect and the tools announced.
#[test]
fn after_a_failed_start_a_mnemo2_started_by_hand_brings_the_tools_in() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let mut env = sandbox.env();
    env.spawner = Arc::new(|_| Err("could not start the Mnemo2 app".to_string()));
    let (bridge, out) = test_bridge(env);

    process(&bridge, &initialize(1));
    process(&bridge, &request(2, "tools/list"));
    let failed = out.messages().into_iter().find(|message| message["id"] == 2).expect("answer");
    let message = failed["error"]["message"].as_str().expect("error");
    assert!(message.contains("could not start the Mnemo2 app"), "{message}");

    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);
    wait_for_list_changed(&out, Duration::from_secs(3));
    process(&bridge, &request(3, "tools/list"));
    let listed = out.messages().into_iter().find(|message| message["id"] == 3).expect("answer");
    assert_eq!(listed["result"]["tools"][0]["name"], "team_status");
}

/// Nothing announces a change that did not happen.
#[test]
fn a_tools_list_answered_in_time_announces_nothing() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);
    let (bridge, out) = test_bridge(sandbox.env());
    process(&bridge, &initialize(1));
    process(&bridge, &request(2, "tools/list"));
    std::thread::sleep(Duration::from_millis(100));
    let messages = out.messages();
    assert_eq!(messages.len(), 2, "{messages:?}");
    assert!(messages.iter().all(|message| message.get("method").is_none()), "{messages:?}");
}

#[test]
fn the_stub_keeps_answering_initialize_itself_outside_registered_projects() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.elsewhere]);
    let (bridge, out) = test_bridge(sandbox.env());
    assert!(!bridge.local_initialize);
    process(&bridge, &initialize(1));
    let init = &out.messages()[0];
    assert!(init["result"].get("instructions").is_none());
}

// ---- the one-time permissions note ------------------------------------------

fn test_bridge_with_claude_dir(env: Env, claude_dir: PathBuf) -> (Arc<Bridge>, SharedBuf) {
    let out = SharedBuf::default();
    let bridge = Arc::new(Bridge::new(env, Some(claude_dir), Box::new(out.clone())));
    (bridge, out)
}

fn claude_dir_in(sandbox: &Sandbox) -> PathBuf {
    sandbox.dir.path().join("claude-home")
}

fn setup_note(message: &Value) -> Option<String> {
    message["result"]["content"]
        .as_array()?
        .iter()
        .filter_map(|block| block["text"].as_str())
        .find(|text| text.starts_with("[Mnemo setup, shown once]"))
        .map(str::to_string)
}

#[test]
fn the_first_tool_result_of_a_session_carries_the_note_and_later_ones_do_not() {
    let sandbox = Sandbox::new();
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);
    let env = sandbox.env();
    let (bridge, out) = test_bridge_with_claude_dir(env.clone(), claude_dir_in(&sandbox));

    process(&bridge, &request(1, "tools/list"));
    process(&bridge, &request(2, "tools/call"));
    process(&bridge, &request(3, "tools/call"));

    let messages = out.messages();
    assert!(setup_note(&messages[0]).is_none(), "a tool listing is not a tool call");
    let blocks = messages[1]["result"]["content"].as_array().unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0]["text"], "done");
    assert_eq!(messages[1]["result"]["isError"], false);
    let note = setup_note(&messages[1]).expect("the first tool result carries the note");
    let escaped_helper = serde_json::to_string(&env.helper.to_string_lossy()).unwrap();
    assert!(note.contains(escaped_helper.trim_matches('"')), "{note}");
    assert_eq!(messages[2]["result"]["content"].as_array().unwrap().len(), 1);
    assert!(sandbox.config_dir.join("claude-lead").join("permissions-offered").is_file());

    // A new session (the marker is there now) is not offered anything.
    let (next, next_out) = test_bridge_with_claude_dir(env, claude_dir_in(&sandbox));
    process(&next, &request(1, "tools/call"));
    assert!(setup_note(&next_out.messages()[0]).is_none());
}

#[test]
fn of_several_tool_calls_in_flight_exactly_one_carries_the_note() {
    let sandbox = Sandbox::new();
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);
    let out = SharedBuf::default();
    let code = run_with(
        sandbox.env(),
        Some(claude_dir_in(&sandbox)),
        Cursor::new(
            [initialize(1), request(2, "tools/call"), request(3, "tools/call"), request(4, "tools/call")]
                .join("\n")
                .into_bytes(),
        ),
        Box::new(out.clone()),
    );
    assert_eq!(code, 0);
    let messages = out.messages();
    assert_eq!(messages.iter().filter(|message| setup_note(message).is_some()).count(), 1);
}

#[test]
fn a_project_that_already_allows_mnemo_is_not_offered_the_note() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);
    let settings = sandbox.project.join(".claude").join("settings.local.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, r#"{"permissions":{"allow":["mcp__plugin_mnemo_mnemo"]}}"#).unwrap();

    let (bridge, out) = test_bridge_with_claude_dir(sandbox.env(), claude_dir_in(&sandbox));
    process(&bridge, &request(1, "tools/call"));
    let messages = out.messages();
    assert_eq!(messages[0]["result"]["content"].as_array().unwrap().len(), 1);
    assert!(!sandbox.config_dir.join("claude-lead").exists());
}

#[test]
fn an_error_answer_to_a_tool_call_does_not_use_the_note_up() {
    let sandbox = Sandbox::new();
    let project = sandbox.project_str();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = {
        let calls = calls.clone();
        FakeServer::start("tok", move |req| match req.path.as_str() {
            "/health" => health(77, Some(&project)),
            "/lead/detach" => Reply::Status(200, "{}".to_string()),
            "/mcp" => {
                let id = req.json()["id"].clone();
                let reply = if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32602, "message": "bad arguments" } })
                } else {
                    json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": "done" }] } })
                };
                Reply::Status(200, reply.to_string())
            }
            _ => Reply::Status(404, String::new()),
        })
    };
    attach(&sandbox, &server);
    let (bridge, out) = test_bridge_with_claude_dir(sandbox.env(), claude_dir_in(&sandbox));

    process(&bridge, &request(1, "tools/call"));
    process(&bridge, &request(2, "tools/call"));

    let messages = out.messages();
    assert_eq!(messages[0]["error"]["message"], "bad arguments");
    assert!(messages[0].get("result").is_none());
    assert!(setup_note(&messages[1]).is_some(), "the next fitting result carries it: {messages:?}");
}

// ---- live tool-list revisions ---------------------------------------------

/// A fake app whose every `/mcp` reply carries `_mnemo_tools_revision`, the
/// value read from `revision` at the time of the request.
fn revision_server(project: &str, pid: u32, revision: Arc<Mutex<String>>) -> FakeServer {
    let project = project.to_string();
    FakeServer::start("tok", move |req| match req.path.as_str() {
        "/health" => health(pid, Some(&project)),
        "/lead/detach" => Reply::Status(200, "{}".to_string()),
        "/mcp" => {
            let message = req.json();
            let Some(id) = message.get("id").cloned() else {
                return Reply::Status(202, String::new());
            };
            let result = match message["method"].as_str().unwrap_or("") {
                "tools/list" => json!({ "tools": [{ "name": "team_status" }] }),
                _ => json!({}),
            };
            let reply = json!({
                "jsonrpc": "2.0", "id": id, "result": result,
                "_mnemo_tools_revision": revision.lock().unwrap().clone(),
            });
            Reply::Status(200, reply.to_string())
        }
        _ => Reply::Status(404, String::new()),
    })
}

fn list_changed_count(out: &SharedBuf) -> usize {
    out.messages()
        .iter()
        .filter(|message| message["method"] == "notifications/tools/list_changed")
        .count()
}

#[test]
fn helper_notifies_list_changed_once_per_revision() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let revision = Arc::new(Mutex::new("A".to_string()));
    let server = revision_server(&sandbox.project_str(), 77, revision.clone());
    attach(&sandbox, &server);
    let (bridge, out) = test_bridge(sandbox.env());

    process(&bridge, &request(1, "tools/list"));
    process(&bridge, &request(2, "ping"));
    assert_eq!(list_changed_count(&out), 0, "{:?}", out.messages());

    *revision.lock().unwrap() = "B".to_string();
    process(&bridge, &request(3, "ping"));
    process(&bridge, &request(4, "ping"));

    // Replies A (list), A, B, B: one notification, right after the first B.
    let messages = out.messages();
    let ids: Vec<Value> = messages
        .iter()
        .map(|message| {
            if message["method"] == "notifications/tools/list_changed" {
                json!("notification")
            } else {
                message["id"].clone()
            }
        })
        .collect();
    assert_eq!(ids, vec![json!(1), json!(2), json!(3), json!("notification"), json!(4)]);
    assert!(!out.text().contains("_mnemo_tools_revision"), "{}", out.text());
}

#[test]
fn helper_notifies_after_reconnecting_to_a_new_revision() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let project = sandbox.project_str();
    let first = revision_server(&project, 77, Arc::new(Mutex::new("A".to_string())));
    attach(&sandbox, &first);

    // The rebuilt Mnemo2 that "starting" brings up serves another revision.
    let second: Arc<Mutex<Option<FakeServer>>> = Arc::new(Mutex::new(None));
    let mut env = sandbox.env();
    env.spawner = {
        let second = second.clone();
        let (run_dir, project) = (sandbox.run_dir.clone(), project.clone());
        Arc::new(move |_| {
            let server = revision_server(&project, 78, Arc::new(Mutex::new("B".to_string())));
            write_run_file(&run_dir, 78, server.port, "tok", Some(&project), 9);
            *second.lock().unwrap() = Some(server);
            Ok(None)
        })
    };
    let (bridge, out) = test_bridge(env);

    process(&bridge, &request(1, "tools/list"));
    let first_port = first.port;
    drop(first);
    wait_until_connect_fails(first_port);
    process(&bridge, &request(2, "ping"));
    process(&bridge, &request(3, "ping"));

    let messages = out.messages();
    assert_eq!(list_changed_count(&out), 1, "{messages:?}");
    assert_eq!(messages[1]["id"], 2);
    assert_eq!(messages[2]["method"], "notifications/tools/list_changed");
    assert!(!out.text().contains("_mnemo_tools_revision"));
}

#[test]
fn a_reply_without_a_revision_never_notifies() {
    let sandbox = Sandbox::new();
    sandbox.register(&[&sandbox.project]);
    let server = mcp_server(&sandbox.project_str());
    attach(&sandbox, &server);
    let (bridge, out) = test_bridge(sandbox.env());
    process(&bridge, &request(1, "tools/list"));
    process(&bridge, &request(2, "ping"));
    assert_eq!(list_changed_count(&out), 0);
}
