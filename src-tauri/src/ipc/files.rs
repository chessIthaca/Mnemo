// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! File-browser / conversation Tauri commands.
//!
//! Read/list project files (sandbox-validated), list markdown docs for the
//! viewer dropdown, read the git branch for the status bar, and save/load
//! conversation transcripts.

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::io::Read;
use tauri::AppHandle;
use tauri::State;
use tauri_plugin_dialog::DialogExt;

use mnemo::project::Project;
use mnemo::runtime::channels::AgentId;
use mnemo::tool::agent::image_tools::{magic_matches, mime_from_ext};
use mnemo::tool::agent::sandbox::Sandbox;

use crate::ipc::error::IpcError;
use crate::ipc::state::IpcState;

/// Read a file from the project (for the md viewer / file browser).
/// Routes the path through the sandbox — no file outside the project root
/// is accessible.
///
/// The read is BLOCKING, so it runs on `spawn_blocking` (the path is
/// sandbox-validated BEFORE the spawn) — a slow disk never parks an async
/// worker. Mirrors `get_git_branch` (review N2, 2026-06-14).
#[tauri::command]
pub async fn read_file(state: State<'_, IpcState>, path: String) -> Result<String, IpcError> {
    let sandbox = state.project.sandbox.clone();
    let validated = sandbox
        .validate(std::path::Path::new(&path))
        .map_err(|e| format!("path rejected: {e}"))?;
    tokio::task::spawn_blocking(move || std::fs::read_to_string(&validated))
        .await
        .map_err(|e| format!("read task failed: {e}"))
        .and_then(|r| r.map_err(|e| format!("failed to read {path}: {e}")))
        .map_err(IpcError::from)
}

/// Size cap for images loaded into the chat (20 MiB of raw bytes) — a
/// malicious or accidental giant file must not ship tens of MB over IPC.
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

/// Read `path` (project-relative, forward slashes) and return it as a base64
/// `data:` URL for inline `<img>` rendering (sync — unit-testable; the
/// [`read_image_data_url`] command wraps this on `spawn_blocking`).
///
/// The path is sandbox-validated (cannot escape the project root) and the
/// read is BOUNDED: the file is streamed through a `take(MAX + 1)` cap, so
/// even a file that grows between open and read can never ship more than
/// [`MAX_IMAGE_BYTES`] + 1 over IPC (no stat-then-read TOCTOU). The extension
/// must map to a known image MIME type and the content must match that
/// type's magic bytes — the same checks the agent's `load_image_data_url`
/// loader applies, so a mislabeled file renders nothing instead of garbage.
/// Errors with a human-readable reason; never panics.
fn read_image_data_url_sync(sandbox: &Sandbox, path: &str) -> Result<String, String> {
    let validated = sandbox
        .validate(std::path::Path::new(path))
        .map_err(|e| format!("path rejected: {e}"))?;
    let ext = validated
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| format!("no image extension: {path}"))?;
    let mime = mime_from_ext(ext).ok_or_else(|| {
        format!("unsupported image extension '.{ext}' (allowed: png, jpg, jpeg, gif, webp, bmp)")
    })?;
    let mut file =
        std::fs::File::open(&validated).map_err(|e| format!("failed to open {path}: {e}"))?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_IMAGE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("failed to read {path}: {e}"))?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(format!(
            "image too large: {path} (cap is {} MiB)",
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }
    if !magic_matches(mime, &bytes) {
        return Err(format!(
            "image content does not match '.{ext}' (expected {mime}) — the magic bytes do not match"
        ));
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime};base64,{b64}"))
}

/// Read an image from the project as a base64 `data:` URL for inline chat
/// rendering (the ToolImage component). Sandbox-validated, extension
/// allowlisted, content magic-checked, size capped — see
/// [`read_image_data_url_sync`].
///
/// The read is BLOCKING, so it runs on `spawn_blocking`; the closure
/// sandbox-validates the path FIRST, inside the blocking thread (owned
/// `Sandbox` clone + owned path moved in — `State` never crosses the await).
/// Mirrors `read_file` (review N2, 2026-06-14).
#[tauri::command]
pub async fn read_image_data_url(
    state: State<'_, IpcState>,
    path: String,
) -> Result<String, IpcError> {
    let sandbox = state.project.sandbox.clone();
    tokio::task::spawn_blocking(move || read_image_data_url_sync(&sandbox, &path))
        .await
        .map_err(|e| format!("image read task failed: {e}"))
        .and_then(|r| r)
        .map_err(IpcError::from)
}

/// Validate + write `content` to `path` through the project sandbox (sync —
/// unit-testable; the [`write_file`] command wraps this on `spawn_blocking`).
///
/// Mirrors the agent file tools' protections: the path is sandbox-validated
/// (cannot escape the project root) and protected paths (.coding
/// state/bookkeeping or the .git control plane) are refused with the shared
/// message, so a UI edit can't desync live workflow state. Returns the
/// written path with forward
/// slashes (the app's display convention).
fn write_sandboxed(sandbox: &Sandbox, path: &str, content: &str) -> Result<String, String> {
    let validated = sandbox
        .validate(std::path::Path::new(path))
        .map_err(|e| format!("path rejected: {e}"))?;
    sandbox
        .refuse_if_protected(&validated)
        .map_err(|e| format!("{e}"))?;
    std::fs::write(&validated, content).map_err(|e| format!("failed to write {path}: {e}"))?;
    Ok(validated.to_string_lossy().replace('\\', "/"))
}

/// Persist a FileViewer markdown edit (the editor's Save button / Ctrl+S).
///
/// The write is BLOCKING, so it runs on `spawn_blocking` with owned data
/// (convention N2, 2026-06-14) — a slow disk never parks an async worker and
/// `State` never crosses the await. Returns the written path.
#[tauri::command]
pub async fn write_file(
    state: State<'_, IpcState>,
    path: String,
    content: String,
) -> Result<String, IpcError> {
    let sandbox = state.project.sandbox.clone();
    tokio::task::spawn_blocking(move || write_sandboxed(&sandbox, &path, &content))
        .await
        .map_err(|e| format!("write task failed: {e}"))
        .map_err(IpcError::from)?
        .map_err(IpcError::from)
}

#[cfg(test)]
mod tests {
    use super::{
        git_diff_head_at, git_history_at, git_init_at, read_git_branch, read_image_data_url_sync,
        repo_relative_pathspec, write_sandboxed, GIT_DIFF_CHAR_BUDGET, MAX_IMAGE_BYTES,
    };
    use mnemo::tool::agent::sandbox::Sandbox;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    /// Regression (F1 freeze, 2026-04-19): `read_git_branch` must report the
    /// repo's branch and fall back to `"no-branch"` outside a repo. It backs
    /// `get_git_branch`, which now runs it via `spawn_blocking` off the async
    /// runtime — this test pins the sync helper's behavior.
    #[test]
    fn read_git_branch_reports_branch_and_no_branch_fallback() {
        // Repo with one commit so HEAD resolves, checked out on a named
        // branch (an unborn HEAD makes `rev-parse HEAD` fail).
        let dir = tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "commit.gpgsign", "false"]);
        fs::write(root.join("file.txt"), "x\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "initial"]);
        git(&["checkout", "-q", "-b", "fix-test"]);
        assert_eq!(read_git_branch(root), "fix-test");

        // Not a repo → "no-branch".
        let bare = tempdir().unwrap();
        assert_eq!(read_git_branch(bare.path()), "no-branch");
    }

    /// Helper: init a committed repo in `root` with `files` committed.
    fn committed_repo(root: &std::path::Path, files: &[(&str, &str)]) {
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        git(&["config", "user.email", "test@example.com"]);
        git(&["config", "user.name", "Test"]);
        git(&["config", "commit.gpgsign", "false"]);
        for (name, content) in files {
            let p = root.join(name);
            // Create parent dirs for nested paths (fs::write creates none).
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&p, content).unwrap();
        }
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "initial"]);
    }

    /// The comprehensive diff: reports tracked working-tree changes vs HEAD,
    /// scopes to one file when a path is given, and is empty on a clean tree.
    #[test]
    fn git_diff_head_reports_changes_and_scope() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        committed_repo(root, &[("a.txt", "one\n"), ("b.txt", "two\n")]);

        // Clean tree → empty diff.
        assert_eq!(git_diff_head_at(root, None).unwrap(), "");

        // Modify both files: the whole-tree diff covers both, the scoped
        // diff covers only the requested file.
        fs::write(root.join("a.txt"), "one\nchanged\n").unwrap();
        fs::write(root.join("b.txt"), "TWO\n").unwrap();
        let all = git_diff_head_at(root, None).unwrap();
        assert!(all.contains("+changed"), "whole diff covers a.txt:\n{all}");
        assert!(all.contains("+TWO"), "whole diff covers b.txt:\n{all}");
        let only_a =
            git_diff_head_at(root, Some(&root.join("a.txt").display().to_string())).unwrap();
        assert!(
            only_a.contains("+changed"),
            "scoped diff covers a.txt:\n{only_a}"
        );
        assert!(
            !only_a.contains("b.txt"),
            "scoped diff must not leak other files:\n{only_a}"
        );

        // A change staged (not just in the working tree) is still reported —
        // the view is comprehensive vs HEAD.
        fs::write(root.join("b.txt"), "TWO\nstaged\n").unwrap();
        let status = Command::new("git")
            .args(["add", "b.txt"])
            .current_dir(root)
            .status()
            .unwrap();
        assert!(status.success());
        let staged =
            git_diff_head_at(root, Some(&root.join("b.txt").display().to_string())).unwrap();
        assert!(
            staged.contains("+staged"),
            "staged change vs HEAD:\n{staged}"
        );

        // No ANSI color codes even when git would colorize.
        assert!(
            !staged.contains('\u{1b}'),
            "diff must be color-free:\n{staged}"
        );
    }

    /// Outside a repo the command returns a short canonical message (not
    /// git's giant `--no-index` usage dump) so the UI can render a friendly
    /// panel.
    #[test]
    fn git_diff_head_non_repo_errors() {
        let bare = tempdir().unwrap();
        let err = git_diff_head_at(bare.path(), None).unwrap_err();
        assert_eq!(err, "not a git repository");
    }

    /// A repo with no commits yet (an unborn HEAD) yields a short canonical
    /// message — the case the Diff tab hits immediately after the user clicks
    /// "Initialize Git Repository".
    #[test]
    fn git_diff_head_no_commits_canonical() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["init", "-q"]);
        // No commit → HEAD is unborn → `git diff HEAD` fails.
        let err = git_diff_head_at(root, None).unwrap_err();
        assert_eq!(err, "no commits yet");
    }

    /// `git_init_at` creates a repository in place: after it succeeds, the
    /// diff read no longer reports "not a git repository" (it reports "no
    /// commits yet" instead — proving the init took effect).
    #[test]
    fn git_init_at_creates_repository() {
        let bare = tempdir().unwrap();
        // Before init: not a repo.
        assert_eq!(
            git_diff_head_at(bare.path(), None).unwrap_err(),
            "not a git repository"
        );
        // Initialize in place.
        git_init_at(bare.path()).expect("git init should succeed");
        // After init: a repo with no commits yet (no longer "not a repo").
        assert_eq!(
            git_diff_head_at(bare.path(), None).unwrap_err(),
            "no commits yet"
        );
    }

    /// A tiny valid 1x1 PNG (8 bytes header + IHDR + IDAT + IEND) so the
    /// happy path exercises real decoding-ready bytes.
    const TINY_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG signature
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR length + type
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
        0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, // bit depth etc + CRC
        0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, // IDAT length + type
        0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, // zlib data
        0x0D, 0x0A, 0x2D, 0xB4, // IDAT CRC
        0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, // IEND length + type
        0xAE, 0x42, 0x60, 0x82, // IEND CRC
    ];

    /// The inline-image IPC: a valid image comes back as a base64 `data:`
    /// URL with the right MIME; non-images, missing files, unknown
    /// extensions, mislabeled content, oversize files and out-of-sandbox
    /// paths are all rejected.
    #[test]
    fn read_image_data_url_validates_and_encodes() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let sandbox = Sandbox::new(root.to_path_buf()).unwrap();
        fs::write(root.join("shot.png"), TINY_PNG).unwrap();

        // Happy path: data:image/png;base64,<bytes> — the base64 decodes
        // back to the exact original bytes.
        let url = read_image_data_url_sync(&sandbox, "shot.png").unwrap();
        assert!(
            url.starts_with("data:image/png;base64,"),
            "mime prefix: {url}"
        );
        let b64 = url.strip_prefix("data:image/png;base64,").unwrap();
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("base64 decodes");
        assert_eq!(decoded, TINY_PNG, "round-trips the original bytes");

        // Non-image extension → rejected before any read.
        fs::write(root.join("notes.txt"), "hello").unwrap();
        let err = read_image_data_url_sync(&sandbox, "notes.txt").unwrap_err();
        assert!(err.contains("unsupported image extension"), "{err}");

        // Missing file → rejected.
        let err = read_image_data_url_sync(&sandbox, "nope.png").unwrap_err();
        assert!(!err.is_empty(), "missing file must error");

        // Mislabeled content → rejected (magic bytes must match the
        // extension's MIME type — a renamed text file is not an image).
        fs::write(root.join("fake.png"), b"this is not a png at all").unwrap();
        let err = read_image_data_url_sync(&sandbox, "fake.png").unwrap_err();
        assert!(err.contains("magic bytes"), "{err}");

        // Path escaping the sandbox → rejected.
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("x.png"), TINY_PNG).unwrap();
        let escaped = format!(
            "{}/x.png",
            outside.path().display().to_string().replace('\\', "/")
        );
        let err = read_image_data_url_sync(&sandbox, &escaped).unwrap_err();
        assert!(err.contains("path rejected"), "{err}");

        // jpg maps to image/jpeg.
        fs::write(root.join("photo.jpg"), [0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
        let url = read_image_data_url_sync(&sandbox, "photo.jpg").unwrap();
        assert!(url.starts_with("data:image/jpeg;base64,"), "{url}");
    }

    /// The size cap is a BOUNDED read, not a stat-then-read: a file larger
    /// than `MAX_IMAGE_BYTES` is rejected without ever buffering the whole
    /// payload into the data URL.
    #[test]
    fn read_image_data_url_rejects_oversize_images() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let sandbox = Sandbox::new(root.to_path_buf()).unwrap();
        // A valid PNG signature + a payload that exceeds the cap. The magic
        // check only needs the signature, so the size error wins on large
        // files regardless of the tail content.
        let mut big = Vec::from(TINY_PNG);
        big.resize((MAX_IMAGE_BYTES + 1024) as usize, 0);
        fs::write(root.join("big.png"), &big).unwrap();
        let err = read_image_data_url_sync(&sandbox, "big.png").unwrap_err();
        assert!(err.contains("too large"), "{err}");
    }

    /// The Git tab's dataset: branches (current/main/merged flags), the
    /// commit DAG with parents + decorations, and main's first-parent spine.
    ///
    /// Scenario: initial + one commit on main, a feature branch with one
    /// commit, then a --no-ff merge of the feature branch into main — so the
    /// merge commit has two parents and the spine (first-parent chain) skips
    /// the feature commit.
    #[test]
    fn git_history_reports_branches_commits_and_spine() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        committed_repo(root, &[("a.txt", "one\n")]); // "initial" on the default branch
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["branch", "-m", "main"]); // normalize the default branch name
        fs::write(root.join("b.txt"), "two\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "second on main"]);
        git(&["checkout", "-q", "-b", "feat/x"]);
        fs::write(root.join("c.txt"), "three\n").unwrap();
        git(&["add", "-A"]);
        git(&["commit", "-q", "-m", "feature commit"]);
        git(&["checkout", "-q", "main"]);
        git(&["merge", "-q", "--no-ff", "-m", "merge feat/x", "feat/x"]);

        let h = git_history_at(root).unwrap();

        // Current branch + branch flags. feat/x is fully merged into main.
        assert_eq!(h.current_branch, "main");
        assert_eq!(h.branches.len(), 2);
        assert_eq!(
            h.branches[0].name, "main",
            "main sorts first: {:?}",
            h.branches
        );
        let main = &h.branches[0];
        assert!(main.is_main && main.is_current, "main flags: {main:?}");
        assert!(main.upstream_track.is_none(), "no remote configured");
        let fx = h.branches.iter().find(|b| b.name == "feat/x").unwrap();
        assert!(!fx.is_main && !fx.is_current);
        assert!(fx.merged_into_main, "feat/x merged: {fx:?}");
        assert!(!fx.tip_sha.is_empty());

        // The DAG: the merge commit carries two parents and HEAD decoration;
        // the feature commit one parent.
        let merge = h
            .commits
            .iter()
            .find(|c| c.subject == "merge feat/x")
            .expect("merge commit present");
        assert_eq!(merge.parents.len(), 2, "merge has two parents: {merge:?}");
        assert!(
            merge.refs.iter().any(|r| r == "HEAD -> main"),
            "HEAD decoration: {:?}",
            merge.refs
        );
        let feature = h
            .commits
            .iter()
            .find(|c| c.subject == "feature commit")
            .expect("feature commit present");
        assert_eq!(feature.parents.len(), 1);

        // main's spine is the FIRST-PARENT chain (merge -> second -> initial);
        // the feature commit is reachable only through the second parent.
        assert_eq!(h.main_spine.len(), 3, "spine: {:?}", h.main_spine);
        assert_eq!(h.main_spine[0], merge.sha, "spine starts at the merge");
        assert!(
            !h.main_spine.contains(&feature.sha),
            "first-parent spine skips the feature commit"
        );
    }

    /// Outside a repo the history read errors (the Git tab surfaces the
    /// message) instead of returning an empty-but-successful graph.
    #[test]
    fn git_history_non_repo_errors() {
        let bare = tempdir().unwrap();
        let err = git_history_at(bare.path()).unwrap_err();
        assert!(!err.is_empty(), "expected a git error message");
    }

    /// Subjects containing the "obvious" separators (`|`, `, `, `->`) must
    /// survive the unit-separator (%x1f) parse verbatim — the whole reason
    /// the format uses %x1f instead of `|`. Also pins the no-`main` path:
    /// a repo whose default branch isn't main gets an empty spine and no
    /// merged flags rather than an error. The default branch is FORCED to
    /// `trunk` so the missing-main path always runs regardless of the
    /// machine's `init.defaultBranch`.
    #[test]
    fn git_history_tolerates_pipe_in_subject_and_missing_main() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        committed_repo(root, &[("a.txt", "one\n")]);
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        git(&["branch", "-m", "trunk"]); // force a non-main default branch
        git(&[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "pipe | comma , arrow -> subject",
        ]);

        let h = git_history_at(root).unwrap();
        let c = h
            .commits
            .iter()
            .find(|x| x.subject.starts_with("pipe"))
            .expect("separator-heavy subject present");
        assert_eq!(c.subject, "pipe | comma , arrow -> subject");
        // No branch named main — the command must not error, the spine is
        // empty, and no branch is flagged merged.
        assert!(
            h.branches.iter().all(|b| !b.is_main),
            "forced non-main default: {:?}",
            h.branches
        );
        assert!(h.main_spine.is_empty(), "no main → empty spine");
        assert!(
            h.branches.iter().all(|b| !b.merged_into_main),
            "no main → no merged flags"
        );
    }

    /// Regression (review H1): the scoped path must reach git as a
    /// REPO-RELATIVE pathspec. The production input is the sandbox-validated
    /// (canonicalized, verbatim `\\?\`-prefixed on Windows) absolute path —
    /// which git-for-Windows can reject as outside the repository. Pin that
    /// `repo_relative_pathspec` maps the sandbox output to a plain relative
    /// path, and that git accepts the scoped diff with it.
    #[test]
    fn git_diff_head_scoped_path_is_repo_relative() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        committed_repo(root, &[("a.txt", "one\n"), ("sub/b.txt", "two\n")]);
        fs::write(root.join("a.txt"), "one\nchanged\n").unwrap();
        let sandbox = Sandbox::new(root).unwrap();

        // Production-shaped inputs: relative as typed, and the canonicalized
        // absolute form the UI sends. Both must land as plain repo-relative
        // forward-slash pathspecs — never verbatim-prefixed.
        assert_eq!(repo_relative_pathspec(&sandbox, "a.txt").unwrap(), "a.txt");
        let abs = root.join("a.txt").display().to_string();
        let rel = repo_relative_pathspec(&sandbox, &abs).unwrap();
        assert_eq!(rel, "a.txt");
        assert!(!rel.starts_with("\\\\?\\"), "no verbatim prefix: {rel}");
        // Nested paths join with forward slashes.
        assert_eq!(
            repo_relative_pathspec(&sandbox, "sub/b.txt").unwrap(),
            "sub/b.txt"
        );
        // Escaping the root is rejected before git runs.
        assert!(repo_relative_pathspec(&sandbox, "../outside.txt").is_err());

        // And git accepts the relative pathspec for the scoped diff.
        let diff = git_diff_head_at(root, Some(&rel)).unwrap();
        assert!(
            diff.contains("+changed"),
            "scoped diff covers a.txt:\n{diff}"
        );
        assert!(!diff.contains("b.txt"), "no leak of other files:\n{diff}");
    }

    /// Regression (review M2 — F3 freeze class): an over-budget diff is
    /// truncated (char-boundary safe) and visibly marked, so the IPC payload
    /// + DOM node count stay bounded on huge changesets.
    #[test]
    fn git_diff_head_caps_output_at_the_char_budget() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        // A file whose full rewrite produces a diff far over the budget
        // (~6_000 lines × ~29 chars per side ≈ 350k chars of -/+ lines).
        let original: String = (0..6_000)
            .map(|i| format!("line {i} — 0123456789abcdef\n"))
            .collect();
        committed_repo(root, &[("big.txt", &original)]);
        let changed: String = (0..6_000)
            .map(|i| format!("LINE {i} — fedcba9876543210\n"))
            .collect();
        fs::write(root.join("big.txt"), changed).unwrap();

        let diff = git_diff_head_at(root, None).unwrap();
        assert!(
            diff.contains("diff truncated"),
            "over-budget diff carries the truncation marker:\n{}",
            &diff[diff.len().saturating_sub(200)..]
        );
        // Budget + marker stay bounded (marker is well under 200 chars).
        assert!(
            diff.chars().count() <= GIT_DIFF_CHAR_BUDGET + 200,
            "payload stays near the budget"
        );
    }

    /// write_file round-trip: the sync helper writes content through the
    /// sandbox and returns a forward-slash path.
    #[test]
    fn write_sandboxed_round_trips() {
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let written = write_sandboxed(&sandbox, "notes.md", "# Hello\n").unwrap();
        assert!(written.ends_with("notes.md"));
        assert!(
            !written.contains('\\'),
            "path should use forward slashes: {written}"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("notes.md")).unwrap(),
            "# Hello\n"
        );
    }

    /// A path escaping the project root is rejected before any write happens.
    #[test]
    fn write_sandboxed_rejects_escape() {
        let dir = tempdir().unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let err = write_sandboxed(&sandbox, "../outside.md", "x").unwrap_err();
        assert!(err.contains("path rejected"), "unexpected error: {err}");
        assert!(!dir.path().parent().unwrap().join("outside.md").exists());
    }

    /// Protected live-state files (plan markdown) are refused — parity with
    /// the agent file tools' `refuse_if_protected` gate.
    #[test]
    fn write_sandboxed_refuses_protected_plan_file() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".coding").join("plans")).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let err = write_sandboxed(&sandbox, ".coding/plans/p1.md", "# P").unwrap_err();
        assert!(err.contains("protected"), "unexpected error: {err}");
        assert!(!dir
            .path()
            .join(".coding")
            .join("plans")
            .join("p1.md")
            .exists());
    }

    /// The .git control plane is refused - parity with the agent file tools
    /// (2027-01-09 security review HIGH-1: planted hooks / core.fsmonitor
    /// execute unsandboxed on the next commit or git status).
    #[test]
    fn write_sandboxed_refuses_git_control_plane() {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        let err = write_sandboxed(&sandbox, ".git/config", "[core]").unwrap_err();
        assert!(err.contains("protected"), "unexpected error: {err}");
        assert!(!dir.path().join(".git").join("config").exists());
        // The mirror must not over-block: .gitignore stays writable.
        let written = write_sandboxed(&sandbox, ".gitignore", "target/\n").unwrap();
        assert!(written.ends_with(".gitignore"));
    }
}

/// List files in a directory (for the file browser).
/// Routes the directory through the sandbox.
#[tauri::command]
pub async fn list_files(
    state: State<'_, IpcState>,
    dir: Option<String>,
) -> Result<Vec<FileEntry>, IpcError> {
    let sandbox = state.project.sandbox.clone();
    let root = sandbox.root().to_path_buf();
    let base = match dir {
        Some(d) => sandbox
            .validate(std::path::Path::new(&d))
            .map_err(|e| format!("path rejected: {e}"))?,
        None => root.clone(),
    };
    let mut entries = Vec::new();
    let read_dir = std::fs::read_dir(&base).map_err(|e| format!("failed to list dir: {e}"))?;
    for entry in read_dir.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        // Skip .git but show .coding/ (the project's config dir).
        if name == ".git" {
            continue;
        }
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let relative = entry
            .path()
            .strip_prefix(&root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or(name.clone());
        entries.push(FileEntry {
            name,
            path: relative,
            is_dir,
        });
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name)));
    Ok(entries)
}

/// A file entry for the file browser.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

/// A markdown file picked from the native file dialog (the "Browse" button).
///
/// `path` is the absolute path with backslashes normalized to forward slashes
/// (so it displays consistently on Windows); `content` is the file's text.
#[derive(Debug, Clone, Serialize)]
pub struct BrowsedFile {
    pub path: String,
    pub content: String,
}

/// Open a native file picker (initialized at the project root) filtered to
/// `.md` files, read the chosen file, and return its path + content.
///
/// Unlike `read_file`, this is **not** sandboxed — the picker can reach any
/// file on the computer, which is the point (load an `.md` from outside the
/// project). Returns `Ok(None)` when the user cancels the dialog.
#[tauri::command]
pub async fn browse_markdown_file(
    app: AppHandle,
    state: State<'_, IpcState>,
) -> Result<Option<BrowsedFile>, IpcError> {
    let root = state.project.sandbox.root().to_path_buf();
    // The dialog is blocking (it pumps the OS message loop), so run it on a
    // blocking thread to avoid stalling the async runtime.
    let picked = tokio::task::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter("Markdown", &["md"])
            .set_directory(&root)
            .blocking_pick_file()
    })
    .await
    .map_err(|e| format!("dialog failed: {e}"))?;

    let Some(file_path) = picked else {
        return Ok(None);
    };

    let path_buf = file_path
        .into_path()
        .map_err(|e| format!("invalid picked path: {e}"))?;
    // The read is BLOCKING (and can be a large file from anywhere on disk),
    // so it runs on `spawn_blocking` — same convention as `get_git_branch`
    // (review N2, 2026-06-14).
    let path_str = path_buf.display().to_string();
    let content = tokio::task::spawn_blocking(move || std::fs::read_to_string(&path_buf))
        .await
        .map_err(|e| format!("read task failed: {e}"))?
        .map_err(|e| format!("failed to read {path_str}: {e}"))?;
    Ok(Some(BrowsedFile {
        path: path_str.replace('\\', "/"),
        content,
    }))
}

/// Read the current git branch of the repo at `root` (sync — run via
/// `spawn_blocking` only, never on the async runtime).
///
/// Returns the trimmed branch name, or `"no-branch"` when `root` is not a
/// repo, `git` is missing, or the command fails for any other reason.
///
/// `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` are stripped from the
/// child's environment (review L5): an inherited env override would silently
/// point the status-bar branch read at a *different* repo than the open
/// project — and made the regression test environment-sensitive.
fn read_git_branch(root: &std::path::Path) -> String {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("rev-parse")
        .arg("--abbrev-ref")
        .arg("HEAD")
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");

    // On Windows, suppress the console window that would otherwise pop up
    // when spawning a child process from a GUI app (CREATE_NO_WINDOW).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    match cmd.output() {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => "no-branch".to_string(),
    }
}

/// Get the git branch (for the status bar).
///
/// The git subprocess is BLOCKING, so it runs on `spawn_blocking` — and the
/// project root is cloned (lock dropped) BEFORE spawning, so a stalled git
/// call can neither park a tokio worker nor hold the project lock hostage.
/// Regression: this was the freeze vector behind the 2026-04-19 diagnosis.
#[tauri::command]
pub async fn get_git_branch(state: State<'_, IpcState>) -> Result<String, IpcError> {
    let root = state.project.root.lock().await.root.clone();
    tokio::task::spawn_blocking(move || read_git_branch(&root))
        .await
        .map_err(|e| format!("git branch task failed: {e}"))
        .map_err(IpcError::from)
}

/// Char budget for the diff payload (mirrors the approval-preview's
/// `PREVIEW_DIFF_CHAR_BUDGET`): an uncapped `git diff HEAD` on a large
/// changeset would ship tens of MB over IPC and paint 100k+ DOM nodes —
/// the unbounded-frontend freeze class from the 2026-04-19 F3 diagnosis.
const GIT_DIFF_CHAR_BUDGET: usize = 120_000;

/// Resolve a UI-supplied path to a REPO-RELATIVE, forward-slash pathspec
/// for `git diff HEAD -- <pathspec>` (extracted for testing).
///
/// `Sandbox::validate` canonicalizes — on Windows that means a verbatim
/// `\\?\`-prefixed absolute path, which git-for-Windows can reject as
/// "outside repository". Handing git the path RELATIVE to the project root
/// (with `current_dir(root)`) is immune to verbatim prefixes, drive-letter
/// case, and canonicalization drift. Both sides are canonicalized by the
/// same `std::fs::canonicalize` (root at `Sandbox::new`, candidate at
/// `validate`), so `strip_prefix` always applies. Errors when the path
/// escapes the project root — before git ever runs.
fn repo_relative_pathspec(sandbox: &Sandbox, path: &str) -> Result<String, String> {
    let validated = sandbox
        .validate(std::path::Path::new(path))
        .map_err(|e| format!("path rejected: {e}"))?;
    let rel = validated
        .strip_prefix(sandbox.root())
        .map_err(|_| format!("path rejected: {path} is outside the project root"))?;
    Ok(rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/"))
}

/// Truncate a diff payload to [`GIT_DIFF_CHAR_BUDGET`] chars (char-boundary
/// safe), appending a visible marker when cut (extracted for testing).
fn cap_diff(text: &str) -> String {
    let trimmed = text.trim_end();
    if trimmed.chars().count() <= GIT_DIFF_CHAR_BUDGET {
        return trimmed.to_string();
    }
    let capped: String = trimmed.chars().take(GIT_DIFF_CHAR_BUDGET).collect();
    format!(
        "{capped}\n… [diff truncated — {GIT_DIFF_CHAR_BUDGET}-char budget; narrow the scope to one file] …"
    )
}

/// Compute the diff of the working tree (staged + unstaged) against `HEAD`
/// (sync — run via `spawn_blocking` only, never on the async runtime).
///
/// `path` scopes the diff to one file (an ABSOLUTE, sandbox-validated path —
/// the `--` separator means it can never be read as a flag); `None` diffs
/// the whole tree. `--no-color` keeps the output plain unified diff text.
/// Returns `Ok("")` when there are no changes. Untracked files are NOT
/// included (`git diff HEAD` only sees tracked changes) — a limitation of
/// the comprehensive view, documented for the UI.
///
/// The two common failure modes are canonicalized into short, stable error
/// strings so the UI can render a friendly panel instead of git's raw
/// multi-line output: a directory that is not a repository yields
/// `"not a git repository"` (git otherwise dumps its full `--no-index`
/// usage text), and a repository with no commits yet (an unborn HEAD) yields
/// `"no commits yet"`. All other failures carry git's stderr verbatim.
///
/// `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` are stripped from the
/// child's environment (same review-L5 hardening as [`read_git_branch`]):
/// an inherited override would silently diff a *different* repo than the
/// open project.
fn git_diff_head_at(root: &std::path::Path, path: Option<&str>) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("diff")
        .arg("HEAD")
        .arg("--no-color")
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    if let Some(p) = path {
        cmd.arg("--").arg(p);
    }

    // On Windows, suppress the console window that would otherwise pop up
    // when spawning a child process from a GUI app (CREATE_NO_WINDOW).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    match cmd.output() {
        Ok(o) if o.status.success() => Ok(cap_diff(&String::from_utf8_lossy(&o.stdout))),
        Ok(o) => {
            let stderr = String::from_utf8_lossy(&o.stderr);
            let stdout = String::from_utf8_lossy(&o.stdout);
            let lower = format!("{stderr}{stdout}").to_ascii_lowercase();
            if lower.contains("not a git repository") {
                return Err("not a git repository".to_string());
            }
            if lower.contains("bad revision") || lower.contains("unknown revision") {
                return Err("no commits yet".to_string());
            }
            let hint = if o.stderr.is_empty() {
                stdout.trim().to_string()
            } else {
                stderr.trim().to_string()
            };
            Err(if hint.is_empty() {
                "git diff failed".to_string()
            } else {
                hint
            })
        }
        Err(e) => Err(format!("failed to run git: {e}")),
    }
}

/// The comprehensive working-tree diff against `HEAD` for the Diff tab's
/// "vs Git" mode: every staged + unstaged tracked change, scoped to `path`
/// when given (else the whole tree). A given `path` is validated through
/// the project sandbox FIRST so the UI cannot diff files outside the open
/// project.
///
/// The git subprocess is BLOCKING, so it runs on `spawn_blocking` — and the
/// project root is cloned (lock dropped) BEFORE spawning, so a stalled git
/// call can neither park a tokio worker nor hold the project lock hostage
/// (same F1 freeze-lesson shape as [`get_git_branch`]).
#[tauri::command]
pub async fn git_diff_head(
    state: State<'_, IpcState>,
    path: Option<String>,
) -> Result<String, IpcError> {
    let root = state.project.root.lock().await.root.clone();
    // Repo-relative pathspec (review H1): the sandbox-validated path is
    // canonicalized — verbatim `\\?\`-prefixed on Windows — which
    // git-for-Windows can reject as outside the repository; git gets the
    // path relative to the project root instead.
    let validated = match path.as_deref().map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => Some(repo_relative_pathspec(&state.project.sandbox, p)?),
        None => None,
    };
    tokio::task::spawn_blocking(move || git_diff_head_at(&root, validated.as_deref()))
        .await
        .map_err(|e| format!("git diff task failed: {e}"))
        .map_err(IpcError::from)?
        .map_err(IpcError::from)
}

/// Initialize a git repository at `root` (`git init`). Used by the Diff tab's
/// "vs Git" mode when the open directory is not yet a repository — the user
/// can create one in place instead of seeing a dead-end error.
///
/// Idempotent: `git init` on an existing repository is a no-op (it re-runs
/// the init templates without harming history). The same env + console-window
/// hardening as [`git_diff_head_at`] applies. Returns `Ok(())` on success;
/// `Err` carries git's stderr.
fn git_init_at(root: &std::path::Path) -> Result<(), String> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("init")
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");

    // On Windows, suppress the console window that would otherwise pop up
    // when spawning a child process from a GUI app (CREATE_NO_WINDOW).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    match cmd.output() {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => {
            let hint = if o.stderr.is_empty() {
                String::from_utf8_lossy(&o.stdout).trim().to_string()
            } else {
                String::from_utf8_lossy(&o.stderr).trim().to_string()
            };
            Err(if hint.is_empty() {
                "git init failed".to_string()
            } else {
                hint
            })
        }
        Err(e) => Err(format!("failed to run git: {e}")),
    }
}

/// Initialize a git repository at the project root. Backs the Diff tab's
/// "Initialize Git Repository" button (shown when the open directory is not a
/// repository). Idempotent — `git init` on an existing repo is a no-op.
///
/// The git subprocess is BLOCKING, so it runs on `spawn_blocking` — and the
/// project root is cloned (lock dropped) BEFORE spawning, so a stalled git
/// call can neither park a tokio worker nor hold the project lock hostage
/// (same F1 freeze-lesson shape as [`git_diff_head`]).
#[tauri::command]
pub async fn git_init(state: State<'_, IpcState>) -> Result<(), IpcError> {
    let root = state.project.root.lock().await.root.clone();
    tokio::task::spawn_blocking(move || git_init_at(&root))
        .await
        .map_err(|e| format!("git init task failed: {e}"))
        .map_err(IpcError::from)?
        .map_err(IpcError::from)
}

/// One local branch (`refs/heads`) — the Git tab's branch list.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GitBranchInfo {
    /// The short branch name (e.g. `main`, `feat/x`).
    pub name: String,
    /// True when this is the currently checked-out branch.
    pub is_current: bool,
    /// True when the branch is named `main`.
    pub is_main: bool,
    /// The full commit sha this branch points at.
    pub tip_sha: String,
    /// Raw upstream-tracking text from git (e.g. `[ahead 3, behind 1]`),
    /// or `None` when the branch has no upstream / is in sync.
    pub upstream_track: Option<String>,
    /// True when git reports the branch fully merged into `main`.
    pub merged_into_main: bool,
}

/// One commit in the history DAG — the Git tab's graph.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GitCommitInfo {
    /// The full commit sha.
    pub sha: String,
    /// The abbreviated sha (git's `%h`).
    pub short_sha: String,
    /// Parent shas in git's order (first parent first); empty for roots.
    pub parents: Vec<String>,
    /// The commit subject (first line of the message).
    pub subject: String,
    /// The author name.
    pub author: String,
    /// The author date as unix seconds.
    pub timestamp: i64,
    /// Ref decorations from `%D` (`HEAD -> main`, `main`, `tag: v1`, …).
    pub refs: Vec<String>,
}

/// The Git tab's full dataset: local branches + the commit DAG + main's
/// first-parent spine.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct GitHistory {
    /// The currently checked-out branch.
    pub current_branch: String,
    /// Local branches (`refs/heads`), `main` first (then current, then name).
    pub branches: Vec<GitBranchInfo>,
    /// Up to [`GIT_HISTORY_COMMIT_CAP`] commits across all local branches,
    /// in `--topo-order` (newest first, descendants before ancestors).
    pub commits: Vec<GitCommitInfo>,
    /// `main`'s first-parent chain (the graph's predominant spine), newest
    /// first; empty when the repo has no `main` branch.
    pub main_spine: Vec<String>,
}

/// How many commits the Git tab loads per refresh (bounded so a huge repo
/// cannot flood the IPC bridge or the SVG).
const GIT_HISTORY_COMMIT_CAP: usize = 400;

/// Run git in `root`, returning its stdout — shared hardening for the
/// history reads (sync — run via `spawn_blocking` only).
///
/// `GIT_DIR` / `GIT_WORK_TREE` / `GIT_INDEX_FILE` are stripped from the
/// child's environment and console windows are suppressed on Windows, same
/// as [`read_git_branch`] / [`git_diff_head_at`]. Errors carry git's stderr.
fn git_stdout(root: &std::path::Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(args)
        .current_dir(root)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    match cmd.output() {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).trim().to_string()),
        Ok(o) => {
            let hint = if o.stderr.is_empty() {
                String::from_utf8_lossy(&o.stdout).trim().to_string()
            } else {
                String::from_utf8_lossy(&o.stderr).trim().to_string()
            };
            Err(if hint.is_empty() {
                format!("git {args:?} failed")
            } else {
                hint
            })
        }
        Err(e) => Err(format!("failed to run git: {e}")),
    }
}

/// Collect the Git tab's dataset from the repo at `root` (sync — run via
/// `spawn_blocking` only, never on the async runtime).
///
/// Four git reads: (a) `for-each-ref refs/heads` for the branch list (with
/// current/upstream-tracking markers), (b) `log --branches --topo-order` for
/// the commit DAG (shas + parents + decorations, unit-separator delimited so
/// subjects containing `|` or `, ` cannot corrupt the parse), (c) `rev-list
/// --first-parent main` for the predominant spine, and (d) `branch --merged
/// main` for the merged set. (c)/(d) are skipped when `main` doesn't exist.
///
/// Errors outside a repository (or on any git failure) so the UI can surface
/// the message instead of rendering an empty-but-successful graph.
fn git_history_at(root: &std::path::Path) -> Result<GitHistory, String> {
    // (a) Branches. %(HEAD) is "*" for the current branch, "" otherwise;
    // %(upstream:track) is e.g. "[ahead 3]" or "" when in sync/absent.
    let refs_out = git_stdout(
        root,
        &[
            "for-each-ref",
            "refs/heads",
            "--format=%(refname:short)\u{1f}%(HEAD)\u{1f}%(objectname)\u{1f}%(upstream:track)",
        ],
    )?;
    let mut branches: Vec<GitBranchInfo> = refs_out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let mut parts = line.split('\u{1f}');
            let name = parts.next().unwrap_or_default().trim().to_string();
            let head = parts.next().unwrap_or_default().trim();
            let sha = parts.next().unwrap_or_default().trim().to_string();
            let track = parts.next().unwrap_or_default().trim().to_string();
            GitBranchInfo {
                is_main: name == "main",
                is_current: head == "*",
                merged_into_main: false, // filled by (d) below
                tip_sha: sha,
                upstream_track: if track.is_empty() { None } else { Some(track) },
                name,
            }
        })
        .collect();

    let has_main = branches.iter().any(|b| b.is_main);

    // (b) Commit DAG. %P (parents) is space-separated inside one field; %D
    // decorations are ", "-separated (or empty on undecorated commits).
    let log_out = git_stdout(
        root,
        &[
            "log",
            "--branches",
            "--topo-order",
            &format!("--pretty=format:%H\u{1f}%h\u{1f}%P\u{1f}%s\u{1f}%an\u{1f}%at\u{1f}%D"),
            "-n",
            &GIT_HISTORY_COMMIT_CAP.to_string(),
        ],
    )?;
    let commits: Vec<GitCommitInfo> = log_out
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|line| {
            let mut parts = line.split('\u{1f}');
            let sha = parts.next().unwrap_or_default().trim().to_string();
            let short_sha = parts.next().unwrap_or_default().trim().to_string();
            let parents = parts
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .map(str::to_string)
                .collect();
            let subject = parts.next().unwrap_or_default().trim().to_string();
            let author = parts.next().unwrap_or_default().trim().to_string();
            let timestamp = parts
                .next()
                .unwrap_or_default()
                .trim()
                .parse::<i64>()
                .unwrap_or(0);
            let refs: Vec<String> = parts
                .next()
                .unwrap_or_default()
                .split(", ")
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(str::to_string)
                .collect();
            GitCommitInfo {
                sha,
                short_sha,
                parents,
                subject,
                author,
                timestamp,
                refs,
            }
        })
        .collect();

    // (c) main's first-parent chain — the spine lane 0 renders prominently.
    // Same cap as the log read so the spine never truncates before the DAG.
    let main_spine = if has_main {
        git_stdout(
            root,
            &[
                "rev-list",
                "--first-parent",
                "main",
                "-n",
                &GIT_HISTORY_COMMIT_CAP.to_string(),
            ],
        )?
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
    } else {
        Vec::new()
    };

    // (d) Branches fully merged into main (their merge offer is a no-op).
    if has_main {
        let merged = git_stdout(
            root,
            &["branch", "--merged", "main", "--format=%(refname:short)"],
        )?;
        let merged: std::collections::HashSet<&str> = merged
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect();
        for b in &mut branches {
            b.merged_into_main = merged.contains(b.name.as_str());
        }
    }

    // Stable, useful order: main first, then the current branch, then names.
    branches.sort_by(|a, b| {
        b.is_main
            .cmp(&a.is_main)
            .then(b.is_current.cmp(&a.is_current))
            .then(a.name.cmp(&b.name))
    });

    let current_branch = branches
        .iter()
        .find(|b| b.is_current)
        .map(|b| b.name.clone())
        .unwrap_or_else(|| "no-branch".to_string());

    Ok(GitHistory {
        current_branch,
        branches,
        commits,
        main_spine,
    })
}

/// The Git tab's dataset: local branches, the commit DAG (with parents, so
/// the frontend lays out lanes), and main's first-parent spine.
///
/// The git subprocesses are BLOCKING, so they run on `spawn_blocking` — and
/// the project root is cloned (lock dropped) BEFORE spawning, so a stalled
/// git call can neither park a tokio worker nor hold the project lock
/// hostage (same F1 freeze-lesson shape as [`get_git_branch`]).
#[tauri::command]
pub async fn git_history(state: State<'_, IpcState>) -> Result<GitHistory, IpcError> {
    let root = state.project.root.lock().await.root.clone();
    tokio::task::spawn_blocking(move || git_history_at(&root))
        .await
        .map_err(|e| format!("git history task failed: {e}"))
        .map_err(IpcError::from)?
        .map_err(IpcError::from)
}

/// Save an agent's conversation transcript to disk as JSON.
///
/// The transcript is serialized by the frontend (it owns the display state)
/// and passed here as `content`. The `path` is validated through the sandbox
/// so it can't escape the project root. If `path` is empty, a default of
/// `.coding/conversations/<timestamp>.json` is used. Returns the path written.
#[tauri::command]
pub async fn save_conversation(
    state: State<'_, IpcState>,
    _agent_id: AgentId,
    path: String,
    content: String,
) -> Result<String, IpcError> {
    let project = state.project.root.lock().await;
    let sandbox = state.project.sandbox.clone();

    let resolved = if path.trim().is_empty() {
        // Default: .coding/conversations/<timestamp>.json under the project root.
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        project
            .root
            .join(Project::CODING_DIR_NAME)
            .join("conversations")
            .join(format!("{ts}.json"))
    } else {
        sandbox
            .validate(std::path::Path::new(&path))
            .map_err(|e| format!("path rejected: {e}"))?
    };
    drop(project);

    // Ensure the parent directory exists + write the file. Both are BLOCKING
    // (the transcript JSON can be multiple MB), so they run on
    // `spawn_blocking` with owned data (review N2, 2026-06-14).
    let resolved_display = resolved.display().to_string();
    let written = tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        if let Some(parent) = resolved.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&resolved, content)?;
        Ok(resolved.to_string_lossy().replace('\\', "/"))
    })
    .await
    .map_err(|e| format!("save task failed: {e}"))
    .map_err(IpcError::from)?
    .map_err(|e| format!("failed to write {resolved_display}: {e}"))?;
    Ok(written)
}

/// Load a conversation transcript from disk.
///
/// Reads the JSON file at `path` (validated through the sandbox) and returns
/// its contents so the frontend can restore the transcript into its store.
/// The read is BLOCKING (the transcript JSON can be multiple MB), so it runs
/// on `spawn_blocking` (review N2, 2026-06-14).
#[tauri::command]
pub async fn load_conversation(
    state: State<'_, IpcState>,
    _agent_id: AgentId,
    path: String,
) -> Result<String, IpcError> {
    let sandbox = state.project.sandbox.clone();
    let validated = sandbox
        .validate(std::path::Path::new(&path))
        .map_err(|e| format!("path rejected: {e}"))?;
    tokio::task::spawn_blocking(move || std::fs::read_to_string(&validated))
        .await
        .map_err(|e| format!("read task failed: {e}"))
        .and_then(|r| r.map_err(|e| format!("failed to read {path}: {e}")))
        .map_err(IpcError::from)
}
