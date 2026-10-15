// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The kernel workspace (2c.3/G3, §5.9.3): a working root + a state dir.
//!
//! The kernel no longer assumes every project keeps its state under
//! `<root>/.coding` (that is now the APP's project model —
//! `mnemo::app::project::Project`, with the discovery/registry/init +
//! scaffold moved out of `mnemo::project`). G3: an engine works on ANY
//! directory — a non-git temp dir included — with its state dir anywhere
//! ([`Workspace::state_dir`]). The kernel never scaffolds and never writes
//! `.gitignore`/`.gitattributes` (that is the app's [`Project::init`]);
//! the kernel default state dir is `<root>/.coding`
//! ([`CODING_DIR_NAME`]), overridable per build, and the kernel works
//! without a git repo ([`Workspace::require_git`] default false — the
//! Git TOOL group just does not register on a non-repo root; a true
//! value becomes an open-time error on the 3a Engine's `EngineOptions`).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The conventional name of the state dir under a project root. Kept
/// kernel-side so the kernel's default derivation and the app's
/// project-model paths render from the same string (G3).
pub const CODING_DIR_NAME: &str = ".coding";

/// The kernel workspace: root + `state_dir` (+ the git expectation).
/// Paths only — no filesystem writes happen here. Serde-able since 3a.3:
/// the public `EngineOptions` carries it across the wire (§5.3 :286).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    /// The working directory (the project root).
    pub root: PathBuf,
    /// The state dir (plan files, the memory/codegraph dbs, safety rules,
    /// knowledge, reviews). Defaults to `<root>/.coding`; G3 lets it sit
    /// anywhere.
    pub state_dir: PathBuf,
    /// Whether the workspace REQUIRES a git repo at the root
    /// (§5.9.3 :664: default false — engines work in plain directories;
    /// true makes a missing repo an open-time error, which is the 3a
    /// Engine's option). Either way the Git tool group registers only
    /// when a repo exists (row 27's kernel form).
    pub require_git: bool,
}

impl Workspace {
    /// The default workspace: `state_dir` = `<root>/.coding`,
    /// `require_git` = false.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let state_dir = root.join(CODING_DIR_NAME);
        Self {
            root,
            state_dir,
            require_git: false,
        }
    }

    /// A workspace whose state dir lives elsewhere (G3's row-27 shape).
    pub fn with_state_dir(
        root: impl Into<PathBuf>,
        state_dir: impl Into<PathBuf>,
    ) -> Self {
        Self {
            root: root.into(),
            state_dir: state_dir.into(),
            require_git: false,
        }
    }

    /// Mark the workspace as requiring a git repo (the 3a Engine errors
    /// at open when the root is not one; the type stays honest in 2c).
    pub fn requiring_git(mut self) -> Self {
        self.require_git = true;
        self
    }

    /// Whether `<root>/.git` exists — a directory (a normal repo) or a
    /// plain file (a linked worktree / submodule). Cheap sync probe.
    pub fn is_git_repo(&self) -> bool {
        Self::root_is_git_repo(&self.root)
    }

    /// The free-standing form (used by the factory, which keeps
    /// `project_root` params rather than threading the whole type).
    pub fn root_is_git_repo(root: &Path) -> bool {
        root.join(".git").exists()
    }
}

/// The kernel-minimal project bootstrap the engine may perform when a
/// `--project` flag points at a directory that is not yet a project (the
/// CLI's initialize workflow): scaffold `.coding/` + `plans/`, seed the
/// shipped skills (the kernel-owned catalog), and write the root `agent.md`
/// template. THE APP-OWNED `.gitignore`/`.gitattributes` merge stays the
/// app's `Project::init` (row 27's letter: the kernel never writes
/// `.gitignore`) — the app seeds those on its own init pass.
///
/// Idempotent (mkdir-all + write-if-missing, like the app's init).
pub fn ensure_minimal_project(root: &Path) -> crate::error::Result<()> {
    let state_dir = root.join(CODING_DIR_NAME);
    std::fs::create_dir_all(state_dir.join("plans")).map_err(|e| {
        crate::error::Error::Project(format!(
            "mkdir {}: {e}",
            state_dir.join("plans").display()
        ))
    })?;
    crate::project::skills_seed::seed_skills(&state_dir.join("skills")).map_err(|e| {
        crate::error::Error::Project(format!(
            "failed to seed skills into {}: {e}",
            state_dir.join("skills").display()
        ))
    })?;
    let agent_md_path = root.join("agent.md");
    if !agent_md_path.exists() {
        std::fs::write(&agent_md_path, crate::project::agent_md::DEFAULT_PROJECT_TEMPLATE)
            .map_err(|e| {
                crate::error::Error::Project(format!("write {}: {e}", agent_md_path.display()))
            })?;
    }
    Ok(())
}

///<span> Walk</span> up from `start` looking for a directory containing `.coding/`.
/// Returns the project root (the parent of `.coding/`) if found. The
/// kernel's project-detection primitive (the app's `Project` delegates to
/// this; G3 keeps the `.coding` knowledge kernel-side).
pub fn find_coding_dir_ancestor(start: &Path) -> Option<PathBuf> {
    let mut current = Some(start);
    while let Some(dir) = current {
        if dir.join(CODING_DIR_NAME).is_dir() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_state_dir_is_coding_dir_under_root() {
        let ws = Workspace::new("/proj");
        assert_eq!(ws.root, PathBuf::from("/proj"));
        assert_eq!(ws.state_dir, PathBuf::from("/proj/.coding"));
        assert!(!ws.require_git);
        assert!(ws.requiring_git().require_git);
    }

    #[test]
    fn state_dir_may_sit_anywhere() {
        let ws = Workspace::with_state_dir("/proj", "/elsewhere/state");
        assert_eq!(ws.state_dir, PathBuf::from("/elsewhere/state"));
    }

    #[test]
    fn git_repo_detection_accepts_dir_and_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!Workspace::root_is_git_repo(dir.path()));
        let git = dir.path().join(".git");
        std::fs::create_dir_all(&git).unwrap();
        assert!(Workspace::root_is_git_repo(dir.path()));
        // A linked worktree's `.git` = a plain FILE.
        std::fs::remove_dir(&git).unwrap();
        std::fs::write(&git, "gitdir: /else/where\n").unwrap();
        assert!(Workspace::root_is_git_repo(dir.path()));
    }

    #[test]
    fn ancestor_walk_finds_coding_dir() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("a").join("b").join("c");
        std::fs::create_dir_all(&deep).unwrap();
        assert!(find_coding_dir_ancestor(&deep).is_none());
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        assert_eq!(
            find_coding_dir_ancestor(&deep).as_deref(),
            Some(dir.path())
        );
    }

    #[test]
    fn minimal_bootstrap_is_idempotent_and_never_writes_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        ensure_minimal_project(dir.path()).unwrap();
        ensure_minimal_project(dir.path()).unwrap();
        assert!(dir.path().join(".coding").join("plans").is_dir());
        assert!(dir.path().join("agent.md").is_file());
        // Row 27's letter: the kernel never writes .gitignore/.gitattributes.
        assert!(!dir.path().join(".gitignore").exists());
        assert!(!dir.path().join(".gitattributes").exists());
        // The shipped skills seeded.
        assert!(dir
            .path()
            .join(".coding")
            .join("skills")
            .join("merge_to_main.toml")
            .is_file());
    }
}
