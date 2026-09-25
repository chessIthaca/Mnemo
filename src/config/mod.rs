// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Global configuration — four-file loader.
//!
//! Global config lives in `~/.mnemo/` and is split across these files:
//! - `config.toml` — general preferences (default provider/model, safety, UI)
//! - `endpoints.toml` — endpoint definitions (base_url, kind, models)
//! - `keys.toml` — secrets (API keys only; never logged)
//! - `mcp.toml` — configured MCP servers (env-var names, never values)
//! - `projects.toml` — the known-projects registry (name → path)
//!
//! `Config::load()` merges all of them. `keys.toml` is loaded last and never
//! included in `Debug` output.

pub mod endpoints;
pub mod general;
pub mod keys;
pub mod mcp;
pub mod patch;
pub mod projects;
pub mod settings_dto;

pub use endpoints::{Endpoint, EndpointKind, ModelSpec, PricingEntry};
pub use general::{
    EmbeddingModel, GeneralConfig, GeneralSection, GitConfig, LayaConfig, LayaMode, MarkdownConfig,
    ModelRef, ModelsConfig, SafetyMode, ShellFilterConfig, ShellFilterOverride, TraceConfig,
    VisionModel, EMBEDDING_MODEL_SENTINEL_HASH,
};
pub use keys::KeyStore;
pub use mcp::McpServerDef;
pub use projects::{ProjectEntry, ProjectRegistry};

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::Result;

/// The merged global configuration, loaded from `~/.mnemo/`.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// General preferences from `config.toml`.
    pub general: GeneralConfig,
    /// Endpoint definitions from `endpoints.toml`.
    pub endpoints: Vec<Endpoint>,
    /// Per-model pricing from `endpoints.toml` (`[[pricing]]`).
    pub pricing: Vec<PricingEntry>,
    /// API keys from `keys.toml`. Kept separate so it is never accidentally logged.
    pub keys: KeyStore,
    /// MCP server definitions from `mcp.toml` (names + transports; env-var
    /// names only — no secret values ever live in config).
    pub mcp: Vec<McpServerDef>,
    /// Known-projects registry from `projects.toml`.
    pub projects: ProjectRegistry,
}

impl Config {
    /// Load all global config files from the given config directory.
    ///
    /// Missing files are not an error — each file is optional and defaults to
    /// an empty value. This lets a fresh install work with no config at all.
    ///
    /// Before reading, recovers any interrupted atomic write: if a live file is
    /// missing but a sibling `.bak` or committed `.tmp` remains (Windows
    /// crash window), that survivor is restored into place.
    pub fn load(config_dir: &Path) -> Result<Self> {
        for name in ["config.toml", "endpoints.toml", "keys.toml", "mcp.toml"] {
            recover_interrupted_write(&config_dir.join(name));
        }
        let general = GeneralConfig::load_or_default(&config_dir.join("config.toml"))?;
        let (endpoints, pricing) = endpoints::load_or_default(&config_dir.join("endpoints.toml"))?;
        let keys = KeyStore::load_or_default(&config_dir.join("keys.toml"))?;
        let mcp = mcp::load_or_default(&config_dir.join("mcp.toml"))?;
        let projects = ProjectRegistry::load_or_default(&config_dir.join("projects.toml"))?;
        Ok(Self {
            general,
            endpoints,
            pricing,
            keys,
            mcp,
            projects,
        })
    }

    /// Look up an endpoint by name.
    pub fn endpoint(&self, name: &str) -> Option<&Endpoint> {
        self.endpoints.iter().find(|e| e.name == name)
    }

    /// Look up the pricing entry for a model (by model id).
    pub fn pricing_for(&self, model: &str) -> Option<&PricingEntry> {
        self.pricing.iter().find(|p| p.model == model)
    }

    /// Look up the API key for an endpoint by its name.
    pub fn key_for(&self, endpoint_name: &str) -> Option<&str> {
        self.keys.get(endpoint_name)
    }

    /// Resolve a [`ModelRef`] for a per-context model override key, validating
    /// that its endpoint exists in `endpoints.toml`.
    ///
    /// Returns the [`ModelRef`] only when its `endpoint` names a configured
    /// endpoint — a dangling reference (e.g. an endpoint that was since
    /// deleted) is dropped so the caller falls back to the default model
    /// rather than building a provider against a missing endpoint. Used by the
    /// [`ModelResolver`](crate::model_resolver) at turn time.
    pub fn resolve_model_ref(&self, model: Option<&ModelRef>) -> Option<ModelRef> {
        model.and_then(|m| {
            self.endpoint(&m.endpoint).map(|_| ModelRef {
                endpoint: m.endpoint.clone(),
                model: m.model.clone(),
                reasoning_effort: m.reasoning_effort.clone(),
            })
        })
    }

    /// Resolve the default endpoint + model from config, falling back to the
    /// first endpoint if `default_provider` is unset.
    pub fn default_endpoint(&self) -> Option<&Endpoint> {
        if let Some(name) = &self.general.general.default_provider {
            if let Some(ep) = self.endpoint(name) {
                return Some(ep);
            }
        }
        self.endpoints.first()
    }

    /// Persist all four editable config files to the given config directory:
    /// `config.toml` (general), `endpoints.toml` (endpoints + pricing),
    /// `keys.toml` (API keys), and `mcp.toml` (MCP servers).
    /// `projects.toml` is owned by the project registry
    /// and is not touched here. Each file is fully rewritten from the in-memory
    /// `Config`, so the schema is the source of truth (unknown keys are
    /// dropped).
    ///
    /// **Transactional write (best-effort on plain FS):**
    /// 1. Serialize all four payloads first (serde failure never touches disk).
    /// 2. Write all four `*.tmp` staging files.
    /// 3. Snapshot each existing target to a sibling `*.bak` (if present).
    /// 4. Rename each `*.tmp` into place.
    /// 5. On any rename failure, restore every file that already had a `.bak`
    ///    and surface the error — so a mid-commit crash leaves a recoverable
    ///    previous generation rather than a mixed old/new set.
    /// 6. On full success, delete the `.bak` files.
    pub fn save_all(&self, config_dir: &Path) -> Result<()> {
        // Serialize everything *before* touching disk so a serde failure
        // never partially updates files.
        let config_text = toml::to_string_pretty(&self.general)?;
        let endpoints_text = {
            let file = endpoints::EndpointsFile {
                endpoint: self.endpoints.clone(),
                pricing: self.pricing.clone(),
            };
            toml::to_string_pretty(&file)?
        };
        let keys_text = {
            // MCP OAuth tokens live in keys.toml but are managed by the MCP
            // flow, NOT by the Settings keys UI — a plain rewrite from
            // memory would wipe them on every save. Merge: preserve on-disk
            // `mcp-*` entries (the OAuth namespace) that memory lacks;
            // everything else comes from memory (user edits win, deletions
            // still stick for endpoint keys).
            let mut keys = self.keys.clone();
            let keys_path = config_dir.join("keys.toml");
            if let Ok(disk) = KeyStore::load_or_default(&keys_path) {
                for (name, value) in disk.iter() {
                    if name.starts_with("mcp-") && keys.get(name).is_none() {
                        keys.insert(name.to_string(), value.to_string());
                    }
                }
            }
            keys.to_toml_string()?
        };
        let mcp_text = {
            let file = mcp::McpFile {
                server: self.mcp.clone(),
            };
            toml::to_string_pretty(&file)?
        };

        let paths = [
            config_dir.join("config.toml"),
            config_dir.join("endpoints.toml"),
            config_dir.join("keys.toml"),
            config_dir.join("mcp.toml"),
        ];
        let texts = [
            config_text.as_str(),
            endpoints_text.as_str(),
            keys_text.as_str(),
            mcp_text.as_str(),
        ];

        // Stage all temps first — failure here leaves live files untouched.
        for (path, text) in paths.iter().zip(texts.iter()) {
            write_atomic_staged(path, text)?;
        }

        // Snapshot existing targets to .bak so we can roll back mid-rename.
        for path in &paths {
            if path.exists() {
                let bak = backup_path(path);
                fs::copy(path, &bak)?;
            }
        }

        // Commit renames; on failure restore any file that has a backup.
        let mut committed: Vec<&Path> = Vec::new();
        for path in &paths {
            if let Err(e) = commit_atomic_rename(path) {
                // Roll back everything already committed + leave uncommitted
                // temps cleaned up best-effort. Restore failures are logged
                // and surfaced in the returned error (quality review LOW 6) —
                // a silent rollback failure leaves a half-renamed config set
                // with no trace.
                let restore_failures = rollback_partial_commit(&paths, &committed, path);
                return Err(describe_rollback_failure(e, &restore_failures));
            }
            committed.push(path.as_path());
        }

        // Success — drop backups.
        for path in &paths {
            let _ = fs::remove_file(backup_path(path));
        }
        Ok(())
    }
}

/// Path of the staging file for `path` (`foo.toml` → `foo.toml.tmp`).
fn staging_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| "file".into());
    name.push(".tmp");
    path.with_file_name(name)
}

/// Path of the rollback backup for `path` (`foo.toml` → `foo.toml.bak`).
fn backup_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| "file".into());
    name.push(".bak");
    path.with_file_name(name)
}

/// Write `contents` to `path.tmp` (does not rename yet).
fn write_atomic_staged(path: &Path, contents: &str) -> Result<()> {
    let tmp = staging_path(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&tmp, contents)?;
    Ok(())
}

/// Rename `path.tmp` → `path` without a delete-before-rename gap.
///
/// Strategy:
/// 1. Try a direct `rename(tmp → path)` (works on Unix when replacing).
/// 2. If that fails and `path` exists, **move** the live file aside to a
///    unique `*.bak.commit` (not delete), then rename tmp into place. If the
///    second rename fails, move the aside file back.
///
/// Never `remove_file(path)` before a successful replace — a crash between
/// delete and rename used to leave config/keys missing with no auto-recovery.
fn commit_atomic_rename(path: &Path) -> Result<()> {
    let tmp = staging_path(path);
    // Fast path: replace in one rename when the OS allows it.
    match fs::rename(&tmp, path) {
        Ok(()) => return Ok(()),
        Err(_) if !path.exists() => {
            // Target missing — retry plain rename (tmp should still be there).
            fs::rename(&tmp, path)?;
            return Ok(());
        }
        Err(_) => {}
    }

    // Windows-style: cannot rename over existing file. Move live → aside,
    // then tmp → live. Aside is NOT the durable `.bak` from save_all (that
    // already holds the pre-save snapshot); this is a short-lived pivot.
    let aside = {
        let mut name = path
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_else(|| "file".into());
        name.push(".bak.commit");
        path.with_file_name(name)
    };
    // Clear a stale aside from a previous crash.
    let _ = fs::remove_file(&aside);
    fs::rename(path, &aside)?;
    match fs::rename(&tmp, path) {
        Ok(()) => {
            let _ = fs::remove_file(&aside);
            Ok(())
        }
        Err(e) => {
            // Put the previous live file back.
            let _ = fs::rename(&aside, path);
            Err(e.into())
        }
    }
}

/// Restore `path` from its `.bak` sibling if the backup exists.
///
/// Used by `save_all` rollback: the live file may already hold the *new*
/// contents after a partial commit, so we always replace from `.bak` when
/// present (unlike [`recover_interrupted_write`], which only fills a *missing*
/// live file).
fn restore_from_backup(path: &Path) -> Result<()> {
    let bak = backup_path(path);
    if !bak.exists() {
        return Ok(());
    }
    // Move bak into place without delete-before-rename when possible.
    match fs::rename(&bak, path) {
        Ok(()) => Ok(()),
        Err(_) if path.exists() => {
            let aside = {
                let mut name = path
                    .file_name()
                    .map(|s| s.to_os_string())
                    .unwrap_or_else(|| "file".into());
                name.push(".bak.restore");
                path.with_file_name(name)
            };
            let _ = fs::remove_file(&aside);
            fs::rename(path, &aside)?;
            match fs::rename(&bak, path) {
                Ok(()) => {
                    let _ = fs::remove_file(&aside);
                    Ok(())
                }
                Err(e) => {
                    let _ = fs::rename(&aside, path);
                    Err(e.into())
                }
            }
        }
        Err(e) => Err(e.into()),
    }
}

/// Roll back a partially-committed multi-file save: restore every already
/// committed path (and the failed one) from its `.bak`, and clean the
/// leftover staging temps best-effort.
///
/// Each restore failure is LOGGED and collected in the returned list (path +
/// error) so the caller can surface it — a silent rollback failure leaves a
/// half-renamed config set with no trace (quality review LOW 6). Split from
/// `save_all` so the collection is unit-testable.
fn rollback_partial_commit(
    paths: &[PathBuf],
    committed: &[&Path],
    failed: &Path,
) -> Vec<(String, crate::error::Error)> {
    rollback_with_restore(&restore_from_backup, paths, committed, failed)
}

/// The rollback loop, parameterized over the restore function so tests can
/// inject restore failures deterministically (cross-platform, no filesystem
/// failure tricks — real rename-failure injection is not portable: Windows
/// rescues or allows directory-over-file renames that Unix rejects).
fn rollback_with_restore(
    restore: &dyn Fn(&Path) -> Result<()>,
    paths: &[PathBuf],
    committed: &[&Path],
    failed: &Path,
) -> Vec<(String, crate::error::Error)> {
    let mut failures = Vec::new();
    // Roll back everything already committed, then the failed path itself (a
    // bak may exist — the rename may have deleted the original before
    // failing).
    for path in committed.iter().copied().chain(std::iter::once(failed)) {
        if let Err(e) = restore(path) {
            eprintln!("mnemo: config rollback failed for {}: {e}", path.display());
            failures.push((path.display().to_string(), e));
        }
    }
    // Clean leftover temps for uncommitted files.
    for p in paths {
        let _ = fs::remove_file(staging_path(p));
    }
    failures
}

/// The user-facing error for a failed multi-file save whose rollback also
/// failed: the original commit error plus each restore failure, so the user
/// sees the config set may be half-committed (quality review LOW 6). With no
/// restore failures the original commit error is returned unchanged.
fn describe_rollback_failure(
    commit_err: crate::error::Error,
    restore_failures: &[(String, crate::error::Error)],
) -> crate::error::Error {
    if restore_failures.is_empty() {
        return commit_err;
    }
    let details = restore_failures
        .iter()
        .map(|(path, e)| format!("{path}: {e}"))
        .collect::<Vec<_>>()
        .join("; ");
    crate::error::Error::Config(format!(
        "{commit_err}; additionally, rollback failed — the config set may be \
         half-committed: {details}"
    ))
}

/// Best-effort recovery after a crash mid-atomic-write.
///
/// Priority when the live `path` is missing:
/// 1. `path.bak.commit` (pivot from commit_atomic_rename)
/// 2. `path.bak` (pre-save snapshot from save_all)
/// 3. `path.tmp` (staged new contents — better than empty defaults)
fn recover_interrupted_write(path: &Path) {
    if path.exists() {
        // Clean stale pivot/tmp left beside a good live file.
        let mut aside_name = path
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_else(|| "file".into());
        aside_name.push(".bak.commit");
        let _ = fs::remove_file(path.with_file_name(aside_name));
        return;
    }
    let mut aside_name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| "file".into());
    aside_name.push(".bak.commit");
    let aside = path.with_file_name(&aside_name);
    if aside.exists() {
        let _ = fs::rename(&aside, path);
        return;
    }
    let bak = backup_path(path);
    if bak.exists() {
        let _ = fs::rename(&bak, path);
        return;
    }
    let tmp = staging_path(path);
    if tmp.exists() {
        let _ = fs::rename(&tmp, path);
    }
}

/// Write `contents` to `path` via temp file + rename (single-file atomic).
pub fn write_atomic(path: &Path, contents: &str) -> Result<()> {
    write_atomic_staged(path, contents)?;
    commit_atomic_rename(path)?;
    Ok(())
}

/// Resolve a home-scoped config directory by name (`.mnemo` / `.myharness`).
///
/// Uses the user's home directory. On all platforms this is `$HOME` or the
/// equivalent; we fall back to a relative path if neither is set.
fn config_dir_named(dir_name: &str) -> PathBuf {
    if let Some(dir) = directories::BaseDirs::new() {
        return dir.home_dir().join(dir_name);
    }
    // Fallback: $HOME or current dir.
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(dir_name);
    }
    if let Ok(userprofile) = std::env::var("USERPROFILE") {
        return PathBuf::from(userprofile).join(dir_name);
    }
    PathBuf::from(dir_name)
}

/// Resolve the global config directory (`~/.mnemo/`).
///
/// Renamed from `~/.myharness` in the myharness→Mnemo rename; existing
/// installs are moved over by [`migrate_legacy_config_dir`].
pub fn global_config_dir() -> PathBuf {
    config_dir_named(".mnemo")
}

/// The pre-rename global config directory (`~/.myharness/`).
fn legacy_config_dir() -> PathBuf {
    config_dir_named(".myharness")
}

/// One-time migration for installs that predate the Mnemo rename: move the
/// legacy `~/.myharness` config dir to `~/.mnemo`.
///
/// Call once at process startup, BEFORE the first config load. A no-op when
/// the new dir already exists (never clobbers a fresh config) or the legacy
/// dir is gone. Failures are logged and swallowed — a failed migration must
/// never block startup; the app just starts with a fresh (empty) config dir.
pub fn migrate_legacy_config_dir() {
    migrate_legacy_dir(&legacy_config_dir(), &global_config_dir());
}

/// Rename `old` to `new` when `new` does not exist and `old` does. Returns
/// whether a migration happened. Split from [`migrate_legacy_config_dir`] so
/// tests can point it at tempdirs.
fn migrate_legacy_dir(old: &Path, new: &Path) -> bool {
    if new.exists() || !old.exists() {
        return false;
    }
    match fs::rename(old, new) {
        Ok(()) => true,
        Err(e) => {
            eprintln!(
                "mnemo: could not migrate legacy config dir {old:?} → {new:?}: {e} — \
                 starting with a fresh config dir"
            );
            false
        }
    }
}
/// The path of the one-shot "pending project" marker file.
///
/// When the user switches projects from the UI, the chosen project path is
/// written here, then the app is restarted. On restart, [`build_brain`] reads
/// (and consumes) this marker to know which project to open — because a Tauri
/// restart reuses the original argv, the marker is the side-channel that
/// carries the selection across the restart boundary.
///
/// Lives in the global config dir (alongside `keys.toml` / `endpoints.toml` /
/// `projects.toml`) so it survives the restart regardless of the cwd.
pub fn pending_project_path() -> PathBuf {
    global_config_dir().join(".pending_project")
}

/// Write the chosen project path to the pending-project marker file so the
/// next process startup (after [`tauri::process::restart`]) opens it. Uses an
/// atomic write so a crash mid-write can't corrupt the marker.
pub fn write_pending_project(path: &Path) -> Result<()> {
    write_marker_at(&pending_project_path(), path)
}

/// Write a project path to an explicit marker `file` (atomic). Split out from
/// [`write_pending_project`] so the consume-on-read semantics can be unit
/// tested against a temp path instead of the real global config dir.
fn write_marker_at(file: &Path, path: &Path) -> Result<()> {
    let text = path.to_string_lossy();
    write_atomic(file, &text)
}

/// Consume the pending-project marker: if the marker file exists and holds a
/// non-empty path, delete it and return `Some(path)`. A missing or empty
/// marker yields `None` (and leaves no file behind). The marker is always
/// consumed on read so a stale selection can never bleed into a later startup.
pub fn take_pending_project() -> Option<PathBuf> {
    consume_marker_at(&pending_project_path())
}

/// Read + consume a marker at an explicit `file` path. Returns `None` when the
/// file is missing or empty (cleaning up an empty file so it doesn't linger).
/// Split out from [`take_pending_project`] for unit testing.
fn consume_marker_at(file: &Path) -> Option<PathBuf> {
    let text = match fs::read_to_string(file) {
        Ok(t) => t,
        Err(_) => return None,
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        // Clean up an empty marker so it doesn't linger.
        let _ = fs::remove_file(file);
        return None;
    }
    // Consume the marker before returning so a failure downstream can't
    // re-trigger the switch on the next startup.
    let _ = fs::remove_file(file);
    Some(PathBuf::from(trimmed))
}
/// Peek at the pending-project marker WITHOUT consuming it: `Some(path)`
/// when the marker exists and holds a non-empty path, `None` otherwise.
///
/// Used at window creation to detect the reload half of a switch restart
/// (so the relaunched window can be focused like the one it replaced) — the
/// marker itself is still consumed later by [`take_pending_project`] in
/// `build_brain`.
pub fn peek_pending_project() -> Option<PathBuf> {
    read_marker_at(&pending_project_path())
}

/// Read a marker at an explicit `file` path without consuming it. Mirrors
/// [`consume_marker_at`] (same read + trim semantics) but never removes the
/// file. Split out from [`peek_pending_project`] for unit testing.
fn read_marker_at(file: &Path) -> Option<PathBuf> {
    let text = match fs::read_to_string(file) {
        Ok(t) => t,
        Err(_) => return None,
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(PathBuf::from(trimmed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn migrate_legacy_dir_moves_when_new_missing() {
        let dir = tempdir().unwrap();
        let old = dir.path().join(".myharness");
        let new = dir.path().join(".mnemo");
        fs::create_dir_all(&old).unwrap();
        write(&old.join("config.toml"), "x = 1");
        assert!(migrate_legacy_dir(&old, &new));
        assert!(!old.exists());
        assert_eq!(
            fs::read_to_string(new.join("config.toml")).unwrap(),
            "x = 1"
        );
    }

    #[test]
    fn migrate_legacy_dir_keeps_existing_new_dir() {
        let dir = tempdir().unwrap();
        let old = dir.path().join(".myharness");
        let new = dir.path().join(".mnemo");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        write(&old.join("config.toml"), "old = 1");
        write(&new.join("config.toml"), "new = 1");
        assert!(!migrate_legacy_dir(&old, &new));
        assert!(old.exists());
        assert_eq!(
            fs::read_to_string(new.join("config.toml")).unwrap(),
            "new = 1"
        );
    }

    #[test]
    fn migrate_legacy_dir_no_legacy_is_noop() {
        let dir = tempdir().unwrap();
        let old = dir.path().join(".myharness");
        let new = dir.path().join(".mnemo");
        assert!(!migrate_legacy_dir(&old, &new));
        assert!(!new.exists());
    }

    #[test]
    fn loads_all_three_files() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("config.toml"),
            r#"
[general]
default_provider = "openai"
default_model = "gpt-4o"
safety = "approve-each-action"

[context]
summarize_at_fill_rate = 0.5

[ui]
theme = "dark"
show_token_usage = true
"#,
        );
        write(
            &dir.path().join("endpoints.toml"),
            r#"
[[endpoint]]
name = "openai"
kind = "openai"
base_url = "https://api.openai.com/v1/"
models = ["gpt-4o", "gpt-4o-mini"]

[[endpoint]]
name = "ollama-local"
kind = "local"
base_url = "http://localhost:11434/v1/"
models = ["llama3.1", "qwen2.5"]
"#,
        );
        write(
            &dir.path().join("keys.toml"),
            r#"
[openai]
api_key = "sk-test-123"

[ollama-local]
api_key = "dummy"
"#,
        );
        write(
            &dir.path().join("projects.toml"),
            r#"
[[project]]
name = "myproject"
path = "C:/myProject"
"#,
        );

        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(
            cfg.general.general.default_provider.as_deref(),
            Some("openai")
        );
        assert_eq!(cfg.general.general.default_model.as_deref(), Some("gpt-4o"));
        assert_eq!(cfg.general.general.safety, SafetyMode::ApproveEachAction);
        assert_eq!(cfg.endpoints.len(), 2);
        assert_eq!(cfg.endpoint("openai").unwrap().kind, EndpointKind::OpenAI);
        assert_eq!(
            cfg.endpoint("ollama-local").unwrap().kind,
            EndpointKind::Local
        );
        assert_eq!(cfg.key_for("openai"), Some("sk-test-123"));
        assert_eq!(cfg.key_for("ollama-local"), Some("dummy"));
        assert_eq!(cfg.key_for("nonexistent"), None);
        assert_eq!(cfg.projects.len(), 1);
        assert_eq!(cfg.projects.get("myproject").unwrap(), "C:/myProject");
    }

    #[test]
    fn missing_files_are_ok() {
        let dir = tempdir().unwrap();
        // No files at all.
        let cfg = Config::load(dir.path()).unwrap();
        assert!(cfg.endpoints.is_empty());
        assert!(cfg.projects.is_empty());
        assert_eq!(cfg.general.general.safety, SafetyMode::ApproveEachAction); // default
    }

    #[test]
    fn default_endpoint_falls_back_to_first() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("endpoints.toml"),
            r#"
[[endpoint]]
name = "only"
kind = "local"
base_url = "http://localhost:11434/v1/"
models = ["llama3.1"]
"#,
        );
        let cfg = Config::load(dir.path()).unwrap();
        // default_provider unset → first endpoint
        assert_eq!(cfg.default_endpoint().unwrap().name, "only");
    }

    #[test]
    fn default_endpoint_resolves_named() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("config.toml"),
            r#"
[general]
default_provider = "second"
"#,
        );
        write(
            &dir.path().join("endpoints.toml"),
            r#"
[[endpoint]]
name = "first"
kind = "openai"
base_url = "https://api.openai.com/v1/"
models = []

[[endpoint]]
name = "second"
kind = "local"
base_url = "http://localhost:11434/v1/"
models = ["qwen2.5"]
"#,
        );
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.default_endpoint().unwrap().name, "second");
    }

    #[test]
    fn keys_never_in_debug() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("keys.toml"),
            r#"
[openai]
api_key = "sk-super-secret"
"#,
        );
        let cfg = Config::load(dir.path()).unwrap();
        let debug = format!("{:?}", cfg);
        // The secret must not appear in the Debug output of Config.
        assert!(
            !debug.contains("sk-super-secret"),
            "API key leaked in Debug output: {debug}"
        );
        // But it is still accessible via the key store.
        assert_eq!(cfg.key_for("openai"), Some("sk-super-secret"));
    }

    #[test]
    fn save_all_preserves_mcp_oauth_tokens_on_disk() {
        // HIGH 2 regression: MCP OAuth tokens (`mcp-*` keys) are managed by
        // the MCP flow, NOT by the Settings keys UI — a save_all from a
        // Config whose in-memory keys lack them must PRESERVE the on-disk
        // tokens, while ordinary endpoint keys keep memory-wins semantics.
        let dir = tempdir().unwrap();
        let mut config = Config::default();
        config.keys.insert("openai".to_string(), "k1".to_string());
        // Tokens already on disk (written by the OAuth flow) but absent
        // from the in-memory store.
        let keys_path = dir.path().join("keys.toml");
        let mut disk = KeyStore::default();
        disk.insert(
            "mcp-fs".to_string(),
            "{\"access_token\":\"tok\"}".to_string(),
        );
        disk.insert("openai".to_string(), "stale".to_string());
        disk.save(&keys_path).unwrap();

        config.save_all(dir.path()).unwrap();

        let reloaded = KeyStore::load_or_default(&keys_path).unwrap();
        assert_eq!(
            reloaded.get("openai"),
            Some("k1"),
            "memory wins for endpoint keys"
        );
        assert_eq!(
            reloaded.get("mcp-fs"),
            Some("{\"access_token\":\"tok\"}"),
            "OAuth tokens survive a save_all that doesn't know about them"
        );
    }

    #[test]
    fn save_all_round_trips_everything() {
        let dir = tempdir().unwrap();
        // Seed an initial config so load has something to round-trip.
        write(
            &dir.path().join("config.toml"),
            r#"
[general]
default_provider = "openai"
default_model = "gpt-4o"
safety = "auto-read-approve-writes"

[context]
summarize_at_fill_rate = 0.4

[ui]
theme = "dark"
show_token_usage = true
"#,
        );
        write(
            &dir.path().join("endpoints.toml"),
            r#"
[[endpoint]]
name = "openai"
kind = "openai"
base_url = "https://api.openai.com/v1/"
models = ["gpt-4o"]
reasoning_effort = "high"

[[pricing]]
model = "gpt-4o"
input_per_1m = 2.5
output_per_1m = 10.0
cached_per_1m = 1.25
"#,
        );
        write(
            &dir.path().join("keys.toml"),
            r#"
[openai]
api_key = "sk-seed"
"#,
        );

        let cfg = Config::load(dir.path()).unwrap();
        cfg.save_all(dir.path()).unwrap();

        let reloaded = Config::load(dir.path()).unwrap();
        assert_eq!(reloaded.endpoints.len(), 1);
        assert_eq!(reloaded.endpoints[0].name, "openai");
        assert_eq!(
            reloaded.endpoints[0].reasoning_effort.as_deref(),
            Some("high")
        );
        assert_eq!(reloaded.pricing.len(), 1);
        assert_eq!(
            reloaded.general.general.default_provider.as_deref(),
            Some("openai")
        );
        assert_eq!(reloaded.key_for("openai"), Some("sk-seed"));
    }

    #[test]
    fn write_atomic_replaces_file_and_leaves_no_tmp() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("sample.toml");
        write_atomic(&path, "a = 1\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a = 1\n");
        assert!(!staging_path(&path).exists());
        write_atomic(&path, "a = 2\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "a = 2\n");
        assert!(!staging_path(&path).exists());
    }

    #[test]
    fn recover_interrupted_write_restores_from_bak_when_live_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("keys.toml");
        let bak = backup_path(&path);
        fs::write(&bak, "recovered = true\n").unwrap();
        assert!(!path.exists());
        recover_interrupted_write(&path);
        assert_eq!(fs::read_to_string(&path).unwrap(), "recovered = true\n");
        assert!(!bak.exists());
    }

    #[test]
    fn recover_interrupted_write_prefers_bak_commit_pivot() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut aside_name = path.file_name().unwrap().to_os_string();
        aside_name.push(".bak.commit");
        let aside = path.with_file_name(aside_name);
        fs::write(&aside, "from_pivot = 1\n").unwrap();
        fs::write(backup_path(&path), "from_bak = 1\n").unwrap();
        recover_interrupted_write(&path);
        assert_eq!(fs::read_to_string(&path).unwrap(), "from_pivot = 1\n");
    }

    #[test]
    fn load_recovers_missing_keys_from_bak() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("config.toml"),
            "[general]\ndefault_provider = \"x\"\n",
        );
        write(
            &dir.path().join("endpoints.toml"),
            r#"
[[endpoint]]
name = "x"
kind = "openai"
base_url = "http://localhost/v1/"
models = ["m"]
"#,
        );
        // Simulate crash: keys live gone, bak remains.
        fs::write(
            backup_path(&dir.path().join("keys.toml")),
            "[x]\napi_key = \"sk-recovered\"\n",
        )
        .unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        assert_eq!(cfg.key_for("x"), Some("sk-recovered"));
    }

    #[test]
    fn save_all_restores_from_bak_if_second_rename_fails() {
        // Simulate a partial commit by verifying .bak exists after a normal
        // save_all snapshot step isn't left behind, and that restore_from_backup
        // puts the original content back.
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "old = true\n").unwrap();
        let bak = backup_path(&path);
        fs::copy(&path, &bak).unwrap();
        fs::write(&path, "new = true\n").unwrap();
        restore_from_backup(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "old = true\n");
        assert!(!bak.exists());
    }

    #[test]
    fn save_all_leaves_no_tmp_files() {
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("config.toml"),
            r#"
[general]
default_provider = "openai"
"#,
        );
        write(
            &dir.path().join("endpoints.toml"),
            r#"
[[endpoint]]
name = "openai"
kind = "openai"
base_url = "https://api.openai.com/v1/"
models = ["gpt-4o"]
"#,
        );
        write(
            &dir.path().join("keys.toml"),
            r#"
[openai]
api_key = "sk-x"
"#,
        );
        let cfg = Config::load(dir.path()).unwrap();
        cfg.save_all(dir.path()).unwrap();
        for name in ["config.toml", "endpoints.toml", "keys.toml"] {
            let p = dir.path().join(name);
            assert!(p.exists(), "missing {name}");
            assert!(
                !staging_path(&p).exists(),
                "leftover staging file for {name}"
            );
        }
    }

    #[test]
    fn save_all_preserves_unrelated_general_sections() {
        // Editing endpoints must not clobber context/memory/ui sections.
        let dir = tempdir().unwrap();
        write(
            &dir.path().join("config.toml"),
            r#"
[general]
default_provider = "openai"
default_model = "gpt-4o"

[context]
summarize_at_fill_rate = 0.3

[ui]
theme = "light"
show_token_usage = false
"#,
        );
        write(
            &dir.path().join("endpoints.toml"),
            r#"
[[endpoint]]
name = "openai"
kind = "openai"
base_url = "https://api.openai.com/v1/"
models = ["gpt-4o"]
"#,
        );
        write(
            &dir.path().join("keys.toml"),
            r#"
[openai]
api_key = "sk-x"
"#,
        );

        let mut cfg = Config::load(dir.path()).unwrap();
        // Simulate an endpoint edit: add a model, leave general untouched.
        cfg.endpoints[0].models.push("gpt-4o-mini".into());
        cfg.save_all(dir.path()).unwrap();

        let reloaded = Config::load(dir.path()).unwrap();
        assert_eq!(
            reloaded.endpoints[0].model_ids(),
            vec!["gpt-4o", "gpt-4o-mini"]
        );
        // Unrelated sections preserved:
        assert!((reloaded.general.context.summarize_at_fill_rate - 0.3).abs() < 1e-9);
        assert_eq!(reloaded.general.ui.theme, "light");
        assert!(!reloaded.general.ui.show_token_usage);
    }

    // ── pending-project marker ──────────────────────────────────────────────
    //
    // The marker is a one-shot side-channel for project switching across a
    // restart. These tests pin its consume-on-read semantics so a stale
    // selection can never bleed into a later startup. They exercise the
    // path-explicit helpers (`write_marker_at` / `consume_marker_at`) against
    // a temp dir instead of the real global config dir.

    #[test]
    fn pending_marker_write_then_consume_returns_path_and_clears_file() {
        let dir = tempdir().unwrap();
        let marker = dir.path().join(".pending_project");
        write_marker_at(&marker, Path::new("/some/project")).unwrap();
        assert!(marker.exists());

        let taken = consume_marker_at(&marker);
        assert_eq!(taken, Some(PathBuf::from("/some/project")));
        assert!(!marker.exists(), "marker must be consumed on read");
    }

    #[test]
    fn pending_marker_consume_on_missing_file_returns_none() {
        let dir = tempdir().unwrap();
        let marker = dir.path().join(".pending_project");
        assert!(!marker.exists());
        assert_eq!(consume_marker_at(&marker), None);
        assert!(!marker.exists());
    }

    #[test]
    fn pending_marker_consume_on_empty_file_returns_none_and_cleans_up() {
        let dir = tempdir().unwrap();
        let marker = dir.path().join(".pending_project");
        fs::write(&marker, "   \n  ").unwrap();
        assert!(marker.exists());

        assert_eq!(
            consume_marker_at(&marker),
            None,
            "empty marker must yield None"
        );
        assert!(!marker.exists(), "empty marker must be cleaned up");
    }

    #[test]
    fn pending_marker_consume_is_idempotent() {
        // A second consume after the first must return None (the marker was
        // consumed) — no stale re-trigger of a project switch.
        let dir = tempdir().unwrap();
        let marker = dir.path().join(".pending_project");
        write_marker_at(&marker, Path::new("/proj/a")).unwrap();

        assert_eq!(consume_marker_at(&marker), Some(PathBuf::from("/proj/a")));
        assert_eq!(
            consume_marker_at(&marker),
            None,
            "second consume must be None"
        );
    }

    #[test]
    fn pending_marker_peek_does_not_consume() {
        // The switch-restart focus path peeks the marker before the window
        // exists; build_brain's take must still see it afterwards.
        let dir = tempdir().unwrap();
        let marker = dir.path().join(".pending_project");
        write_marker_at(&marker, Path::new("/some/project")).unwrap();

        assert_eq!(
            read_marker_at(&marker),
            Some(PathBuf::from("/some/project"))
        );
        assert!(marker.exists(), "peek must leave the marker in place");
        // A peek-then-take sequence still consumes exactly once.
        assert_eq!(
            consume_marker_at(&marker),
            Some(PathBuf::from("/some/project"))
        );
        assert_eq!(read_marker_at(&marker), None);
    }

    #[test]
    fn pending_marker_peek_on_missing_or_empty_file_returns_none() {
        let dir = tempdir().unwrap();
        let marker = dir.path().join(".pending_project");
        assert_eq!(read_marker_at(&marker), None);

        fs::write(&marker, "   \n  ").unwrap();
        assert_eq!(
            read_marker_at(&marker),
            None,
            "empty marker must peek as None"
        );
        // Unlike consume, peek never mutates: an empty marker file is left
        // for consume_marker_at to clean up.
        assert!(marker.exists());
    }

    // ── save_all rollback failure collection (quality review LOW 6) ────────
    //
    // A failed rollback used to be swallowed (`let _ =
    // restore_from_backup`) — a half-renamed config set with no trace, and
    // the returned error told the user nothing about it. These tests pin
    // the collection: restore failures are logged and surfaced in the
    // returned error.

    #[test]
    fn rollback_collects_restore_failures() {
        // Injected restore failures (deterministic + cross-platform — no
        // filesystem tricks): config.toml's restore fails, endpoints.toml's
        // succeeds. Exactly one failure is collected, and the leftover temps
        // are cleaned.
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let endpoints = dir.path().join("endpoints.toml");
        fs::write(&config, "new\n").unwrap();
        fs::write(&endpoints, "new\n").unwrap();
        fs::write(staging_path(&config), "staged\n").unwrap();
        fs::write(staging_path(&endpoints), "staged\n").unwrap();

        let restore = |p: &Path| {
            if p == &config {
                Err(crate::error::Error::Config("disk full".into()))
            } else {
                Ok(())
            }
        };
        let failures = rollback_with_restore(
            &restore,
            &[config.clone(), endpoints.clone()],
            &[config.as_path()],
            &endpoints,
        );

        assert_eq!(failures.len(), 1, "exactly the config.toml restore fails");
        assert!(failures[0].0.ends_with("config.toml"));
        assert!(failures[0].1.to_string().contains("disk full"));
        assert!(!staging_path(&config).exists(), "config tmp cleaned");
        assert!(!staging_path(&endpoints).exists(), "endpoints tmp cleaned");
    }

    #[test]
    fn rollback_clean_when_restores_succeed() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let endpoints = dir.path().join("endpoints.toml");
        fs::write(&config, "new\n").unwrap();
        fs::write(backup_path(&config), "old\n").unwrap();
        fs::write(&endpoints, "new\n").unwrap();
        fs::write(staging_path(&endpoints), "staged\n").unwrap();

        let failures = rollback_partial_commit(
            &[config.clone(), endpoints.clone()],
            &[config.as_path()],
            &endpoints,
        );

        assert!(failures.is_empty(), "no restore failures: {failures:?}");
        assert_eq!(fs::read_to_string(&config).unwrap(), "old\n");
        assert!(!backup_path(&config).exists(), "bak consumed");
        assert!(!staging_path(&endpoints).exists(), "tmp cleaned");
    }

    #[test]
    fn describe_rollback_failure_without_failures_returns_original() {
        let err = describe_rollback_failure(
            crate::error::Error::Config("commit failed".into()),
            &[],
        );
        assert_eq!(err.to_string(), "config error: commit failed");
    }

    #[test]
    fn describe_rollback_failure_with_failures_names_paths() {
        let failures = vec![(
            "config.toml".to_string(),
            crate::error::Error::Config("device full".into()),
        )];
        let err = describe_rollback_failure(
            crate::error::Error::Config("commit failed".into()),
            &failures,
        );
        let msg = err.to_string();
        assert!(msg.contains("commit failed"), "{msg}");
        assert!(msg.contains("config.toml"), "{msg}");
        assert!(msg.contains("device full"), "{msg}");
        assert!(msg.contains("half-committed"), "{msg}");
    }
}
