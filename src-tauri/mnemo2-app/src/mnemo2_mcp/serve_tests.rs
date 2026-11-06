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

// ---- the inert stub --------------------------------------------------------

#[test]
fn outside_a_registered_project_the_stub_answers_the_handshake_and_offers_nothing() {
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
    assert_eq!(init["result"]["capabilities"], json!({ "tools": {} }));
    assert_eq!(init["result"]["serverInfo"]["name"], "mnemo");
    assert!(init["result"].get("instructions").is_none(), "the stub must not carry instructions");

    assert_eq!(messages[1]["result"], json!({ "tools": [] }));
    assert_eq!(messages[2]["result"], json!({}));
    for index in [3, 4] {
        assert_eq!(messages[index]["error"]["code"], -32601, "{:?}", messages[index]);
        assert!(messages[index]["error"]["message"].as_str().unwrap().contains("registered"));
    }
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
    let (messages, _) = run_lines(&sandbox.env(), &[&request(1, "tools/list")]);
    assert_eq!(messages[0]["result"], json!({ "tools": [] }));
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
