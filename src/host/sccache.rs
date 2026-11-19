// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `sccache` discovery — the shared Rust build cache (plan d3336a8a step 7).
//!
//! One `sccache` server caches `rustc` output for every Mnemo2 instance (and
//! every agent session) on the machine, so two workers building the same
//! dependency graph compile it once instead of twice. There is no setting and
//! no toggle: when the binary is on `PATH` it is used, and a user's own
//! `RUSTC_WRAPPER` is never overridden.
//!
//! The answer is resolved lazily at the first command spawn and refreshed on
//! every Settings read, so a `cargo install sccache` done mid-session is picked
//! up without a restart. A missing binary costs one cached `PATH` scan and
//! nothing else — the build cache is an optimisation, never a failure mode.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, RwLock};

/// The environment variable `cargo` reads to pick the compiler wrapper.
const WRAPPER_VAR: &str = "RUSTC_WRAPPER";

/// The program that provides the shared Rust build cache.
const SCCACHE: &str = "sccache";

/// The last resolution: `None` before the first look, `Some(None)` when the
/// binary was absent, `Some(Some(path))` when it was found. Refreshed by
/// [`refresh`], so a mid-session install lands on the next command.
static RESOLVED: LazyLock<RwLock<Option<Option<PathBuf>>>> =
    LazyLock::new(|| RwLock::new(None));

/// The cached `sccache` path, resolving it on first use.
///
/// This is the accessor every spawn path uses: the first command pays the
/// `PATH` scan, every later one reads the cache.
pub fn current() -> Option<PathBuf> {
    cached(&RESOLVED, lookup, false)
}

/// Look `sccache` up on `PATH` right now and cache the answer.
///
/// Called on every Settings read (`model_settings_get`), so a binary installed
/// after startup shows up in the Machine page without an app restart.
pub fn refresh() -> Option<PathBuf> {
    cached(&RESOLVED, lookup, true)
}

/// Look `sccache` up on `PATH` without touching the cache.
///
/// The one-shot form of [`current`]/[`refresh`]; the real lookup is
/// [`lookup`], and the cache behaviour is what the module's tests exercise.
pub fn resolve() -> Option<PathBuf> {
    lookup()
}

/// Set `RUSTC_WRAPPER` on `command` when the machine has `sccache` and the
/// inherited environment does not already name a wrapper.
///
/// Applied to every spawned agent command, harness verification and Mnemo2
/// proof run: on a non-`cargo` command the variable is inert, and one code
/// path is simpler (and cannot disagree with itself) than a heavy-only rule.
/// A user's own `RUSTC_WRAPPER` — set or empty — always wins: Mnemo never
/// replaces a wrapper it did not choose.
pub fn apply_env(command: &mut tokio::process::Command) {
    if let Some((key, value)) = wrapper_env(
        std::env::var_os(WRAPPER_VAR).as_deref(),
        current().as_deref(),
    ) {
        command.env(key, value);
    }
}

/// The `(name, value)` pair to add to a child's environment, or `None`.
///
/// `inherited` is the parent process's `RUSTC_WRAPPER` (absent or blank = no
/// wrapper) and `found` is the resolved `sccache` path. Pure, so the three
/// rules below are cheap to test without touching the real `PATH`:
///
/// - found + no inherited wrapper → set `RUSTC_WRAPPER` to the path;
/// - any inherited wrapper → nothing (never override the user);
/// - no binary → nothing.
pub fn wrapper_env(
    inherited: Option<&OsStr>,
    found: Option<&Path>,
) -> Option<(&'static str, OsString)> {
    let inherited_is_set = inherited
        .map(|value| !value.to_string_lossy().trim().is_empty())
        .unwrap_or(false);
    if inherited_is_set {
        return None;
    }
    found.map(|path| (WRAPPER_VAR, path.as_os_str().to_os_string()))
}

/// The real lookup: `which` honours `PATH` and, on Windows, `PATHEXT`.
fn lookup() -> Option<PathBuf> {
    which::which(SCCACHE).ok()
}

/// The cache core: an unexpired cached answer short-circuits unless `refresh`,
/// otherwise `lookup` runs and its answer (including "absent") is stored.
fn cached(
    cache: &RwLock<Option<Option<PathBuf>>>,
    lookup: impl FnOnce() -> Option<PathBuf>,
    refresh: bool,
) -> Option<PathBuf> {
    if !refresh {
        if let Some(answer) = cache
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            return answer;
        }
    }
    let answer = lookup();
    *cache
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(answer.clone());
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Found on `PATH` and no wrapper inherited: the wrapper is set.
    #[test]
    fn wrapper_set_when_found() {
        let path = PathBuf::from("/opt/sccache/bin/sccache");
        let (key, value) =
            wrapper_env(None, Some(&path)).expect("a found binary must be used");
        assert_eq!(key, WRAPPER_VAR);
        assert_eq!(value, path.as_os_str().to_os_string());
        // A blank inherited value is "no wrapper", not a user choice.
        assert!(wrapper_env(Some(OsStr::new("  ")), Some(&path)).is_some());
    }

    /// The user's own wrapper is never replaced, found binary or not.
    #[test]
    fn user_wrapper_not_overridden() {
        let path = PathBuf::from("/opt/sccache/bin/sccache");
        assert_eq!(
            wrapper_env(Some(OsStr::new("sccache-custom")), Some(&path)),
            None
        );
    }

    /// No binary found: nothing is set, however the environment looks.
    #[test]
    fn not_found_sets_nothing() {
        assert_eq!(wrapper_env(None, None), None);
        assert_eq!(wrapper_env(Some(OsStr::new("mine")), None), None);
    }

    /// The injected lookup: the first answer is cached (a later "found" is not
    /// seen) and `refresh` replaces it (a mid-session install is picked up).
    #[test]
    fn cache_resolves_once_and_refreshes() {
        let cache = RwLock::new(None);
        let installed = PathBuf::from("/new/sccache");
        assert_eq!(
            cached(&cache, || None, false),
            None,
            "the first look caches the absence"
        );
        assert_eq!(
            cached(&cache, || Some(installed.clone()), false),
            None,
            "a cached 'not found' is not re-looked"
        );
        assert_eq!(
            cached(&cache, || Some(installed.clone()), true),
            Some(installed.clone()),
            "refresh picks up a newly installed binary"
        );
        assert_eq!(
            cached(&cache, || None, false),
            Some(installed),
            "the refreshed answer is what the next spawn reads"
        );
    }
}
