// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! MCP server settings commands for Mnemo2's Settings dialog — the backend of
//! its **MCP servers** page (plan f1bd54e5, step 2).
//!
//! The page reads and writes the same two files the engines read:
//! `<config_dir>/mcp.toml` (global) and `<project>/.coding/mcp.toml`
//! (project, shadowing a global twin by name). Listing and saving go through
//! [`Setup::mcp_servers`] / [`Setup::save_mcp_servers`], whose core
//! (`src/config/mcp.rs`) validates every entry, restores shadowed global
//! twins so a project edit never deletes another project's baseline, and
//! swaps a live factory's shared manager after the write. No validation or
//! file logic is duplicated here.
//!
//! **When a save applies.** The manager swap is immediate, but an agent's
//! tool table (its `mcp.<server>` deferred groups) is built when the agent
//! starts: team workers dispatched after the save see the new servers, and
//! the lead's own session was started when the project opened, so it picks
//! them up at the next project open. The page says exactly that instead of
//! implying an open lead gained tools mid-session.
//!
//! **Test.** A Test runs on a THROWAWAY manager built from the merged
//! configuration — so it works with or without a live lead, can test a
//! project server, and never caches a connection in the live manager. The
//! manager is dropped when the test ends, which closes the connection and
//! kills any stdio child spawned for it (`kill_on_drop` in
//! `src/mcp/stdio.rs`). The whole handshake + `tools/list` is bounded by
//! [`TEST_TIMEOUT`], so a server that never answers cannot hang the page.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use tauri::State;

use mnemo::config::mcp::{McpServerWire, SOURCE_PROJECT};
use mnemo::headless::{McpTestResult, Setup};
use mnemo::mcp::McpManager;

use crate::mnemo2::ipc::{settings_setup, Mnemo2State};

/// How long one Test may take before the page gives up. The kernel's own
/// handshake budget is 30 s; the Test is a Settings click, so it answers
/// sooner and reports "no answer" rather than leaving the button spinning.
pub const TEST_TIMEOUT: Duration = Duration::from_secs(20);

// ── Wire shape (the `mcp_settings_get` snapshot) ────────────────────────────

/// One live server status (`McpManager::statuses`), flattened for the page.
/// Absent entries never connected in this session — the page shows "not
/// connected yet" for those.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct McpStatusWire {
    /// The configured server id.
    pub name: String,
    /// Whether the manager currently holds a live connection.
    pub connected: bool,
    /// How many tools the server advertised (0 before the first connect).
    pub tool_count: usize,
    /// The last call failure (cleared by the next success / fresh connect).
    pub last_error: Option<String>,
    /// How many times the connection had to be re-established.
    pub restarts: u64,
}

/// The MCP page's snapshot: the merged server list with each entry's source,
/// the live statuses, and the two hints the page needs to stay honest about
/// when a save reaches an agent.
#[derive(Debug, Clone, Serialize)]
pub struct McpSettingsWire {
    /// The merged servers (global + project, the project definition winning
    /// by name), each tagged `"global"` or `"project"`.
    pub servers: Vec<McpServerWire>,
    /// Whether a project is open — project servers can only be edited with
    /// one (there is no `.coding/mcp.toml` without it).
    pub project_open: bool,
    /// Whether an open lead's engine is live, so a save swaps the shared
    /// manager now. Its own session still needs a project open to see the
    /// new servers (see the module docs).
    pub live_lead: bool,
    /// Live per-server status, sorted by name (empty without a live lead).
    pub statuses: Vec<McpStatusWire>,
}

// ── Snapshot + save (the testable core) ─────────────────────────────────────

/// Save `servers` split by source through the shared core, then read the
/// fresh snapshot back. Project rows without an open project are refused:
/// `save_split` would validate them and then have nowhere to write them, a
/// silent data loss the API alone does not prevent.
async fn save_servers(
    setup: &Setup,
    project: Option<&Path>,
    servers: Vec<McpServerWire>,
) -> Result<McpSettingsWire, String> {
    if project.is_none() && servers.iter().any(|wire| wire.source == SOURCE_PROJECT) {
        return Err(
            "Open a project first — project MCP servers live in the project's .coding folder."
                .into(),
        );
    }
    setup
        .save_mcp_servers(project, servers)
        .await
        .map_err(|err| err.to_string())?;
    Ok(read_snapshot(setup, project).await)
}

/// Read the page snapshot: the merged servers, plus (with a live engine) the
/// per-server connection statuses.
async fn read_snapshot(setup: &Setup, project: Option<&Path>) -> McpSettingsWire {
    let servers = setup.mcp_servers(project);
    let statuses = setup
        .mcp_statuses()
        .await
        .into_iter()
        .map(|(name, status)| McpStatusWire {
            name,
            connected: status.connected,
            tool_count: status.tool_count,
            last_error: status.last_error,
            restarts: status.restarts,
        })
        .collect();
    McpSettingsWire {
        servers,
        project_open: project.is_some(),
        live_lead: project.is_some(),
        statuses,
    }
}

// ── Test (a throwaway connection, never the live manager) ──────────────────

/// The failed-result shape every Test error path returns (the page renders
/// `error` inline; the button never receives an Err for a server problem).
fn failure(message: String) -> McpTestResult {
    McpTestResult {
        ok: false,
        tool_count: 0,
        tool_names: Vec::new(),
        error: Some(message),
    }
}

/// Find one server in the merged list by name (the project twin wins, as the
/// runtime merge does).
fn find_server<'a>(servers: &'a [McpServerWire], name: &str) -> Option<&'a McpServerWire> {
    servers.iter().find(|wire| wire.def.name == name)
}

/// Connect to one configured server on a throwaway manager and list its
/// tools, bounded by `timeout`. The manager is dropped before this returns,
/// so a stdio child spawned for the test is killed and no connection is
/// cached anywhere.
async fn test_server_with_timeout(
    servers: &[McpServerWire],
    name: &str,
    timeout: Duration,
) -> McpTestResult {
    let name = name.trim();
    let Some(wire) = find_server(servers, name) else {
        return failure(format!(
            "No MCP server named '{name}' is configured — save it first, then test."
        ));
    };
    if let Err(error) = wire.def.validate() {
        return failure(error.to_string());
    }
    // A throwaway manager: the live manager is never touched, so the test
    // can run with no lead open, and it cannot leave a connection behind.
    let manager = McpManager::from_config(std::slice::from_ref(&wire.def));
    match tokio::time::timeout(timeout, manager.server_tools(name)).await {
        Ok(Ok(tools)) => McpTestResult {
            ok: true,
            tool_count: tools.len(),
            tool_names: tools.iter().map(|tool| tool.name.clone()).collect(),
            error: None,
        },
        Ok(Err(error)) => failure(error.to_string()),
        Err(_) => failure(format!(
            "The server did not answer within {:.1} s — is the command on PATH and does it \
             speak MCP?",
            timeout.as_secs_f64()
        )),
    }
}

/// [`test_server_with_timeout`] at the page's budget.
async fn test_server(servers: &[McpServerWire], name: &str) -> McpTestResult {
    test_server_with_timeout(servers, name, TEST_TIMEOUT).await
}

// ── Commands ────────────────────────────────────────────────────────────────

/// The MCP page snapshot: every configured server (global + the open
/// project's), their sources, and the live connection statuses.
#[tauri::command]
pub async fn mcp_settings_get(state: State<'_, Mnemo2State>) -> Result<McpSettingsWire, String> {
    let (setup, runtime) = settings_setup(&state).await?;
    let project = runtime
        .as_deref()
        .map(|runtime| runtime.workspace_root());
    Ok(read_snapshot(&setup, project).await)
}

/// Replace the configured servers (the whole list, split by source) and
/// return the fresh snapshot. Validation failures leave both files
/// untouched; on an open lead the shared manager swaps now, and agents
/// started after this save see the new set.
#[tauri::command]
pub async fn mcp_settings_save(
    state: State<'_, Mnemo2State>,
    servers: Vec<McpServerWire>,
) -> Result<McpSettingsWire, String> {
    let (setup, runtime) = settings_setup(&state).await?;
    let project = runtime
        .as_deref()
        .map(|runtime| runtime.workspace_root());
    save_servers(&setup, project, servers).await
}

/// Test one saved server: connect, list its tools, disconnect. A server
/// problem (unknown name, bad definition, spawn failure, timeout) comes back
/// as `ok: false` with the reason — not an Err, which is reserved for the
/// settings state itself.
#[tauri::command]
pub async fn mcp_settings_test(
    state: State<'_, Mnemo2State>,
    name: String,
) -> Result<McpTestResult, String> {
    let (setup, runtime) = settings_setup(&state).await?;
    let project = runtime
        .as_deref()
        .map(|runtime| runtime.workspace_root());
    let servers = setup.mcp_servers(project);
    Ok(test_server(&servers, &name).await)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use mnemo::config::mcp::{save, McpServerDef, SOURCE_GLOBAL};

    /// One stdio server definition with every field spelled out (the def has
    /// no `Default`).
    fn stdio_def(name: &str, command: &str) -> McpServerDef {
        McpServerDef {
            name: name.into(),
            enabled: true,
            command: Some(command.into()),
            args: Vec::new(),
            env: Vec::new(),
            url: None,
            headers_env: BTreeMap::new(),
            auth: None,
            client_id: None,
            scopes: Vec::new(),
            trusted: false,
            idle_timeout_secs: None,
        }
    }

    /// A temp config dir holding one global server, plus a temp project dir
    /// with no `.coding/mcp.toml` yet.
    fn fixture() -> (tempfile::TempDir, tempfile::TempDir, Setup) {
        let config_dir = tempfile::tempdir().expect("config dir");
        save(
            &config_dir.path().join("mcp.toml"),
            &[stdio_def("global-fs", "npx")],
        )
        .expect("write the global mcp.toml");
        let project = tempfile::tempdir().expect("project dir");
        let setup = Setup::open(Some(config_dir.path().to_path_buf())).expect("setup");
        (config_dir, project, setup)
    }

    #[tokio::test]
    async fn snapshot_lists_global_and_project_servers_with_sources() {
        let (_config_dir, project, setup) = fixture();
        // Without a project: the global definition only, no statuses (a
        // standalone Setup holds no connections).
        let snapshot = read_snapshot(&setup, None).await;
        assert!(!snapshot.project_open);
        assert!(snapshot.statuses.is_empty());
        assert_eq!(snapshot.servers.len(), 1);
        assert_eq!(snapshot.servers[0].def.name, "global-fs");
        assert_eq!(snapshot.servers[0].source, SOURCE_GLOBAL);

        // With a project whose file shadows the global name plus adds one:
        // the project definition wins and is tagged `project`.
        save(
            &project.path().join(".coding").join("mcp.toml"),
            &[
                stdio_def("global-fs", "project-npx"),
                stdio_def("project-only", "uvx"),
            ],
        )
        .expect("write the project mcp.toml");
        let snapshot = read_snapshot(&setup, Some(project.path())).await;
        assert!(snapshot.project_open);
        let names: Vec<&str> = snapshot
            .servers
            .iter()
            .map(|wire| wire.def.name.as_str())
            .collect();
        assert_eq!(names, vec!["global-fs", "project-only"]);
        assert!(
            snapshot.servers.iter().all(|wire| wire.source == SOURCE_PROJECT),
            "both are project rows"
        );
        assert_eq!(
            snapshot.servers[0].def.command.as_deref(),
            Some("project-npx"),
            "the project definition shadows the global twin"
        );
    }

    #[tokio::test]
    async fn save_writes_each_source_to_its_own_file_and_round_trips() {
        let (config_dir, project, setup) = fixture();
        let servers = vec![
            McpServerWire::new(stdio_def("global-fs", "npx"), SOURCE_GLOBAL),
            McpServerWire::new(stdio_def("from-project", "uvx"), SOURCE_PROJECT),
        ];
        let snapshot = save_servers(&setup, Some(project.path()), servers)
            .await
            .expect("save both sources");
        let names: Vec<&str> = snapshot
            .servers
            .iter()
            .map(|wire| wire.def.name.as_str())
            .collect();
        assert_eq!(names, vec!["from-project", "global-fs"]);

        // The files landed where the engines read them.
        let global = mnemo::config::mcp::load_or_default(&config_dir.path().join("mcp.toml"))
            .expect("reload global");
        assert_eq!(global.len(), 1);
        assert_eq!(global[0].name, "global-fs");
        let scoped = mnemo::config::mcp::load_or_default(
            &project.path().join(".coding").join("mcp.toml"),
        )
        .expect("reload project");
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].name, "from-project");
    }

    #[tokio::test]
    async fn save_refuses_project_rows_without_an_open_project() {
        let (config_dir, _project, setup) = fixture();
        let err = save_servers(
            &setup,
            None,
            vec![McpServerWire::new(
                stdio_def("orphan", "npx"),
                SOURCE_PROJECT,
            )],
        )
        .await
        .expect_err("a project row without a project must be refused");
        assert!(
            err.contains("Open a project first"),
            "the error names the fix, got: {err}"
        );
        // The global file still holds exactly its one server.
        let global = mnemo::config::mcp::load_or_default(&config_dir.path().join("mcp.toml"))
            .expect("reload global");
        assert_eq!(global.len(), 1);
        assert_eq!(global[0].name, "global-fs");
    }

    #[tokio::test]
    async fn save_rejects_an_invalid_definition_and_leaves_the_file_alone() {
        let (config_dir, _project, setup) = fixture();
        let mut both = stdio_def("both-transports", "npx");
        both.url = Some("https://also-set.example.com/mcp".into());
        let err = save_servers(
            &setup,
            None,
            vec![McpServerWire::new(both, SOURCE_GLOBAL)],
        )
        .await
        .expect_err("both transports must be rejected");
        assert!(
            err.contains("both command and url"),
            "the shared validator's message surfaces, got: {err}"
        );
        let global = mnemo::config::mcp::load_or_default(&config_dir.path().join("mcp.toml"))
            .expect("reload global");
        assert_eq!(
            global.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            vec!["global-fs"],
            "a rejected save never touches the file"
        );
    }

    #[tokio::test]
    async fn test_reports_an_unknown_name_without_spawning_anything() {
        let (_config_dir, project, setup) = fixture();
        let servers = setup.mcp_servers(Some(project.path()));
        let result = test_server(&servers, "not-configured").await;
        assert!(!result.ok);
        assert_eq!(result.tool_count, 0);
        assert!(result.tool_names.is_empty());
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("No MCP server named 'not-configured'"),
            "the error names the missing server, got: {:?}",
            result.error
        );
    }

    #[tokio::test]
    async fn test_reports_a_spawn_failure_as_a_readable_error() {
        let mut def = stdio_def("gone", "mnemo2-no-such-binary-for-the-mcp-page-test");
        def.args = vec!["--stdio".into()];
        let servers = vec![McpServerWire::new(def, SOURCE_GLOBAL)];
        let result = test_server(&servers, "gone").await;
        assert!(!result.ok, "a missing command cannot pass the test");
        let error = result.error.unwrap_or_default();
        assert!(
            error.contains("failed to spawn"),
            "the spawn failure is reported, got: {error}"
        );
    }

    /// The page must never hang on a server that starts but never answers:
    /// the bounded Test returns a timeout error, and dropping the throwaway
    /// manager kills the child.
    #[tokio::test]
    async fn test_gives_up_on_a_server_that_never_answers() {
        #[cfg(windows)]
        let (command, args): (&str, Vec<String>) = (
            "powershell",
            vec![
                "-NoProfile".into(),
                "-Command".into(),
                "Start-Sleep -Seconds 30".into(),
            ],
        );
        #[cfg(unix)]
        let (command, args): (&str, Vec<String>) = ("sleep", vec!["30".into()]);
        let mut def = stdio_def("silent", command);
        def.args = args;
        let servers = vec![McpServerWire::new(def, SOURCE_GLOBAL)];
        let started = std::time::Instant::now();
        let result = test_server_with_timeout(&servers, "silent", Duration::from_millis(800)).await;
        assert!(!result.ok, "a silent server cannot pass the test");
        let error = result.error.unwrap_or_default();
        assert!(
            error.contains("did not answer"),
            "the timeout is reported, got: {error}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the test returned on its own budget, not the server's"
        );
    }

    /// The page's copy names the budget; keep it in one place.
    #[test]
    fn the_test_budget_is_twenty_seconds() {
        assert_eq!(TEST_TIMEOUT, Duration::from_secs(20));
    }
}
