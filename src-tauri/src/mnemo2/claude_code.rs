// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Ships the Claude Code plugin and its helper into `<config>/claude-plugin`,
//! and connects it to Claude Code on request (plan 5b9975c1, step 2).
//!
//! The plugin's files are compiled into the app (`include_str!`), so every
//! start writes `<config>/claude-plugin/` as a ready local marketplace:
//! `.claude-plugin/marketplace.json` names the marketplace `mnemo` and lists
//! two plugins: `mnemo` from `./plugin`, which holds the plugin itself plus
//! the `mnemo2-mcp` helper its MCP server and hooks run, and `mnemo-workers`
//! from `./mnemo-workers`, the Claude Desktop mod that shows the running
//! workers above the prompt in its Code tab. The helper is copied from beside
//! the running app, so an installed Mnemo2 carries it without a source build.
//! The copy keeps the source's mtime, which is what the helper's delegate
//! rule reads: a copy that is not older than another one takes over from it.
//!
//! The Settings → Claude Code page (step 3 of the plan) calls
//! [`claude_code_status`] for the snapshot and [`claude_code_connect`] to
//! register the marketplace and install the plugins through the `claude` CLI.
//! A `mnemo` marketplace that points at a source checkout is left alone:
//! developers keep their own.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use serde::Serialize;

/// The plugin files compiled into the app: (path inside the plugin folder, text).
const PLUGIN_FILES: &[(&str, &str)] = &[
    (
        ".claude-plugin/plugin.json",
        include_str!("../../../mnemo2/plugin/.claude-plugin/plugin.json"),
    ),
    (
        ".mcp.json",
        include_str!("../../../mnemo2/plugin/.mcp.json"),
    ),
    (
        "hooks/hooks.json",
        include_str!("../../../mnemo2/plugin/hooks/hooks.json"),
    ),
    (
        "skills/mnemo-lead/SKILL.md",
        include_str!("../../../mnemo2/plugin/skills/mnemo-lead/SKILL.md"),
    ),
    (
        "skills/mnemo-plan/SKILL.md",
        include_str!("../../../mnemo2/plugin/skills/mnemo-plan/SKILL.md"),
    ),
    (
        "skills/mnemo-review/SKILL.md",
        include_str!("../../../mnemo2/plugin/skills/mnemo-review/SKILL.md"),
    ),
    (
        "skills/mnemo-status/SKILL.md",
        include_str!("../../../mnemo2/plugin/skills/mnemo-status/SKILL.md"),
    ),
    (
        "README.md",
        include_str!("../../../mnemo2/plugin/README.md"),
    ),
];

/// The mnemo-workers mod's shipped files compiled into the app: (path
/// inside the mod folder, text). The mod's tests are not shipped, so they
/// stay out.
const WORKERS_MOD_FILES: &[(&str, &str)] = &[
    (
        ".claude-plugin/plugin.json",
        include_str!(
            "../../../mnemo2/claude-desktop-mod/mnemo-workers/.claude-plugin/plugin.json"
        ),
    ),
    (
        "hooks/hooks.json",
        include_str!("../../../mnemo2/claude-desktop-mod/mnemo-workers/hooks/hooks.json"),
    ),
    (
        "hooks/register.tsx",
        include_str!("../../../mnemo2/claude-desktop-mod/mnemo-workers/hooks/register.tsx"),
    ),
    (
        "types/index.d.ts",
        include_str!("../../../mnemo2/claude-desktop-mod/mnemo-workers/types/index.d.ts"),
    ),
    (
        "README.md",
        include_str!("../../../mnemo2/claude-desktop-mod/mnemo-workers/README.md"),
    ),
];

/// The local marketplace Claude Code registers: two plugins, `mnemo` and
/// `mnemo-workers`, sourced from the `plugin/` and `mnemo-workers/` folders
/// written beside this file.
const MARKETPLACE_JSON: &str = r#"{
  "name": "mnemo",
  "owner": {
    "name": "Carsten Hess"
  },
  "metadata": {
    "description": "Plugins for working with Mnemo2 from Claude Code."
  },
  "plugins": [
    {
      "name": "mnemo",
      "source": "./plugin",
      "description": "Lead a Mnemo2 team from Claude Code: plan, dispatch workers by tier, review and merge through Mnemo2's team tools."
    },
    {
      "name": "mnemo-workers",
      "source": "./mnemo-workers",
      "description": "Shows the running Mnemo2 workers and their models above the prompt in Claude Desktop's Code tab; click a worker to open it in mission control."
    }
  ]
}
"#;

/// The helper's file name beside the app and in every installed copy.
#[cfg(windows)]
pub(crate) const HELPER_EXE: &str = "mnemo2-mcp.exe";

/// The helper's file name beside the app and in every installed copy.
#[cfg(not(windows))]
pub(crate) const HELPER_EXE: &str = "mnemo2-mcp";

/// The names a Claude Code CLI executable can have on this platform.
#[cfg(windows)]
const CLAUDE_NAMES: &[&str] = &["claude.exe", "claude.cmd"];

/// The names a Claude Code CLI executable can have on this platform.
#[cfg(not(windows))]
const CLAUDE_NAMES: &[&str] = &["claude"];

/// The ready local Claude Code marketplace the app maintains under `<config>`.
pub(crate) fn plugin_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("claude-plugin")
}

/// Write `contents` to `path` when it differs; true when the file changed.
///
/// Every file the install writes goes through here, so a second start writes
/// nothing and leaves every mtime alone.
fn write_if_changed(path: &Path, contents: &str) -> std::io::Result<bool> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let unchanged = std::fs::read(path)
        .map(|existing| existing == contents.as_bytes())
        .unwrap_or(false);
    if unchanged {
        return Ok(false);
    }
    std::fs::write(path, contents)?;
    Ok(true)
}

/// A file's modification time, when the filesystem reports one.
fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

/// Unix epoch milliseconds for a leftover's unique name.
fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0)
}

/// The one error shape every helper install reports.
fn install_error(dest: &Path, error: std::io::Error) -> String {
    format!("could not install {}: {error}", dest.display())
}

/// Best effort: delete helper copies a previous, locked replace renamed aside.
///
/// A running helper keeps its file open, and Windows refuses to overwrite an
/// open executable; the replace renames the old file aside instead, and this
/// cleans up those leftovers once they are no longer locked.
fn remove_leftovers(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().contains(".old-") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Copy the app's sibling helper into `dest_dir`, keeping the source's mtime.
///
/// A copy that is not older than the source is kept, so an installed helper is
/// never replaced by a stale one. Returns true when the copy changed.
fn install_helper(src: &Path, dest_dir: &Path) -> Result<bool, String> {
    let dest = dest_dir.join(HELPER_EXE);
    std::fs::create_dir_all(dest_dir).map_err(|error| install_error(&dest, error))?;
    remove_leftovers(dest_dir);
    if let (Some(source), Some(installed)) = (modified(src), modified(&dest)) {
        if installed >= source {
            return Ok(false);
        }
    }
    if std::fs::remove_file(&dest).is_err() && dest.exists() {
        let leftover = dest.with_file_name(format!("{HELPER_EXE}.old-{}", now_millis()));
        std::fs::rename(&dest, &leftover).map_err(|error| install_error(&dest, error))?;
    }
    std::fs::copy(src, &dest).map_err(|error| install_error(&dest, error))?;
    let source_time = modified(src).ok_or_else(|| {
        format!(
            "could not install {}: its modification time is unreadable",
            dest.display()
        )
    })?;
    std::fs::File::options()
        .write(true)
        .open(&dest)
        .and_then(|file| file.set_modified(source_time))
        .map_err(|error| install_error(&dest, error))?;
    Ok(true)
}

/// What one startup install did; the setup hook logs the helper update.
#[derive(Debug)]
pub(crate) struct InstallReport {
    /// The ready marketplace folder under `<config>`.
    pub plugin_dir: PathBuf,
    /// Whether the running app had a sibling helper to install.
    pub helper_found: bool,
    /// Whether an installed helper copy was replaced by the sibling's.
    pub helper_updated: bool,
}

/// Write the embedded plugin and mod as a ready local marketplace and
/// install the running app's sibling helper beside it and in `<config>/bin`.
///
/// Files are written only when they differ, so a second start writes nothing.
/// Both helper folders are attempted even when one fails (a running helper can
/// lock its file); the first error is returned after the loop so one locked
/// folder never skips the other.
pub(crate) fn install_bundled(
    config_dir: &Path,
    app_exe: &Path,
) -> Result<InstallReport, String> {
    let root = plugin_dir(config_dir);
    let marketplace = root.join(".claude-plugin").join("marketplace.json");
    write_if_changed(&marketplace, MARKETPLACE_JSON)
        .map_err(|error| format!("could not write {}: {error}", marketplace.display()))?;
    let plugin_root = root.join("plugin");
    for (relative, contents) in PLUGIN_FILES {
        let path = plugin_root.join(relative);
        write_if_changed(&path, contents)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    }
    let mod_root = root.join("mnemo-workers");
    for (relative, contents) in WORKERS_MOD_FILES {
        let path = mod_root.join(relative);
        write_if_changed(&path, contents)
            .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    }

    let sibling = app_exe
        .parent()
        .map(|dir| dir.join(HELPER_EXE))
        .unwrap_or_else(|| PathBuf::from(HELPER_EXE));
    let helper_found = sibling.is_file();
    let mut helper_updated = false;
    if helper_found {
        let mut first_error: Option<String> = None;
        for dest_dir in [config_dir.join("bin"), plugin_root.join("bin")] {
            match install_helper(&sibling, &dest_dir) {
                Ok(updated) => {
                    helper_updated |= updated;
                    let hint = dest_dir.join("mnemo2-path.txt");
                    let contents = format!("{}\n", app_exe.display());
                    if let Err(error) = write_if_changed(&hint, &contents) {
                        if first_error.is_none() {
                            first_error =
                                Some(format!("could not write {}: {error}", hint.display()));
                        }
                    }
                }
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
    }
    Ok(InstallReport {
        plugin_dir: root,
        helper_found,
        helper_updated,
    })
}

/// Find Claude Code's CLI: every PATH folder in order, then the known install
/// folders, so a terminal `claude` wins over a stale one in a user folder.
pub(crate) fn find_claude_cli(
    path_var: Option<&OsStr>,
    home: Option<&Path>,
    appdata: Option<&Path>,
) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path_var) = path_var {
        for dir in std::env::split_paths(path_var) {
            // An empty PATH entry names the cwd; never resolve a CLI from it.
            if dir.as_os_str().is_empty() {
                continue;
            }
            for name in CLAUDE_NAMES {
                candidates.push(dir.join(name));
            }
        }
    }
    if let Some(home) = home {
        for name in CLAUDE_NAMES {
            candidates.push(home.join(".local").join("bin").join(name));
            candidates.push(home.join(".claude").join("local").join(name));
        }
        #[cfg(not(windows))]
        {
            candidates.push(PathBuf::from("/opt/homebrew/bin").join(CLAUDE_NAMES[0]));
            candidates.push(PathBuf::from("/usr/local/bin").join(CLAUDE_NAMES[0]));
            for name in CLAUDE_NAMES {
                candidates.push(home.join(".npm-global").join("bin").join(name));
            }
        }
    }
    #[cfg(windows)]
    if let Some(appdata) = appdata {
        candidates.push(appdata.join("npm").join("claude.cmd"));
    }
    #[cfg(not(windows))]
    let _ = appdata;
    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// [`find_claude_cli`] with this process's environment.
fn cli_from_env() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH");
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME").map(PathBuf::from);
    #[cfg(windows)]
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    #[cfg(not(windows))]
    let appdata: Option<PathBuf> = None;
    find_claude_cli(path_var.as_deref(), home.as_deref(), appdata.as_deref())
}

/// The Settings → Claude Code snapshot: helper, CLI and plugin state.
#[derive(Serialize)]
pub struct ClaudeCodeStatus {
    /// `<config>/bin/mnemo2-mcp[.exe]` when that copy exists.
    pub helper_path: Option<String>,
    /// The ready marketplace folder under `<config>` (`<config>/claude-plugin`).
    pub plugin_dir: String,
    /// The `claude` CLI this app found, when any.
    pub cli_path: Option<String>,
    /// What Claude Code's marketplace list says about a marketplace `mnemo`.
    pub marketplace: MarketplaceState,
    /// The path of an existing `mnemo` marketplace, whatever its state.
    pub marketplace_path: Option<String>,
    /// Whether `mnemo@mnemo` is installed.
    pub plugin_installed: bool,
    /// The installed plugin's version, when Claude Code reports one.
    pub plugin_version: Option<String>,
    /// Whether `mnemo-workers@mnemo` is installed.
    pub workers_installed: bool,
    /// The CLI call that failed, when one did; the rest keep their defaults.
    pub error: Option<String>,
}

/// What Claude Code's marketplace list says about a marketplace named `mnemo`.
#[derive(Serialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum MarketplaceState {
    /// No marketplace named `mnemo`.
    Missing,
    /// A marketplace named `mnemo` pointing at this app's plugin folder.
    Mnemo2,
    /// A marketplace named `mnemo` pointing elsewhere (a source checkout).
    Other,
}

/// One `claude` command Connect ran and what it printed.
#[derive(Serialize)]
pub struct ConnectStep {
    /// The command as the page shows it, e.g. `claude plugin install mnemo@mnemo`.
    pub command: String,
    /// Whether the command exited successfully.
    pub ok: bool,
    /// Trimmed stdout and stderr of the command.
    pub output: String,
}

/// What Connect did: an overall outcome, a message for the page, and the steps.
#[derive(Serialize)]
pub struct ClaudeConnectResult {
    /// Whether the plugin is connected (or was deliberately left alone).
    pub ok: bool,
    /// The one-line outcome the page shows.
    pub message: String,
    /// The commands Connect ran, in order; empty when there was nothing to run.
    pub steps: Vec<ConnectStep>,
}

/// What Claude Code's `plugin marketplace list --json` says about `mnemo`.
fn marketplace_state(
    list_json: &str,
    plugin_dir: &Path,
) -> Result<(MarketplaceState, Option<String>), String> {
    let list: serde_json::Value = serde_json::from_str(list_json)
        .map_err(|error| format!("unreadable marketplace list: {error}"))?;
    let entries = list
        .as_array()
        .ok_or_else(|| "the marketplace list is not an array".to_string())?;
    for entry in entries {
        if entry.get("name").and_then(|name| name.as_str()) != Some("mnemo") {
            continue;
        }
        let path = entry
            .get("path")
            .or_else(|| entry.get("installLocation"))
            .and_then(|path| path.as_str())
            .map(PathBuf::from);
        let state = match &path {
            Some(path) if same_dir(path, plugin_dir) => MarketplaceState::Mnemo2,
            _ => MarketplaceState::Other,
        };
        return Ok((state, path.map(|path| path.display().to_string())));
    }
    Ok((MarketplaceState::Missing, None))
}

/// Whether two paths name the same directory: canonical when both resolve,
/// else a normalized string comparison (case-insensitive on Windows).
fn same_dir(a: &Path, b: &Path) -> bool {
    if let (Ok(a), Ok(b)) = (dunce::canonicalize(a), dunce::canonicalize(b)) {
        return a == b;
    }
    normalized_dir(a) == normalized_dir(b)
}

/// The fallback comparison key for a directory path.
fn normalized_dir(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text.trim_end_matches('/');
    #[cfg(windows)]
    {
        text.to_lowercase()
    }
    #[cfg(not(windows))]
    {
        text.to_string()
    }
}

/// What Claude Code's `plugin list --json` says about one plugin id.
fn plugin_state(list_json: &str, id: &str) -> Result<(bool, Option<String>), String> {
    let list: serde_json::Value = serde_json::from_str(list_json)
        .map_err(|error| format!("unreadable plugin list: {error}"))?;
    let entries = list
        .as_array()
        .ok_or_else(|| "the plugin list is not an array".to_string())?;
    for entry in entries {
        if entry.get("id").and_then(|found| found.as_str()) != Some(id) {
            continue;
        }
        let version = entry
            .get("version")
            .and_then(|version| version.as_str())
            .map(str::to_string);
        return Ok((true, version));
    }
    Ok((false, None))
}

/// The `claude` commands Connect runs, in order, for a state.
///
/// A marketplace named `mnemo` that points elsewhere is a developer's source
/// checkout: the plan is empty and Connect leaves it alone.
fn connect_plan(
    marketplace: MarketplaceState,
    plugin_installed: bool,
    workers_installed: bool,
    plugin_dir: &Path,
) -> Vec<Vec<String>> {
    let mut plan = Vec::new();
    match marketplace {
        MarketplaceState::Other => return plan,
        MarketplaceState::Missing => {
            let add = plugin_dir.display().to_string();
            plan.push(args(&["plugin", "marketplace", "add", add.as_str()]));
        }
        MarketplaceState::Mnemo2 => {
            plan.push(args(&["plugin", "marketplace", "update", "mnemo"]));
        }
    }
    plan.push(install_or_update(plugin_installed, "mnemo@mnemo"));
    plan.push(install_or_update(workers_installed, "mnemo-workers@mnemo"));
    plan
}

/// The `claude` command that installs or updates one plugin id.
fn install_or_update(installed: bool, id: &str) -> Vec<String> {
    if installed {
        args(&["plugin", "update", id])
    } else {
        args(&["plugin", "install", id])
    }
}

/// A `claude` argument list as owned strings, for [`run_claude`].
fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

/// Run one `claude` command and capture its output; never fails.
///
/// On Windows a `.cmd`/`.bat` CLI (npm's shim) runs through `cmd.exe /C`, and
/// the child gets `CREATE_NO_WINDOW` like the app's other Git calls. A spawn
/// error or a timeout past 120 s is a failed step whose output says why.
async fn run_claude(cli: &Path, args: &[String]) -> ConnectStep {
    let command = format!("claude {}", args.join(" "));
    let mut process;
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let script = cli
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| matches!(extension.to_ascii_lowercase().as_str(), "cmd" | "bat"))
            .unwrap_or(false);
        if script {
            process = tokio::process::Command::new("cmd.exe");
            process.arg("/C").arg(cli);
        } else {
            process = tokio::process::Command::new(cli);
        }
        process.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        process = tokio::process::Command::new(cli);
    }
    process
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match tokio::time::timeout(Duration::from_secs(120), process.output()).await {
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let text = [stdout, stderr]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            ConnectStep {
                command,
                ok: output.status.success(),
                output: text,
            }
        }
        Ok(Err(error)) => ConnectStep {
            command,
            ok: false,
            output: error.to_string(),
        },
        Err(_) => ConnectStep {
            command,
            ok: false,
            output: "the command timed out after 120 seconds".to_string(),
        },
    }
}

/// Read Claude Code's marketplace and plugin state.
///
/// The first failed CLI call lands in [`ClaudeCodeStatus::error`]; the other
/// call's data is still filled in, so the page can show what is known. A
/// missing CLI leaves every CLI-derived field at its default without an error.
async fn status_with(config_dir: &Path, cli: Option<PathBuf>) -> ClaudeCodeStatus {
    let root = plugin_dir(config_dir);
    let helper = config_dir.join("bin").join(HELPER_EXE);
    let mut status = ClaudeCodeStatus {
        helper_path: helper.is_file().then(|| helper.display().to_string()),
        plugin_dir: root.display().to_string(),
        cli_path: cli.as_ref().map(|cli| cli.display().to_string()),
        marketplace: MarketplaceState::Missing,
        marketplace_path: None,
        plugin_installed: false,
        plugin_version: None,
        workers_installed: false,
        error: None,
    };
    let Some(cli) = cli else {
        return status;
    };
    let marketplaces = run_claude(&cli, &args(&["plugin", "marketplace", "list", "--json"])).await;
    if marketplaces.ok {
        match marketplace_state(&marketplaces.output, &root) {
            Ok((marketplace, path)) => {
                status.marketplace = marketplace;
                status.marketplace_path = path;
            }
            Err(reason) => status.error = Some(format!("{}: {reason}", marketplaces.command)),
        }
    } else {
        status.error = Some(format!("{}: {}", marketplaces.command, marketplaces.output));
    }
    let plugins = run_claude(&cli, &args(&["plugin", "list", "--json"])).await;
    if plugins.ok {
        match plugin_state(&plugins.output, "mnemo@mnemo") {
            Ok((installed, version)) => {
                status.plugin_installed = installed;
                status.plugin_version = version;
            }
            Err(reason) => {
                status
                    .error
                    .get_or_insert_with(|| format!("{}: {reason}", plugins.command));
            }
        }
        match plugin_state(&plugins.output, "mnemo-workers@mnemo") {
            Ok((installed, _)) => status.workers_installed = installed,
            Err(reason) => {
                status
                    .error
                    .get_or_insert_with(|| format!("{}: {reason}", plugins.command));
            }
        }
    } else {
        status
            .error
            .get_or_insert_with(|| format!("{}: {}", plugins.command, plugins.output));
    }
    status
}

/// Register the local marketplace and install (or update) the `mnemo` and
/// `mnemo-workers` plugins.
///
/// A marketplace named `mnemo` that points at another folder is a developer's
/// source checkout: Connect leaves it untouched and says so. A missing helper
/// is guidance the page shows; a CLI call failure ends the run at that step,
/// with every step's output preserved.
async fn connect_with(config_dir: &Path, cli: &Path) -> ClaudeConnectResult {
    let status = status_with(config_dir, Some(cli.to_path_buf())).await;
    if let Some(error) = status.error.as_deref() {
        return ClaudeConnectResult {
            ok: false,
            message: format!("Could not read Claude Code's plugins: {error}"),
            steps: Vec::new(),
        };
    }
    if status.marketplace == MarketplaceState::Other {
        return ClaudeConnectResult {
            ok: true,
            message: format!(
                "Claude Code already uses the mnemo plugin from {} (a source checkout). Connect leaves it as it is.",
                status.marketplace_path.as_deref().unwrap_or("another folder")
            ),
            steps: Vec::new(),
        };
    }
    let root = plugin_dir(config_dir);
    let helper = root.join("plugin").join("bin").join(HELPER_EXE);
    if !helper.is_file() {
        return ClaudeConnectResult {
            ok: false,
            message: "The Mnemo helper is not installed yet. Restart Mnemo2, or reinstall it, and try again."
                .to_string(),
            steps: Vec::new(),
        };
    }
    let mut steps = Vec::new();
    for args in connect_plan(
        status.marketplace,
        status.plugin_installed,
        status.workers_installed,
        &root,
    ) {
        let step = run_claude(cli, &args).await;
        let failed = (!step.ok).then(|| step.command.clone());
        steps.push(step);
        if let Some(command) = failed {
            return ClaudeConnectResult {
                ok: false,
                message: format!("Connect stopped at `{command}`; see its output below."),
                steps,
            };
        }
    }
    ClaudeConnectResult {
        ok: true,
        message: "Connected. Start a new Claude Code session in a Mnemo2 project to use it."
            .to_string(),
        steps,
    }
}

/// The Settings → Claude Code snapshot: helper, CLI and plugin state.
#[tauri::command]
pub async fn claude_code_status() -> Result<ClaudeCodeStatus, String> {
    let config_dir = mnemo::config::resolve_config_dir(None);
    Ok(status_with(&config_dir, cli_from_env()).await)
}

/// Register the Mnemo plugin with Claude Code (Settings → Claude Code → Connect).
#[tauri::command]
pub async fn claude_code_connect() -> Result<ClaudeConnectResult, String> {
    let config_dir = mnemo::config::resolve_config_dir(None);
    let cli = cli_from_env().ok_or_else(|| {
        "Claude Code's `claude` command was not found. Install Claude Code, or add the plugin from Claude Desktop."
            .to_string()
    })?;
    Ok(connect_with(&config_dir, &cli).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_mtime(path: &Path, time: SystemTime) {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    #[test]
    fn install_writes_the_marketplace_and_every_plugin_file() {
        let config = tempfile::tempdir().unwrap();
        let app = tempfile::tempdir().unwrap();
        let exe = app.path().join("mnemo2");
        let report = install_bundled(config.path(), &exe).expect("install");
        let root = plugin_dir(config.path());
        for (relative, contents) in PLUGIN_FILES {
            let path = root.join("plugin").join(relative);
            let written = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            assert_eq!(&written, contents, "{} differs", path.display());
        }
        for (relative, contents) in WORKERS_MOD_FILES {
            let path = root.join("mnemo-workers").join(relative);
            let written = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
            assert_eq!(&written, contents, "{} differs", path.display());
        }
        let marketplace =
            std::fs::read_to_string(root.join(".claude-plugin").join("marketplace.json")).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&marketplace).unwrap();
        assert_eq!(parsed["name"], "mnemo");
        assert_eq!(parsed["plugins"][0]["name"], "mnemo");
        assert_eq!(parsed["plugins"][0]["source"], "./plugin");
        assert_eq!(parsed["plugins"][1]["name"], "mnemo-workers");
        assert_eq!(parsed["plugins"][1]["source"], "./mnemo-workers");
        assert_eq!(report.plugin_dir, root);
        assert!(!report.helper_found);
        assert!(!config.path().join("bin").exists());
    }

    #[test]
    fn install_copies_the_sibling_helper_and_records_the_app() {
        let config = tempfile::tempdir().unwrap();
        let app = tempfile::tempdir().unwrap();
        let source = app.path().join(HELPER_EXE);
        std::fs::write(&source, "v1").unwrap();
        let exe = app.path().join("mnemo2");
        let report = install_bundled(config.path(), &exe).expect("install");
        assert!(report.helper_found);
        assert!(report.helper_updated);
        let copies = [
            config.path().join("bin").join(HELPER_EXE),
            plugin_dir(config.path())
                .join("plugin")
                .join("bin")
                .join(HELPER_EXE),
        ];
        for copy in &copies {
            assert_eq!(std::fs::read_to_string(copy).unwrap(), "v1");
            assert_eq!(modified(copy), modified(&source));
        }
        for dir in [
            config.path().join("bin"),
            plugin_dir(config.path()).join("plugin").join("bin"),
        ] {
            assert_eq!(
                std::fs::read_to_string(dir.join("mnemo2-path.txt")).unwrap(),
                format!("{}\n", exe.display())
            );
        }
    }

    #[test]
    fn install_keeps_a_newer_helper() {
        let config = tempfile::tempdir().unwrap();
        let app = tempfile::tempdir().unwrap();
        let source = app.path().join(HELPER_EXE);
        std::fs::write(&source, "v1").unwrap();
        let installed = config.path().join("bin").join(HELPER_EXE);
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, "dev").unwrap();
        set_mtime(
            &installed,
            modified(&source).unwrap() + Duration::from_secs(60),
        );
        let exe = app.path().join("mnemo2");
        let report = install_bundled(config.path(), &exe).expect("install");
        assert_eq!(std::fs::read_to_string(&installed).unwrap(), "dev");
        // The plugin folder's own copy still comes from the app, so Claude
        // Code always has a helper to run.
        let plugin_copy = plugin_dir(config.path())
            .join("plugin")
            .join("bin")
            .join(HELPER_EXE);
        assert_eq!(std::fs::read_to_string(&plugin_copy).unwrap(), "v1");
        assert!(report.helper_updated);
    }

    #[test]
    fn install_replaces_an_older_helper() {
        let config = tempfile::tempdir().unwrap();
        let app = tempfile::tempdir().unwrap();
        let source = app.path().join(HELPER_EXE);
        std::fs::write(&source, "v1").unwrap();
        let installed = config.path().join("bin").join(HELPER_EXE);
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, "dev").unwrap();
        set_mtime(
            &installed,
            modified(&source).unwrap() - Duration::from_secs(60),
        );
        let exe = app.path().join("mnemo2");
        install_bundled(config.path(), &exe).expect("install");
        assert_eq!(std::fs::read_to_string(&installed).unwrap(), "v1");
        assert_eq!(modified(&installed), modified(&source));
    }

    #[test]
    fn a_second_install_changes_nothing() {
        let config = tempfile::tempdir().unwrap();
        let app = tempfile::tempdir().unwrap();
        std::fs::write(app.path().join(HELPER_EXE), "v1").unwrap();
        let exe = app.path().join("mnemo2");
        install_bundled(config.path(), &exe).expect("first install");
        let root = plugin_dir(config.path());
        let mut watched: Vec<PathBuf> = PLUGIN_FILES
            .iter()
            .map(|(relative, _)| root.join("plugin").join(relative))
            .collect();
        watched.extend(
            WORKERS_MOD_FILES
                .iter()
                .map(|(relative, _)| root.join("mnemo-workers").join(relative)),
        );
        watched.push(root.join(".claude-plugin").join("marketplace.json"));
        let before: Vec<(PathBuf, Option<SystemTime>)> = watched
            .into_iter()
            .map(|path| {
                let time = modified(&path);
                (path, time)
            })
            .collect();
        let report = install_bundled(config.path(), &exe).expect("second install");
        assert!(!report.helper_updated);
        for (path, time) in before {
            assert_eq!(modified(&path), time, "{} was rewritten", path.display());
        }
    }

    #[test]
    fn embedded_plugin_covers_every_plugin_file() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../mnemo2/plugin");
        let mut found = Vec::new();
        walk_shipped(&root, Path::new(""), "bin", &mut found);
        found.sort();
        let mut embedded: Vec<String> = PLUGIN_FILES
            .iter()
            .map(|(relative, _)| (*relative).to_string())
            .collect();
        embedded.sort();
        assert_eq!(
            found, embedded,
            "a file under mnemo2/plugin is missing from PLUGIN_FILES"
        );
    }

    #[test]
    fn embedded_mod_covers_every_mod_file() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../mnemo2/claude-desktop-mod/mnemo-workers");
        let mut found = Vec::new();
        walk_shipped(&root, Path::new(""), "tests", &mut found);
        found.sort();
        let mut embedded: Vec<String> = WORKERS_MOD_FILES
            .iter()
            .map(|(relative, _)| (*relative).to_string())
            .collect();
        embedded.sort();
        assert_eq!(
            found, embedded,
            "a file under mnemo-workers is missing from WORKERS_MOD_FILES"
        );
    }

    /// Every regular file under `dir`, relative with `/`; the top-level
    /// folder named `skip` stays out (an installed helper, the mod's tests —
    /// nothing the app ships).
    fn walk_shipped(dir: &Path, prefix: &Path, skip: &str, found: &mut Vec<String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect();
        paths.sort();
        for path in paths {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            let relative = prefix.join(&name);
            if path.is_dir() {
                if prefix.as_os_str().is_empty() && name == skip {
                    continue;
                }
                walk_shipped(&path, &relative, skip, found);
                continue;
            }
            found.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }

    #[test]
    fn find_claude_cli_prefers_path_then_known_locations() {
        let dir = tempfile::tempdir().unwrap();
        let first = CLAUDE_NAMES[0];
        let path_dir = dir.path().join("path");
        std::fs::create_dir_all(&path_dir).unwrap();
        let in_path = path_dir.join(first);
        std::fs::write(&in_path, "x").unwrap();
        let path_var = std::ffi::OsString::from(path_dir.as_os_str());
        assert_eq!(
            find_claude_cli(Some(path_var.as_os_str()), None, None),
            Some(in_path)
        );

        let home = dir.path().join("home");
        let known = home.join(".local").join("bin").join(first);
        std::fs::create_dir_all(known.parent().unwrap()).unwrap();
        std::fs::write(&known, "x").unwrap();
        let empty_path = std::ffi::OsString::from("");
        assert_eq!(
            find_claude_cli(Some(empty_path.as_os_str()), Some(&home), None),
            Some(known)
        );
        assert_eq!(
            find_claude_cli(Some(empty_path.as_os_str()), Some(&home.join("nothing")), None),
            None
        );
    }

    #[test]
    fn marketplace_state_reads_the_list() {
        let dir = tempfile::tempdir().unwrap();
        let ours = dir.path().join("claude-plugin");
        std::fs::create_dir_all(&ours).unwrap();
        let other = dir.path().join("checkout");
        std::fs::create_dir_all(&other).unwrap();
        let list = |path: &Path| {
            serde_json::json!([{
                "name": "mnemo",
                "path": path.display().to_string(),
            }])
            .to_string()
        };
        assert_eq!(
            marketplace_state(&list(&ours), &ours),
            Ok((MarketplaceState::Mnemo2, Some(ours.display().to_string())))
        );
        assert_eq!(
            marketplace_state(&list(&other), &ours),
            Ok((MarketplaceState::Other, Some(other.display().to_string())))
        );
        assert_eq!(
            marketplace_state("[]", &ours),
            Ok((MarketplaceState::Missing, None))
        );
        assert!(marketplace_state("not json", &ours).is_err());
    }

    #[test]
    fn plugin_state_reads_the_list() {
        let list = serde_json::json!([
            {"id": "mnemo@mnemo", "version": "2.3.1"},
            {"id": "mnemo-workers@mnemo", "version": "0.1.0"},
            {"id": "other@elsewhere", "version": "1"},
        ])
        .to_string();
        assert_eq!(
            plugin_state(&list, "mnemo@mnemo"),
            Ok((true, Some("2.3.1".to_string())))
        );
        assert_eq!(
            plugin_state(&list, "mnemo-workers@mnemo"),
            Ok((true, Some("0.1.0".to_string())))
        );
        assert_eq!(plugin_state(&list, "missing@mnemo"), Ok((false, None)));
        assert_eq!(plugin_state("[]", "mnemo@mnemo"), Ok((false, None)));
        assert!(plugin_state("not json", "mnemo@mnemo").is_err());
    }

    #[test]
    fn connect_plan_adds_or_updates() {
        let dir = PathBuf::from("claude-plugin-dir");
        assert_eq!(
            connect_plan(MarketplaceState::Missing, false, false, &dir),
            vec![
                args(&["plugin", "marketplace", "add", "claude-plugin-dir"]),
                args(&["plugin", "install", "mnemo@mnemo"]),
                args(&["plugin", "install", "mnemo-workers@mnemo"]),
            ]
        );
        assert_eq!(
            connect_plan(MarketplaceState::Mnemo2, true, true, &dir),
            vec![
                args(&["plugin", "marketplace", "update", "mnemo"]),
                args(&["plugin", "update", "mnemo@mnemo"]),
                args(&["plugin", "update", "mnemo-workers@mnemo"]),
            ]
        );
        // Each plugin installs or updates on its own.
        assert_eq!(
            connect_plan(MarketplaceState::Missing, true, false, &dir),
            vec![
                args(&["plugin", "marketplace", "add", "claude-plugin-dir"]),
                args(&["plugin", "update", "mnemo@mnemo"]),
                args(&["plugin", "install", "mnemo-workers@mnemo"]),
            ]
        );
        assert!(connect_plan(MarketplaceState::Other, true, true, &dir).is_empty());
        assert!(connect_plan(MarketplaceState::Other, false, false, &dir).is_empty());
    }

    #[test]
    fn both_marketplaces_list_both_plugins_with_existing_sources() {
        // The generated marketplace: a fresh install holds every folder its
        // two plugin sources point at.
        let config = tempfile::tempdir().unwrap();
        let app = tempfile::tempdir().unwrap();
        let exe = app.path().join("mnemo2");
        install_bundled(config.path(), &exe).expect("install");
        let root = plugin_dir(config.path());
        let generated: serde_json::Value = serde_json::from_str(MARKETPLACE_JSON).unwrap();
        let plugins = generated["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 2);
        assert_eq!(generated["plugins"][0]["name"], "mnemo");
        assert_eq!(generated["plugins"][1]["name"], "mnemo-workers");
        for plugin in plugins {
            let name = plugin["name"].as_str().unwrap();
            let source = plugin["source"].as_str().unwrap();
            let dir = root.join(source.strip_prefix("./").unwrap_or(source));
            assert!(
                dir.is_dir(),
                "{name}'s source {source} is missing under {}",
                root.display()
            );
        }

        // The repository's marketplace: every source folder exists in the
        // checkout and holds a plugin manifest.
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let text = std::fs::read_to_string(repo.join(".claude-plugin").join("marketplace.json"))
            .unwrap();
        let listed: serde_json::Value = serde_json::from_str(&text).unwrap();
        let plugins = listed["plugins"].as_array().unwrap();
        assert_eq!(plugins.len(), 2);
        assert_eq!(listed["plugins"][0]["name"], "mnemo");
        assert_eq!(listed["plugins"][1]["name"], "mnemo-workers");
        for plugin in plugins {
            let name = plugin["name"].as_str().unwrap();
            let source = plugin["source"].as_str().unwrap();
            let dir = repo.join(source.strip_prefix("./").unwrap_or(source));
            let manifest = dir.join(".claude-plugin").join("plugin.json");
            assert!(
                manifest.is_file(),
                "{name}'s source {source} has no .claude-plugin/plugin.json"
            );
        }
    }

}
