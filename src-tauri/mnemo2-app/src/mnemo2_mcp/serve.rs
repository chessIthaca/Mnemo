// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `serve`: the stdio MCP bridge Claude Code launches as the plugin's MCP server.
//!
//! Claude Code speaks newline-delimited JSON-RPC on stdin/stdout; Mnemo2 speaks
//! the same messages over its loopback HTTP endpoint. Stdout carries protocol
//! lines only (logs go to stderr). Directories outside every registered project
//! get an inert stub so the plugin stays quiet there.
//!
//! Inside a registered project `initialize` is answered here, at once: Claude
//! Code gives an MCP server about 30 s to connect and a cold Mnemo2 start can
//! take longer. The instance is found (or started) in the background and every
//! later request waits for it. `tools/list` gets the same 30 s, so it waits at
//! most `Env::list_wait`: past that it is answered with no tools, and the
//! bridge sends `notifications/tools/list_changed` once the instance is up.
//! After a failed start it keeps looking, so a Mnemo2 the user starts by hand
//! still brings the tools in.
//!
//! The first tool result of a session may carry a one-time note asking the
//! user whether Mnemo may run without permission prompts (see `permissions`).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, Weak};
use std::time::{Duration, Instant};

use mnemo_app::mnemo2::mcp_server::lead_instructions;
use serde_json::{json, Value};

use crate::http::{self, Response};
use crate::instance::{
    canonical, find_instance, registered_root, resolve, root_body, Env, Instance, Resolution,
};
use crate::permissions::{self, Offer};

/// After stdin closes, how long in-flight requests may still finish.
const EOF_GRACE: Duration = Duration::from_secs(5);
/// A request that queues behind the instance lookup waits for the lookup's own
/// limit (`Env::spawn_timeout`) plus this.
const QUEUE_SLACK: Duration = Duration::from_secs(15);
/// JSON-RPC "server error" band, used for every bridge-side failure.
const SERVER_ERROR: i64 = -32000;
const METHOD_NOT_FOUND: i64 = -32601;
const PARSE_ERROR: i64 = -32700;
/// Tells Claude Code to fetch the tool list again.
const LIST_CHANGED: &str = r#"{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}"#;

#[derive(Clone)]
enum Target {
    Live(Instance),
    /// Not a registered project: answer the handshake, offer nothing.
    Stub,
}

/// The cached outcome of looking for the instance, shared by all requests.
#[derive(Default)]
struct Slot {
    target: Option<Target>,
    /// A thread is looking for (or starting) the instance right now.
    resolving: bool,
    /// Counts finished lookups, so queued requests can tell theirs ended.
    generation: u64,
    /// Why the last lookup failed, for the requests that queued behind it.
    last_error: Option<String>,
}

struct Bridge {
    env: Env,
    cwd: PathBuf,
    slot: Mutex<Slot>,
    /// Signalled when a lookup ends.
    resolved: Condvar,
    out: Mutex<Box<dyn Write + Send>>,
    in_flight: AtomicUsize,
    /// This process forwarded a `tools/call`, which is what attaches the
    /// external lead on the server. Only then may it detach it again.
    attached: AtomicBool,
    /// Identifies this helper process to the server: the `X-Mnemo-Bridge`
    /// header on every forwarded request and `bridge` in the detach body, so
    /// closing one helper cannot release a lead another helper still holds.
    id: String,
    /// The one-time "run Mnemo without prompts" note for the first tool result.
    offer: Offer,
    /// The cwd is in a registered project: answer `initialize` locally.
    local_initialize: bool,
    /// Claude Code holds an out-of-date tool list (an empty or failed
    /// `tools/list` while Mnemo2 was starting or down): announce the change
    /// once there is an instance.
    stale_list: AtomicBool,
    /// A thread is looking for an instance after a failed start.
    watching: AtomicBool,
}

/// How a bounded wait for the instance ended.
enum Ready {
    Yes,
    /// Still starting.
    Pending,
    /// The lookup ended without an instance, for this reason.
    Failed(String),
}

enum Forward {
    Reply(Response),
    /// Nothing was delivered (refused connection, rejected token): look the
    /// instance up again and resend.
    Retry(String),
    /// The connection broke mid-request: resending could run a tool twice.
    Fail(String),
}

/// Run the bridge until `input` ends. Returns the process exit code.
pub fn run<R: BufRead>(env: Env, input: R, out: Box<dyn Write + Send>) -> u8 {
    run_with(env, permissions::claude_dir_from_process(), input, out)
}

/// [`run`] with Claude Code's user config dir given (None: no permissions
/// offer), so tests never read the real one.
pub fn run_with<R: BufRead>(
    env: Env,
    claude_dir: Option<PathBuf>,
    mut input: R,
    out: Box<dyn Write + Send>,
) -> u8 {
    let bridge = std::sync::Arc::new(Bridge::new(env, claude_dir, out));
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match input.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(err) => {
                eprintln!("mnemo2-mcp: stdin failed: {err}");
                break;
            }
        }
        // A byte-order mark can lead the first line when a Windows host pipes text.
        let line = String::from_utf8_lossy(&buf)
            .trim_start_matches('\u{feff}')
            .trim()
            .to_string();
        if line.is_empty() {
            continue;
        }
        let parsed: Option<Value> = serde_json::from_str(&line).ok();
        let is_request = parsed.as_ref().is_some_and(|msg| request_id(msg).is_some());
        if is_request || parsed.is_none() {
            // Requests (tool calls can run for minutes) must not hold up the
            // notifications behind them, e.g. a cancellation.
            bridge.in_flight.fetch_add(1, Ordering::SeqCst);
            let worker = std::sync::Arc::clone(&bridge);
            std::thread::spawn(move || {
                worker.process(&line, parsed);
                worker.in_flight.fetch_sub(1, Ordering::SeqCst);
            });
        } else {
            bridge.process(&line, parsed);
        }
    }
    bridge.finish();
    0
}

/// The JSON-RPC id of a request; None for notifications and client responses.
fn request_id(msg: &Value) -> Option<Value> {
    msg.get("method")?;
    msg.get("id").filter(|id| !id.is_null()).cloned()
}

fn method_of(msg: &Value) -> &str {
    msg.get("method").and_then(Value::as_str).unwrap_or("")
}

fn rpc_error(id: &Value, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
    .to_string()
}

impl Bridge {
    fn new(env: Env, claude_dir: Option<PathBuf>, out: Box<dyn Write + Send>) -> Bridge {
        let cwd = canonical(&env.process_cwd);
        let offer = Offer::new(&env, claude_dir);
        // Only a registered project gets the local handshake; anywhere else the
        // normal lookup decides (and usually ends in the stub).
        let local_initialize = matches!(registered_root(&env, &cwd), Ok(Some(_)));
        Bridge {
            env,
            cwd,
            slot: Mutex::new(Slot::default()),
            resolved: Condvar::new(),
            out: Mutex::new(out),
            in_flight: AtomicUsize::new(0),
            attached: AtomicBool::new(false),
            id: uuid::Uuid::new_v4().simple().to_string(),
            offer,
            local_initialize,
            stale_list: AtomicBool::new(false),
            watching: AtomicBool::new(false),
        }
    }

    fn emit(&self, line: &str) {
        let mut out = self.out.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }

    fn lock_slot(&self) -> MutexGuard<'_, Slot> {
        self.slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// True once the instance lookup has succeeded.
    fn is_ready(&self) -> bool {
        self.lock_slot().target.is_some()
    }

    /// Handle one stdin line and write its answer, if it has one.
    fn process(self: &Arc<Self>, line: &str, parsed: Option<Value>) {
        let reply = match &parsed {
            None => Some(rpc_error(&Value::Null, PARSE_ERROR, "Parse error")),
            Some(msg) => self.answer(line, msg),
        };
        if let Some(reply) = reply {
            self.emit(&reply);
        }
        // An answer that left the tool list out of date is followed by the
        // announcement as soon as there is something to announce.
        self.settle();
        // The client has its handshake answer; now find (or start) Mnemo2 on
        // this worker thread, so the instance is up before the first real request.
        let is_initialize = parsed
            .as_ref()
            .is_some_and(|msg| method_of(msg) == "initialize" && request_id(msg).is_some());
        if self.local_initialize && is_initialize {
            let _ = self.target();
        }
    }

    fn answer(self: &Arc<Self>, line: &str, msg: &Value) -> Option<String> {
        let id = request_id(msg);
        let method = method_of(msg);
        if self.local_initialize {
            match (method, id.as_ref()) {
                ("initialize", Some(id)) => {
                    let helper = self.env.helper.to_string_lossy();
                    return Some(initialize_reply(msg, id, Some(&lead_instructions(&helper))));
                }
                // The server never needed it, and sending it would only queue
                // the reader behind a cold start.
                ("notifications/initialized", None) => return None,
                // Without an instance there is nobody to tell.
                (_, None) if !self.is_ready() => return None,
                // Answer in time even while Mnemo2 starts: Claude Code keeps
                // no tools for the session after a `tools/list` times out.
                ("tools/list", Some(id)) => match self.wait_ready(self.env.list_wait) {
                    Ready::Yes => {}
                    Ready::Pending => {
                        self.stale_list.store(true, Ordering::SeqCst);
                        let result = json!({ "tools": [] });
                        return Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string());
                    }
                    Ready::Failed(message) => {
                        self.stale_list.store(true, Ordering::SeqCst);
                        return Some(rpc_error(id, SERVER_ERROR, &message));
                    }
                },
                _ => {}
            }
        }
        let mut last_error = String::new();
        for _ in 0..2 {
            let target = match self.target() {
                Ok(target) => target,
                Err(message) => return id.map(|id| rpc_error(&id, SERVER_ERROR, &message)),
            };
            let instance = match target {
                Target::Stub => return stub_answer(msg, id.as_ref()),
                Target::Live(instance) => instance,
            };
            if method == "tools/call" {
                // The server attaches the external lead on a tool call. Set
                // before sending: even a failed send may have got through.
                self.attached.store(true, Ordering::SeqCst);
            }
            match self.forward(&instance, line) {
                Forward::Reply(response) => {
                    return match reply_from(id.as_ref(), response) {
                        Some(line) if method == "tools/call" => {
                            Some(self.offer.apply(line, Path::new(&instance.root)))
                        }
                        other => other,
                    };
                }
                Forward::Retry(reason) => {
                    self.invalidate(&instance);
                    last_error = reason;
                }
                Forward::Fail(reason) => {
                    self.invalidate(&instance);
                    last_error = reason;
                    break;
                }
            }
        }
        let message = format!(
            "Mnemo2 isn't reachable for this project: {last_error}. \
             Start Mnemo2 (or open the project in it) and try again."
        );
        id.map(|id| rpc_error(&id, SERVER_ERROR, &message))
    }

    /// The cached target, resolving it (and starting Mnemo2) on first use.
    ///
    /// One thread does the lookup; concurrent requests queue behind it (up to
    /// the lookup's own limit plus a slack) and share its outcome, a failure
    /// included, instead of each starting Mnemo2 again.
    fn target(self: &Arc<Self>) -> Result<Target, String> {
        let limit = self.env.spawn_timeout + QUEUE_SLACK;
        let deadline = Instant::now() + limit;
        let mut slot = self.lock_slot();
        let mut seen = slot.generation;
        loop {
            if let Some(target) = slot.target.as_ref() {
                return Ok(target.clone());
            }
            if slot.generation != seen {
                // A lookup ended while we queued, and left no target behind.
                if let Some(error) = slot.last_error.clone() {
                    return Err(error);
                }
                seen = slot.generation;
                continue;
            }
            if !slot.resolving {
                break;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!(
                    "Mnemo2 did not become ready within {} s. Open Mnemo2 from the tray and check the project.",
                    limit.as_secs()
                ));
            }
            slot = self
                .resolved
                .wait_timeout(slot, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
        slot.resolving = true;
        drop(slot);
        self.finish_resolve()
    }

    /// Run the lookup this thread claimed (`resolving` is set) and publish its
    /// outcome to every request waiting on it.
    fn finish_resolve(self: &Arc<Self>) -> Result<Target, String> {
        let resolution = resolve(&self.env, &self.cwd);

        let mut slot = self.lock_slot();
        slot.resolving = false;
        slot.generation += 1;
        let result = match resolution {
            Resolution::Live(instance) => Ok(Target::Live(instance)),
            Resolution::Unregistered => Ok(Target::Stub),
            Resolution::Failed(message) => Err(message),
        };
        match &result {
            Ok(target) => {
                slot.target = Some(target.clone());
                slot.last_error = None;
            }
            Err(message) => slot.last_error = Some(message.clone()),
        }
        drop(slot);
        self.resolved.notify_all();
        self.settle();
        result
    }

    /// Wait up to `limit` for the instance without blocking on a lookup of
    /// this thread's own: when none is running one starts in the background,
    /// and it carries on if the wait ends first.
    fn wait_ready(self: &Arc<Self>, limit: Duration) -> Ready {
        let deadline = Instant::now() + limit;
        let mut slot = self.lock_slot();
        let mut seen = slot.generation;
        loop {
            if slot.target.is_some() {
                return Ready::Yes;
            }
            if slot.generation != seen {
                if let Some(error) = slot.last_error.clone() {
                    return Ready::Failed(error);
                }
                seen = slot.generation;
                continue;
            }
            if !slot.resolving {
                slot.resolving = true;
                let bridge = Arc::clone(self);
                std::thread::spawn(move || {
                    let _ = bridge.finish_resolve();
                });
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ready::Pending;
            }
            slot = self
                .resolved
                .wait_timeout(slot, left)
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .0;
        }
    }

    /// Bring an out-of-date tool list in Claude Code up to date: announce the
    /// change once there is an instance or, after a failed start, keep
    /// looking for one (the user may start Mnemo2 or fix the project).
    fn settle(self: &Arc<Self>) {
        if !self.stale_list.load(Ordering::SeqCst) {
            return;
        }
        let slot = self.lock_slot();
        let (ready, idle) = (slot.target.is_some(), !slot.resolving);
        drop(slot);
        if ready {
            if self.stale_list.swap(false, Ordering::SeqCst) {
                self.emit(LIST_CHANGED);
            }
        } else if idle && !self.watching.swap(true, Ordering::SeqCst) {
            let bridge = Arc::downgrade(self);
            std::thread::spawn(move || watch(bridge));
        }
    }

    /// Use an instance found outside a lookup (by the watcher).
    fn adopt(&self, instance: Instance) {
        let mut slot = self.lock_slot();
        if slot.target.is_none() {
            slot.target = Some(Target::Live(instance));
            slot.last_error = None;
        }
        drop(slot);
        self.resolved.notify_all();
    }

    /// Forget `instance` if it is still the cached target.
    fn invalidate(&self, instance: &Instance) {
        let mut slot = self.lock_slot();
        if matches!(slot.target.as_ref(), Some(Target::Live(cached)) if cached.same_as(instance)) {
            slot.target = None;
        }
    }

    fn forward(&self, instance: &Instance, line: &str) -> Forward {
        let helper = self.env.helper.to_string_lossy().into_owned();
        let headers = [
            ("X-Mnemo-Root", instance.root.as_str()),
            ("X-Mnemo-Helper", helper.as_str()),
            ("X-Mnemo-Bridge", self.id.as_str()),
        ];
        // No read timeout: tool calls and the server's own waits can be long.
        match http::post(&instance.endpoint, "/mcp", &headers, line, None) {
            Ok(response) if response.status == 401 => {
                Forward::Retry("the run-file token was rejected".to_string())
            }
            Ok(response) => Forward::Reply(response),
            Err(err) if err.is_connect() => Forward::Retry(err.to_string()),
            Err(err) => Forward::Fail(err.to_string()),
        }
    }

    /// stdin closed: let quick requests finish, then release the external lead
    /// if this session is the one that led (it forwarded a tool call). A
    /// session that only listed tools must not drop another session's lead, and
    /// the server releases the lead only when no other leading helper remains:
    /// the detach names this helper's id, so a short-lived second helper cannot
    /// detach the lead the main session still holds.
    fn finish(&self) {
        let deadline = Instant::now() + EOF_GRACE;
        while self.in_flight.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        if !self.attached.load(Ordering::SeqCst) {
            return;
        }
        let target = self.lock_slot().target.clone();
        if let Some(Target::Live(instance)) = target {
            let mut body = root_body(&instance);
            body["bridge"] = json!(self.id);
            let _ = http::post(
                &instance.endpoint,
                "/lead/detach",
                &[],
                &body.to_string(),
                Some(Duration::from_secs(3)),
            );
        }
    }
}

/// After a failed start: look for an instance every `Env::wait_poll` until one
/// serves the project (then announce the tools) or the bridge is gone. Only
/// looks; starting Mnemo2 again is left to the next request.
fn watch(bridge: Weak<Bridge>) {
    loop {
        let Some(poll) = bridge.upgrade().map(|bridge| bridge.env.wait_poll) else {
            return;
        };
        std::thread::sleep(poll);
        let Some(bridge) = bridge.upgrade() else {
            return;
        };
        let ready = bridge.is_ready()
            || find_instance(&bridge.env, &bridge.cwd)
                .map(|instance| bridge.adopt(instance))
                .is_some();
        if ready {
            bridge.watching.store(false, Ordering::SeqCst);
            bridge.settle();
            return;
        }
    }
}

/// Turn Mnemo2's HTTP answer into the line to write, if any.
fn reply_from(id: Option<&Value>, response: Response) -> Option<String> {
    if response.status == 202 {
        return None;
    }
    let ok = (200..300).contains(&response.status);
    let body = response.body.trim();
    if body.is_empty() {
        if ok {
            return None;
        }
        return id.map(|id| {
            rpc_error(id, SERVER_ERROR, &format!("Mnemo2 answered HTTP {}", response.status))
        });
    }
    match serde_json::from_str::<Value>(body) {
        // Re-serialising keeps the reply on one line whatever the server sent.
        Ok(value) if ok || value.get("jsonrpc").is_some() => Some(value.to_string()),
        _ => {
            let excerpt: String = body.chars().take(200).collect();
            id.map(|id| {
                rpc_error(
                    id,
                    SERVER_ERROR,
                    &format!("Mnemo2 answered HTTP {}: {excerpt}", response.status),
                )
            })
        }
    }
}

/// The `initialize` answer: the client's protocol version echoed, tools only,
/// and the lead brief when there is one (the inert stub has none). The bridge
/// that gives the brief also announces tool-list changes; the stub never does.
fn initialize_reply(msg: &Value, id: &Value, instructions: Option<&str>) -> String {
    let tools = match instructions {
        Some(_) => json!({ "listChanged": true }),
        None => json!({}),
    };
    let mut result = json!({
        "protocolVersion": msg
            .pointer("/params/protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or("2025-06-18"),
        "capabilities": { "tools": tools },
        "serverInfo": { "name": "mnemo", "version": env!("CARGO_PKG_VERSION") },
    });
    if let Some(text) = instructions {
        result["instructions"] = json!(text);
    }
    json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string()
}

/// The inert server for directories outside every registered project.
fn stub_answer(msg: &Value, id: Option<&Value>) -> Option<String> {
    let id = id?;
    let result = match method_of(msg) {
        "initialize" => return Some(initialize_reply(msg, id, None)),
        "ping" => json!({}),
        "tools/list" => json!({ "tools": [] }),
        method => {
            return Some(rpc_error(
                id,
                METHOD_NOT_FOUND,
                &format!(
                    "Mnemo is inactive here: this folder is not inside a registered Mnemo2 project ({method} is unavailable)."
                ),
            ))
        }
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }).to_string())
}

#[cfg(test)]
#[path = "serve_tests.rs"]
mod tests;
