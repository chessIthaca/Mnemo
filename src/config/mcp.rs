// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `mcp.toml` — configured MCP (Model Context Protocol) servers.
//!
//! Each `[[server]]` entry names a server and picks exactly one transport:
//! stdio (spawn `command` with `args`) or remote (connect to `url`). Secrets
//! are NEVER stored in this file — `env` and `headers_env` carry environment
//! variable NAMES only; the values are resolved from the environment at
//! connect time. That keeps the file safe to print in the Settings UI and in
//! logs (the derived `Debug` output is intentionally loggable).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The wire-format file: a list of `[[server]]` tables.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct McpFile {
    /// The configured servers, in file order.
    #[serde(default)]
    pub server: Vec<McpServerDef>,
}

/// One configured MCP server. Exactly one transport is set: stdio
/// ([`Self::command`] + [`Self::args`]) or remote ([`Self::url`]).
///
/// By design no secret VALUES live here: `env` lists environment variable
/// names to pass through to the spawned process, and `headers_env` maps
/// header names to environment variable names whose values are sent as those
/// headers on remote connections. Everything in this struct is safe to log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerDef {
    /// The stable server id. Tool names (`mcp__<server>__<tool>`), the
    /// deferred tool group (`mcp.<server>`), and the Settings UI all
    /// reference it.
    pub name: String,
    /// Whether the server participates at all. Defaults to `true`; a
    /// disabled server is omitted from the tool-group index entirely
    /// (cheaper than deleting the entry).
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// stdio transport: the executable to spawn. Mutually exclusive with
    /// [`Self::url`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// stdio transport: the arguments passed to [`Self::command`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// stdio transport: environment variable NAMES whose values are passed
    /// to the child process. Entries must be bare names — never `K=V`
    /// pairs — so no secret can leak into this file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// Remote transport: the server URL (streamable HTTP / SSE). Mutually
    /// exclusive with [`Self::command`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Remote transport: header name → environment variable NAME whose
    /// value is sent as that header (e.g. `Authorization = "MY_TOKEN"`).
    /// Values are resolved at connect time, never persisted here.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers_env: BTreeMap<String, String>,
    /// Optional auth scheme for REMOTE servers: `"oauth"` enables the
    /// OAuth 2.1 authorization-code flow (PKCE S256, loopback redirect,
    /// tokens in keys.toml, automatic refresh on 401). `None` = no auth
    /// beyond `headers_env`. stdio servers must not set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    /// Pre-registered OAuth client id (required when `auth = "oauth"`) —
    /// dynamic client registration is not supported, so the id comes from
    /// the server operator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// OAuth scope(s) requested during authorization (space-joined).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// Per-server AUTO-APPROVE trust: when true, the server's tools become
    /// AutoRun for APPROVAL purposes (no per-call prompt, subject to the
    /// user's safety mode) — default deny preserved. Trust relaxes ONLY
    /// the approval gate: the plan-first state gates never widen, so even
    /// a trusted server's tools stay hidden in Planning/Complete/research
    /// (the ToolFilter excludes `mcp__` names from the AutoRun visibility
    /// arm). Opt-in per server — only for servers you trust.
    #[serde(default)]
    pub trusted: bool,
    /// Optional connection IDLE timeout in seconds: a cached connection
    /// that has seen no traffic for this long is dropped and re-established
    /// lazily on the next call (no background task). `None` = keep
    /// connections until they fail (the default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_timeout_secs: Option<u64>,
}

/// Serde default for flags that stay on when the key is absent from older
/// `mcp.toml` files (back-compat).
fn default_true() -> bool {
    true
}

/// An env-var NAME must be non-empty and contain no `=` or whitespace (a
/// `K=V` pair would smuggle a value into the config file; a spaced string
/// like `Bearer abc` is a value, not a name). This file stores names only.
fn is_bare_env_name(s: &str) -> bool {
    !s.is_empty() && !s.contains('=') && !s.chars().any(char::is_whitespace)
}

impl McpServerDef {
    /// Validate this entry in isolation: non-empty name, exactly one
    /// transport set (not both, not neither), a non-blank `command`/`url`
    /// when present, and bare env-var names in `env`/`headers_env`.
    ///
    /// Cross-entry rules (duplicate names) are [`validate_set`]'s job.
    pub fn validate(&self) -> Result<()> {
        if self.name.trim().is_empty() {
            return Err(Error::Config("mcp server name must not be empty".into()));
        }
        // The name feeds the deferred group (mcp.<name>) and the tool
        // namespacing (mcp__<name>__<tool>): whitespace would make the
        // group unrevealable (load_tools trims its argument), and '__'
        // would alias other servers' tool names (review LOW 3).
        if self.name.chars().any(char::is_whitespace) {
            return Err(Error::Config(format!(
                "mcp server name '{}' must not contain whitespace",
                self.name
            )));
        }
        if self.name.contains("__") {
            return Err(Error::Config(format!(
                "mcp server name '{}' must not contain '__' — it would alias tool names \
                 under the mcp__<server>__<tool> namespacing",
                self.name
            )));
        }
        match (self.command.as_deref(), self.url.as_deref()) {
            (Some(_), Some(_)) => {
                return Err(Error::Config(format!(
                    "mcp server '{}' sets both command and url — pick exactly one transport",
                    self.name
                )));
            }
            (None, None) => {
                return Err(Error::Config(format!(
                    "mcp server '{}' sets neither command nor url — one transport is required",
                    self.name
                )));
            }
            (Some(c), _) if c.trim().is_empty() => {
                return Err(Error::Config(format!(
                    "mcp server '{}' has an empty command",
                    self.name
                )));
            }
            (_, Some(u)) if u.trim().is_empty() => {
                return Err(Error::Config(format!(
                    "mcp server '{}' has an empty url",
                    self.name
                )));
            }
            _ => {}
        }
        // Auth scheme validation (review-safe defaults): only "oauth" is
        // known, it is remote-only, and it requires a pre-registered
        // client id (no dynamic client registration in v1).
        match self.auth.as_deref() {
            None | Some("") => {}
            Some("oauth") => {
                if !self.is_remote() {
                    return Err(Error::Config(format!(
                        "mcp server '{}': auth = \"oauth\" requires the remote (url) transport",
                        self.name
                    )));
                }
                if self.client_id.as_deref().unwrap_or("").trim().is_empty() {
                    return Err(Error::Config(format!(
                        "mcp server '{}': auth = \"oauth\" requires a client_id \
                         (dynamic client registration is not supported)",
                        self.name
                    )));
                }
            }
            Some(other) => {
                return Err(Error::Config(format!(
                    "mcp server '{}': unknown auth scheme '{other}' (supported: \"oauth\")",
                    self.name
                )));
            }
        }
        // An idle timeout of 0 would expire connections instantly (every
        // call reconnects) — a configuration mistake, rejected here.
        if let Some(secs) = self.idle_timeout_secs {
            if secs == 0 {
                return Err(Error::Config(format!(
                    "mcp server '{}': idle_timeout_secs must be >= 1",
                    self.name
                )));
            }
        }
        for e in &self.env {
            if !is_bare_env_name(e) {
                return Err(Error::Config(format!(
                    "mcp server '{}': env entries are variable NAMES (got '{e}'); values are \
                     resolved from the environment at connect time",
                    self.name
                )));
            }
        }
        for (header, var) in &self.headers_env {
            if header.trim().is_empty() {
                return Err(Error::Config(format!(
                    "mcp server '{}': headers_env has an empty header name",
                    self.name
                )));
            }
            if !is_bare_env_name(var) {
                return Err(Error::Config(format!(
                    "mcp server '{}': headers_env['{header}'] must be a variable NAME (got \
                     '{var}'), not a value",
                    self.name
                )));
            }
        }
        Ok(())
    }

    /// Whether this is a stdio (spawn) server — `true` when `command` is the
    /// set transport.
    pub fn is_stdio(&self) -> bool {
        self.command.is_some()
    }

    /// Whether this is a remote (url) server.
    pub fn is_remote(&self) -> bool {
        self.url.is_some()
    }
}

/// Validate a whole server set: every entry passes [`McpServerDef::validate`]
/// and names are unique (the name keys tool names, groups, and the UI — a
/// duplicate would make both entries unreachable/ambiguous).
pub fn validate_set(servers: &[McpServerDef]) -> Result<()> {
    for s in servers {
        s.validate()?;
    }
    for (i, a) in servers.iter().enumerate() {
        if servers[..i].iter().any(|b| b.name == a.name) {
            return Err(Error::Config(format!(
                "duplicate mcp server name '{}' — names must be unique",
                a.name
            )));
        }
    }
    Ok(())
}

/// Union-merge the global server set with a project's overrides: the
/// PROJECT file wins by name (same philosophy as skills — global
/// baseline, project overrides) and non-colliding global servers are
/// kept. The merged set is what agents see. Project defs come first, so
/// a name collision simply drops the global twin; ordering is otherwise
/// stable (project order, then global order).
pub fn merge_servers(global: &[McpServerDef], project: &[McpServerDef]) -> Vec<McpServerDef> {
    let mut out: Vec<McpServerDef> = Vec::with_capacity(global.len() + project.len());
    for p in project {
        out.push(p.clone());
    }
    for g in global {
        if !out.iter().any(|s| s.name == g.name) {
            out.push(g.clone());
        }
    }
    out
}

/// Preserve global servers that a project override SHADOWS: a project def
/// with the same name hides the global twin from the merged view (so it
/// never appears in the Settings payload) — rewriting the global file from
/// that payload alone would silently DELETE the shadowed baseline. This
/// re-adds global defs whose names are claimed by a project def and not
/// already present in the new global set (pure — unit-tested).
pub fn restore_shadowed_globals(
    new_global: &[McpServerDef],
    project: &[McpServerDef],
    current_global: &[McpServerDef],
) -> Vec<McpServerDef> {
    let mut out: Vec<McpServerDef> = new_global.to_vec();
    let project_names: std::collections::BTreeSet<&str> =
        project.iter().map(|s| s.name.as_str()).collect();
    // Owned clones: the set must not borrow `out` across the pushes below.
    let out_names: std::collections::BTreeSet<String> =
        out.iter().map(|s| s.name.clone()).collect();
    for g in current_global {
        if project_names.contains(g.name.as_str()) && !out_names.contains(g.name.as_str()) {
            out.push(g.clone());
        }
    }
    out
}

/// Load servers from `mcp.toml`, or return an empty list if the file is
/// missing or empty.
///
/// Forgiving on semantic errors, consistent with the skill loader: an entry
/// that fails [`McpServerDef::validate`] (or a later duplicate name) is
/// logged to stderr and skipped, so one bad entry never takes down app
/// startup. The Settings UI validates strictly on save.
pub fn load_or_default(path: &Path) -> Result<Vec<McpServerDef>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(path)?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let file: McpFile = toml::from_str(&text)?;
    let mut out: Vec<McpServerDef> = Vec::with_capacity(file.server.len());
    for s in file.server {
        let dup = out.iter().any(|e| e.name == s.name);
        if dup || s.validate().is_err() {
            eprintln!(
                "config: skipping invalid mcp.toml server entry '{}' (logged above if \
                 malformed); fix or remove it in Settings",
                s.name
            );
            continue;
        }
        out.push(s);
    }
    Ok(out)
}

/// Write servers to `mcp.toml` as `[[server]]` tables. Validates the whole
/// set first ([`validate_set`]) so an invalid entry is rejected BEFORE the
/// file is touched, then atomically rewrites the file (temp-file + rename —
/// a crash mid-write leaves the previous file intact).
pub fn save(path: &Path, servers: &[McpServerDef]) -> Result<()> {
    validate_set(servers)?;
    let file = McpFile {
        server: servers.to_vec(),
    };
    let text = toml::to_string_pretty(&file)?;
    crate::config::write_atomic(path, &text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// A minimal valid stdio server entry.
    fn stdio_server() -> McpServerDef {
        toml::from_str(
            r#"name = "filesystem"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem"]
env = ["SOME_TOKEN"]
"#,
        )
        .unwrap()
    }

    /// A minimal valid remote server entry.
    fn remote_server() -> McpServerDef {
        toml::from_str(
            r#"name = "github"
url = "https://mcp.example.com/mcp"

[headers_env]
Authorization = "GITHUB_MCP_TOKEN"
"#,
        )
        .unwrap()
    }

    #[test]
    fn parses_stdio_entry() {
        let s = stdio_server();
        s.validate().unwrap();
        assert_eq!(s.name, "filesystem");
        assert!(s.enabled, "enabled defaults to true");
        assert!(s.is_stdio());
        assert_eq!(s.args.len(), 2);
        assert_eq!(s.env, vec!["SOME_TOKEN".to_string()]);
    }

    #[test]
    fn parses_remote_entry() {
        let s = remote_server();
        s.validate().unwrap();
        assert!(!s.is_stdio());
        assert_eq!(s.url.as_deref(), Some("https://mcp.example.com/mcp"));
        assert_eq!(
            s.headers_env.get("Authorization").map(String::as_str),
            Some("GITHUB_MCP_TOKEN")
        );
    }

    #[test]
    fn missing_file_loads_empty() {
        let dir = tempdir().unwrap();
        let servers = load_or_default(&dir.path().join("mcp.toml")).unwrap();
        assert!(servers.is_empty());
    }

    #[test]
    fn rejects_both_transports() {
        let mut s = stdio_server();
        s.url = Some("https://example.com".into());
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("both command and url"), "{err}");
    }

    #[test]
    fn rejects_neither_transport() {
        let mut s = stdio_server();
        s.command = None;
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("neither command nor url"), "{err}");
    }

    #[test]
    fn rejects_blank_command_and_url() {
        let mut s = stdio_server();
        s.command = Some("   ".into());
        assert!(s.validate().is_err());
        let mut r = remote_server();
        r.url = Some("".into());
        assert!(r.validate().is_err());
    }

    #[test]
    fn trusted_and_idle_timeout_fields_parse_and_validate() {
        // trusted defaults to false (default deny); idle_timeout_secs
        // defaults to None (connections live until failure).
        let s: McpServerDef = toml::from_str("name = \"fs\"\ncommand = \"npx\"\n").unwrap();
        assert!(!s.trusted, "trusted defaults to false");
        assert_eq!(s.idle_timeout_secs, None);

        // Both round-trip through save/load.
        let dir = tempdir().unwrap();
        let path = dir.path().join("mcp.toml");
        let mut s = stdio_server();
        s.trusted = true;
        s.idle_timeout_secs = Some(60);
        save(&path, &[s]).unwrap();
        let loaded = load_or_default(&path).unwrap();
        assert!(loaded[0].trusted);
        assert_eq!(loaded[0].idle_timeout_secs, Some(60));

        // A zero idle timeout would expire connections instantly — rejected.
        let mut s = stdio_server();
        s.idle_timeout_secs = Some(0);
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("idle_timeout_secs"), "{err}");
    }

    #[test]
    fn rejects_env_pairs_and_values() {
        // `K=V` in env smuggles a value into the file — rejected.
        let mut s = stdio_server();
        s.env = vec!["TOKEN=secret".into()];
        let err = s.validate().unwrap_err().to_string();
        assert!(err.contains("variable NAMES"), "{err}");
        // A literal value in headers_env is likewise rejected.
        let mut r = remote_server();
        r.headers_env
            .insert("Authorization".into(), "Bearer abc".into());
        assert!(r.validate().is_err());
    }

    #[test]
    fn rejects_empty_and_duplicate_names() {
        let mut s = stdio_server();
        s.name = "  ".into();
        assert!(s.validate().is_err());

        // LOW 3: names feed mcp.<name> + mcp__<name>__<tool> — whitespace
        // (unrevealable group) and '__' (tool-name aliasing) are rejected.
        let mut spaced = stdio_server();
        spaced.name = "fs ".into();
        let err = spaced.validate().unwrap_err().to_string();
        assert!(err.contains("whitespace"), "{err}");
        let mut dunder = stdio_server();
        dunder.name = "a__b".into();
        let err = dunder.validate().unwrap_err().to_string();
        assert!(err.contains("__"), "{err}");

        let a = stdio_server();
        let mut b = remote_server();
        b.name = a.name.clone();
        let err = validate_set(&[a, b]).unwrap_err().to_string();
        assert!(err.contains("duplicate"), "{err}");
    }

    #[test]
    fn merge_servers_project_wins_and_keeps_globals() {
        // Project overrides shadow the global twin by name; non-colliding
        // globals are kept; project defs come first.
        let g1 = toml::from_str::<McpServerDef>("name = \"shared\"\ncommand = \"global-npx\"\n")
            .unwrap();
        let g2 =
            toml::from_str::<McpServerDef>("name = \"only-global\"\ncommand = \"g\"\n").unwrap();
        let p1 = toml::from_str::<McpServerDef>("name = \"shared\"\ncommand = \"project-npx\"\n")
            .unwrap();
        let p2 = toml::from_str::<McpServerDef>("name = \"only-project\"\nurl = \"https://p\"\n")
            .unwrap();
        let merged = merge_servers(&[g1, g2.clone()], &[p1, p2]);
        let names: Vec<&str> = merged.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["shared", "only-project", "only-global"]);
        assert_eq!(
            merged
                .iter()
                .find(|s| s.name == "shared")
                .unwrap()
                .command
                .as_deref(),
            Some("project-npx"),
            "the project override wins"
        );
        // Disabled state survives the merge.
        let disabled =
            toml::from_str::<McpServerDef>("name = \"off\"\ncommand = \"x\"\nenabled = false\n")
                .unwrap();
        let merged = merge_servers(&[disabled], &[]);
        assert!(!merged[0].enabled);
        // Empty project file = the global set verbatim.
        assert_eq!(merge_servers(&[g2], &[]).len(), 1);
    }

    #[test]
    fn restore_shadowed_globals_keeps_shadowed_baselines() {
        // HIGH 1: a project override shadows the global twin — the twin is
        // invisible in the Settings payload, and a naive whole-file rewrite
        // would delete it from the global file (every OTHER project would
        // lose the server). Restore the baseline for shadowed names only.
        let global_shared: McpServerDef =
            toml::from_str("name = \"shared\"\ncommand = \"global-npx\"\n").unwrap();
        let global_other: McpServerDef =
            toml::from_str("name = \"other\"\ncommand = \"g\"\n").unwrap();
        let project_shared: McpServerDef =
            toml::from_str("name = \"shared\"\ncommand = \"project-npx\"\n").unwrap();
        let new_global: McpServerDef =
            toml::from_str("name = \"brand-new\"\ncommand = \"n\"\n").unwrap();

        let restored = restore_shadowed_globals(
            &[new_global],
            &[project_shared],
            &[global_shared, global_other],
        );
        let names: Vec<&str> = restored.iter().map(|s| s.name.as_str()).collect();
        // "shared" (shadowed) comes back; "other" (dropped by the user in
        // the UI) does NOT resurrect; the new server stays.
        assert_eq!(names, vec!["brand-new", "shared"]);

        // No project overrides → nothing restored (pure global save).
        let global_shared: McpServerDef =
            toml::from_str("name = \"shared\"\ncommand = \"global-npx\"\n").unwrap();
        let restored = restore_shadowed_globals(&[], &[], &[global_shared]);
        assert!(restored.is_empty());
    }

    #[test]
    fn load_skips_invalid_entries_and_later_duplicates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mcp.toml");
        std::fs::write(
            &path,
            r#"[[server]]
name = "good"
command = "npx"

[[server]]
name = "bad"
command = "run-me"
url = "https://also-set.example.com"

[[server]]
name = "good"
command = "other"

[[server]]
name = "remote"
url = "https://mcp.example.com/mcp"
"#,
        )
        .unwrap();
        let servers = load_or_default(&path).unwrap();
        let names: Vec<&str> = servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["good", "remote"], "invalid + duplicate skipped");
    }

    #[test]
    fn round_trips_save_and_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mcp.toml");
        let servers = vec![stdio_server(), remote_server()];
        save(&path, &servers).unwrap();
        let loaded = load_or_default(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "filesystem");
        assert_eq!(loaded[1].name, "github");
        assert_eq!(loaded[1].headers_env["Authorization"], "GITHUB_MCP_TOKEN");
        // enabled round-trips as an explicit key (default true either way).
        assert!(loaded.iter().all(|s| s.enabled));
    }

    #[test]
    fn save_rejects_invalid_set_before_touching_disk() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mcp.toml");
        let mut bad = stdio_server();
        bad.url = Some("https://example.com".into());
        assert!(save(&path, &[bad]).is_err());
        assert!(!path.exists(), "an invalid set never creates the file");
    }
}
