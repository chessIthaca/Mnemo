// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Read-only worker summaries and mission-control navigation for UI mods.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::{json, Value};

use crate::http;
use crate::instance::{self, Env, Resolution};

const EMPTY_SUMMARY: &str = "{}";
const CALL_TIMEOUT: Duration = Duration::from_secs(4);

/// Print the live team summary as JSON, or `{}` when none is available.
///
/// This deliberately uses the hook lookup: it neither starts Mnemo2 nor
/// attaches its external lead, and it never touches the notice queue.
pub fn team_summary(env: &Env, args: &[String]) -> String {
    let Some(cwd) = parse_cwd(env, args, true) else {
        return EMPTY_SUMMARY.to_string();
    };
    let Some(instance) = live_registered_instance(env, &cwd) else {
        return EMPTY_SUMMARY.to_string();
    };
    instance::call(
        &instance,
        "/lead/summary",
        &instance::root_body(&instance),
        instance::HOOK_TIMEOUT,
    )
    .filter(Value::is_object)
    .map(|value| value.to_string())
    .unwrap_or_else(|| EMPTY_SUMMARY.to_string())
}

/// Find an already-running instance for a registered project, without starting one.
fn live_registered_instance(env: &Env, cwd: &std::path::Path) -> Option<instance::Instance> {
    instance::registered_root(env, cwd).ok().flatten()?;
    instance::find_instance_for_hook(env, cwd)
}

/// Ask Mnemo2 to show one worker in mission control.
///
/// Unlike [`team_summary`], this may start Mnemo2 for a registered project,
/// just as the helper's `serve` command does.
pub fn show_worker(env: &Env, args: &[String]) -> Result<(), String> {
    let Some((worker_id, rest)) = args.split_first() else {
        return Err("usage: mnemo2-mcp show-worker <worker_id> [--cwd <dir>]".to_string());
    };
    if worker_id.starts_with("--") || worker_id.is_empty() {
        return Err("usage: mnemo2-mcp show-worker <worker_id> [--cwd <dir>]".to_string());
    }
    let cwd = parse_cwd(env, rest, false)
        .ok_or_else(|| "usage: mnemo2-mcp show-worker <worker_id> [--cwd <dir>]".to_string())?;
    let instance = match instance::resolve(env, &cwd) {
        Resolution::Live(instance) => instance,
        Resolution::Unregistered => {
            return Err("this directory is not a registered Mnemo2 project".to_string());
        }
        Resolution::Failed(reason) => return Err(one_line(&reason)),
    };
    let body = json!({ "root": instance.root, "worker_id": worker_id }).to_string();
    let response = http::post(
        &instance.endpoint,
        "/lead/show-worker",
        &[],
        &body,
        Some(CALL_TIMEOUT),
    )
    .map_err(|error| format!("could not show worker {worker_id}: {error}"))?;
    if (200..300).contains(&response.status) {
        return Ok(());
    }
    let reason = serde_json::from_str::<Value>(&response.body)
        .ok()
        .and_then(|value| value["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| format!("Mnemo2 returned HTTP {}", response.status));
    Err(one_line(&format!(
        "could not show worker {worker_id}: {reason}"
    )))
}

/// Parse `--cwd <dir>` (and, for the summary command, `--json`).
fn parse_cwd(env: &Env, args: &[String], allow_json: bool) -> Option<PathBuf> {
    let mut cwd = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--json" if allow_json => index += 1,
            "--cwd" => {
                cwd = Some(PathBuf::from(args.get(index + 1)?));
                index += 2;
            }
            argument => {
                let path = argument.strip_prefix("--cwd=")?;
                cwd = Some(PathBuf::from(path));
                index += 1;
            }
        }
    }
    Some(
        cwd.or_else(|| env.project_dir.clone())
            .unwrap_or_else(|| env.process_cwd.clone()),
    )
}

/// Keep command failures to the one line promised by the helper interface.
fn one_line(text: &str) -> String {
    text.lines().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{health, FakeServer, Reply, Sandbox};
    use std::time::Instant;

    /// A fake Mnemo2 with the sandbox project open whose `path` answers `body`.
    fn server_with(sandbox: &Sandbox, path: &'static str, status: u16, body: Value) -> FakeServer {
        let project = sandbox.project_str();
        let server = FakeServer::start("tok", move |request| {
            if request.path == "/health" {
                health(55, Some(&project))
            } else if request.path == path {
                Reply::Status(status, body.to_string())
            } else {
                Reply::Status(404, String::new())
            }
        });
        sandbox.write_run(55, server.port, "tok", Some(&sandbox.project_str()));
        sandbox.register(&[&sandbox.project]);
        server
    }

    #[test]
    fn team_summary_returns_the_workers_json_without_leadership_calls() {
        let sandbox = Sandbox::new();
        let payload = json!({
            "workers": [{
                "id": "worker-kimi",
                "name": "worker-kimi",
                "model": "Kimi/kimi-k3.1-pro",
                "model_short": "kimi",
                "tier": "routine",
                "status": "running",
                "needs_you": false,
                "elapsed_ms": 1250
            }],
            "open_plans": 1,
            "running": 1,
            "waiting": 0
        });
        let server = server_with(&sandbox, "/lead/summary", 200, payload.clone());
        let args = vec![
            "--json".to_string(),
            "--cwd".to_string(),
            sandbox.deep.to_string_lossy().into_owned(),
        ];

        let output = team_summary(&sandbox.env(), &args);

        assert_eq!(
            serde_json::from_str::<Value>(&output).expect("JSON"),
            payload
        );
        let sent = server.requests_to("/lead/summary");
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].json(), json!({ "root": sandbox.project_str() }));
        assert!(server.requests_to("/lead/notices").is_empty());
        assert!(server.requests_to("/mcp").is_empty());
    }

    #[test]
    fn team_summary_is_quiet_outside_a_registered_project() {
        let sandbox = Sandbox::new();
        let server = server_with(&sandbox, "/lead/summary", 200, json!({ "workers": [] }));
        let mut env = sandbox.env();
        env.process_cwd = sandbox.elsewhere.clone();

        assert_eq!(team_summary(&env, &["--json".to_string()]), EMPTY_SUMMARY);
        assert!(
            server.requests().is_empty(),
            "the app must not even be probed"
        );
    }

    #[test]
    fn team_summary_is_fast_and_does_not_start_a_stopped_app() {
        let sandbox = Sandbox::new();
        sandbox.register(&[&sandbox.project]);
        let start = Instant::now();

        assert_eq!(
            team_summary(&sandbox.env(), &["--json".to_string()]),
            EMPTY_SUMMARY
        );
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "lookup took {:?}",
            start.elapsed()
        );
        // Sandbox::env installs a spawner that always fails, so reaching it would fail this test.
    }

    #[test]
    fn show_worker_uses_only_its_read_only_navigation_route() {
        let sandbox = Sandbox::new();
        let server = server_with(&sandbox, "/lead/show-worker", 200, json!({ "shown": true }));
        let args = vec![
            "worker-kimi".to_string(),
            "--cwd".to_string(),
            sandbox.deep.to_string_lossy().into_owned(),
        ];

        show_worker(&sandbox.env(), &args).expect("worker shown");

        let sent = server.requests_to("/lead/show-worker");
        assert_eq!(sent.len(), 1);
        assert_eq!(
            sent[0].json(),
            json!({ "root": sandbox.project_str(), "worker_id": "worker-kimi" })
        );
        assert!(server.requests_to("/lead/notices").is_empty());
        assert!(server.requests_to("/mcp").is_empty());
    }

    #[test]
    fn show_worker_reports_an_unknown_worker_on_one_line() {
        let sandbox = Sandbox::new();
        let _server = server_with(
            &sandbox,
            "/lead/show-worker",
            404,
            json!({ "error": "unknown worker: worker-kimi" }),
        );

        let error = show_worker(&sandbox.env(), &["worker-kimi".to_string()])
            .expect_err("unknown worker must fail");
        assert!(error.contains("unknown worker: worker-kimi"), "{error}");
        assert!(!error.contains(['\r', '\n']), "{error:?}");
    }
}
