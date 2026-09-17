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
//! `file_append`, `search`, `describe_image`) now wrap their ENTRY-POINT
//! blocking work — including the `validate` call — in one `spawn_blocking`
//! closure (they `clone()` the cheap `Sandbox` and move it in). Four sync-path
//! callers remain in THIS crate's tool layer. The app's IPC file commands
//! (`src-tauri/src/ipc/files.rs`) validate by design as well — inline on the
//! async task or inside their own `spawn_blocking` closure depending on the
//! command, never with `State` held across an await. Those four perform one
//! `canonicalize` (the fourth adds one preview read) and are kept sync by
//! design, with the trade-off noted:
//! - [`crate::agent::approval::is_project_scoped`] — one `validate` per
//!   tool-call approval decision; async-ifying it would make `needs_approval`
//!   + its callers async (a larger cross-cutting seam). Low-leverage (one
//!   syscall, infrequent).
//! - `shell::resolve_cwd` — one `validate` + `is_dir` per shell command; the
//!   command itself already runs via `tokio::process::Command` (async). Low
//!   leverage (one syscall per command).
//! - `dispatch::research_write_verdict` (2027-01-11, plan ee65fd4b) — the
//!   research artifact carve-out: one `validate` plus a depth-bounded
//!   `symlink_metadata` walk ([`Sandbox::lexical_path_is_link_free`]) per
//!   file-tool-named call while a research plan executes. It runs **under the
//!   workflow mutex** (the guard is held across it), so that hold grows by
//!   those syscalls; it is bounded to the four `RESEARCH_ARTIFACT_TOOLS` names
//!   and only fires while the filter is `ExecutingResearch`, so no other plan
//!   kind or state can reach it.
//! - the **approval-preview hook** — `Tool::approval_preview` called inline
//!   from `dispatch::execute_tool_call` (no `spawn_blocking`) →
//!   `file_edit`/`file_write::prepare_for_approval`, which calls `validate` /
//!   `validate_for_creation` and then reads the file to build the diff. One
//!   `canonicalize` + one preview read per *prompted* file-tool call, in the
//!   same approval flow as `is_project_scoped`. Only those two tools override
//!   the trait default (`file_append` and `convert_line_endings` have no hook);
//!   their own `execute` paths ARE wrapped, so the hook is the exception rather
//!   than the rule.
//!
//! The creation ladder ([`Sandbox::validate_for_write`], used by `file_write` /
//! `file_append`) added three bounded syscalls on 2027-01-15 (plan b4812291): a
//! depth-bounded `symlink_metadata` walk
//! ([`Sandbox::lexical_path_is_link_free`]) plus one `canonicalize` of the
//! deepest existing ancestor on the creation path, one `metadata` per
//! `.coding/**` protection check (the hardlink guard), and one final
//! `symlink_metadata` on the resolved write target. All three are per-write and
//! run inside the tools' existing `spawn_blocking` closure, so the async-runtime
//! contract above is unchanged.
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

    /// Validate a DIRECTORY a caller is about to `create_dir_all` itself.
    ///
    /// [`validate_for_write`](Self::validate_for_write) runs its link-free gate
    /// (step 2) and its canonical-ancestor protection check (step 2b) BEFORE it
    /// creates parent dirs (step 4), so a file write can never `mkdir` through a
    /// link. A tool that creates its own directory FIRST — `skill_create` must,
    /// because the ladder's lexical fallback cannot spell a path that does not
    /// exist yet — would issue that one `mkdir` outside the gate: with `.coding`
    /// planted as a link to an existing directory `D`,
    /// `create_dir_all(root/.coding/skills)` puts the directory at `D/skills`,
    /// and pointed at a protected tree it plants a `skills` subtree inside it.
    /// This applies the ladder's step-2/2b judgement to the directory itself.
    ///
    /// A directory that already exists is returned canonicalized (and refused
    /// when its canonical form is a protected target — e.g. a `.coding/skills`
    /// link resolving to `.coding/knowledge`). One that does not exist yet is
    /// judged through its DEEPEST EXISTING ANCESTOR, canonicalized: outside the
    /// root → refused (the mkdir would leave the project), inside a protected
    /// tree → refused. Everything below that ancestor is absent, so no component
    /// there can be a link.
    ///
    /// Judging the ancestor rather than walking every component of the caller's
    /// spelling is what keeps a link ABOVE the project from refusing a valid dir:
    /// macOS spells `/tmp` and `/var` as links into `/private`, so a spelled walk
    /// that starts at the filesystem root refused a scratch project's ordinary
    /// "skills dir does not exist yet" case (round-3 HIGH 1). Canonicalization
    /// resolves exactly those OS-level links, while a link INSIDE the tree that
    /// leaves the project still shows up — as an ancestor outside the root, or as
    /// `validate`'s own canonical result. The ancestor form also decides the
    /// out-of-root case whose parent chain is entirely missing, which the
    /// `validate` error alone cannot tell apart from "merely absent" (round-3
    /// LOW 2).
    pub fn validate_dir_creation(&self, dir: &Path) -> Result<PathBuf> {
        match self.validate(dir) {
            Ok(canonical) => {
                self.refuse_if_protected(&canonical)?;
                Ok(canonical)
            }
            Err(e) => {
                if let Some(ancestor) = canonical_existing_ancestor(dir) {
                    if !ancestor.starts_with(&self.root) {
                        return Err(Error::PathOutsideRoot(ancestor));
                    }
                    // `validate` can resolve the DIR itself outside the root even
                    // when its deepest existing ancestor is inside it: the dir is a
                    // link whose destination exists elsewhere. Keep that refusal —
                    // it is the same escape the ancestor walk just cleared.
                    if matches!(&e, Error::PathOutsideRoot(_)) {
                        return Err(e);
                    }
                    // Inside the root: the ladder's step 2b — an ancestor that
                    // resolves into a protected tree (`.coding/skills` →
                    // `.coding/knowledge`) must not get a subtree planted.
                    self.refuse_if_protected(&ancestor)?;
                }
                // Absent, with no link and no root escape in the way: the mkdir is
                // the whole point. A `None` ancestor means nothing on the chain
                // exists at all, so there is nothing to traverse.
                Ok(dir.to_path_buf())
            }
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
    /// A `.coding/**` path is ALSO protected when it is a hardlink (link count
    /// > 1, plan b4812291): canonicalize cannot see a hardlink — there is no
    /// link to follow, the path IS the file — so `.coding/x.toml` hardlinked to
    /// `.coding/safety.toml` would otherwise truncate the protected file's
    /// shared inode through an innocent-looking name. The guard is fail-closed
    /// (which file it shares its inode with does not change the refusal) and
    /// scoped to `.coding/`, where the by-name protection lives; the mirror
    /// image (a hardlink outside `.coding/` pointing at a protected file) is not
    /// detectable here — it would need an inode walk of the protected trees.
    /// Directories are exempt: their link count is structural (POSIX counts
    /// subdirectories), not a sign of sharing.
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
    /// root-absolute, so `strip_prefix(root)` yields the relative component walk
    /// below — which works before the file exists (closing the creation gap).
    pub fn is_protected_write_target(&self, path: &Path) -> bool {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        // Component-wise comparison, each component lowercased AND stripped of
        // trailing dots/spaces. Two platform realities drive this:
        //  * NTFS is case-insensitive, so `.coding/SAFETY.TOML` resolves to the
        //    real protected file and must not bypass this guard.
        //  * Win32 path normalization strips trailing dots/spaces from EVERY
        //    component, so `.coding/plans./x.md` creates and writes inside the
        //    real `.coding/plans/` — a string-prefix test on the raw spelling
        //    misses it (review LOW-3, 2027-01-11).
        // Trimming on POSIX as well over-refuses a directory literally named
        // `plans.`, which is the fail-safe direction for a REFUSAL.
        let comps: Vec<String> = rel
            .components()
            .map(|c| {
                c.as_os_str()
                    .to_string_lossy()
                    .trim_end_matches(['.', ' '])
                    .to_ascii_lowercase()
            })
            .collect();

        // NTFS Alternate Data Streams (ADS): on Windows, `file:stream` writes
        // a hidden stream attached to `file` — e.g. `.coding/memory.db:evil`
        // writes to the memory DB's ADS, bypassing the exact-match protected
        // check below. Reject ANY path whose final component contains a `:`
        // after the drive prefix (the drive prefix `C:` is on the root, not
        // the final component, so it's not affected). This closes the ADS
        // exfiltration/tampering vector on protected files.
        if comps.last().is_some_and(|c| c.contains(':')) {
            return true;
        }

        // The `.git` control plane (2027-01-09 security review HIGH-1): a
        // planted .git/hooks/pre-commit or .git/config (core.fsmonitor)
        // executes unsandboxed on the next commit or an auto-approved
        // `git status`. Any `.git` path component counts: the root
        // control-plane dir, its contents, a nested repo's .git, and the
        // plain `.git` FILE used by linked worktrees. Component equality only —
        // .gitignore/.gitattributes stay writable.
        if comps.iter().any(|c| c == ".git") {
            return true;
        }

        // Only the `.coding/` side-car tree is protected by name, and
        // everything BELOW one of these entries stays protected too (a nested
        // write into them is the same refusal).
        if !comps.first().is_some_and(|c| c == ".coding") {
            return false;
        }
        // HARDLINK GUARD (2027-01-15, plan b4812291): a hardlink inside
        // `.coding/` shares its inode with the file it was linked from, and
        // canonicalize cannot see it — there is no link to follow: the path IS
        // the file. `.coding/x.toml` hardlinked to `.coding/safety.toml` is not
        // protected by NAME, so without this check any plan kind could truncate
        // the protected file's inode through the innocent-looking alias (review
        // round-4 §Q5 item 1). Fail closed: a `.coding/**` file whose link count
        // exceeds 1 is refused — WHICH protected file it shares its inode with
        // does not change the answer. Scoped to `.coding/`, where the by-name
        // protection lives: the mirror image (a hardlink OUTSIDE `.coding/`
        // pointing at a protected file) cannot be seen here — it needs an inode
        // walk of the protected trees, which this predicate deliberately does
        // not do.
        if link_count(&self.root.join(rel)).is_some_and(|n| n > 1) {
            return true;
        }
        match comps.get(1).map(String::as_str) {
            // The memory DB + codegraph DB, each with its SQLite sidecar files,
            // plus safety.toml and the backlog file. The legacy pre-jsonl path
            // stays protected (it may linger until git removes it after
            // migration).
            Some(
                "memory.db" | "memory.db-wal" | "memory.db-shm" | "codegraph.db"
                | "codegraph.db-wal" | "codegraph.db-shm" | "safety.toml" | "backlog.jsonl"
                | "backlog.json",
            ) => true,
            // Plan files, review reports and knowledge records are authored
            // ONLY by their own tools' writers: review reports by the
            // reviewer's `write_review_report` (constructor-granted — the main
            // agent cannot fabricate one), knowledge records by the memory
            // tools' file-backed writer, plan files by the plan tools.
            Some("plans" | "reviews" | "knowledge") => true,
            _ => false,
        }
    }

    /// Whether `path` points at an ARTIFACT the file tools may write even under
    /// a research plan: the `.coding/**` side-car tree, minus everything
    /// [`is_protected_write_target`](Self::is_protected_write_target) refuses.
    ///
    /// This is the permission-GRANTING side of the path policy, so it fails
    /// closed: `path` must already be root-absolute (a validated path — from
    /// [`validate`](Self::validate) or
    /// [`validate_for_creation`](Self::validate_for_creation), which also folds
    /// `..` lexically and refuses escapes); anything else — an unresolved raw
    /// argument, a path outside the root — is never an artifact.
    ///
    /// Protected entries deliberately stay OUT of the allowance, so the
    /// research-plan carve-out in the dispatch layer (`execute_tool_call`) can
    /// never widen into `.coding/plans/`, `.coding/reviews/`,
    /// `.coding/knowledge/`, `.coding/backlog.jsonl`, the SQLite DBs,
    /// `.coding/safety.toml`, or `.git/**`. The comparison is component-wise
    /// (platform-correct separator handling) and case-insensitive — NTFS
    /// resolves `.CODING/` to the same tree.
    pub fn is_artifact_write_target(&self, path: &Path) -> bool {
        // Fail closed on an unresolved path: a permission grant must never be
        // inferred from an argument the caller has not normalized.
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return false;
        };
        // Compare COMPONENTS, not a re-written string. A backslash is an
        // ordinary name character on POSIX, so a string-level `\`→`/` fold
        // (the idiom both predicates used before the 2027-01-11 review) would
        // read the single component `.coding\\x.md` as `.coding/x.md` and grant a
        // stray file in the repo root (review LOW-2b). `components()` splits
        // exactly the way the platform does; the comparison is case-insensitive
        // because NTFS resolves `.CODING/` to the same tree.
        let mut comps = rel.components();
        match comps.next() {
            Some(std::path::Component::Normal(first))
                if first.to_string_lossy().eq_ignore_ascii_case(".coding") => {}
            _ => return false,
        }
        // A write always names a FILE under `.coding/` — the bare directory is
        // not a writable target.
        if comps.next().is_none() {
            return false;
        }
        !self.is_protected_write_target(path)
    }

    /// Whether a LEXICALLY resolved path (from
    /// [`validate_for_creation`](Self::validate_for_creation)) can be trusted:
    /// `true` only when no component that already exists is a symlink or a
    /// Windows junction — including a dangling one.
    ///
    /// The research artifact carve-out judges the lexical form only when
    /// [`validate`](Self::validate) refused the path (nothing to canonicalize
    /// yet). This is that fallback's fail-closed companion: a link inside
    /// `.coding/` pointing out of the root reads as an artifact in lexical form
    /// while the OS would follow the link at write time, so a grant inferred
    /// from the spelling must never survive a link component. Components that do
    /// not exist yet cannot be links (nothing below them exists either), so the
    /// walk stops at the first missing one.
    pub fn lexical_path_is_link_free(&self, path: &Path) -> bool {
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return false;
        };
        let mut prefix = self.root.clone();
        for comp in rel.components() {
            prefix.push(comp);
            match std::fs::symlink_metadata(&prefix) {
                Ok(meta) if meta.file_type().is_symlink() => return false,
                Ok(_) => {}
                // Not created yet — nothing below it can exist either.
                Err(_) => return true,
            }
        }
        true
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
    ///    normalize + `starts_with(root)`) — but trust the lexical spelling
    ///    ONLY when [`lexical_path_is_link_free`](Self::lexical_path_is_link_free)
    ///    agrees: the OS follows a link at write time, so a grant inferred from
    ///    the spelling must not survive one (2027-01-15, plan b4812291).
    /// 2b. Re-run the protection check on the CANONICAL form of the deepest
    ///    existing ancestor, so a spelling the lexical walk cannot see — a Win32
    ///    8.3 alias like `.coding/KNOWLE~1/…` is not a symlink — cannot get a
    ///    subtree planted inside a protected tree.
    /// 3. Check [`is_protected_write_target`](Self::is_protected_write_target)
    ///    — refuse BEFORE creating any dirs (a protected path must never be
    ///    touched, even to mkdir its parents).
    /// 4. Create parent dirs if they don't exist (so `file_append` can write to
    ///    a new nested path, matching `file_write`).
    /// 5. Revalidate (canonicalize now that the parent exists) so the returned
    ///    path is fully canonical, re-run the protection check on THAT canonical
    ///    result, and refuse a leaf that is itself a link (a dangling one comes
    ///    back as its own path and the OS would follow it). The lexical fallback
    ///    survives only for the "we just created the parent" case AND only when
    ///    the path is still link-free at that point (a link planted after the
    ///    step-2 gate is caught by that re-check); every other revalidation
    ///    failure fails closed.
    pub fn validate_for_write(&self, path: &Path) -> Result<PathBuf> {
        // Steps 1-2: validate, falling back to creation validation. The lexical
        // spelling is trusted ONLY when no existing component is a link: the OS
        // follows links at write time, so a path granted from its spelling must
        // never survive one (the research carve-out's rule, now applied to the
        // ladder itself — bypass 3, 2027-01-15, plan b4812291).
        let validated = match self.validate(path) {
            Ok(p) => p,
            Err(_) => {
                let lexical = self.validate_for_creation(path)?;
                if !self.lexical_path_is_link_free(&lexical) {
                    return Err(Error::InvalidInput(format!(
                        "refused: '{}' traverses a symbolic link or junction — name the \
                         resolved path instead",
                        path.display()
                    )));
                }
                // Step 2b: the CANONICAL form of the deepest existing ancestor.
                // A Win32 8.3 alias is not a symlink, so the walk above cannot
                // see `.coding/KNOWLE~1/…` — and mkdir through it would plant a
                // subtree inside the protected `.coding/knowledge/`. Refuse
                // BEFORE create_dir_all so nothing is even planted (bypass 2).
                if let Some(ancestor) = canonical_existing_ancestor(&lexical) {
                    if self.is_protected_write_target(&ancestor) {
                        return Err(protected_refusal(path));
                    }
                }
                lexical
            }
        };
        // Step 3: refuse protected targets BEFORE any filesystem mutation.
        if self.is_protected_write_target(&validated) {
            return Err(protected_refusal(path));
        }
        // Step 4: create parent dirs if missing (consistency — file_append now
        // creates parents like file_write).
        let mut created_parent = false;
        if let Some(parent) = validated.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent)?;
                created_parent = true;
            }
        }
        // Step 5: revalidate so the returned path is fully canonical (the
        // parent now exists, so canonicalize resolves the full path).
        match self.validate(path) {
            Ok(canonical) => {
                // The canonical result can differ from the spelling checked in
                // step 3 — re-run the protection check on IT (bypass 2's stated
                // fix direction: `KNOWLE~1` resolves to `knowledge`).
                if self.is_protected_write_target(&canonical) {
                    return Err(protected_refusal(path));
                }
                // The leaf must not be a link either: `validate` resolves an
                // EXISTING link, but a DANGLING one comes back as its own path
                // and the OS would follow it at write time — outside the root,
                // or into a protected tree (the shape review round 3 named for
                // the research route).
                if std::fs::symlink_metadata(&canonical).is_ok_and(|m| m.file_type().is_symlink())
                {
                    return Err(Error::InvalidInput(format!(
                        "refused: '{}' is a symbolic link — write to its target instead",
                        path.display()
                    )));
                }
                Ok(canonical)
            }
            // The lexical fallback is allowed ONLY when we just created the
            // parent, and only after re-checking link-freedom: the step-2 gate ran
            // BEFORE `create_dir_all`, so a link planted in the meantime — a
            // concurrent writer; the app's own approval-gated `shell` residual can
            // plant one on a timer — would have been created THROUGH and
            // `validate` then fails on the escaping parent. Re-checking makes this
            // arm fail closed in that race (review round 1, LOW 2); any other
            // revalidation failure propagates the error instead of handing the OS
            // a lexical path that resolves outside the sandbox (bypass 3).
            Err(_) if created_parent => {
                if !self.lexical_path_is_link_free(&validated) {
                    return Err(Error::InvalidInput(format!(
                        "refused: '{}' traverses a symbolic link or junction — name the \
                         resolved path instead",
                        path.display()
                    )));
                }
                Ok(validated)
            }
            Err(e) => Err(e),
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

/// The link count (hardlink count) of `path`, or `None` when it does not exist,
/// is not a regular file, or the platform cannot report it.
///
/// `std::fs::metadata` follows symlinks, which is what the hardlink guard in
/// [`Sandbox::is_protected_write_target`] wants: the count belongs to the file
/// the write would actually touch. Directories are excluded on purpose — their
/// link count is structural (POSIX counts each subdirectory's `..` entry, so any
/// directory with subdirectories reads as > 1), not a sign of inode sharing,
/// and a hardlinked directory cannot be created in the first place. The
/// exclusion matters because the guard is also run against a deepest-existing
/// ANCESTOR, which is a directory by construction.
fn link_count(path: &Path) -> Option<u64> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    platform_link_count(path, &meta)
}

/// The platform half of [`link_count`] for POSIX systems.
#[cfg(unix)]
fn platform_link_count(_path: &Path, meta: &std::fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(meta.nlink())
}

/// The platform half of [`link_count`] for Windows.
///
/// `MetadataExt::number_of_links()` is still unstable (`windows_by_handle`,
/// rust-lang/rust#63010), so the count comes straight from the Win32 API —
/// `windows-sys` is already a dependency and `src/config/keys.rs` uses the same
/// pattern for the `keys.toml` DACL.
#[cfg(windows)]
fn platform_link_count(path: &Path, _meta: &std::fs::Metadata) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_NORMAL,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // Desired access 0 queries attributes without reading data; sharing all
    // three modes keeps the count readable while another handle holds the file.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    let ok = unsafe { GetFileInformationByHandle(handle, &mut info) };
    unsafe { CloseHandle(handle) };
    (ok != 0).then(|| u64::from(info.nNumberOfLinks))
}

/// The platform half of [`link_count`] where neither POSIX nor Win32 APIs are
/// available: there is no count to read, so the guard stays quiet.
#[cfg(not(any(unix, windows)))]
fn platform_link_count(_path: &Path, _meta: &std::fs::Metadata) -> Option<u64> {
    None
}

/// Canonicalize the deepest ancestor of `path` that exists, or `None` when the
/// chain is missing all the way up (or canonicalization fails).
///
/// [`Sandbox::validate_for_write`] uses this to re-run the protection check in
/// CANONICAL form before it creates anything: a Win32 8.3 alias
/// (`.coding/KNOWLE~1/…`) is not a symlink, so only the resolved name reveals
/// that the write would land inside a protected tree (`knowledge`, `plans`,
/// `reviews`) — and refusing there means the alias cannot even get a subtree
/// planted (plan b4812291, bypass 2). A canonicalization failure skips the
/// check rather than refusing: step 5 of the ladder re-derives the canonical
/// path anyway, and it fails closed whenever it cannot — the one exception
/// being a parent this call itself just created, which is re-checked for
/// link-freedom before the lexical fallback is used.
fn canonical_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let mut ancestor = path.parent();
    while let Some(candidate) = ancestor {
        if candidate.exists() {
            return candidate.canonicalize().ok();
        }
        ancestor = candidate.parent();
    }
    None
}

/// Link fixtures shared by the escaping-link tests in this crate.
///
/// ONE canonical spelling of "plant a directory/file link, preferring the
/// privilege-free Windows one" — previously duplicated in `agent::dispatch` and
/// `agent::tests`. It lives here (test-only) so the sandbox tests, the file-tool
/// tests and the dispatch-layer tests all exercise the SAME fixture: a fixture
/// that silently gives up (no Developer Mode, no junction) is how the
/// dangling-leaf hole survived three review rounds (review LOW-1, round 3), so
/// every caller can see — and must say — when it could not plant the link.
#[cfg(test)]
pub(crate) mod link_fixture {
    /// Create a FILE link (`target` → `link`) for the escaping-link test;
    /// `false` where the platform or environment refuses.
    pub(crate) fn plant_file_link(target: &std::path::Path, link: &std::path::Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::os::windows::fs::symlink_file(target, link).is_ok()
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            false
        }
    }

    /// Plant a directory link at `link` pointing at `target`, preferring the
    /// PRIVILEGE-FREE Windows spelling — a junction via `mklink /J`, mirrored by
    /// a symlink on POSIX — so the link guard is really exercised on Windows
    /// instead of silently skipped (`symlink_dir` needs Developer Mode; review
    /// LOW-1, round 3). `false` when no spelling is available.
    pub(crate) fn plant_dir_link(target: &std::path::Path, link: &std::path::Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            // `mklink /J` is a cmd BUILTIN and is unreliable when spawned
            // through std::process::Command's argument quoting (it reported
            // `Invalid switch - "link"` for a command line PowerShell ran
            // happily). New-Item's Junction type is the API-level spelling and
            // needs no elevation either.
            let script = format!(
                "New-Item -ItemType Junction -Path '{}' -Target '{}' | Out-Null",
                link.display(),
                target.display()
            );
            let junction = std::process::Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command", script.as_str()])
                .output();
            match junction {
                Ok(out) if out.status.success() => return true,
                // Say WHY: a silently skipped guard is how the dangling-leaf
                // hole survived three review rounds (review LOW-1, round 3).
                Ok(out) => eprintln!(
                    "New-Item Junction failed ({}): {}{}",
                    out.status,
                    String::from_utf8_lossy(&out.stdout).trim(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Err(e) => eprintln!("powershell could not run: {e}"),
            }
            // Developer Mode fallback: a real directory symlink.
            std::os::windows::fs::symlink_dir(target, link).is_ok()
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            false
        }
    }
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

    // --- research-plan artifact allowance (2027-01-11) ---

    #[test]
    fn artifact_target_is_the_coding_tree_minus_protected() {
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        // Side-car artifacts are artifacts in both path forms: the lexical
        // creation path and the canonical (existing-file) path.
        std::fs::create_dir_all(dir.path().join(".coding/analysis")).unwrap();
        for raw in [
            ".coding/analysis/cache-report.md",
            ".CODING/Analysis/Report.MD",
        ] {
            let created = sandbox.validate_for_creation(Path::new(raw)).unwrap();
            assert!(
                sandbox.is_artifact_write_target(&created),
                "{raw} (creation form) must be an artifact"
            );
            // The canonical form needs the parent to EXIST, and on a
            // case-sensitive filesystem `.CODING/Analysis` is a DIFFERENT
            // directory than the one just created — so the case variant only
            // resolves on Windows (review LOW-2, round 3).
            #[cfg(windows)]
            {
                let canonical = sandbox.validate(Path::new(raw)).unwrap();
                assert!(
                    sandbox.is_artifact_write_target(&canonical),
                    "{raw} (canonical form) must be an artifact"
                );
            }
        }
        // The canonical-form assertion stays unconditional for the lowercase
        // spelling, which resolves on every platform.
        let canonical = sandbox
            .validate(Path::new(".coding/analysis/cache-report.md"))
            .unwrap();
        assert!(sandbox.is_artifact_write_target(&canonical));
        // The protected entries INSIDE .coding/ stay out of the allowance.
        for raw in [
            ".coding/plans/stack.json",
            ".coding/reviews/2026-04-08-review.md",
            ".coding/knowledge/decision/2026-08-23-x.md",
            ".coding/backlog.jsonl",
            ".coding/memory.db",
            ".coding/safety.toml",
            ".git/hooks/pre-commit",
        ] {
            let created = sandbox.validate_for_creation(Path::new(raw)).unwrap();
            assert!(
                !sandbox.is_artifact_write_target(&created),
                "{raw} must stay protected (not an artifact)"
            );
        }
        // Source, docs and config are never artifacts.
        for raw in [
            "src/lib.rs",
            "frontend/src/App.tsx",
            "docs/FEATURES.md",
            "README.md",
        ] {
            let created = sandbox.validate_for_creation(Path::new(raw)).unwrap();
            assert!(
                !sandbox.is_artifact_write_target(&created),
                "{raw} is not an artifact"
            );
        }
    }

    #[test]
    fn artifact_target_rejects_traversal_escapes() {
        // The trap the dispatch carve-out leans on: `.coding/..` must never be
        // read as "inside .coding/". Two layers close it — the resolver folds
        // `..` lexically (so the escape is either refused as out-of-root or
        // resolved to its true target), and the predicate then judges the
        // FOLDED path.
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        // Folds back INSIDE the root, onto a source file: resolves fine, but is
        // NOT an artifact — a naive `starts_with(".coding/")` on the raw
        // argument would have said it was.
        let folded = sandbox
            .validate_for_creation(Path::new(".coding/../src/main.rs"))
            .unwrap();
        assert!(folded.starts_with(sandbox.root()));
        assert!(folded.ends_with("src/main.rs"));
        assert!(
            !sandbox.is_artifact_write_target(&folded),
            ".coding/../src/main.rs must not count as an artifact"
        );
        // Escapes ABOVE the root are refused outright.
        for raw in ["../.coding/x.md", ".coding/../../outside.md"] {
            let err = sandbox.validate_for_creation(Path::new(raw)).unwrap_err();
            assert!(
                matches!(err, Error::PathOutsideRoot(_)),
                "{raw} must be refused as outside the root, got {err:?}"
            );
        }
        // A raw, unresolved argument is never an artifact: the
        // permission-granting side fails closed.
        assert!(!sandbox.is_artifact_write_target(Path::new(".coding/analysis/x.md")));
    }

    #[test]
    fn protected_check_trims_win32_trailing_dots_and_spaces() {
        // Win32 path normalization strips trailing dots/spaces from every
        // component, so `.coding/plans./x.md` creates and writes INSIDE the
        // real `.coding/plans/` — the protected check must match what the
        // filesystem does, not the spelling (review LOW-3, 2027-01-11).
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        for raw in [
            ".coding/plans./x.md",
            ".coding/reviews./2026-04-08-review.md",
            ".coding/knowledge./spec/x.md",
            ".coding/memory.db.",
            ".coding/codegraph.db ",
            ".coding/safety.toml.",
            ".coding/backlog.jsonl ",
            ".git./hooks/pre-commit",
        ] {
            let created = sandbox.validate_for_creation(Path::new(raw)).unwrap();
            assert!(
                sandbox.is_protected_write_target(&created),
                "{raw} must be protected (Win32 strips the trailing dot/space)"
            );
            assert!(
                !sandbox.is_artifact_write_target(&created),
                "{raw} must not be an artifact either"
            );
        }
    }

    #[test]
    fn artifact_grant_does_not_fold_posix_backslashes() {
        // A backslash is an ordinary name character on POSIX, so
        // `.coding\x.md` is ONE component: a stray file in the repo root, not
        // an artifact. Folding `\`→`/` (the REFUSAL-side idiom, where a false
        // positive is fail-safe) would silently grant it (review LOW-2b).
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let created = sandbox
            .validate_for_creation(Path::new(".coding\\x.md"))
            .unwrap();
        #[cfg(windows)]
        assert!(
            sandbox.is_artifact_write_target(&created),
            "on Windows a backslash IS the separator: this is .coding/x.md"
        );
        #[cfg(not(windows))]
        assert!(
            !sandbox.is_artifact_write_target(&created),
            "on POSIX .coding\\x.md is a repo-root file, never an artifact"
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

    #[test]
    fn canonical_existing_ancestor_pins_the_alias_layer() {
        // Step 2b is the layer that stops a Win32 8.3 alias from planting a
        // subtree inside a protected tree BEFORE any mkdir — but an alias
        // spelling cannot be built on a volume that generates none, so the LAYER
        // is pinned here through its helper: the deepest existing ancestor is
        // canonicalized (exactly the resolution an alias spelling relies on) and
        // the protection check then refuses it. Without this, deleting step 2b
        // would keep the suite green wherever the 8.3 test skips (round-1 review
        // LOW 4; plan b4812291, bypass 2).
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding/knowledge")).unwrap();
        std::fs::create_dir_all(dir.path().join(".coding/analysis")).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();

        let protected = canonical_existing_ancestor(&dir.path().join(".coding/knowledge/deep/x.md"))
            .expect("`.coding/knowledge` exists, so an ancestor resolves");
        assert!(
            sandbox.is_protected_write_target(&protected),
            "the canonical ancestor of a to-be-created file inside knowledge is protected: {}",
            protected.display()
        );
        assert!(
            protected.ends_with("knowledge"),
            "and it is the DEEPEST existing ancestor — refusing before create_dir_all depends on it: {}",
            protected.display()
        );

        let artifact = canonical_existing_ancestor(&dir.path().join(".coding/analysis/deep/x.md"))
            .expect("`.coding/analysis` exists, so an ancestor resolves");
        assert!(
            !sandbox.is_protected_write_target(&artifact),
            "an artifact ancestor stays writable: {}",
            artifact.display()
        );
    }

    /// (2) Windows-only: a Win32 8.3 short-name alias of a protected directory
    /// is not a symlink, so the per-component case/trailing-dot trim can never
    /// see it — the refusal has to come from the canonical form of the path
    /// (review round-4 §Q5 item 2, plan b4812291).
    #[cfg(windows)]
    #[test]
    fn protected_win32_8dot3_alias_refused() {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};

        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding/knowledge")).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let long = dir.path().join(".coding/knowledge");

        // The OS's own 8.3 spelling for the directory. `knowledge` is longer
        // than 8 characters, so an alias-enabled volume always shortens it.
        let wide: Vec<u16> = long.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut buf = vec![0u16; 512];
        let written = unsafe {
            windows_sys::Win32::Storage::FileSystem::GetShortPathNameW(
                wide.as_ptr(),
                buf.as_mut_ptr(),
                buf.len() as u32,
            )
        };
        if written == 0 || written as usize >= buf.len() {
            eprintln!("SKIP: GetShortPathNameW did not resolve the alias fixture");
            return;
        }
        buf.truncate(written as usize);
        let short = std::path::PathBuf::from(std::ffi::OsString::from_wide(&buf));
        let alias = short
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        if alias.is_empty() || alias.eq_ignore_ascii_case("knowledge") {
            eprintln!(
                "SKIP: this volume generates no 8.3 alias for 'knowledge' \
                 (8dot3name disabled) — the alias assertion is not exercised"
            );
            return;
        }

        let rel = format!(".coding/{alias}/new/x.md");
        assert!(
            dir.path().join(".coding").join(&alias).is_dir(),
            "the alias fixture must resolve to the protected directory"
        );
        let result = sandbox.validate_for_write(Path::new(&rel));
        assert!(
            result.is_err(),
            "the 8.3 spelling must be refused as protected: {rel}"
        );
        assert!(
            !dir.path().join(".coding/knowledge/new").exists(),
            "no subtree may be planted inside the protected tree"
        );
    }

    #[test]
    fn validate_for_write_fails_closed_on_out_of_root_link() {
        // (3) A link inside `.coding/` pointing OUT of the root: `validate`
        // refuses the canonical resolution, and the ladder's step-5 fallback
        // used to hand the OS the lexical path — which resolves outside the
        // sandbox (create_dir_all ran through the link too). Every spelling
        // must fail closed, and nothing may be created outside. The dangling
        // LEAF is the same family one step further: `validate` returns Ok for
        // it, so the refusal has to come from the ladder's canonical branch
        // (review LOW-1, round 3, named that shape for the research route).
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        if !link_fixture::plant_dir_link(&outside.path(), &dir.path().join(".coding/link")) {
            eprintln!("SKIP: could not create a directory link out of the root");
            return;
        }

        for rel in [".coding/link/x.md", ".coding/link/nested/x.md"] {
            let result = sandbox.validate_for_write(Path::new(rel));
            assert!(result.is_err(), "{rel} resolves outside the root — refused");
        }
        assert_eq!(
            std::fs::read_dir(outside.path()).unwrap().count(),
            0,
            "nothing may be created through the escaping link"
        );

        // The dangling LEAF: `validate` returns `Ok` for it (its parent+file-name
        // fallback appends the raw leaf name), so the refusal has to come from
        // the ladder's own leaf check. A file symlink needs Developer Mode on
        // Windows, so a dangling DIRECTORY link (a privilege-free junction) is
        // the fallback spelling — either one reaches the same branch, and when
        // neither can be planted the skip is SAID, never silent (round-1 review
        // LOW 3).
        let (file_leaf, dir_leaf) = (".coding/dangling.md", ".coding/dangling");
        let leaf = if link_fixture::plant_file_link(
            &outside.path().join("missing.md"),
            &dir.path().join(file_leaf),
        ) {
            Some(file_leaf)
        } else if link_fixture::plant_dir_link(
            &outside.path().join("missing-dir"),
            &dir.path().join(dir_leaf),
        ) {
            Some(dir_leaf)
        } else {
            None
        };
        match leaf {
            Some(leaf) => {
                let result = sandbox.validate_for_write(Path::new(leaf));
                assert!(
                    result.is_err(),
                    "a dangling link leaf must be refused too: {leaf}"
                );
                assert_eq!(
                    std::fs::read_dir(outside.path()).unwrap().count(),
                    0,
                    "the dangling target must not be created"
                );
            }
            None => eprintln!(
                "SKIP: could not create a dangling link leaf (file symlink or junction) for the ladder fixture"
            ),
        }
    }

    #[test]
    fn dir_creation_gate_refuses_a_link_component() {
        // A tool that must `create_dir_all` for itself runs that mkdir BEFORE
        // validate_for_write for the file, so the ladder's step-2 link gate has
        // to be applied to the DIRECTORY too — otherwise the mkdir lands at the
        // link's destination (review round-2 LOW 1, 2027-01-17).
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        std::fs::create_dir_all(dir.path().join(".coding")).unwrap();

        // The plain case: a not-yet-existing dir with no link in its path is
        // exactly what the gate is for.
        assert!(
            sandbox
                .validate_dir_creation(&dir.path().join(".coding/skills"))
                .is_ok(),
            "a link-free, not-yet-existing dir is allowed"
        );

        // A link component: refused before the mkdir. The fixture prefers the
        // privilege-free junction spelling on Windows, and when neither
        // spelling can be planted the skip is SAID, never silent.
        let outside_link = dir.path().join(".coding/skills_outside");
        if link_fixture::plant_dir_link(outside.path(), &outside_link) {
            assert!(
                sandbox.validate_dir_creation(&outside_link).is_err(),
                "a dir link escaping the root must be refused before the mkdir"
            );
        } else {
            eprintln!("SKIP: could not plant a directory link for the dir-creation gate fixture");
        }

        // A link resolving INSIDE the root into a protected tree is refused by
        // the canonical protection re-check, not merely by the link walk.
        std::fs::create_dir_all(dir.path().join(".coding/knowledge")).unwrap();
        let protected_link = dir.path().join(".coding/skills_knowledge");
        if link_fixture::plant_dir_link(&dir.path().join(".coding/knowledge"), &protected_link) {
            assert!(
                sandbox.validate_dir_creation(&protected_link).is_err(),
                "a dir link into a protected tree must be refused"
            );
        } else {
            eprintln!("SKIP: could not plant a second directory link for the gate fixture");
        }
    }

    #[test]
    fn dir_creation_gate_allows_a_link_above_the_root() {
        // Round-3 HIGH 1: the gate must judge the CANONICAL ANCESTOR, not every
        // component of the caller's spelling. macOS spells `/tmp` and `/var` as
        // links into `/private`, so a spelled walk that started at the filesystem
        // root refused the ordinary "skills dir does not exist yet" case — the
        // three `skill_create` happy-path tests fail there. A link ABOVE the root
        // is not this sandbox's business: the root is canonicalized, so the alias
        // resolves to the same tree the mkdir lands in.
        let real = tempdir().unwrap();
        let holder = tempdir().unwrap();
        let alias = holder.path().join("alias");
        if !link_fixture::plant_dir_link(real.path(), &alias) {
            eprintln!("SKIP: could not plant a directory link for the ancestor fixture");
            return;
        }
        let sandbox = Sandbox::new(&alias).unwrap();
        let target = alias.join(".coding/skills");
        assert!(
            sandbox.validate_dir_creation(&target).is_ok(),
            "a link above the root must not refuse a not-yet-existing dir"
        );
        // …and the mkdir really lands inside the root the sandbox named.
        std::fs::create_dir_all(&target).unwrap();
        assert!(
            real.path().join(".coding/skills").is_dir(),
            "the directory is created through the alias, inside the root"
        );
    }

    #[test]
    fn dir_creation_gate_refuses_an_out_of_root_dir_without_any_link() {
        // No link anywhere: the escape is invisible to `validate` (the parent
        // chain is missing entirely), so only the canonical-ancestor CONTAINMENT
        // check can see it (round-3 LOW 2, pinned here — the round-4 review found
        // the arm deletable with a green suite). Assert the VARIANT, not a bare
        // `is_err()`: pre-fix this returned `Ok(dir)` on Windows and the spelled
        // walk's link refusal on macOS, so only the error kind fails pre-fix on
        // both platforms.
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let target = outside.path().join("missing/sub/skills");
        assert!(
            matches!(
                sandbox.validate_dir_creation(&target),
                Err(Error::PathOutsideRoot(_))
            ),
            "an out-of-root mkdir must be refused by containment, not by a link walk"
        );
    }

    #[test]
    fn protected_hardlink_inside_coding_refused() {
        // (1) A hardlink inside `.coding/` shares its inode with a protected
        // file, and canonicalize cannot see it — there is no link to follow:
        // the path IS the file. Without an inode check any plan kind could
        // truncate the protected file through the innocent-looking alias name
        // (review round-4 §Q5 item 1, plan b4812291).
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding/analysis")).unwrap();
        std::fs::write(dir.path().join(".coding/safety.toml"), "[safety]\n").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();

        if std::fs::hard_link(
            dir.path().join(".coding/safety.toml"),
            dir.path().join(".coding/x.toml"),
        )
        .is_err()
        {
            eprintln!("SKIP: this volume cannot create a hardlink (no shared inode to test)");
            return;
        }

        let validated = sandbox.validate(Path::new(".coding/x.toml")).unwrap();
        assert!(
            sandbox.is_protected_write_target(&validated),
            "a hardlinked alias under .coding/ is a protected write target"
        );
        let result = sandbox.validate_for_write(Path::new(".coding/x.toml"));
        assert!(result.is_err(), "the hardlink must not be writable");
        assert!(format!("{}", result.unwrap_err()).contains("protected"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join(".coding/safety.toml")).unwrap(),
            "[safety]\n",
            "the shared inode stays untouched"
        );

        // A plain (link count 1) artifact under .coding/ still goes through:
        // the refusal is about inode sharing, not about `.coding/` as such.
        assert!(
            sandbox
                .validate_for_write(Path::new(".coding/analysis/notes.md"))
                .is_ok(),
            "ordinary artifacts stay writable"
        );
    }

    #[test]
    fn validate_for_write_refuses_in_root_link_into_protected_tree() {
        // (2) The cross-platform twin of the Win32 8.3 alias: `.coding/link` is
        // a real link to the protected `.coding/knowledge/`. The ladder's
        // lexical branch trusted the spelling, ran create_dir_all THROUGH the
        // link (planting a subtree inside the protected tree) and returned a
        // path under `.coding/knowledge/`. The layer that refuses it is the
        // step-2 link-free GATE — the canonical-ancestor check (2b) and the
        // step-5 canonical re-check are the backstops for spellings the gate
        // cannot see (a Win32 8.3 alias is not a link, so the gate passes it):
        // `canonical_existing_ancestor_pins_the_alias_layer` pins the 2b helper
        // contract cross-platform (review round-4 §Q5 item 2, plan b4812291;
        // round-1 review LOW 4).
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".coding/knowledge")).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        if !link_fixture::plant_dir_link(
            &dir.path().join(".coding/knowledge"),
            &dir.path().join(".coding/link"),
        ) {
            eprintln!("SKIP: could not create a directory link into the protected tree");
            return;
        }

        let result = sandbox.validate_for_write(Path::new(".coding/link/new/x.md"));
        assert!(result.is_err(), "a write through the link must be refused");
        assert!(
            !dir.path().join(".coding/knowledge/new").exists(),
            "no subtree may be planted inside the protected tree"
        );
    }
}
