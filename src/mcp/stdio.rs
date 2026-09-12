// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! stdio transport — newline-delimited JSON-RPC over a spawned child
//! process (the classic MCP server shape: `npx`, `uvx`, a local binary).
//!
//! One reader task owns the child's stdout and routes each parsed
//! response to its pending request by JSON-RPC id (a `oneshot` per
//! request). Notifications from the server (`notifications/*`) carry no
//! id and are dropped — v1 has no server→client requests. The child is
//! spawned with `kill_on_drop`, so dropping the client (e.g. the manager
//! discarding a failed connection) terminates the server process.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};

use crate::config::mcp::McpServerDef;
use crate::error::{Error, Result};

use super::{
    extract_result, parse_call_result, parse_capabilities, parse_prompt_list, parse_resource_list,
    parse_text_content, parse_tool_list, rpc_request, CallToolOutcome, McpClient, McpPromptInfo,
    McpResourceInfo, McpToolInfo, ServerCapabilities, HANDSHAKE_TIMEOUT, REQUEST_TIMEOUT,
};

/// A live stdio connection to one MCP server process.
pub struct StdioMcpClient {
    /// The server process. Kept (and read on fatal paths to kill a hung
    /// server); dropped when the client drops — `kill_on_drop` then
    /// terminates the child.
    child: Child,
    /// The child's stdin, shared with the reader task so server→client
    /// REQUESTS (e.g. sampling/createMessage) can be answered — declined —
    /// without racing the client's own writes.
    stdin: Arc<Mutex<ChildStdin>>,
    /// Pending request completions, keyed by JSON-RPC id. Shared with the
    /// reader task.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>,
    next_id: u64,
}

impl StdioMcpClient {
    /// Spawn the server process for `def`. The child inherits this
    /// process's environment (which is how the configured env-var NAMES
    /// reach the server — values are never stored in config); a listed
    /// variable that is unset is logged as a diagnostic, not an error.
    /// stdout is JSON-RPC; stderr is discarded (servers are chatty).
    pub fn spawn(def: &McpServerDef) -> Result<Self> {
        let command = def.command.clone().unwrap_or_default();
        if command.trim().is_empty() {
            return Err(Error::Mcp(format!(
                "mcp server '{}' has an empty command",
                def.name
            )));
        }
        for name in &def.env {
            if std::env::var(name).is_err() {
                eprintln!(
                    "mcp: server '{}' expects env var '{name}' which is not set in this \
                     environment — the server may fail to authenticate",
                    def.name
                );
            }
        }
        let mut child = Command::new(&command)
            .args(&def.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                Error::Mcp(format!(
                    "failed to spawn mcp server '{}' ('{command}'): {e}",
                    def.name
                ))
            })?;
        let stdin = Arc::new(Mutex::new(
            child.stdin.take().expect("stdin was piped for this child"),
        ));
        let stdout = child
            .stdout
            .take()
            .expect("stdout was piped for this child");
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>> = Arc::default();
        let reader_pending = Arc::clone(&pending);
        let reader_stdin = Arc::clone(&stdin);
        // Reader task: route each response line to its pending request, and
        // DECLINE server→client requests (v1 implements none — a JSON-RPC
        // error beats silence, which would stall the server until its own
        // timeout). On EOF the pending map is cleared — every dropped
        // sender fails its awaiting request with "server closed".
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                match classify_incoming(&msg) {
                    Incoming::Response(id) => {
                        if let Some(sender) = reader_pending.lock().await.remove(&id) {
                            let _ = sender.send(msg);
                        }
                    }
                    Incoming::ServerRequest { id, method } => {
                        let out = serde_json::to_string(&decline_response(id, &method));
                        if let Ok(mut out) = out {
                            out.push('\n');
                            let _ = reader_stdin.lock().await.write_all(out.as_bytes()).await;
                        }
                    }
                    Incoming::Notification => {}
                }
            }
            reader_pending.lock().await.clear();
        });
        Ok(Self {
            child,
            stdin,
            pending,
            next_id: 0,
        })
    }

    /// Send one JSON-RPC request and await its response (bounded by
    /// `timeout`). A hung server is killed so it cannot linger.
    async fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: std::time::Duration,
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let line = serde_json::to_string(&rpc_request(id, method, params))?;
        if let Err(e) = self.write_line(&line).await {
            self.pending.lock().await.remove(&id);
            return Err(Error::Mcp(format!(
                "server stdin write failed ('{method}'): {e}"
            )));
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(resp)) => extract_result(&resp),
            Ok(Err(_)) => Err(Error::Mcp(
                "server closed the connection before answering".into(),
            )),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                let _ = self.child.start_kill();
                Err(Error::Mcp(format!(
                    "'{method}' timed out after {timeout:?} — server killed; the next call \
                     respawns it"
                )))
            }
        }
    }

    /// Send a notification (no id, no response).
    async fn notify(&mut self, method: &str) -> Result<()> {
        let note = json!({"jsonrpc": "2.0", "method": method});
        let line = serde_json::to_string(&note)?;
        self.write_line(&line)
            .await
            .map_err(|e| Error::Mcp(format!("server stdin write failed: {e}")))
    }

    async fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        // The stdin handle is shared with the reader task (which declines
        // server→client requests) — serialize writes through the mutex.
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await?;
        stdin.write_all(b"\n").await?;
        stdin.flush().await
    }
}

#[async_trait]
impl McpClient for StdioMcpClient {
    async fn initialize(&mut self) -> Result<ServerCapabilities> {
        let params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "mnemo", "version": env!("CARGO_PKG_VERSION")}
        });
        let result = self
            .request("initialize", params, HANDSHAKE_TIMEOUT)
            .await?;
        let capabilities = parse_capabilities(&result);
        self.notify("notifications/initialized").await?;
        Ok(capabilities)
    }

    async fn list_tools(&mut self) -> Result<Vec<McpToolInfo>> {
        let result = self
            .request("tools/list", json!({}), REQUEST_TIMEOUT)
            .await?;
        parse_tool_list(&result)
    }

    async fn call_tool(&mut self, name: &str, args: Value) -> Result<CallToolOutcome> {
        let params = json!({"name": name, "arguments": args});
        let result = self.request("tools/call", params, REQUEST_TIMEOUT).await?;
        parse_call_result(&result)
    }

    async fn list_prompts(&mut self) -> Result<Vec<McpPromptInfo>> {
        let result = self
            .request("prompts/list", json!({}), REQUEST_TIMEOUT)
            .await?;
        parse_prompt_list(&result)
    }

    async fn get_prompt(&mut self, name: &str, args: Value) -> Result<String> {
        let params = json!({"name": name, "arguments": args});
        let result = self.request("prompts/get", params, REQUEST_TIMEOUT).await?;
        parse_text_content(&result)
    }

    async fn list_resources(&mut self) -> Result<Vec<McpResourceInfo>> {
        let result = self
            .request("resources/list", json!({}), REQUEST_TIMEOUT)
            .await?;
        parse_resource_list(&result)
    }

    async fn read_resource(&mut self, uri: &str) -> Result<String> {
        let params = json!({"uri": uri});
        let result = self
            .request("resources/read", params, REQUEST_TIMEOUT)
            .await?;
        parse_text_content(&result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_routes_by_shape() {
        // A response (id + result/error) routes to its pending request.
        let response = json!({"jsonrpc": "2.0", "id": 5, "result": {}});
        assert!(matches!(
            classify_incoming(&response),
            Incoming::Response(5)
        ));
        let err = json!({"jsonrpc": "2.0", "id": 1, "error": {"code": -1, "message": "x"}});
        assert!(matches!(classify_incoming(&err), Incoming::Response(1)));

        // A server→client request (id + method, no result/error) is
        // classified for the decline path.
        let request = json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "sampling/createMessage",
            "params": {}
        });
        match classify_incoming(&request) {
            Incoming::ServerRequest { id, method } => {
                assert_eq!(id, 9);
                assert_eq!(method, "sampling/createMessage");
            }
            other => panic!("expected a server request, got {other:?}"),
        }

        // Notifications (method, no id) are ignored.
        let notification = json!({"jsonrpc": "2.0", "method": "notifications/progress"});
        assert!(matches!(
            classify_incoming(&notification),
            Incoming::Notification
        ));
    }

    #[test]
    fn decline_response_is_a_standard_method_not_found_error() {
        let d = decline_response(7, "sampling/createMessage");
        assert_eq!(d["id"], 7);
        assert_eq!(d["error"]["code"], -32601);
        assert!(d["error"]["message"]
            .as_str()
            .unwrap()
            .contains("sampling/createMessage"));
    }
}

/// An incoming line from the server, classified by shape: a RESPONSE has
/// an id + result/error (routes to its pending request); a server→client
/// REQUEST has an id + method (declined — v1 implements none); a
/// NOTIFICATION has a method but no id (ignored).
#[derive(Debug)]
enum Incoming {
    Response(u64),
    ServerRequest { id: u64, method: String },
    Notification,
}

fn classify_incoming(msg: &Value) -> Incoming {
    let has_result_or_error = msg.get("result").is_some() || msg.get("error").is_some();
    match (msg.get("id"), msg.get("method").and_then(Value::as_str)) {
        (Some(id), Some(method)) if !has_result_or_error => Incoming::ServerRequest {
            id: id.as_u64().unwrap_or(0),
            method: method.to_string(),
        },
        (Some(id), _) => Incoming::Response(id.as_u64().unwrap_or(0)),
        _ => Incoming::Notification,
    }
}

/// The JSON-RPC error response declining a server→client request
/// (`-32601`, the standard "method not found" — the only v1-supported
/// answer to e.g. sampling/createMessage, which would otherwise hang the
/// server until its own timeout).
fn decline_response(id: u64, method: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32601,
            "message": format!("method '{method}' is not supported by this client")
        }
    })
}
