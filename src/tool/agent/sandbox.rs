// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Path validation — confines all file ops to the project root.
//!
//! All file agent tools route their path through `Sandbox::validate()` before
//! any I/O. Path traversal, symlinks pointing outside, and absolute paths are
//! all caught by canonicalization + the `starts_with` check.
//!
//! ## Async-callers contract (Perf H1)
//!
//! `validate` / `validate_for_creation` are deliberately **synchronous** (`pub
//! fn`) because they do a single filesystem `canonicalize` (a syscall). They
//! are cheap individually, but they *are* blocking I/O, so callers on the
//! async runtime MUST invoke them from inside a `tokio::task::spawn_blocking`
//! closure so the syscall does not stall a tokio worker under concurrent
//! multi-agent tool use.
//!
//! The six async file/search tools (`file_read`, `file_edit`, `file_write`,
//! `file_append`, `search`, `describe_image`) now wrap their entire blocking
//! work — including the `validate` call — in one `spawn_blocking` closure
//! (they `clone()` the cheap `Sandbox` and move it in). The two remaining
//! sync-path callers each perform a *single* `canonicalize` and are kept sync
//! by design, with the trade-off noted:
//! - [`crate::agent::approval::is_project_scoped`] — one `validate` per
//!   tool-call approval decision; async-ifying it would make `needs_approval`
//!   + its callers async (a larger cross-cutting seam). Low-leverage (one
//!   syscall, infrequent).
//! - `shell::resolve_cwd` — one `validate` + `is_dir` per shell command; the
//!   command itself already runs via `tokio::process::Command` (async). Low
//!   leverage (one syscall per command).
//!
//! Keeping `validate` sync preserves the clean `pub fn` API the tools + tests
//! call directly; the heavy FS work (whole-file reads/writes, the search scan)
//! is what gets offloaded, which is where H1's multi-agent scalability risk
//! actually lives.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

/// A path sandbox rooted at the project directory.
#[derive(Debug, Clone)]
pub struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// Create a new sandbox rooted at `root`. The root is canonicalized.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let root = if root.exists() {
            root.canonicalize().map_err(|e| {
                Error::InvalidInput(format!(
                    "sandbox root '{}' cannot be canonicalized: {e}",
                    root.display()
                ))
            })?
        } else {
            // If the root doesn't exist yet (e.g. during init), use the
            // absolute form without canonicalization.
            root
        };
        Ok(Self { root })
    }

    /// The sandbox root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Validate that `path` is inside the sandbox root. Returns the canonical
    /// absolute path on success.
    ///
    /// - Relative paths are resolved against the root.
    /// - Absolute paths must already be inside the root.
    /// - Path traversal (`../../etc/passwd`) is caught by canonicalization.
    /// - Symlinks pointing outside are caught by canonicalization.
    /// - Non-existent paths: we canonicalize the parent and append the file name,
    ///   so creating a new file inside the root is allowed.
    pub fn validate(&self, path: &Path) -> Result<PathBuf> {
        // Resolve relative to root; absolute paths are taken as-is.
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };

        // If the path exists, canonicalize it fully (resolves symlinks + `..`).
        if candidate.exists() {
            let canonical = candidate.canonicalize()?;
            return self.check_inside(&canonical);
        }

        // For non-existent paths (e.g. a file we're about to create),
        // canonicalize the parent if it exists, then append the file name.
        let parent = candidate.parent().unwrap_or_else(|| Path::new("."));
        if parent.exists() {
            let canonical_parent = parent.canonicalize()?;
            let file_name = candidate
                .file_name()
                .ok_or_else(|| Error::InvalidInput(format!("path has no file name: {path:?}")))?;
            let canonical = canonical_parent.join(file_name);
            return self.check_inside(&canonical);
        }

        // Parent doesn't exist either — reject. The agent should create dirs
        // explicitly via file_write, not via path traversal.
        Err(Error::InvalidInput(format!(
            "path's parent directory does not exist: {path:?}"
        )))
    }

    /// Validate that a path *would be* inside the sandbox root, without
    /// requiring it (or its parent) to exist. This is used by `file_write`
    /// to create nested directories. It resolves `..` components lexically
    /// (without canonicalization) and checks the result is inside the root.
    ///
    /// **Security:** this does NOT resolve symlinks (the path doesn't exist
    /// yet, so there are no symlinks to resolve). It does reject `..`
    /// traversal by normalizing the path and checking `starts_with(root)`.
    /// Once the dirs are created, the final file path is validated normally
    /// (with canonicalization) before writing.
    pub fn validate_for_creation(&self, path: &Path) -> Result<PathBuf> {
        let candidate = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        };
        // Normalize `.` and `..` components lexically.
        let normalized = normalize_path(&candidate);
        if normalized.starts_with(&self.root) {
            Ok(normalized)
        } else {
            Err(Error::PathOutsideRoot(normalized))
        }
    }

    /// Check that a canonical path is inside the root.
    fn check_inside(&self, canonical: &Path) -> Result<PathBuf> {
        if canonical.starts_with(&self.root) {
            Ok(canonical.to_path_buf())
        } else {
            Err(Error::PathOutsideRoot(canonical.to_path_buf()))
        }
    }

    /// Whether a path points at a file that must not be written by the file
    /// tools. This protects live app state + bookkeeping from corruption by a
    /// model (or an approved write) that could break restart/resume, safety
    /// rules, or the plan workflow.
    ///
    /// Protected:
    /// - the memory SQLite DB + its `-wal`/`-shm` sidecars (the app holds an
    ///   open connection to it);
    /// - `.coding/safety.toml` (auto-approve rules — must stay hand-edited);
    /// - `.coding/backlog.jsonl` (the prompt backlog + Run-All state);
    /// - the entire `.coding/plans/` directory (`stack.json` + plan markdown —
    ///   owned by the dedicated workflow tools, never the file tools);
    /// - the entire `.coding/reviews/` directory (review reports are authored
    ///   ONLY by the reviewer's `write_review_report` tool — a spawned
    ///   `role: "reviewer"` subagent's single output channel. The main agent
    ///   can never author a review, so the file tools must not be able to
    ///   fabricate a report there either);
    /// - the entire `.coding/knowledge/` directory — typed semantic records
    ///   (SPEC/DECISION/BUG/HOW) whose FILES are the truth. They are written
    ///   ONLY through the memory tools' file-backed writer (which does not
    ///   route through this check), so the file tools must not bypass the
    ///   record format, the supersede chain, or the indexer reconciliation.
    /// - the `.git` control plane - ANY path containing a `.git` component
    ///   (the root control-plane dir and its contents, a nested repo's
    ///   `.git`, and the plain `.git` FILE used by linked worktrees). A
    ///   planted `.git/hooks/pre-commit` or `.git/config` (`core.fsmonitor`)
    ///   executes unsandboxed on the next commit or an auto-approved
    ///   `git status` (2027-01-09 security review HIGH-1). Component
    ///   equality only: `.gitignore`/`.gitattributes` stay writable.
    ///
    /// The dedicated workflow tools (`create_plan`/`complete_step`/
    /// `update_plan`/`abandon_plan`) write `.coding/plans/` through their own
    /// paths and do **not** route through this check, so expanding the set here
    /// only blocks the file agent tools (`file_write`/`file_edit`/
    /// `file_append`/`convert_line_endings`). Reviewer reports are written
    /// through the `write_review_report` tool, which also does **not** route
    /// through this check — so the file-tool protection is complete: the file
    /// tools refuse, the reviewer tool is the only sanctioned writer. (One
    /// approval-gated residual remains: `shell` is not routed through this
    /// check, matching the standing residual for `.coding/plans/` — every
    /// shell call is user-visible and needs approval.)
    ///
    /// `path` may be either a canonical path (from [`validate`](Self::validate))
    /// or a lexically normalized one (from
    /// [`validate_for_creation`](Self::validate_for_creation)); both are
    /// root-absolute, so the `strip_prefix(root)` + forward-slash normalization
    /// makes the check work before the file exists (closing the creation gap).
    pub fn is_protected_write_target(&self, path: &Path) -> bool {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        // Lowercase the relative path before comparison: NTFS (Windows) is
        // case-insensitive, so `.coding/SAFETY.TOML` resolves to the real
        // protected file and must not bypass this guard. The comparison
        // literals below are all lowercase ASCII, so they match case-variants.
        let rel_str = rel
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();

        // NTFS Alternate Data Streams (ADS): on Windows, `file:stream` writes
        // a hidden stream attached to `file` — e.g. `.coding/memory.db:evil`
        // writes to the memory DB's ADS, bypassing the exact-match protected
        // check below. Reject ANY path whose final component contains a `:`
        // after the drive prefix (the drive prefix `C:` is on the root, not
        // the final component, so it's not affected). This closes the ADS
        // exfiltration/tampering vector on protected files.
        if let Some(file_name) = rel_str.rsplit('/').next() {
            if file_name.contains(':') {
                return true;
            }
        }

        // The memory DB + codegraph DB, each with its SQLite sidecar files.
        rel_str == ".coding/memory.db"
            || rel_str == ".coding/memory.db-wal"
            || rel_str == ".coding/memory.db-shm"
            || rel_str == ".coding/codegraph.db"
            || rel_str == ".coding/codegraph.db-wal"
            || rel_str == ".coding/codegraph.db-shm"
            || rel_str == ".coding/safety.toml"
            || rel_str == ".coding/backlog.jsonl"
            // The legacy pre-jsonl path stays protected (it may linger until
            // git removes it after migration).
            || rel_str == ".coding/backlog.json"
            || rel_str.starts_with(".coding/plans/")
            // Review reports are authored ONLY by the reviewer's
            // write_review_report tool (constructor-granted) — never by the
            // file tools, so the main agent cannot fabricate a report.
            || rel_str.starts_with(".coding/reviews/")
            // Knowledge records (typed semantic files) are authored ONLY
            // through the memory tools' file-backed writer — never by the
            // file tools (the writer is the sole sanctioned path).
            || rel_str.starts_with(".coding/knowledge/")
            // The .git control plane (2027-01-09 security review HIGH-1): a
            // planted .git/hooks/pre-commit or .git/config (core.fsmonitor)
            // executes unsandboxed on the next commit or an auto-approved
            // `git status`. Any `.git` path component counts: the root
            // control-plane dir, its contents, a nested repo's .git, and the
            // plain `.git` FILE used by linked worktrees. Trailing dots and
            // spaces are trimmed per component first (Win32 path normalization
            // strips them, so mkdir(".git.") creates the real `.git`).
            // Component equality only - .gitignore/.gitattributes stay writable.
            || rel_str
                .split('/')
                .any(|c| c.trim_end_matches(['.', ' ']) == ".git")
    }

    /// Validate a path for a write operation, implementing the full creation
    /// ladder: validate → creation-fallback → protected check → mkdir parents →
    /// revalidate. Returns the canonicalized path on success.
    ///
    /// This is the shared ladder used by `file_write` + `file_append` so both
    /// create parent dirs consistently and refuse protected targets with ONE
    /// shared message. `file_edit` does NOT use this (it edits existing files —
    /// use [`refuse_if_protected`](Self::refuse_if_protected) instead).
    ///
    /// Steps:
    /// 1. [`validate`](Self::validate) — canonicalize an existing path.
    /// 2. If that fails (file doesn't exist yet), fall back to
    ///    [`validate_for_creation`](Self::validate_for_creation) (lexical
    ///    normalize + `starts_with(root)`).
    /// 3. Check [`is_protected_write_target`](Self::is_protected_write_target)
    ///    — refuse BEFORE creating any dirs (a protected path must never be
    ///    touched, even to mkdir its parents).
    /// 4. Create parent dirs if they don't exist (so `file_append` can write to
    ///    a new nested path, matching `file_write`).
    /// 5. Revalidate (canonicalize now that the parent exists) so the returned
    ///    path is fully canonical.
    pub fn validate_for_write(&self, path: &Path) -> Result<PathBuf> {
        // Steps 1-2: validate, falling back to creation validation.
        let validated = match self.validate(path) {
            Ok(p) => p,
            Err(_) => self.validate_for_creation(path)?,
        };
        // Step 3: refuse protected targets BEFORE any filesystem mutation.
        if self.is_protected_write_target(&validated) {
            return Err(protected_refusal(path));
        }
        // Step 4: create parent dirs if missing (consistency — file_append now
        // creates parents like file_write).
        if let Some(parent) = validated.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
            }
        }
        // Step 5: revalidate so the returned path is fully canonical (the
        // parent now exists, so canonicalize resolves the full path).
        match self.validate(path) {
            Ok(p) => Ok(p),
            // If revalidation fails (shouldn't — we just created the parent),
            // fall back to the creation-validated path.
            Err(_) => Ok(validated),
        }
    }

    /// Refuse a protected write target with the shared refusal message. Used by
    /// `file_edit` and `convert_line_endings` (which edit/rewrite existing
    /// files and do NOT need the creation ladder — they validate normally then
    /// call this).
    pub fn refuse_if_protected(&self, validated: &Path) -> Result<()> {
        if self.is_protected_write_target(validated) {
            return Err(protected_refusal(validated));
        }
        Ok(())
    }
}

/// The shared refusal message for a protected write target. ONE message across
/// all file tools (`file_write`/`file_append`/`file_edit`/
/// `convert_line_endings`) so the error is consistent and testable. Contains
/// "protected" (all existing tests assert `.contains("protected")`).
fn protected_refusal(path: &Path) -> Error {
    Error::InvalidInput(format!(
        "refused: '{}' is a protected path (.coding state/bookkeeping or the .git control plane) — use the \
         dedicated tools (memory/plan/safety/git) instead of the file tools",
        path.display()
    ))
}

/// Normalize `.` and `..` components in a path lexically (without touching
/// the filesystem). This resolves `..` by popping the previous component.
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {} // skip "."
            std::path::Component::ParentDir => {
                // Pop the previous component, but only if it's a normal component
                // (don't pop past the root).
                if out
                    .last()
                    .is_some_and(|c| matches!(c, std::path::Component::Normal(_)))
                {
                    out.pop();
                }
            }
            c => out.push(c),
        }
    }
    out.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn validates_relative_path_inside_root() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("file.txt");
        std::fs::write(&file, "hi").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let validated = sandbox.validate(Path::new("file.txt")).unwrap();
        assert_eq!(validated, file.canonicalize().unwrap());
    }

    #[test]
    fn validates_absolute_path_inside_root() {
        let dir = tempdir().unwrap();
        let file = dir.path().join("file.txt");
        std::fs::write(&file, "hi").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let validated = sandbox.validate(&file).unwrap();
        assert_eq!(validated, file.canonicalize().unwrap());
    }

    #[test]
    fn allows_nonexistent_path_with_existing_parent() {
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        // A file that doesn't exist yet, but its parent (root) does.
        let validated = sandbox.validate(Path::new("new_file.txt")).unwrap();
        assert!(validated.ends_with("new_file.txt"));
        assert!(validated.starts_with(sandbox.root()));
    }

    #[test]
    fn rejects_path_traversal() {
        let dir = tempdir().unwrap();
        // Create a file outside the root to traverse to.
        let outside = dir.path().parent().unwrap().join("outside.txt");
        std::fs::write(&outside, "secret").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let result = sandbox.validate(Path::new("../outside.txt"));
        assert!(result.is_err());
        match result.unwrap_err() {
            Error::PathOutsideRoot(_) => {}
            other => panic!("expected PathOutsideRoot, got {other:?}"),
        }
    }

    #[test]
    fn rejects_absolute_path_outside_root() {
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        // An absolute path outside the root.
        let outside = if cfg!(windows) {
            PathBuf::from("C:\\Windows\\System32\\drivers\\etc\\hosts")
        } else {
            PathBuf::from("/etc/passwd")
        };
        let result = sandbox.validate(&outside);
        assert!(result.is_err());
    }

    #[test]
    fn allows_nested_subdir_path() {
        let dir = tempdir().unwrap();
        let subdir = dir.path().join("src").join("deep");
        std::fs::create_dir_all(&subdir).unwrap();
        let file = subdir.join("mod.rs");
        std::fs::write(&file, "fn main(){}").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let validated = sandbox.validate(Path::new("src/deep/mod.rs")).unwrap();
        assert_eq!(validated, file.canonicalize().unwrap());
    }

    #[test]
    fn rejects_nonexistent_parent() {
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        // Parent doesn't exist.
        let result = sandbox.validate(Path::new("no/such/dir/file.txt"));
        assert!(result.is_err());
    }

    // ---- protected write targets (Quality M5) ----

    #[test]
    fn protected_memory_db_and_sidecars() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        for rel in [
            ".coding/memory.db",
            ".coding/memory.db-wal",
            ".coding/memory.db-shm",
        ] {
            let validated = sandbox.validate(Path::new(rel)).unwrap();
            assert!(
                sandbox.is_protected_write_target(&validated),
                "{rel} should be protected"
            );
        }
    }

    /// Review L6 (2026-04-19 freeze-fix review): the codegraph DB is a
    /// rebuildable cache but corrupting it via an agent write would break the
    /// `graph_*` tools until a manual delete — protect it exactly like
    /// `memory.db` (main file + SQLite `-wal`/`-shm` sidecars).
    #[test]
    fn protected_codegraph_db_and_sidecars() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        for rel in [
            ".coding/codegraph.db",
            ".coding/codegraph.db-wal",
            ".coding/codegraph.db-shm",
        ] {
            let validated = sandbox.validate(Path::new(rel)).unwrap();
            assert!(
                sandbox.is_protected_write_target(&validated),
                "{rel} should be protected"
            );
        }
    }

    #[test]
    fn protected_bookkeeping_files() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding/plans")).unwrap();
        std::fs::create_dir_all(dir.path().join(".coding/reviews")).unwrap();
        std::fs::create_dir_all(dir.path().join(".coding/knowledge/decision")).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        // safety.toml + backlog.jsonl (existing or not, the check is lexical).
        for rel in [".coding/safety.toml", ".coding/backlog.jsonl"] {
            let validated = sandbox.validate(Path::new(rel)).unwrap();
            assert!(
                sandbox.is_protected_write_target(&validated),
                "{rel} should be protected"
            );
        }
        // The plan stack + any plan markdown under .coding/plans/.
        for rel in [".coding/plans/stack.json", ".coding/plans/abc-123.md"] {
            let validated = sandbox.validate(Path::new(rel)).unwrap();
            assert!(
                sandbox.is_protected_write_target(&validated),
                "{rel} should be protected"
            );
        }
        // Review reports under .coding/reviews/ — authored only by the
        // reviewer's write_review_report tool, never by the file tools.
        let validated = sandbox
            .validate(Path::new(".coding/reviews/2026-04-08-review.md"))
            .unwrap();
        assert!(
            sandbox.is_protected_write_target(&validated),
            ".coding/reviews/ reports should be protected"
        );
        // Knowledge records under .coding/knowledge/ — authored only through
        // the memory tools' file-backed writer, never by the file tools.
        let validated = sandbox
            .validate(Path::new(".coding/knowledge/decision/2026-08-23-x.md"))
            .unwrap();
        assert!(
            sandbox.is_protected_write_target(&validated),
            ".coding/knowledge/ records should be protected"
        );
    }

    #[test]
    fn protected_nonexistent_path_caught_via_creation() {
        // The creation gap: a non-existent protected path must still be caught
        // by is_protected_write_target on the validate_for_creation result
        // (lexical, no filesystem access). This is what file_write checks
        // before creating the file.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        // .coding/plans/ does not exist yet here.
        let validated = sandbox
            .validate_for_creation(Path::new(".coding/plans/stack.json"))
            .unwrap();
        assert!(
            sandbox.is_protected_write_target(&validated),
            "non-existent stack.json must be protected"
        );
        let validated = sandbox
            .validate_for_creation(Path::new(".coding/safety.toml"))
            .unwrap();
        assert!(
            sandbox.is_protected_write_target(&validated),
            "non-existent safety.toml must be protected"
        );
    }

    // --- B3: validate_for_write ladder tests ---

    #[test]
    fn validate_for_write_creates_parent_dirs() {
        // A new nested path: validate_for_write creates the parent dirs so the
        // returned path is fully canonical (file_append now gets this too —
        // consistency with file_write).
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let validated = sandbox
            .validate_for_write(Path::new("nested/deep/file.txt"))
            .unwrap();
        assert!(validated.starts_with(sandbox.root()));
        assert!(validated.ends_with("file.txt"));
        // The parent dir was created.
        assert!(dir.path().join("nested/deep").exists());
    }

    #[test]
    fn validate_for_write_refuses_protected_nonexistent() {
        // A non-existent protected path must be refused BEFORE any dirs are
        // created (the creation gap, now in the shared ladder).
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let result = sandbox.validate_for_write(Path::new(".coding/plans/stack.json"));
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("protected"), "msg: {msg}");
        // The .coding/plans dir must NOT have been created.
        assert!(!dir.path().join(".coding/plans").exists());
    }

    #[test]
    fn validate_for_write_refuses_protected_existing() {
        // An existing protected file is refused.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        std::fs::write(dir.path().join(".coding/safety.toml"), "rules").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let result = sandbox.validate_for_write(Path::new(".coding/safety.toml"));
        assert!(result.is_err());
        assert!(format!("{}", result.unwrap_err()).contains("protected"));
    }

    #[test]
    fn validate_for_write_normal_file_passes() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("existing.txt"), "hi").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let validated = sandbox
            .validate_for_write(Path::new("existing.txt"))
            .unwrap();
        assert!(validated.ends_with("existing.txt"));
    }

    #[test]
    fn refuse_if_protected_blocks_existing() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        std::fs::write(dir.path().join(".coding/backlog.jsonl"), "{}").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let validated = sandbox
            .validate(Path::new(".coding/backlog.jsonl"))
            .unwrap();
        let result = sandbox.refuse_if_protected(&validated);
        assert!(result.is_err());
        assert!(format!("{}", result.unwrap_err()).contains("protected"));
    }

    #[test]
    fn refuse_if_protected_allows_normal() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("src.rs"), "fn main() {}").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let validated = sandbox.validate(Path::new("src.rs")).unwrap();
        assert!(sandbox.refuse_if_protected(&validated).is_ok());
    }

    // --- C2: NTFS Alternate Data Streams (ADS) protection ---

    #[test]
    fn ntfs_ads_on_protected_file_refused() {
        // `.coding/memory.db:evil` writes an ADS attached to the memory DB —
        // must be refused (it bypasses the exact-match protected check).
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        std::fs::write(dir.path().join(".coding/memory.db"), "db").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let validated = sandbox
            .validate_for_creation(Path::new(".coding/memory.db:evil"))
            .unwrap();
        assert!(
            sandbox.is_protected_write_target(&validated),
            "ADS on memory.db must be protected"
        );
    }

    #[test]
    fn ntfs_ads_on_any_file_refused() {
        // Any final component containing `:` is treated as protected (ADS) —
        // even on a non-protected file, since the stream syntax is a Windows
        // filesystem feature that can hide data.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let validated = sandbox
            .validate_for_creation(Path::new("normal.txt:hidden"))
            .unwrap();
        assert!(
            sandbox.is_protected_write_target(&validated),
            "ADS on any file must be protected"
        );
    }

    #[test]
    fn normal_file_without_colon_not_ads_blocked() {
        // A normal file with no `:` in its name is not ADS-blocked.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("src.rs"), "fn main() {}").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let validated = sandbox.validate(Path::new("src.rs")).unwrap();
        assert!(
            !sandbox.is_protected_write_target(&validated),
            "normal file must not be ADS-blocked"
        );
    }

    #[test]
    fn review_reports_protected_from_file_tools() {
        // Review reports are authored ONLY by the reviewer's
        // write_review_report tool — the file tools must refuse every path
        // under .coding/reviews/ so the main agent cannot fabricate a report.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding/reviews")).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let validated = sandbox
            .validate(Path::new(".coding/reviews/2026-04-08-review.md"))
            .unwrap();
        assert!(
            sandbox.is_protected_write_target(&validated),
            ".coding/reviews/ must be protected from the file tools"
        );
        // The creation gap: a NOT-YET-EXISTING report must be protected too
        // (the validate_for_creation lexical path — no FS access needed).
        let created = sandbox
            .validate_for_creation(Path::new(".coding/reviews/2026-04-09-new.md"))
            .unwrap();
        assert!(
            sandbox.is_protected_write_target(&created),
            "a not-yet-existing report under .coding/reviews/ must be protected"
        );
    }

    #[test]
    fn allows_writing_normal_source_files() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let validated = sandbox.validate(Path::new("src/main.rs")).unwrap();
        assert!(!sandbox.is_protected_write_target(&validated));
    }

    #[test]
    fn protected_case_variant_caught() {
        // M1-security: NTFS is case-insensitive, so a case-variant path like
        // `.coding/SAFETY.TOML` resolves to the real protected file and must
        // not bypass the guard. The check is lexical (via
        // validate_for_creation, no FS access) so this works whether or not
        // the file exists.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        for rel in [
            ".coding/SAFETY.TOML",
            ".coding/MEMORY.DB",
            ".coding/MEMORY.DB-WAL",
            ".coding/BACKLOG.JSONL",
            ".coding/PLANS/stack.json",
            ".coding/plans/UPPERCASE.md",
        ] {
            let validated = sandbox.validate_for_creation(Path::new(rel)).unwrap();
            assert!(
                sandbox.is_protected_write_target(&validated),
                "{rel} (case variant) should be protected"
            );
        }
        // A case-variant of a NON-protected path must still be allowed.
        let validated = sandbox
            .validate_for_creation(Path::new(".coding/REVIEWS/Report.MD"))
            .unwrap();
        assert!(
            sandbox.is_protected_write_target(&validated),
            ".coding/reviews/ must be protected regardless of case (the file tools \
             must never fabricate a review report)"
        );
    }

    // --- .git control plane (2027-01-09 security review HIGH-1) ---

    #[test]
    fn protected_git_control_plane() {
        // The .git control plane must never be writable by the file tools:
        // a planted .git/hooks/pre-commit or .git/config (core.fsmonitor)
        // executes unsandboxed on the next commit or an auto-approved
        // `git status`.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git/hooks")).unwrap();
        std::fs::write(dir.path().join(".git/config"), "[core]").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        for rel in [".git/config", ".git/hooks/pre-commit"] {
            let validated = sandbox.validate(Path::new(rel)).unwrap();
            assert!(
                sandbox.is_protected_write_target(&validated),
                "{rel} should be protected"
            );
        }
        // The creation gap: a not-yet-existing hook file must be protected
        // too (lexical, no FS access).
        let created = sandbox
            .validate_for_creation(Path::new(".git/hooks/pre-commit"))
            .unwrap();
        assert!(
            sandbox.is_protected_write_target(&created),
            "non-existent .git/hooks/pre-commit must be protected"
        );
    }

    #[test]
    fn protected_git_plain_file_and_nested_repo() {
        // Linked worktrees use a plain `.git` FILE (a gitdir pointer) -
        // overwriting it repoints the worktree's git. A nested repo's
        // control plane (subdir/.git/...) is the same hook-planting vector.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".git"), "gitdir: ../.git/worktrees/x").unwrap();
        std::fs::create_dir_all(dir.path().join("vendor/repo/.git/hooks")).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let validated = sandbox.validate(Path::new(".git")).unwrap();
        assert!(
            sandbox.is_protected_write_target(&validated),
            "the .git plain file (linked worktree) must be protected"
        );
        let validated = sandbox
            .validate(Path::new("vendor/repo/.git/config"))
            .unwrap();
        assert!(
            sandbox.is_protected_write_target(&validated),
            "a nested repo's .git must be protected"
        );
    }

    #[test]
    fn protected_git_case_variant_caught() {
        // NTFS is case-insensitive: .GIT/config resolves to .git/config.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let validated = sandbox
            .validate_for_creation(Path::new(".GIT/config"))
            .unwrap();
        assert!(
            sandbox.is_protected_write_target(&validated),
            ".GIT/config (case variant) should be protected"
        );
    }

    #[test]
    fn protected_git_trailing_dot_and_space_variants() {
        // Win32 path normalization strips trailing dots/spaces per component,
        // so mkdir(".git.") creates the real .git - the lexical creation path
        // must trim them before the component comparison.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        for rel in [".git./config", ".git /config", ".git./hooks/pre-commit"] {
            let validated = sandbox.validate_for_creation(Path::new(rel)).unwrap();
            assert!(
                sandbox.is_protected_write_target(&validated),
                "{rel} (trailing dot/space variant) should be protected"
            );
        }
    }

    #[test]
    fn validate_for_write_refuses_git_hook_before_mkdir() {
        // The creation ladder must refuse a .git target BEFORE creating any
        // dirs - .git/hooks must not come into existence via the refusal.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let result = sandbox.validate_for_write(Path::new(".git/hooks/pre-commit"));
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("protected"), "msg: {msg}");
        assert!(!dir.path().join(".git").exists());
    }

    #[test]
    fn refuse_if_protected_blocks_git_config() {
        // The file_edit / convert_line_endings gate.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".git/config"), "[core]").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let validated = sandbox.validate(Path::new(".git/config")).unwrap();
        let result = sandbox.refuse_if_protected(&validated);
        assert!(result.is_err());
        assert!(format!("{}", result.unwrap_err()).contains("protected"));
    }

    #[test]
    fn gitignore_and_gitattributes_stay_writable() {
        // Component equality only: .gitignore/.gitattributes/.gitmodules
        // are normal files the agent may legitimately edit.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        for rel in [".gitignore", ".gitattributes", ".gitmodules", "src/main.rs"] {
            let validated = sandbox.validate_for_creation(Path::new(rel)).unwrap();
            assert!(
                !sandbox.is_protected_write_target(&validated),
                "{rel} must stay writable"
            );
        }
    }
}
