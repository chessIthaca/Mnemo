// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! MCP (Model Context Protocol) client + connection manager.
//!
//! The app speaks exactly three JSON-RPC methods to a server —
//! `initialize`, `tools/list`, `tools/call` — over two transports:
//! stdio (a spawned child process, newline-delimited JSON-RPC on
//! stdin/stdout) and remote (streamable HTTP, JSON or SSE responses).
//! That surface is small enough that we implement it in-crate on the
//! existing `tokio` + `reqwest` dependencies rather than pulling in the
//! `rmcp` SDK (evaluated 2026-02-13: rejected to avoid its dependency
//! tree and version churn for three calls; revisit if we ever need
//! prompts/resources/sampling).
//!
//! [`McpManager`] is deliberately LAZY: constructing it from config
//! spawns nothing and connects nowhere. A connection (and the
//! `initialize` handshake + `tools/list` cache) happens on first use of
//! that server, and a failed connection is dropped so the next call
//! transparently re-establishes it. This mirrors the browser tools'
//! posture — configured-but-inert until the agent actually asks.
//!
//! Secrets never enter this module's config surface (see
//! [`crate::config::mcp`]): `env`/`headers_env` carry environment
//! variable NAMES, resolved at connect time.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::config::mcp::McpServerDef;
use crate::error::{Error, Result};

pub mod http;
pub mod oauth;
pub mod stdio;
pub mod tool;

pub use http::HttpMcpClient;
pub use oauth::{McpTokenStore, OAuthTokens};
pub use stdio::StdioMcpClient;
pub use tool::{mcp_tool_name, McpReveal, McpTool, MCP_TOOL_PREFIX};

/// How long the `initialize` handshake may take before the connect is
/// considered failed. Generous: stdio servers often boot a runtime (npx,
/// uvx) before answering.
pub const HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a single `tools/call` may take. Tool calls can legitimately be
/// slow (searches, builds); this only bounds obviously-hung servers.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// One tool advertised by an MCP server (from `tools/list`).
#[derive(Debug, Clone)]
pub struct McpToolInfo {
    /// The tool name, unique within its server. The agent-facing tool name
    /// is namespaced: `mcp__<server>__<tool>`.
    pub name: String,
    /// The human/model-readable description of what the tool does.
    pub description: String,
    /// The tool's JSON Schema for its `arguments` object (passed through
    /// to the agent as-is).
    pub input_schema: Value,
}

/// One prompt advertised by an MCP server (from `prompts/list`).
#[derive(Debug, Clone)]
pub struct McpPromptInfo {
    /// The prompt name (passed to `prompts/get`).
    pub name: String,
    /// The human/model-readable description.
    pub description: String,
}

/// One resource advertised by an MCP server (from `resources/list`).
#[derive(Debug, Clone)]
pub struct McpResourceInfo {
    /// The resource URI (passed to `resources/read`).
    pub uri: String,
    /// The human-readable name.
    pub name: String,
    /// The description (may be empty).
    pub description: String,
}

/// What the server advertised in its `initialize` result — gates which
/// per-server meta tools (`get_prompt`, `read_resource`) the reveal
/// registers.
#[derive(Debug, Clone, Copy, Default)]
pub struct ServerCapabilities {
    /// The server supports `prompts/list` + `prompts/get`.
    pub prompts: bool,
    /// The server supports `resources/list` + `resources/read`.
    pub resources: bool,
}

/// Parse the `capabilities` object out of an `initialize` result. Only
/// the two booleans the client actually uses are read; everything else is
/// ignored. Pure — unit-tested.
pub(crate) fn parse_capabilities(result: &Value) -> ServerCapabilities {
    let caps = result.get("capabilities");
    ServerCapabilities {
        prompts: caps
            .and_then(|c| c.get("prompts"))
            .is_some_and(|v| !v.is_null()),
        resources: caps
            .and_then(|c| c.get("resources"))
            .is_some_and(|v| !v.is_null()),
    }
}

/// The outcome of a `tools/call`: the concatenated text content plus the
/// protocol's `isError` flag (a server-side error still arrives as a
/// normal response — the tool ran but reported failure).
#[derive(Debug, Clone)]
pub struct CallToolOutcome {
    /// All `type: "text"` content blocks joined with newlines. Non-text
    /// content (images, resources) becomes a `[non-text content]` marker —
    /// v1 has no channel to surface binary payloads.
    pub text: String,
    /// Whether the server flagged the result as an error.
    pub is_error: bool,
}

/// Build a JSON-RPC 2.0 request `Value` for `method` with `params`.
pub(crate) fn rpc_request(id: u64, method: &str, params: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": method,
        "params": params,
    })
}

/// Extract the `result` from a JSON-RPC response, mapping the protocol's
/// `error` object onto [`Error::Mcp`].
pub(crate) fn extract_result(resp: &Value) -> Result<Value> {
    if let Some(err) = resp.get("error") {
        let code = err.get("code").and_then(Value::as_i64).unwrap_or(0);
        let message = err
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(Error::Mcp(format!("server error {code}: {message}")));
    }
    resp.get("result")
        .cloned()
        .ok_or_else(|| Error::Mcp("response carries neither result nor error".into()))
}

/// Parse the `tools/list` result into [`McpToolInfo`]s.
pub(crate) fn parse_tool_list(result: &Value) -> Result<Vec<McpToolInfo>> {
    let tools = result
        .get("tools")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Mcp("tools/list result has no tools array".into()))?;
    Ok(tools
        .iter()
        .filter_map(|t| {
            let name = t.get("name")?.as_str()?.to_string();
            if name.is_empty() {
                return None;
            }
            Some(McpToolInfo {
                name,
                description: t
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                input_schema: t
                    .get("inputSchema")
                    .cloned()
                    .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
            })
        })
        .collect())
}

/// Concatenate the text content blocks of a result (shared shape across
/// tools/call, prompts/get, and resources/read). Non-text content
/// becomes a `[non-text content]` marker.
pub(crate) fn join_text_content(result: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(content) = result.get("content").and_then(Value::as_array) {
        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => parts.push(
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                ),
                _ => parts.push("[non-text content]".to_string()),
            }
        }
    }
    parts.join("\n")
}

/// Parse a `tools/call` result into a [`CallToolOutcome`].
pub(crate) fn parse_call_result(result: &Value) -> Result<CallToolOutcome> {
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(CallToolOutcome {
        text: join_text_content(result),
        is_error,
    })
}

/// Parse a `prompts/list` result into [`McpPromptInfo`]s. Missing array →
/// empty list (a server may advertise prompts with no entries).
pub(crate) fn parse_prompt_list(result: &Value) -> Result<Vec<McpPromptInfo>> {
    Ok(result
        .get("prompts")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    let name = p.get("name")?.as_str()?.to_string();
                    if name.is_empty() {
                        return None;
                    }
                    Some(McpPromptInfo {
                        name,
                        description: p
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Parse a `resources/list` result into [`McpResourceInfo`]s.
pub(crate) fn parse_resource_list(result: &Value) -> Result<Vec<McpResourceInfo>> {
    Ok(result
        .get("resources")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let uri = r.get("uri")?.as_str()?.to_string();
                    if uri.is_empty() {
                        return None;
                    }
                    Some(McpResourceInfo {
                        uri,
                        name: r
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        description: r
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Parse a prompts/get or resources/read result: the concatenated text
/// content (same block shape as tools/call).
pub(crate) fn parse_text_content(result: &Value) -> Result<String> {
    Ok(join_text_content(result))
}

/// A live connection to one MCP server. Implemented per transport
/// ([`StdioMcpClient`], [`HttpMcpClient`]); faked in tests.
#[async_trait]
pub trait McpClient: Send + Sync {
    /// Run the `initialize` handshake (request + `notifications/initialized`).
    /// Returns the capabilities the server advertised. Called exactly once
    /// per connection by the manager.
    async fn initialize(&mut self) -> Result<ServerCapabilities>;

    /// List the server's tools. The manager caches the result per
    /// connection.
    async fn list_tools(&mut self) -> Result<Vec<McpToolInfo>>;

    /// Invoke one tool. `args` is the arguments object (already validated
    /// against the tool's schema by the model, not by us).
    async fn call_tool(&mut self, name: &str, args: Value) -> Result<CallToolOutcome>;

    /// List the server's prompts (`prompts/list`). Servers that did not
    /// advertise the capability return a server-side error — the reveal
    /// gates on [`ServerCapabilities::prompts`] so this is not hit in
    /// practice.
    async fn list_prompts(&mut self) -> Result<Vec<McpPromptInfo>>;

    /// Render one prompt (`prompts/get`), returning the concatenated text
    /// content.
    async fn get_prompt(&mut self, name: &str, args: Value) -> Result<String>;

    /// List the server's resources (`resources/list`).
    async fn list_resources(&mut self) -> Result<Vec<McpResourceInfo>>;

    /// Read one resource (`resources/read`), returning the concatenated
    /// text content.
    async fn read_resource(&mut self, uri: &str) -> Result<String>;
}

/// Constructs the transport client for a server definition. A type alias
/// so tests (and only tests, in practice) can inject a fake.
pub type ClientFactory = Arc<dyn Fn(&McpServerDef) -> Result<Box<dyn McpClient>> + Send + Sync>;

/// The real transport factory: stdio for `command` servers, streamable
/// HTTP for `url` servers. Remote servers whose def carries
/// `auth = "oauth"` get a client wired to the shared token store
/// (Bearer injection + 401 refresh).
fn default_factory(tokens: Option<Arc<oauth::McpTokenStore>>) -> ClientFactory {
    Arc::new(move |def: &McpServerDef| {
        if def.is_stdio() {
            StdioMcpClient::spawn(def).map(|c| Box::new(c) as Box<dyn McpClient>)
        } else {
            HttpMcpClient::new(def, tokens.clone()).map(|c| Box::new(c) as Box<dyn McpClient>)
        }
    })
}

/// One established connection: the transport client plus its cached
/// tool list and the capabilities the server advertised.
struct ServerConn {
    client: Box<dyn McpClient>,
    tools: Vec<McpToolInfo>,
    capabilities: ServerCapabilities,
    /// Last use — the lazy idle-timeout clock (any traffic through the
    /// manager refreshes it).
    last_used: std::time::Instant,
}

/// Live per-server status for the Settings MCP page: whether the manager
/// currently holds a connection, how many tools were listed, the last
/// call failure (cleared on the next success), and how many times the
/// connection was re-established after a failure / idle expiry / crash.
#[derive(Debug, Clone, Default)]
pub struct ServerStatus {
    /// The manager currently holds a live connection.
    pub connected: bool,
    /// How many tools the server advertised (0 before first connect).
    pub tool_count: usize,
    /// The last call failure message (None after a success / fresh connect).
    pub last_error: Option<String>,
    /// How many times the connection was re-established.
    pub restarts: u64,
    /// Whether a successful connect EVER happened. Private (not part of
    /// the wire surface): distinguishes a re-establishment (the server
    /// WAS up and died) from a first-ever connect that merely failed once
    /// — the first success after failed attempts is NOT a restart, so the
    /// agent-window "restarted after a crash" note never fires for a
    /// server that never ran.
    ever_connected: bool,
}

/// Registry of configured MCP servers with LAZY per-server connections.
///
/// Constructing the manager is free (no processes, no sockets). The first
/// [`Self::server_tools`] / [`Self::call`] for a server connects,
/// handshakes, and caches its tools. A connection that errors during a
/// call is dropped, so the next call transparently re-establishes it
/// (respawning a crashed stdio child). Calls are serialized per manager
/// (one `Mutex` over the connection map) — the agent loop awaits tool
/// calls one at a time, so this costs nothing and keeps the clients'
/// `&mut self` methods honest.
pub struct McpManager {
    /// The configured servers — interior-mutable so a Settings save can
    /// replace the set live (see [`Self::replace_servers`]).
    servers: std::sync::RwLock<Vec<McpServerDef>>,
    factory: ClientFactory,
    connections: tokio::sync::Mutex<HashMap<String, ServerConn>>,
    /// Live per-server status for the Settings page.
    status: tokio::sync::Mutex<HashMap<String, ServerStatus>>,
}

impl McpManager {
    /// Build a manager from the parsed `mcp.toml` entries. Inert until a
    /// server is first used. OAuth-capable remote servers get a token
    /// store over the global `keys.toml`.
    pub fn from_config(servers: &[McpServerDef]) -> Self {
        let tokens = Arc::new(oauth::McpTokenStore::new(
            crate::config::global_config_dir().join("keys.toml"),
        ));
        Self::with_factory(servers, default_factory(Some(tokens)))
    }

    /// Build with an explicit client factory — the test seam (inject a
    /// fake transport; count spawns). No token store: tests don't OAuth.
    pub fn with_factory(servers: &[McpServerDef], factory: ClientFactory) -> Self {
        Self {
            servers: std::sync::RwLock::new(servers.to_vec()),
            factory,
            connections: tokio::sync::Mutex::new(HashMap::new()),
            status: tokio::sync::Mutex::new(HashMap::new()),
        }
    }

    /// The enabled server definitions (deferred-group construction reads
    /// this; disabled servers are invisible to the agent). Cloned — the
    /// list is interior-mutable for live Settings saves.
    pub fn enabled(&self) -> Vec<McpServerDef> {
        self.servers
            .read()
            .expect("mcp server list lock poisoned")
            .iter()
            .filter(|s| s.enabled)
            .cloned()
            .collect()
    }

    /// Every configured server definition, enabled and disabled (cloned).
    /// The factory reads this to build the deferred-group table, which
    /// itself filters by `enabled`.
    pub fn servers(&self) -> Vec<McpServerDef> {
        self.servers
            .read()
            .expect("mcp server list lock poisoned")
            .clone()
    }

    /// The server definition for `name` (enabled or disabled), or `None`.
    /// The reveal reads the `trusted` flag from here to stamp adapters.
    pub fn server_def(&self, name: &str) -> Option<McpServerDef> {
        self.servers
            .read()
            .expect("mcp server list lock poisoned")
            .iter()
            .find(|s| s.name == name)
            .cloned()
    }

    /// Replace the configured servers (the Settings save path) and drop
    /// every live connection, so the next call to any server reconnects
    /// under the new definitions — always correct at Settings-save pacing.
    /// NOTE: already-built agents keep the deferred-group table they were
    /// built with; agents built afterwards (subagents, or after a restart
    /// / project switch) see the new set.
    pub async fn replace_servers(&self, servers: Vec<McpServerDef>) {
        *self.servers.write().expect("mcp server list lock poisoned") = servers;
        self.connections.lock().await.clear();
        // A save invalidates every connection the status map describes —
        // clear it too (review LOW 1): the Settings page must never render
        // "connected" for a connection the manager no longer holds, and a
        // removed server's stale row must not resurface if the name is
        // re-added later. Lock order: status alone (connections already
        // released by the expression above), so no inversion.
        self.status.lock().await.clear();
    }

    /// The server definition for `name`, when configured AND enabled.
    fn def(&self, name: &str) -> Result<McpServerDef> {
        let guard = self.servers.read().expect("mcp server list lock poisoned");
        match guard.iter().find(|s| s.name == name) {
            None => {
                let known: Vec<&str> = guard.iter().map(|s| s.name.as_str()).collect();
                Err(Error::Mcp(format!(
                    "unknown mcp server '{name}' — configured: [{}]",
                    known.join(", ")
                )))
            }
            Some(s) if !s.enabled => Err(Error::Mcp(format!(
                "mcp server '{name}' is disabled — enable it in Settings"
            ))),
            Some(s) => Ok(s.clone()),
        }
    }

    /// Connect to `name` if not already connected: factory → `initialize`
    /// handshake → `tools/list` cache. Returns the tool list clone.
    ///
    /// LAZY IDLE EXPIRY: when the def carries `idle_timeout_secs`, a cached
    /// connection whose `last_used` is older than the timeout is dropped
    /// here and re-established (no background task — the next use pays the
    /// reconnect, counted as a restart for status/notes).
    async fn ensure(&self, name: &str) -> Result<Vec<McpToolInfo>> {
        // Validate outside the lock (config lookups are cheap + lock-free).
        let def = self.def(name)?;
        let mut conns = self.connections.lock().await;
        if let Some(conn) = conns.get_mut(name) {
            let expired = def
                .idle_timeout_secs
                .map(|secs| conn.last_used.elapsed().as_secs() >= secs)
                .unwrap_or(false);
            if expired {
                conns.remove(name);
                // The re-establishment below counts the restart.
                let mut status = self.status.lock().await;
                status.entry(name.to_string()).or_default().connected = false;
            } else {
                conn.last_used = std::time::Instant::now();
                return Ok(conn.tools.clone());
            }
        }
        // Connect: factory spawn → `initialize` handshake → `tools/list`.
        // A failure here records the status honestly (not connected + the
        // failure text) so the Settings card's "error: …" line can render
        // for a DOWN server — not just for failed calls (review LOW 2).
        // The error text is audited: transport errors carry server name /
        // command / URL + status / server-controlled messages only, never
        // env values, headers, or args (see the reveal error path too).
        let connected = async {
            let mut client = (self.factory)(&def)?;
            let capabilities = client.initialize().await?;
            let tools = client.list_tools().await?;
            Ok::<_, Error>((client, capabilities, tools))
        }
        .await;
        let (client, capabilities, tools) = match connected {
            Ok(v) => v,
            Err(e) => {
                let mut status = self.status.lock().await;
                let entry = status.entry(name.to_string()).or_default();
                entry.connected = false;
                entry.last_error = Some(e.to_string());
                drop(status);
                return Err(e);
            }
        };
        let now = std::time::Instant::now();
        conns.insert(
            name.to_string(),
            ServerConn {
                client,
                tools: tools.clone(),
                capabilities,
                last_used: now,
            },
        );
        let mut status = self.status.lock().await;
        // A connect after a drop (failure / idle expiry / crash) is a
        // RESTART — counted here so the reconnecting call flags the
        // agent-window note. The FIRST-ever connect is not a restart,
        // even when earlier connect attempts failed (ever_connected
        // keeps that distinction — a server that never ran cannot
        // "restart").
        let entry = status.entry(name.to_string()).or_default();
        let reestablished = entry.ever_connected && !entry.connected;
        entry.connected = true;
        entry.ever_connected = true;
        entry.tool_count = tools.len();
        entry.last_error = None;
        if reestablished {
            entry.restarts += 1;
        }
        Ok(tools)
    }

    /// The capabilities the server advertised (connecting on first use).
    pub async fn capabilities(&self, name: &str) -> Result<ServerCapabilities> {
        self.ensure(name).await?;
        let conns = self.connections.lock().await;
        Ok(conns.get(name).map(|c| c.capabilities).unwrap_or_default())
    }

    /// List the server's prompts (`prompts/list`). Meta listings surface a
    /// clear retry error when the connection vanished in the ensure/call
    /// gap (same race `call` retries — a listing is cheap to retry).
    pub async fn list_prompts(&self, name: &str) -> Result<Vec<McpPromptInfo>> {
        self.ensure(name).await?;
        let mut conns = self.connections.lock().await;
        let Some(conn) = conns.get_mut(name) else {
            return Err(Error::Mcp(format!(
                "mcp server '{name}' connection vanished — retry"
            )));
        };
        conn.client.list_prompts().await
    }

    /// Render one prompt (`prompts/get`), returning its text content.
    pub async fn get_prompt(&self, name: &str, prompt: &str, args: Value) -> Result<String> {
        self.ensure(name).await?;
        let mut conns = self.connections.lock().await;
        let Some(conn) = conns.get_mut(name) else {
            return Err(Error::Mcp(format!(
                "mcp server '{name}' connection vanished — retry"
            )));
        };
        conn.client.get_prompt(prompt, args).await
    }

    /// List the server's resources (`resources/list`).
    pub async fn list_resources(&self, name: &str) -> Result<Vec<McpResourceInfo>> {
        self.ensure(name).await?;
        let mut conns = self.connections.lock().await;
        let Some(conn) = conns.get_mut(name) else {
            return Err(Error::Mcp(format!(
                "mcp server '{name}' connection vanished — retry"
            )));
        };
        conn.client.list_resources().await
    }

    /// Read one resource (`resources/read`), returning its text content.
    pub async fn read_resource(&self, name: &str, uri: &str) -> Result<String> {
        self.ensure(name).await?;
        let mut conns = self.connections.lock().await;
        let Some(conn) = conns.get_mut(name) else {
            return Err(Error::Mcp(format!(
                "mcp server '{name}' connection vanished — retry"
            )));
        };
        conn.client.read_resource(uri).await
    }

    /// The server's advertised tools (connecting on first use).
    pub async fn server_tools(&self, name: &str) -> Result<Vec<McpToolInfo>> {
        self.ensure(name).await
    }

    /// Invoke `tool` on server `name`. On failure the connection is
    /// dropped — the NEXT call re-establishes it (crash → transparent
    /// respawn) while THIS call surfaces the error to the caller.
    pub async fn call(&self, name: &str, tool: &str, args: Value) -> Result<CallToolOutcome> {
        Ok(self.call_with_info(name, tool, args).await?.0)
    }

    /// Like [`Self::call`] but reports whether THIS call had to
    /// re-establish the connection (a crash, an idle expiry, or a failed
    /// previous call) — the tool adapter prefixes a "server restarted
    /// after a crash" note to its output (the agent-window surface).
    ///
    /// A connection can vanish between [`Self::ensure`] releasing the lock
    /// and this method re-acquiring it (another task's failed call removed
    /// it, or a Settings save ran [`Self::replace_servers`]) — the gap is
    /// re-ensured instead of panicking (review LOW 1: the manager is
    /// shared across agents, so calls genuinely interleave).
    pub async fn call_with_info(
        &self,
        name: &str,
        tool: &str,
        args: Value,
    ) -> Result<(CallToolOutcome, bool)> {
        // Snapshot the restart counter: any re-establish during THIS call
        // (vanish-retry or failure-then-respawn) flags reconnected for the
        // agent-window note.
        let restarts_before = {
            let status = self.status.lock().await;
            status.get(name).map(|s| s.restarts).unwrap_or(0)
        };
        for _ in 0..2 {
            self.ensure(name).await?;
            let mut conns = self.connections.lock().await;
            let Some(conn) = conns.get_mut(name) else {
                // Removed in the gap — one retry re-establishes it; the
                // reconnect (below) counts the restart.
                let mut status = self.status.lock().await;
                status.entry(name.to_string()).or_default().connected = false;
                drop(status);
                continue;
            };
            conn.last_used = std::time::Instant::now();
            match conn.client.call_tool(tool, args).await {
                Ok(outcome) => {
                    let restarts_now = {
                        let status = self.status.lock().await;
                        status.get(name).map(|s| s.restarts).unwrap_or(0)
                    };
                    return Ok((outcome, restarts_now > restarts_before));
                }
                Err(e) => {
                    conns.remove(name);
                    let mut status = self.status.lock().await;
                    let entry = status.entry(name.to_string()).or_default();
                    entry.connected = false;
                    entry.last_error = Some(e.to_string());
                    drop(status);
                    return Err(e);
                }
            }
        }
        Err(Error::Mcp(format!(
            "mcp server '{name}' connection kept vanishing before the call — another agent's \
             failed call or a Settings save is racing it; retry in a moment"
        )))
    }

    /// Live per-server status for the Settings page (sorted by name).
    /// Absent entries = never connected this session.
    pub async fn statuses(&self) -> Vec<(String, ServerStatus)> {
        let status = self.status.lock().await;
        let mut out: Vec<(String, ServerStatus)> =
            status.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        drop(status);
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A scripted fake transport: calls fail while the shared flag is set,
    /// so tests can flip behavior between calls. Spawn counting goes
    /// through the factory closure the manager was built with.
    struct FakeClient {
        fail_calls: Arc<AtomicBool>,
        /// When set, `initialize` fails — a server that is DOWN at connect
        /// time (distinct from a call that fails later).
        fail_connect: Arc<AtomicBool>,
    }

    #[async_trait]
    impl McpClient for FakeClient {
        async fn initialize(&mut self) -> Result<ServerCapabilities> {
            if self.fail_connect.load(Ordering::SeqCst) {
                return Err(Error::Mcp("connect boom (scripted)".into()));
            }
            Ok(ServerCapabilities::default())
        }

        async fn list_tools(&mut self) -> Result<Vec<McpToolInfo>> {
            Ok(vec![McpToolInfo {
                name: "echo".into(),
                description: "echoes its arguments".into(),
                input_schema: json!({"type": "object"}),
            }])
        }

        async fn call_tool(&mut self, name: &str, args: Value) -> Result<CallToolOutcome> {
            if self.fail_calls.load(Ordering::SeqCst) {
                return Err(Error::Mcp("boom (scripted)".into()));
            }
            Ok(CallToolOutcome {
                text: format!("{name}: {args}"),
                is_error: false,
            })
        }

        async fn list_prompts(&mut self) -> Result<Vec<McpPromptInfo>> {
            Ok(Vec::new())
        }

        async fn get_prompt(&mut self, _name: &str, _args: Value) -> Result<String> {
            Ok("prompt text".into())
        }

        async fn list_resources(&mut self) -> Result<Vec<McpResourceInfo>> {
            Ok(Vec::new())
        }

        async fn read_resource(&mut self, _uri: &str) -> Result<String> {
            Ok("resource text".into())
        }
    }

    fn server(name: &str, enabled: bool) -> McpServerDef {
        McpServerDef {
            name: name.into(),
            enabled,
            command: Some("fake".into()),
            args: Vec::new(),
            env: Vec::new(),
            url: None,
            headers_env: Default::default(),
            auth: None,
            client_id: None,
            scopes: Vec::new(),
            trusted: false,
            idle_timeout_secs: None,
        }
    }

    /// Manager + spawn counter over a fake factory.
    fn fake_manager(
        defs: &[McpServerDef],
        fail: Arc<AtomicBool>,
    ) -> (McpManager, Arc<AtomicUsize>) {
        let spawns = Arc::new(AtomicUsize::new(0));
        let factory: ClientFactory = {
            let spawns = Arc::clone(&spawns);
            let fail = Arc::clone(&fail);
            Arc::new(move |_def: &McpServerDef| {
                spawns.fetch_add(1, Ordering::SeqCst);
                let fail = Arc::clone(&fail);
                Ok(Box::new(FakeClient {
                    fail_calls: fail,
                    fail_connect: Arc::new(AtomicBool::new(false)),
                }) as Box<dyn McpClient>)
            })
        };
        (McpManager::with_factory(defs, factory), spawns)
    }

    /// Manager whose spawned clients FAIL the `initialize` handshake while
    /// the shared flag is set — a server that is DOWN at connect time.
    fn failing_connect_manager(defs: &[McpServerDef]) -> (McpManager, Arc<AtomicBool>) {
        let fail_connect = Arc::new(AtomicBool::new(false));
        let factory: ClientFactory = {
            let fail_connect = Arc::clone(&fail_connect);
            Arc::new(move |_def: &McpServerDef| {
                let fail_connect = Arc::clone(&fail_connect);
                Ok(Box::new(FakeClient {
                    fail_calls: Arc::new(AtomicBool::new(false)),
                    fail_connect,
                }) as Box<dyn McpClient>)
            })
        };
        (McpManager::with_factory(defs, factory), fail_connect)
    }

    #[tokio::test]
    async fn connects_lazily_once_and_caches_tools() {
        let fail = Arc::new(AtomicBool::new(false));
        let (mgr, spawns) = fake_manager(&[server("fs", true)], fail);
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            0,
            "constructing the manager spawns nothing"
        );
        let tools = mgr.server_tools("fs").await.unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "echo");
        assert_eq!(spawns.load(Ordering::SeqCst), 1, "first use connects");
        let again = mgr.server_tools("fs").await.unwrap();
        assert_eq!(again.len(), 1);
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            1,
            "tools are cached — no respawn"
        );
    }

    #[tokio::test]
    async fn call_routes_through_and_a_failed_connection_respawns() {
        let fail = Arc::new(AtomicBool::new(true));
        let (mgr, spawns) = fake_manager(&[server("fs", true)], Arc::clone(&fail));
        let err = mgr.call("fs", "echo", json!({"x": 1})).await.unwrap_err();
        assert!(err.to_string().contains("boom"), "{err}");
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        // Flip the script: the failed connection was dropped, so the next
        // call transparently reconnects and succeeds.
        fail.store(false, Ordering::SeqCst);
        let out = mgr.call("fs", "echo", json!({"x": 1})).await.unwrap();
        assert_eq!(out.text, "echo: {\"x\":1}");
        assert_eq!(spawns.load(Ordering::SeqCst), 2, "respawn after failure");
    }

    #[tokio::test]
    async fn idle_timeout_expires_stale_connections_lazily() {
        // A def with an idle timeout: a cached connection idle beyond the
        // timeout is dropped and re-established on the next use (restart
        // counted) — no background task.
        let mut def = server("fs", true);
        def.idle_timeout_secs = Some(1);
        let fail = Arc::new(AtomicBool::new(false));
        let (mgr, spawns) = fake_manager(&[def], fail);

        mgr.server_tools("fs").await.unwrap();
        assert_eq!(spawns.load(Ordering::SeqCst), 1);
        // Fresh: a second use reuses the connection.
        mgr.server_tools("fs").await.unwrap();
        assert_eq!(spawns.load(Ordering::SeqCst), 1, "reuse while fresh");
        // Past the timeout: the next use re-establishes (counted restart).
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
        mgr.server_tools("fs").await.unwrap();
        assert_eq!(
            spawns.load(Ordering::SeqCst),
            2,
            "expired connection re-established"
        );
        let statuses = mgr.statuses().await;
        let (_, s) = statuses.iter().find(|(n, _)| n == "fs").unwrap();
        assert!(s.connected);
        assert_eq!(s.tool_count, 1);
        assert_eq!(s.restarts, 1, "the expiry counts as a restart");
    }

    #[tokio::test]
    async fn status_tracks_connections_failures_and_restarts() {
        let fail = Arc::new(AtomicBool::new(false));
        let (mgr, _spawns) = fake_manager(&[server("fs", true)], Arc::clone(&fail));
        // Never connected → no status entry yet.
        assert!(mgr.statuses().await.is_empty());

        mgr.server_tools("fs").await.unwrap();
        let (_, s) = mgr
            .statuses()
            .await
            .into_iter()
            .find(|(n, _)| n == "fs")
            .unwrap();
        assert!(s.connected);
        assert_eq!(s.tool_count, 1);
        assert_eq!(s.last_error, None);

        // A failed call records the error and drops the connection.
        fail.store(true, Ordering::SeqCst);
        let err = mgr.call("fs", "echo", json!({})).await.unwrap_err();
        assert!(err.to_string().contains("boom"));
        let (_, s) = mgr
            .statuses()
            .await
            .into_iter()
            .find(|(n, _)| n == "fs")
            .unwrap();
        assert!(!s.connected);
        assert!(s.last_error.as_deref().unwrap_or("").contains("boom"));
        assert_eq!(s.restarts, 0, "the failure itself does not count yet");

        // The NEXT call reconnects and flags the respawn.
        fail.store(false, Ordering::SeqCst);
        let (outcome, reconnected) = mgr.call_with_info("fs", "echo", json!({})).await.unwrap();
        assert_eq!(outcome.text, "echo: {}");
        assert!(reconnected, "the reconnecting call flags the restart");
        let (_, s) = mgr
            .statuses()
            .await
            .into_iter()
            .find(|(n, _)| n == "fs")
            .unwrap();
        assert!(s.connected);
        assert_eq!(s.last_error, None, "success clears the error");
        assert_eq!(s.restarts, 1);
    }

    #[tokio::test]
    async fn failed_connect_records_last_error_for_the_status_line() {
        // A server that is DOWN at connect time (the initialize handshake
        // fails): the status entry records the failure so the Settings
        // card can render its "error: …" line — before this regression,
        // connect failures never touched the status map, so the line was
        // only reachable for CALL failures (review LOW 2). A later success
        // clears the error; a failed connect is NOT a restart.
        let (mgr, fail_connect) = failing_connect_manager(&[server("fs", true)]);
        fail_connect.store(true, Ordering::SeqCst);
        let err = mgr.server_tools("fs").await.unwrap_err();
        assert!(err.to_string().contains("connect boom"), "{err}");
        let (_, s) = mgr
            .statuses()
            .await
            .into_iter()
            .find(|(n, _)| n == "fs")
            .unwrap();
        assert!(!s.connected);
        assert!(
            s.last_error
                .as_deref()
                .unwrap_or("")
                .contains("connect boom"),
            "the connect failure text reaches the status line"
        );
        assert_eq!(s.restarts, 0, "a failed connect is not a restart");

        fail_connect.store(false, Ordering::SeqCst);
        mgr.server_tools("fs").await.unwrap();
        let (_, s) = mgr
            .statuses()
            .await
            .into_iter()
            .find(|(n, _)| n == "fs")
            .unwrap();
        assert!(s.connected);
        assert_eq!(s.last_error, None, "the success clears the error");
        assert_eq!(s.restarts, 0);
    }

    #[tokio::test]
    async fn replace_servers_invalidates_the_status_map() {
        // A Settings save drops every connection AND every status row:
        // "connected" must never render for a connection the manager no
        // longer holds, and a re-added name must not resurface stale
        // tool_count / last_error rows (review LOW 1).
        let fail = Arc::new(AtomicBool::new(false));
        let (mgr, _spawns) = fake_manager(&[server("fs", true), server("db", true)], fail);
        mgr.server_tools("fs").await.unwrap();
        assert!(mgr
            .statuses()
            .await
            .iter()
            .any(|(n, s)| n == "fs" && s.connected));

        // Save a new set: the old fs row described a connection that no
        // longer exists — gone.
        mgr.replace_servers(vec![server("fs", true)]).await;
        assert!(mgr.statuses().await.is_empty(), "a save invalidates status");

        // A fresh connect under the new def starts clean — the save is
        // NOT counted as a restart.
        mgr.server_tools("fs").await.unwrap();
        let (_, s) = mgr
            .statuses()
            .await
            .into_iter()
            .find(|(n, _)| n == "fs")
            .unwrap();
        assert!(s.connected);
        assert_eq!(s.restarts, 0, "save-induced reconnect is not a restart");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_calls_survive_connection_removal() {
        // LOW 1 regression: a failed call removes the connection while a
        // racing call (another agent shares this manager) may sit between
        // ensure() and its lock acquisition — the old get_mut().expect()
        // panicked there. Interleave failures and successes; any panic
        // propagates out of the spawned tasks, and a clean call must
        // succeed once the dust settles.
        let fail = Arc::new(AtomicBool::new(false));
        let (mgr, _spawns) = fake_manager(&[server("fs", true)], Arc::clone(&fail));
        let mgr = Arc::new(mgr);
        let mut handles = Vec::new();
        for i in 0..24 {
            let mgr = Arc::clone(&mgr);
            let fail = Arc::clone(&fail);
            handles.push(tokio::spawn(async move {
                if i % 3 == 0 {
                    fail.store(true, Ordering::SeqCst);
                }
                let _ = mgr.call("fs", "echo", json!({})).await;
                if i % 3 == 0 {
                    fail.store(false, Ordering::SeqCst);
                }
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        fail.store(false, Ordering::SeqCst);
        let out = mgr.call("fs", "echo", json!({"x": 1})).await.unwrap();
        assert_eq!(out.text, "echo: {\"x\":1}");
    }

    #[tokio::test]
    async fn unknown_and_disabled_servers_error_clearly() {
        let fail = Arc::new(AtomicBool::new(false));
        let (mgr, _spawns) = fake_manager(&[server("fs", true), server("off", false)], fail);
        let err = mgr.server_tools("nope").await.unwrap_err().to_string();
        assert!(
            err.contains("unknown mcp server 'nope'") && err.contains("fs"),
            "{err}"
        );
        let err = mgr.server_tools("off").await.unwrap_err().to_string();
        assert!(err.contains("disabled"), "{err}");
        // enabled() filters disabled servers — the deferred-group surface
        // never sees them.
        assert_eq!(mgr.enabled().len(), 1);
        assert_eq!(mgr.enabled()[0].name, "fs");
    }

    #[test]
    fn rpc_helpers_round_trip() {
        let req = rpc_request(3, "tools/list", json!({}));
        assert_eq!(req["id"], 3);
        assert_eq!(req["method"], "tools/list");
        assert_eq!(req["jsonrpc"], "2.0");
        assert_eq!(
            extract_result(&json!({"result": {"ok": 1}})).unwrap()["ok"],
            1
        );
        let err =
            extract_result(&json!({"error": {"code": -32000, "message": "bad"}})).unwrap_err();
        assert!(
            err.to_string().contains("-32000") && err.to_string().contains("bad"),
            "{err}"
        );
        assert!(extract_result(&json!({})).is_err());
    }

    #[test]
    fn tool_list_and_call_result_parsing() {
        let tools = parse_tool_list(&json!({
            "tools": [
                {"name": "a", "description": "d", "inputSchema": {"type": "object"}},
                {"name": "", "description": "empty name dropped"},
                {"description": "missing name dropped"}
            ]
        }))
        .unwrap();
        assert_eq!(tools.len(), 1, "malformed entries are dropped");
        assert_eq!(tools[0].name, "a");
        // No inputSchema → a permissive default schema, never a hard error.
        let tools = parse_tool_list(&json!({"tools": [{"name": "b"}]})).unwrap();
        assert_eq!(tools[0].input_schema["type"], "object");
        let out = parse_call_result(&json!({
            "content": [
                {"type": "text", "text": "one"},
                {"type": "image", "data": "..."},
                {"type": "text", "text": "two"}
            ],
            "isError": true
        }))
        .unwrap();
        assert!(out.is_error);
        assert_eq!(out.text, "one\n[non-text content]\ntwo");
        // content omitted → empty text, not an error.
        let out = parse_call_result(&json!({})).unwrap();
        assert_eq!(out.text, "");
        assert!(!out.is_error);
    }

    #[test]
    fn capabilities_and_meta_list_parsing() {
        // Capabilities: prompts/resources read from the initialize result.
        let caps = parse_capabilities(&json!({
            "capabilities": {"prompts": {}, "resources": {"subscribe": true}}
        }));
        assert!(caps.prompts && caps.resources);
        let caps = parse_capabilities(&json!({"capabilities": {"tools": {}}}));
        assert!(!caps.prompts && !caps.resources);
        let caps = parse_capabilities(&json!({}));
        assert!(!caps.prompts && !caps.resources);

        // Prompt/resource listings drop malformed entries; a missing array
        // is empty rather than an error.
        let prompts = parse_prompt_list(&json!({"prompts": [
            {"name": "greet", "description": "d"},
            {"description": "no name dropped"}
        ]}))
        .unwrap();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0].name, "greet");
        assert!(parse_prompt_list(&json!({})).unwrap().is_empty());
        let resources = parse_resource_list(&json!({"resources": [
            {"uri": "file:///x", "name": "x"}
        ]}))
        .unwrap();
        assert_eq!(resources[0].uri, "file:///x");
        assert!(parse_resource_list(&json!({})).unwrap().is_empty());
        // Meta content parsing shares the block-join.
        assert_eq!(
            parse_text_content(&json!({"content": [{"type": "text", "text": "t"}]})).unwrap(),
            "t"
        );
    }
}
