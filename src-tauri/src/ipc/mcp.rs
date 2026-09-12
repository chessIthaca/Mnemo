// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! MCP-server Tauri commands — the Settings → MCP surface.
//!
//! Two config scopes, union-merged for the agent (project wins by name):
//! the installation's `~/.mnemo/mcp.toml` ("global") and the open
//! project's `.coding/mcp.toml` ("project"). The wire type
//! ([`McpServerWire`]) tags each server with its source so the UI can
//! show + change it; saves SPLIT by source and write both files (each
//! validated as its own set, and the merged view re-validated), then
//! push the merged set into the live manager. The wire def is the plain
//! config type — names, transports, env-var NAMES; no secret values ever
//! travel through these commands.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use tauri::State;

use mnemo::config::mcp::{self, McpServerDef};

use crate::ipc::error::IpcError;
use crate::ipc::state::IpcState;

/// The source tag for a server defined in the installation's mcp.toml.
pub const SOURCE_GLOBAL: &str = "global";
/// The source tag for a server defined in the open project's
/// `.coding/mcp.toml`.
pub const SOURCE_PROJECT: &str = "project";

/// One configured MCP server + the file it lives in. `def` is flattened
/// so the frontend payload is the plain server fields plus `source`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerWire {
    #[serde(flatten)]
    pub def: McpServerDef,
    /// `"global"` or `"project"` (defaults to global when absent — a
    /// legacy frontend payload or a hand-rolled caller lands in the
    /// global file, matching v1 behavior).
    #[serde(default = "default_source")]
    pub source: String,
}

fn default_source() -> String {
    SOURCE_GLOBAL.to_string()
}

/// List the configured MCP servers (the live manager — startup merged
/// set plus any saves this session), each tagged with its source:
/// "project" when the name is defined in the open project's
/// `.coding/mcp.toml`, "global" otherwise. Empty when none are
/// configured.
#[tauri::command]
pub async fn mcp_list_servers(state: State<'_, IpcState>) -> Result<Vec<McpServerWire>, IpcError> {
    let project_names: BTreeSet<String> = {
        let project = state.project.root.lock().await;
        mcp::load_or_default(&project.mcp_toml)?
            .into_iter()
            .map(|s| s.name)
            .collect()
    };
    let Some(manager) = state.factory()?.mcp_manager() else {
        return Ok(Vec::new());
    };
    Ok(manager
        .servers()
        .into_iter()
        .map(|def| {
            let source = if project_names.contains(&def.name) {
                SOURCE_PROJECT
            } else {
                SOURCE_GLOBAL
            };
            McpServerWire {
                def,
                source: source.to_string(),
            }
        })
        .collect())
}

/// Live per-server status for the Settings MCP page: connection state +
/// tool count + last error + restart count from the manager, merged with
/// the def's idle-timeout knob.
#[derive(Debug, Clone, Serialize)]
pub struct McpServerStatusWire {
    pub name: String,
    pub connected: bool,
    pub tool_count: usize,
    pub last_error: Option<String>,
    pub restarts: u64,
    pub idle_timeout_secs: Option<u64>,
}

/// Live per-server status (never connects anything — a pure read of the
/// manager's tracked state). Entries appear only for servers the manager
/// has seen traffic for this session; the Settings page renders "not
/// connected yet" for the rest.
#[tauri::command]
pub async fn mcp_status(state: State<'_, IpcState>) -> Result<Vec<McpServerStatusWire>, IpcError> {
    let factory = state.factory()?;
    let Some(manager) = factory.mcp_manager() else {
        return Ok(Vec::new());
    };
    let statuses = manager.statuses().await;
    Ok(statuses
        .into_iter()
        .map(|(name, s)| {
            let idle_timeout_secs = manager.server_def(&name).and_then(|d| d.idle_timeout_secs);
            McpServerStatusWire {
                name,
                connected: s.connected,
                tool_count: s.tool_count,
                last_error: s.last_error,
                restarts: s.restarts,
                idle_timeout_secs,
            }
        })
        .collect())
}

/// Replace the whole MCP server set, split by source: "global" entries
/// are written to `~/.mnemo/mcp.toml` and "project" entries to the open
/// project's `.coding/mcp.toml`. Each file is validated as its own set,
/// the merged view is re-validated, the in-memory `Config` is synced (so
/// a later save_all round-trips the global subset), and the merged set is
/// pushed into the live manager (connections drop; the next call
/// reconnects). Already-built agents keep the deferred-group table they
/// were built with — agents built afterwards (subagents, or after a
/// restart / project switch) see the new set.
#[tauri::command]
pub async fn mcp_save_servers(
    state: State<'_, IpcState>,
    servers: Vec<McpServerWire>,
) -> Result<String, IpcError> {
    let factory = state.factory()?;
    let mut global_defs: Vec<McpServerDef> = Vec::new();
    let mut project_defs: Vec<McpServerDef> = Vec::new();
    for wire in servers {
        match wire.source.as_str() {
            SOURCE_PROJECT => project_defs.push(wire.def),
            _ => global_defs.push(wire.def),
        }
    }
    mcp::validate_set(&global_defs).map_err(|e| format!("{e}"))?;
    mcp::validate_set(&project_defs).map_err(|e| format!("{e}"))?;

    let global_path = mnemo::config::global_config_dir().join("mcp.toml");
    // A project override SHADOWS its global twin (the twin is invisible in
    // the merged list, so it cannot be part of the payload) — rewriting the
    // global file from the payload alone would silently DELETE the shadowed
    // baseline. Re-add shadowed twins from the CURRENT global file first
    // (review HIGH 1).
    let current_global = mcp::load_or_default(&global_path)?;
    let global_defs = mcp::restore_shadowed_globals(&global_defs, &project_defs, &current_global);
    mcp::validate_set(&global_defs).map_err(|e| format!("{e}"))?;
    let merged = mcp::merge_servers(&global_defs, &project_defs);
    mcp::validate_set(&merged).map_err(|e| format!("merged set invalid: {e}"))?;
    mcp::save(&global_path, &global_defs)
        .map_err(|e| format!("failed to write global mcp.toml: {e}"))?;
    let project_path = {
        let project = state.project.root.lock().await;
        project.mcp_toml.clone()
    };
    // Only create the project file when there is something to put in it —
    // a purely-global save must not materialize an empty
    // `.coding/mcp.toml` in projects that never had one (review LOW 5).
    if !project_defs.is_empty() || project_path.exists() {
        mcp::save(&project_path, &project_defs)
            .map_err(|e| format!("failed to write project mcp.toml: {e}"))?;
    }

    // Keep the in-memory Config in sync with the GLOBAL subset so a later
    // save_all round-trips it instead of clobbering the file with a stale
    // copy (the project subset lives in its own file, untouched by
    // save_all).
    {
        let mut config = state.project.config.lock().await;
        config.mcp = global_defs;
    }
    if let Some(manager) = factory.mcp_manager() {
        manager.replace_servers(merged).await;
    }
    Ok(
        "saved — servers apply to agents built after this save (a restart always picks them \
         up); use Test to verify a connection now"
            .into(),
    )
}

/// The outcome of a Settings "Test" click: connect to the named server
/// (initialize handshake) and list its tools. Never carries secret
/// values — env-var names resolve inside the transport at connect time.
#[derive(Debug, Clone, Serialize)]
pub struct McpTestResult {
    /// Whether connect + initialize + tools/list all succeeded.
    pub ok: bool,
    /// How many tools the server advertises (0 unless `ok`).
    pub tool_count: usize,
    /// The advertised tool names (empty unless `ok`).
    pub tool_names: Vec<String>,
    /// The failure reason, when `ok` is false.
    pub error: Option<String>,
}

/// The outcome of starting an OAuth flow: the authorization URL for the
/// frontend to open in the user's browser, plus a note for the UI.
#[derive(Debug, Clone, Serialize)]
pub struct McpOauthStart {
    pub authorize_url: String,
    pub note: String,
}

/// Start the OAuth 2.1 flow for a remote server (auth = "oauth"):
/// discover its authorization-server metadata, bind a loopback redirect
/// listener, and return the authorization URL for the frontend to open.
/// The code exchange completes in a background task (the user can take
/// minutes in the browser) — the tokens land in keys.toml, after which
/// `mcp_test` verifies the connection end-to-end.
#[tauri::command]
pub async fn mcp_oauth_start(
    state: State<'_, IpcState>,
    name: String,
) -> Result<McpOauthStart, IpcError> {
    let name = name.trim().to_string();
    let factory = state.factory()?;
    let manager = match factory.mcp_manager() {
        Some(manager) => manager,
        None => {
            return Err(IpcError::from(
                "MCP is not available in this session".to_string(),
            ))
        }
    };
    let def = match manager.servers().into_iter().find(|s| s.name == name) {
        Some(def) => def,
        None => return Err(IpcError::from(format!("unknown mcp server '{name}'"))),
    };
    if def.auth.as_deref() != Some("oauth") || !def.is_remote() {
        return Err(format!(
            "server '{name}' is not configured for OAuth — set auth = \"oauth\" on a remote \
             (url) server first"
        )
        .into());
    }
    let client_id = def.client_id.clone().unwrap_or_default();

    // Bind the loopback listener FIRST so the port is part of the
    // redirect URI (and therefore of the authorization request).
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|e| format!("failed to bind the OAuth redirect listener: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("failed to read the redirect port: {e}"))?
        .port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");
    let (verifier, challenge) = mnemo::mcp::oauth::pkce();
    let oauth_state = mnemo::mcp::oauth::new_state();
    let server_url = def.url.clone().unwrap_or_default();

    let http = mnemo::mcp::oauth::oauth_http_client();
    let meta = mnemo::mcp::oauth::discover(&http, &server_url)
        .await
        .map_err(|e| format!("{e}"))?;
    let authorize_url = mnemo::mcp::oauth::authorize_url(
        &meta.authorization_endpoint,
        &client_id,
        &redirect_uri,
        &oauth_state,
        &challenge,
        &def.scopes,
    );

    // The exchange completes off-command: wait for the redirect, swap the
    // code for tokens, persist into keys.toml. Outcome is logged — the
    // user verifies via Test.
    let keys_path = mnemo::config::global_config_dir().join("keys.toml");
    let store = std::sync::Arc::new(mnemo::mcp::oauth::McpTokenStore::new(keys_path));
    let token_endpoint = meta.token_endpoint.clone();
    let task_name = name.clone();
    tokio::spawn(async move {
        match mnemo::mcp::oauth::wait_for_code(listener, &oauth_state).await {
            Ok(code) => {
                let body = mnemo::mcp::oauth::build_token_request(
                    &code,
                    &redirect_uri,
                    &client_id,
                    &verifier,
                );
                match mnemo::mcp::oauth::token_request(&http, &token_endpoint, body).await {
                    Ok(mut tokens) => {
                        // Stamp the server origin so stored tokens are
                        // bound to THIS host (re-pointing the url
                        // invalidates them — review LOW 3).
                        tokens.origin = mnemo::mcp::oauth::origin_of(&server_url).ok();
                        match store.store(&task_name, &tokens) {
                            Ok(()) => {
                                eprintln!("mcp: OAuth tokens stored for '{task_name}' — use Test")
                            }
                            Err(e) => {
                                eprintln!("mcp: OAuth token store failed for '{task_name}': {e}")
                            }
                        }
                    }
                    Err(e) => eprintln!("mcp: OAuth token exchange failed for '{task_name}': {e}"),
                }
            }
            Err(e) => eprintln!("mcp: OAuth flow failed for '{task_name}': {e}"),
        }
    });

    Ok(McpOauthStart {
        authorize_url,
        note: format!(
            "Complete the login in your browser — tokens are stored automatically when the \
             redirect returns. Then use Test on '{name}' to verify."
        ),
    })
}

/// Test one MCP server: connect (lazily spawning a stdio child or opening
/// the remote session), run the initialize handshake, and list tools.
/// Bounded at 45s overall so a stuck factory/transport cannot hang the
/// Settings button; internal handshake/request timeouts apply too.
#[tauri::command]
pub async fn mcp_test(state: State<'_, IpcState>, name: String) -> Result<McpTestResult, IpcError> {
    let name = name.trim().to_string();
    let factory = state.factory()?;
    let fail = |msg: String| McpTestResult {
        ok: false,
        tool_count: 0,
        tool_names: Vec::new(),
        error: Some(msg),
    };
    let Some(manager) = factory.mcp_manager() else {
        return Ok(fail("MCP is not available in this session".into()));
    };
    match tokio::time::timeout(
        std::time::Duration::from_secs(45),
        manager.server_tools(&name),
    )
    .await
    {
        Ok(Ok(tools)) => Ok(McpTestResult {
            ok: true,
            tool_count: tools.len(),
            tool_names: tools.iter().map(|t| t.name.clone()).collect(),
            error: None,
        }),
        Ok(Err(e)) => Ok(fail(e.to_string())),
        Err(_) => Ok(fail(
            "timed out after 45s connecting / listing tools — is the command on PATH and \
             does the server start?"
                .into(),
        )),
    }
}
