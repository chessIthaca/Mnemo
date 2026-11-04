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

pub use endpoints::{Endpoint, EndpointKind, ModelSpec, PricingEntry, Tier, TierLadder, kind_wire};
pub use general::{
    BudgetConfig, EmbeddingModel, GeneralConfig, GeneralSection, GitConfig, LayaConfig,
    MarkdownConfig, ModelRef, ModelsConfig, OptimizerConfig, RoutingConfig, SafetyMode,
    ShellFilterConfig, ShellFilterOverride, TraceConfig, VerifyConfig, VisionModel,
    EMBEDDING_MODEL_SENTINEL_HASH,
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
    /// The unified five-tier model ladder from `endpoints.toml` (`[[tier]]`).
    pub tiers: TierLadder,
    /// API keys from `keys.toml`. Kept separate so it is never accidentally logged.
    pub keys: KeyStore,
    /// MCP server definitions from `mcp.toml` (names + transports; env-var
    /// names only — no secret values ever live in config).
    pub mcp: Vec<McpServerDef>,
    /// Known-projects registry from `projects.toml`.
    pub projects: ProjectRegistry,
}

fn configured_default_model(general: &GeneralConfig, endpoints: &[Endpoint]) -> Option<String> {
    let model = general.general.default_model.as_ref()?;
    let endpoint = general
        .general
        .default_provider
        .as_ref()
        .and_then(|name| endpoints.iter().find(|endpoint| endpoint.name == *name))
        .or_else(|| endpoints.first())?;
    Some(format!("{}/{}", endpoint.name, model))
}

struct TierLoad {
    general: GeneralConfig,
    config_text: String,
    endpoints: Vec<Endpoint>,
    pricing: Vec<PricingEntry>,
    tiers: TierLadder,
    lanes_migrated: bool,
    tiers_migrated: bool,
}

fn read_tier_load(config_path: &Path, endpoints_path: &Path) -> Result<TierLoad> {
    let config_text = if config_path.exists() {
        fs::read_to_string(config_path)?
    } else {
        String::new()
    };
    let general = GeneralConfig::from_toml_text(&config_text)?;
    let (endpoints, pricing) = endpoints::load_or_default(endpoints_path)?;
    let default_model = configured_default_model(&general, &endpoints);
    let mut tiers = endpoints::load_tier_ladder(endpoints_path, default_model.as_deref())?;
    let lanes_migrated = general::merge_legacy_lanes(&config_text, &mut tiers)?;
    if lanes_migrated {
        tiers.fill_default_model(default_model.as_deref());
    }
    let tiers_migrated = endpoints::tier_file_needs_migration(endpoints_path)?;
    Ok(TierLoad {
        general,
        config_text,
        endpoints,
        pricing,
        tiers,
        lanes_migrated,
        tiers_migrated,
    })
}

/// Token-owned cross-process guard for every config/endpoints write and for
/// interrupted-write recovery. The lock is deliberately file-based so separate
/// Mnemo processes do not read-preserve-commit over one another.
struct ConfigWriteLock {
    path: PathBuf,
    token: String,
}

impl ConfigWriteLock {
    fn acquire(config_dir: &Path) -> Result<Self> {
        Self::acquire_with_hooks(config_dir, None, None, None)
    }

    fn acquire_with_hooks(
        config_dir: &Path,
        before_stale_rename: Option<&(dyn Fn() + Send + Sync)>,
        on_contended: Option<&(dyn Fn() + Send + Sync)>,
        after_stale_rename: Option<&(dyn Fn() + Send + Sync)>,
    ) -> Result<Self> {
        fs::create_dir_all(config_dir)?;
        let path = config_dir.join("config.write.lock");
        let token = format!("{}-{}", std::process::id(), uuid::Uuid::new_v4().simple());
        let started = std::time::Instant::now();
        loop {
            match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut file) => {
                    use std::io::Write;
                    if let Err(error) = file.write_all(token.as_bytes()).and_then(|_| file.sync_all()) {
                        drop(file);
                        let _ = fs::remove_file(&path);
                        return Err(error.into());
                    }
                    return Ok(Self { path, token });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if let Some(hook) = on_contended {
                        hook();
                    }
                    // Observe the stale owner's identity together with its
                    // mtime so a fresh lock moved by our rename can be restored.
                    let observed_stale_token = fs::read_to_string(&path).unwrap_or_default();
                    let stale = fs::metadata(&path)
                        .and_then(|metadata| metadata.modified())
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age > std::time::Duration::from_secs(30));
                    if stale {
                        if let Some(hook) = before_stale_rename {
                            hook();
                        }
                        // Quarantine first, then verify that the file we moved
                        // is still the stale lock rather than a fresh successor.
                        let quarantine = config_dir.join(format!("config.write.lock.stale-{token}"));
                        let renamed = fs::rename(&path, &quarantine);
                        if let Some(hook) = after_stale_rename {
                            hook();
                        }
                        match renamed {
                            Ok(()) => {
                                let quarantined_token =
                                    fs::read_to_string(&quarantine).unwrap_or_default();
                                if quarantined_token == observed_stale_token {
                                    let _ = fs::remove_file(quarantine);
                                    continue;
                                }
                                // A fresh owner was installed between our
                                // staleness check and rename. Restore its lock
                                // without replacing any lock that may exist now,
                                // then keep waiting instead of acquiring.
                                let _ = fs::hard_link(&quarantine, &path);
                                let _ = fs::remove_file(quarantine);
                            }
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                            Err(_) => {
                                // Another waiter may have won the rename or
                                // installed a fresh lock. Re-check on the next
                                // iteration instead of deleting anything.
                            }
                        }
                    }
                    if started.elapsed() >= std::time::Duration::from_secs(5) {
                        return Err(crate::error::Error::Config(format!(
                            "timed out waiting for config write lock {}",
                            path.display()
                        )));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for ConfigWriteLock {
    fn drop(&mut self) {
        // Never remove a successor's lock after a stale takeover.
        if fs::read_to_string(&self.path).is_ok_and(|content| content == self.token) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn lane_backup_is_valid(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(value) = toml::from_str::<toml::Value>(&text) else {
        return false;
    };
    value
        .get("general")
        .and_then(|general| general.get("routing"))
        .and_then(toml::Value::as_table)
        .is_some_and(|routing| {
            ["cheap", "routine", "capable", "lane_medium", "lane_high", "escalate"]
                .iter()
                .any(|lane| routing.get(*lane).and_then(toml::Value::as_table).is_some())
        })
}

/// Stage and fsync the lane backup before installing it. A valid existing
/// backup is immutable; an empty, malformed, or lane-less one is replaced.
fn write_lane_backup(path: &Path, original: &str) -> Result<()> {
    use std::io::Write;

    if path.exists() && lane_backup_is_valid(path) {
        return Ok(());
    }
    let tmp = staging_path(path);
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        file.write_all(original.as_bytes())?;
        file.sync_all()?;
        drop(file);
        if !path.exists() {
            match fs::rename(&tmp, path) {
                Ok(()) => return Ok(()),
                Err(error) if !path.exists() => return Err(error.into()),
                Err(_) => {}
            }
        }
        if lane_backup_is_valid(path) {
            let _ = fs::remove_file(&tmp);
            return Ok(());
        }
        // An invalid pre-existing backup is replaced from the original text,
        // still available because lane removal has not happened yet.
        commit_atomic_rename(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
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
        let config_path = config_dir.join("config.toml");
        let endpoints_path = config_dir.join("endpoints.toml");
        let mut loaded = read_tier_load(&config_path, &endpoints_path)?;
        if loaded.lanes_migrated || loaded.tiers_migrated {
            let _lock = ConfigWriteLock::acquire(config_dir)?;
            // A competing loader may have completed the migration while we
            // waited. Re-read both inputs before deciding or writing anything.
            loaded = read_tier_load(&config_path, &endpoints_path)?;
            if loaded.lanes_migrated {
                write_lane_backup(
                    &config_dir.join("config.toml.lanes.bak"),
                    &loaded.config_text,
                )?;
            }
            if loaded.lanes_migrated || loaded.tiers_migrated {
                endpoints::save_tier_ladder_unlocked(&endpoints_path, &loaded.tiers)?;
            }
            if loaded.lanes_migrated {
                loaded.general.save_unlocked(&config_path)?;
            }
        }
        let keys = KeyStore::load_or_default(&config_dir.join("keys.toml"))?;
        let mcp = mcp::load_or_default(&config_dir.join("mcp.toml"))?;
        let projects = ProjectRegistry::load_or_default(&config_dir.join("projects.toml"))?;
        Ok(Self {
            general: loaded.general,
            endpoints: loaded.endpoints,
            pricing: loaded.pricing,
            tiers: loaded.tiers,
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
    /// dropped) — except the tier ladder in `endpoints.toml`, which a cached
    /// `Config` does not own: the representation currently on disk (native
    /// `[[tier]]` or legacy `[tiers]`) is carried through unchanged, and only
    /// explicit tier writers replace it.
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
        self.save_all_inner(config_dir, None)
    }

    fn save_all_inner(
        &self,
        config_dir: &Path,
        after_tier_read: Option<&(dyn Fn() + Send + Sync)>,
    ) -> Result<()> {
        // This guard covers the read-preserve-commit window, not just the
        // final rename: a concurrent set_tiers must wait until our snapshot
        // has committed, then apply its own read-modify-write afterward.
        let _lock = ConfigWriteLock::acquire(config_dir)?;
        // Serialize everything *before* touching disk so a serde failure
        // never partially updates files.
        let config_text = toml::to_string_pretty(&self.general)?;
        let endpoints_text = {
            // A cached Config does not own the ladder. Preserve the native or
            // legacy tier representation currently on disk; explicit tier
            // writers are the only paths that replace it.
            let (tier, legacy_tiers) =
                endpoints::tier_storage(&config_dir.join("endpoints.toml"))?;
            if let Some(hook) = after_tier_read {
                hook();
            }
            let file = endpoints::EndpointsFile {
                endpoint: self.endpoints.clone(),
                pricing: self.pricing.clone(),
                tier,
                legacy_tiers,
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
    recover_interrupted_write_with_hook(path, None);
}

fn recover_interrupted_write_with_hook(
    path: &Path,
    on_contended: Option<&(dyn Fn() + Send + Sync)>,
) {
    let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) else {
        return;
    };
    if !recovery_needed(path) {
        return;
    }
    // Recovery can restore an aside or staging file, so it is a write and
    // must wait for the same owner as ordinary config/endpoints writes.
    let Ok(_lock) = ConfigWriteLock::acquire_with_hooks(parent, None, on_contended, None) else {
        return;
    };
    recover_interrupted_write_unlocked(path);
}

fn recovery_needed(path: &Path) -> bool {
    let mut aside_name = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| "file".into());
    aside_name.push(".bak.commit");
    path.with_file_name(aside_name).exists()
        || staging_path(path).exists()
        || (!path.exists() && backup_path(path).exists())
}

fn recover_interrupted_write_unlocked(path: &Path) {
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
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let _lock = ConfigWriteLock::acquire(parent)?;
    write_atomic_unlocked(path, contents)
}

/// Write without acquiring the config lock. The caller must hold it.
pub(crate) fn write_atomic_unlocked(path: &Path, contents: &str) -> Result<()> {
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

/// Resolve an install's config dir (§5.8 :573-574, job 4 D1): an explicit
/// dir wins, else `$MNEMO_HOME`, else `~/.mnemo` (the same base
/// [`global_config_dir`] uses). The ONE resolver the engine and
/// `Setup::open(None)` share — G4's close-grep keeps `global_config_dir`
/// itself out of `src/headless`, and this fn (in `config/`) carries the
/// env override it lacks.
pub fn resolve_config_dir(explicit: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = explicit {
        return dir;
    }
    if let Some(home) = std::env::var_os("MNEMO_HOME") {
        return PathBuf::from(home);
    }
    global_config_dir()
}

/// The cache dir for bundled embedding models (§5.8 :575-576):
/// `<config_dir>/models` — job 4 D6 re-derived it from the ENGINE's
/// config dir (it was machine-global, ignoring a custom config dir;
/// the brain's startup build already derived it this way). Callers
/// pass their dir: the headless Setup its own `config_dir`, the app
/// the machine-global one. Lives in `config/` so the G4 close-grep
/// keeps `global_config_dir` out of `src/headless`.
pub fn embedder_cache_dir(config_dir: &Path) -> PathBuf {
    config_dir.join("models")
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
            if p == config {
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


#[cfg(test)]
mod unified_tier_migration_tests {
    use super::*;

    fn endpoint_table() -> &'static str {
        r#"
[[endpoint]]
name = "ep"
kind = "openai"
base_url = "http://localhost/"
models = ["default", "legacy-tiny", "tiny", "legacy-routine", "routine", "medium", "legacy-hard", "capable", "high", "top"]
"#
    }

    #[test]
    fn legacy_lanes_migrate_with_backup_and_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        let original = r#"[general]
default_provider = "ep"
default_model = "default"

[general.routing]
threshold = 0.7
enforce = true

[general.routing.cheap]
endpoint = "ep"
model = "tiny"

[general.routing.routine]
endpoint = "ep"
model = "routine"

[general.routing.lane_medium]
endpoint = "ep"
model = "medium"

[general.routing.capable]
endpoint = "ep"
model = "capable"
reasoning_effort = "high"

[general.routing.lane_high]
endpoint = "ep"
model = "high"
reasoning_effort = "medium"

[general.routing.escalate]
endpoint = "ep"
model = "top"
"#;
        fs::write(&config_path, original).unwrap();
        fs::write(
            dir.path().join("endpoints.toml"),
            format!(
                "{}\n[tiers]\nsimple = [\"ep/legacy-tiny\"]\naverage = [\"ep/legacy-routine\"]\ndifficult = [\"ep/legacy-hard\"]\n",
                endpoint_table()
            ),
        )
        .unwrap();

        let config = Config::load(dir.path()).unwrap();
        let expected = [
            ("trivial", vec!["ep/legacy-tiny", "ep/tiny"]),
            ("routine", vec!["ep/legacy-routine", "ep/routine"]),
            ("medium", vec!["ep/medium"]),
            ("high", vec!["ep/legacy-hard", "ep/capable", "ep/high"]),
            ("escalate", vec!["ep/top"]),
        ];
        for (id, models) in expected {
            assert_eq!(config.tiers.get(id).unwrap().models, models, "{id}");
        }
        assert!((config.general.general.routing.threshold - 0.7).abs() < 1e-9);
        assert!(config.general.general.routing.enforce);
        assert_eq!(config.tiers.get("high").unwrap().effort.as_deref(), Some("high"));

        let backup = dir.path().join("config.toml.lanes.bak");
        assert_eq!(fs::read_to_string(&backup).unwrap(), original);
        let migrated = fs::read_to_string(&config_path).unwrap();
        for lane in ["cheap", "routine", "capable", "lane_medium", "lane_high", "escalate"] {
            assert!(!migrated.contains(lane), "saved config still contains {lane}");
        }
        assert!(migrated.contains("threshold"));
        assert!(migrated.contains("enforce"));
        let endpoints = fs::read_to_string(dir.path().join("endpoints.toml")).unwrap();
        assert!(endpoints.contains("[[tier]]"));
        assert!(!endpoints.contains("[tiers]"));

        let reloaded = Config::load(dir.path()).unwrap();
        assert_eq!(reloaded.tiers, config.tiers);
        reloaded.save_all(dir.path()).unwrap();
        assert_eq!(fs::read_to_string(&backup).unwrap(), original);
    }

    #[test]
    fn lane_migration_is_safe_when_endpoints_file_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            r#"
[general.routing.cheap]
endpoint = "ep"
model = "tiny"
"#,
        )
        .unwrap();
        let config = Config::load(dir.path()).unwrap();
        assert_eq!(config.tiers.get("trivial").unwrap().models, vec!["ep/tiny"]);
        assert!(dir.path().join("endpoints.toml").exists());
        assert!(dir.path().join("config.toml.lanes.bak").exists());
        assert_eq!(Config::load(dir.path()).unwrap().tiers, config.tiers);
    }

    #[test]
    fn legacy_empty_simple_uses_the_configured_default_model() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("config.toml"),
            "[general]\ndefault_provider = \"ep\"\ndefault_model = \"default\"\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("endpoints.toml"),
            format!("{}
[tiers]
average = [\"ep/routine\"]
", endpoint_table()),
        )
        .unwrap();
        let config = Config::load(dir.path()).unwrap();
        assert_eq!(config.tiers.first_model("trivial"), Some("ep/default"));
        let text = fs::read_to_string(dir.path().join("endpoints.toml")).unwrap();
        assert!(text.contains("ep/default"));
        assert!(!text.contains("[tiers]"));
    }
}


#[cfg(test)]
mod review_regression_tests {
    use super::*;

    const ENDPOINTS: &str = r#"
[[endpoint]]
name = "ep"
kind = "openai"
base_url = "http://localhost/"
models = ["default", "legacy-tiny", "tiny", "routine"]
"#;

    #[test]
    fn cached_config_save_preserves_a_newer_on_disk_ladder() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("endpoints.toml");
        let mut ladder_a = TierLadder::default();
        ladder_a.tiers[0].models = vec!["ep/old".into()];
        endpoints::save_tier_ladder(&path, &ladder_a).unwrap();
        let cached = Config::load(dir.path()).unwrap();
        assert_eq!(cached.tiers, ladder_a);

        let mut ladder_b = TierLadder::default();
        ladder_b.tiers[0].models = vec!["ep/new".into()];
        ladder_b.tiers[2].models = vec!["ep/medium".into()];
        endpoints::save_tier_ladder(&path, &ladder_b).unwrap();
        cached.save_all(dir.path()).unwrap();

        assert_eq!(endpoints::load_tier_ladder(&path, None).unwrap(), ladder_b);
    }

    #[test]
    fn concurrent_migration_keeps_one_ladder_and_the_original_lane_backup() {
        let dir = tempfile::tempdir().unwrap();
        let original = r#"[general]
default_provider = "ep"
default_model = "default"

[general.routing.cheap]
endpoint = "ep"
model = "tiny"
reasoning_effort = "minimal"
"#;
        fs::write(dir.path().join("config.toml"), original).unwrap();
        fs::write(
            dir.path().join("endpoints.toml"),
            format!("{ENDPOINTS}\n[tiers]\nsimple = [\"ep/legacy-tiny\"]\n"),
        )
        .unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for _ in 0..2 {
            let path = dir.path().to_path_buf();
            let barrier = barrier.clone();
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                Config::load(&path).unwrap().tiers
            }));
        }
        let first = handles.remove(0).join().unwrap();
        let second = handles.remove(0).join().unwrap();
        assert_eq!(first, second);
        assert_eq!(
            first.get("trivial").unwrap().models,
            vec!["ep/legacy-tiny", "ep/tiny"]
        );
        assert_eq!(first.get("trivial").unwrap().effort.as_deref(), Some("minimal"));
        assert_eq!(
            fs::read_to_string(dir.path().join("config.toml.lanes.bak")).unwrap(),
            original
        );
        assert!(!dir.path().join("config.write.lock").exists());
    }

    #[test]
    fn plain_endpoint_save_leaves_legacy_tiers_for_default_filling_migration() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("endpoints.toml");
        fs::write(&path, format!("{ENDPOINTS}\n[tiers]\naverage = [\"ep/routine\"]\n"))
            .unwrap();
        let (loaded, pricing) = endpoints::load_or_default(&path).unwrap();
        endpoints::save(&path, &loaded, &pricing).unwrap();
        let before = fs::read_to_string(&path).unwrap();
        assert!(before.contains("[tiers]"));
        assert!(!before.contains("[[tier]]"));
        assert!(before.contains("average = [\"ep/routine\"]"));

        fs::write(
            dir.path().join("config.toml"),
            "[general]\ndefault_provider = \"ep\"\ndefault_model = \"default\"\n",
        )
        .unwrap();
        let config = Config::load(dir.path()).unwrap();
        assert_eq!(config.tiers.first_model("trivial"), Some("ep/default"));
        let after = fs::read_to_string(&path).unwrap();
        assert!(!after.contains("[tiers]"));
        assert!(after.contains("[[tier]]"));
    }
}


#[cfg(test)]
mod config_write_lock_tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::time::Duration;

    #[test]
    fn cached_save_and_tier_writer_are_serialized_across_read_commit() {
        let dir = tempfile::tempdir().unwrap();
        let endpoints_path = dir.path().join("endpoints.toml");
        let mut ladder_a = TierLadder::default();
        ladder_a.tiers[0].models = vec!["ep/a".into()];
        endpoints::save_tier_ladder(&endpoints_path, &ladder_a).unwrap();
        let cached = Config::load(dir.path()).unwrap();
        let mut ladder_b = TierLadder::default();
        ladder_b.tiers[0].models = vec!["ep/b".into()];

        let (read_tx, read_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
        let (contended_tx, contended_rx) = std::sync::mpsc::channel();
        let save = {
            let dir = dir.path().to_path_buf();
            let release_rx = release_rx.clone();
            std::thread::spawn(move || {
                let hook = move || {
                    read_tx.send(()).unwrap();
                    release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(2))
                        .unwrap();
                };
                cached.save_all_inner(&dir, Some(&hook))
            })
        };
        read_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        let writer_ladder = ladder_b.clone();
        let writer = {
            let dir = dir.path().to_path_buf();
            std::thread::spawn(move || {
                let hook = move || {
                    let _ = contended_tx.send(());
                };
                endpoints::save_tier_ladder_with_hook(
                    &dir.join("endpoints.toml"),
                    &writer_ladder,
                    Some(&hook),
                )
            })
        };
        // The tier writer has observed the cached save's lock and cannot pass
        // its read-preserve-commit critical section.
        contended_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        release_tx.send(()).unwrap();
        save.join().unwrap().unwrap();
        writer.join().unwrap().unwrap();
        assert_eq!(endpoints::load_tier_ladder(&endpoints_path, None).unwrap(), ladder_b);
    }

    #[test]
    fn two_waiters_racing_stale_takeover_never_hold_the_lock_together() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("config.write.lock");
        fs::write(&lock_path, "stale-owner").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&lock_path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(60))
            .unwrap();

        let before = Arc::new(Barrier::new(2));
        let after = Arc::new(Barrier::new(2));
        let (contention_tx, contention_rx) = std::sync::mpsc::channel();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let mut handles = Vec::new();
        for _ in 0..2 {
            let dir = dir.path().to_path_buf();
            let before = before.clone();
            let after = after.clone();
            let contention_tx = contention_tx.clone();
            let acquired_tx = acquired_tx.clone();
            handles.push(std::thread::spawn(move || {
                let before_hook = move || {
                    before.wait();
                };
                let after_hook = move || {
                    after.wait();
                };
                let contention_hook = move || {
                    let _ = contention_tx.send(());
                };
                let guard = ConfigWriteLock::acquire_with_hooks(
                    &dir,
                    Some(&before_hook),
                    Some(&contention_hook),
                    Some(&after_hook),
                )
                .unwrap();
                acquired_tx.send(guard).unwrap();
            }));
        }
        // Both waiters first contend on the stale lock, then pass the rename
        // barrier. Exactly one can hold the lock while the other contends on it.
        contention_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        contention_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let first = acquired_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        contention_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(acquired_rx.try_recv().is_err(), "a second waiter entered while the first guard was alive");
        let first_token = first.token.clone();
        drop(first);
        let second = acquired_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_ne!(first_token, second.token);
        drop(second);
        for handle in handles {
            handle.join().unwrap();
        }
        assert!(!lock_path.exists());
    }

    #[test]
    fn fresh_lock_is_restored_when_stale_takeover_races_a_new_owner() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("config.write.lock");
        fs::write(&lock_path, "stale-owner").unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&lock_path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(60))
            .unwrap();

        let (before_tx, before_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
        let (after_tx, after_rx) = std::sync::mpsc::channel();
        let (contention_tx, contention_rx) = std::sync::mpsc::channel();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let waiter = {
            let dir = dir.path().to_path_buf();
            let release_rx = release_rx.clone();
            std::thread::spawn(move || {
                let before_hook = move || {
                    before_tx.send(()).unwrap();
                    release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(2))
                        .unwrap();
                };
                let after_hook = move || {
                    let _ = after_tx.send(());
                };
                let contention_hook = move || {
                    let _ = contention_tx.send(());
                };
                let guard = ConfigWriteLock::acquire_with_hooks(
                    &dir,
                    Some(&before_hook),
                    Some(&contention_hook),
                    Some(&after_hook),
                )
                .unwrap();
                acquired_tx.send(guard).unwrap();
            })
        };

        // The waiter has observed the stale token and paused before rename.
        contention_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        before_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        // Simulate another waiter completing takeover and installing a fresh
        // owner before our waiter performs its rename.
        let first_quarantine = dir.path().join("first-stale-quarantine");
        fs::rename(&lock_path, &first_quarantine).unwrap();
        let fresh_owner = ConfigWriteLock::acquire(dir.path()).unwrap();
        let fresh_token = fresh_owner.token.clone();
        fs::remove_file(first_quarantine).unwrap();
        release_tx.send(()).unwrap();

        // Our waiter moves the fresh lock, detects the token mismatch, and
        // restores it. Its next contention proves restoration has completed.
        after_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        contention_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(fs::read_to_string(&lock_path).unwrap(), fresh_token);
        assert!(
            acquired_rx.try_recv().is_err(),
            "the waiter acquired while the fresh owner was alive"
        );

        drop(fresh_owner);
        let waiter_guard = acquired_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_ne!(waiter_guard.token, fresh_token);
        waiter.join().unwrap();
        drop(waiter_guard);
        assert!(!lock_path.exists());
    }

    #[test]
    fn recovery_waits_for_the_current_write_lock_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let backup = backup_path(&path);
        fs::write(&backup, "recovered = true\n").unwrap();
        let guard = ConfigWriteLock::acquire(dir.path()).unwrap();

        let (contention_tx, contention_rx) = std::sync::mpsc::channel();
        let recovery_path = path.clone();
        let recovery = std::thread::spawn(move || {
            let hook = move || {
                let _ = contention_tx.send(());
            };
            recover_interrupted_write_with_hook(&recovery_path, Some(&hook));
        });
        contention_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!path.exists(), "recovery ran while another owner held the lock");
        assert!(backup.exists());
        drop(guard);
        recovery.join().unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "recovered = true\n");
        assert!(!backup.exists());
    }

    #[test]
    fn empty_lane_backup_is_replaced_from_the_original_text() {
        let dir = tempfile::tempdir().unwrap();
        let original = r#"[general.routing.cheap]
endpoint = "ep"
model = "tiny"
reasoning_effort = "minimal"
"#;
        fs::write(dir.path().join("config.toml"), original).unwrap();
        let backup = dir.path().join("config.toml.lanes.bak");
        fs::write(&backup, "").unwrap();

        let config = Config::load(dir.path()).unwrap();
        assert_eq!(config.tiers.first_model("trivial"), Some("ep/tiny"));
        assert_eq!(fs::read_to_string(&backup).unwrap(), original);
        assert!(!staging_path(&backup).exists());
        assert!(!dir.path().join("config.write.lock").exists());
    }
}
