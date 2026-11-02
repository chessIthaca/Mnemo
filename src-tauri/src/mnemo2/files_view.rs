// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The Files tab of the lead's Workbench: a lazy, Git-aware project tree and a
//! read-only file viewer (plan b5dc77ab, step 1).
//!
//! Every path the tab names crosses [`resolve`] first, so absolute paths, `..`
//! segments and symlinks whose target escapes the project root are refused —
//! the viewer reads the project tree and nothing else. The tree is listed one
//! directory per call ([`list_dir`]) because this repository is far too large
//! to walk eagerly, and Git answers what exists, so ignored build output never
//! reaches the UI.

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::mnemo2::ipc::Mnemo2State;
use crate::mnemo2::phase3::{git, git_error};

/// The most entries one directory listing returns.
const MAX_DIR_ENTRIES: usize = 2_000;

/// The most paths the filter box shows for one query.
const MAX_FIND_RESULTS: usize = 200;

/// The most bytes the viewer shows; longer files are cut and flagged.
const MAX_FILE_BYTES: u64 = 256 * 1024;

/// How much of a file is inspected before it counts as binary.
const BINARY_PROBE_BYTES: usize = 8 * 1024;

/// What a tree row is: a directory (expanded on demand) or a file (openable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Dir,
    File,
}

/// One row of the Files tab's tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileEntry {
    /// Project-relative, `/`-separated path.
    pub path: String,
    /// The last path segment, which is what the row shows.
    pub name: String,
    pub kind: EntryKind,
}

impl FileEntry {
    /// Build a row from a project-relative path and its kind.
    fn new(path: String, kind: EntryKind) -> Self {
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        Self { path, name, kind }
    }
}

/// One directory's immediate children — the tree's lazy unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DirListing {
    /// The listed directory, project-relative (an empty string is the root).
    pub path: String,
    pub entries: Vec<FileEntry>,
    /// True when [`MAX_DIR_ENTRIES`] cut the listing short.
    pub truncated: bool,
}

/// What the read-only viewer showed for one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FileContent {
    /// The project-relative path that was read.
    pub path: String,
    /// The file's text; empty when `binary` is set.
    pub text: String,
    /// The file's size on disk, which may exceed `text`.
    pub bytes: u64,
    /// True when [`MAX_FILE_BYTES`] cut the text short.
    pub truncated: bool,
    /// True when the file is not text and is deliberately not shown.
    pub binary: bool,
}

/// Resolve a project-relative path to an absolute one inside the project.
///
/// Refuses absolute paths (`/etc`, `C:\Windows`, `\\server\share`), any `..`
/// segment, and any symlink or junction whose target lies outside the root:
/// canonicalisation happens after the lexical check, so `starts_with` compares
/// real locations rather than spelled ones.
fn resolve(root: &Path, relative: &str) -> Result<PathBuf, String> {
    if relative.contains('\0') {
        return Err("the path contains a NUL byte".to_string());
    }
    for component in Path::new(relative).components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir => {
                return Err(format!(
                    "'{relative}' is outside the project: '..' is not allowed"
                ));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "'{relative}' is outside the project: only project-relative paths are allowed"
                ));
            }
        }
    }
    let root = dunce::canonicalize(root)
        .map_err(|err| format!("could not resolve the project root: {err}"))?;
    let canonical = dunce::canonicalize(root.join(relative)).map_err(|err| {
        if relative.is_empty() {
            format!("could not resolve the project root: {err}")
        } else {
            format!("'{relative}' is not a readable project path: {err}")
        }
    })?;
    if !canonical.starts_with(&root) {
        return Err(format!("'{relative}' is outside the project"));
    }
    Ok(canonical)
}

/// Trim a caller-supplied directory path: `None`, `""` and `"."` all mean the
/// project root, and a trailing slash is not part of the name.
fn normalize_dir(relative: Option<&str>) -> &str {
    let relative = relative.unwrap_or("").trim();
    if relative == "." {
        return "";
    }
    relative.trim_end_matches('/')
}

/// Order rows the way a file tree reads: directories first, then by name.
fn sort_key(entry: &FileEntry) -> (bool, String) {
    (entry.kind != EntryKind::Dir, entry.name.to_lowercase())
}

/// Parse `git ls-files ... -z` output into the immediate children of
/// `relative`: an entry that still holds a `/` after the listed directory is a
/// directory, everything else is a file.
fn parse_ls_files(raw: &str, relative: &str) -> Vec<(String, EntryKind)> {
    let prefix = if relative.is_empty() {
        String::new()
    } else {
        format!("{relative}/")
    };
    raw.split('\0')
        .filter(|path| !path.is_empty())
        .filter_map(|path| {
            let rest = path.strip_prefix(prefix.as_str())?;
            let rest = rest.trim_end_matches('/');
            let name = rest.split('/').next()?.to_string();
            if name.is_empty() {
                return None;
            }
            let kind = if rest.contains('/') {
                EntryKind::Dir
            } else {
                EntryKind::File
            };
            Some((name, kind))
        })
        .collect()
}

/// Ask Git for one directory's entries: everything the index knows — tracked
/// files and staged-but-uncommitted ones alike — plus untracked files that
/// `.gitignore` does not exclude.
///
/// Reading the index rather than `HEAD` is what makes a just-added file appear
/// before it is committed; a staged deletion drops out with it. `None` when the
/// project is not a Git work tree (or `git` is missing), which sends the caller
/// to a plain disk listing.
async fn git_entries(root: &Path, relative: &str) -> Option<BTreeMap<String, EntryKind>> {
    let spec = if relative.is_empty() {
        ".".to_string()
    } else {
        format!("{relative}/")
    };
    let output = git(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "--directory",
            "-z",
            "--",
            &spec,
        ],
    )
    .await
    .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        parse_ls_files(&String::from_utf8_lossy(&output.stdout), relative)
            .into_iter()
            .collect(),
    )
}

/// List a directory from disk when the project is not a Git work tree.
async fn disk_entries(directory: &Path) -> Result<BTreeMap<String, EntryKind>, String> {
    let directory = directory.to_path_buf();
    let shown = directory.display().to_string();
    tokio::task::spawn_blocking(move || -> Result<BTreeMap<String, EntryKind>, String> {
        let read = std::fs::read_dir(&directory)
            .map_err(|err| format!("could not list {shown}: {err}"))?;
        let mut entries = BTreeMap::new();
        for entry in read {
            let entry = entry.map_err(|err| format!("could not list {shown}: {err}"))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".git" {
                continue;
            }
            let kind = match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => EntryKind::Dir,
                Ok(_) => EntryKind::File,
                Err(_) => continue,
            };
            entries.insert(name, kind);
        }
        Ok(entries)
    })
    .await
    .map_err(|err| format!("the listing task failed: {err}"))?
}

/// List one project directory for the tree — one level, on demand.
pub(crate) async fn list_dir(root: &Path, relative: Option<&str>) -> Result<DirListing, String> {
    let relative = normalize_dir(relative);
    let directory = resolve(root, relative)?;
    if !directory.is_dir() {
        return Err(format!("'{relative}' is not a project directory"));
    }
    let listing = match git_entries(root, relative).await {
        Some(entries) => entries,
        None => disk_entries(&directory).await?,
    };
    let mut entries: Vec<FileEntry> = listing
        .into_iter()
        .map(|(name, kind)| {
            let path = if relative.is_empty() {
                name
            } else {
                format!("{relative}/{name}")
            };
            FileEntry::new(path, kind)
        })
        .collect();
    entries.sort_by_key(sort_key);
    let truncated = entries.len() > MAX_DIR_ENTRIES;
    entries.truncate(MAX_DIR_ENTRIES);
    Ok(DirListing {
        path: relative.to_string(),
        entries,
        truncated,
    })
}

/// Read one project file: text for the viewer, or a note when it is binary or
/// larger than [`MAX_FILE_BYTES`].
fn read_file_blocking(path: &Path, shown: &str) -> Result<FileContent, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|err| format!("could not inspect '{shown}': {err}"))?;
    if !metadata.is_file() {
        return Err(format!("'{shown}' is not a file"));
    }
    let bytes = metadata.len();
    let file = std::fs::File::open(path)
        .map_err(|err| format!("could not open '{shown}': {err}"))?;
    let mut buffer = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut buffer)
        .map_err(|err| format!("could not read '{shown}': {err}"))?;
    let truncated = buffer.len() as u64 > MAX_FILE_BYTES;
    buffer.truncate(MAX_FILE_BYTES as usize);
    if buffer.iter().take(BINARY_PROBE_BYTES).any(|byte| *byte == 0) {
        return Ok(FileContent {
            path: shown.to_string(),
            text: String::new(),
            bytes,
            truncated: false,
            binary: true,
        });
    }
    if truncated {
        // Never cut a multi-byte character in half at the byte limit.
        if let Err(utf8) = std::str::from_utf8(&buffer) {
            buffer.truncate(utf8.valid_up_to());
        }
    }
    Ok(FileContent {
        path: shown.to_string(),
        text: String::from_utf8_lossy(&buffer).into_owned(),
        bytes,
        truncated,
        binary: false,
    })
}

/// Read one project file for the viewer.
pub(crate) async fn read_file(root: &Path, relative: &str) -> Result<FileContent, String> {
    let relative = relative.trim();
    if relative.is_empty() {
        return Err("no file was selected".to_string());
    }
    let path = resolve(root, relative)?;
    let shown = relative.to_string();
    tokio::task::spawn_blocking(move || read_file_blocking(&path, &shown))
        .await
        .map_err(|err| format!("the file-read task failed: {err}"))?
}

/// Find project files whose path contains `query` — the tree's filter box.
pub(crate) async fn find_paths(root: &Path, query: &str) -> Result<Vec<FileEntry>, String> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let args = ["ls-files", "--cached", "--others", "--exclude-standard", "-z"];
    let output = git(root, &args).await?;
    if !output.status.success() {
        return Err(git_error(&args, &output));
    }
    let raw = String::from_utf8_lossy(&output.stdout);
    let mut matches = Vec::new();
    for path in raw.split('\0').filter(|path| !path.is_empty()) {
        if !path.to_lowercase().contains(&query) {
            continue;
        }
        matches.push(FileEntry::new(path.to_string(), EntryKind::File));
        if matches.len() == MAX_FIND_RESULTS {
            break;
        }
    }
    Ok(matches)
}

/// The open project's root, which every Files command is confined to.
async fn project_root(state: &Mnemo2State) -> Result<PathBuf, String> {
    state
        .open_lead()
        .await
        .map(|lead| lead.workspace_root().to_path_buf())
        .ok_or_else(|| "Mnemo2 has no open project; open one first".to_string())
}

/// List one project directory for the Files tab's lazy tree.
#[tauri::command]
pub async fn files_list(
    state: State<'_, Mnemo2State>,
    path: Option<String>,
) -> Result<DirListing, String> {
    let root = project_root(&state).await?;
    list_dir(&root, path.as_deref()).await
}

/// Read one project file for the Files tab's read-only viewer.
#[tauri::command]
pub async fn files_read(
    state: State<'_, Mnemo2State>,
    path: String,
) -> Result<FileContent, String> {
    let root = project_root(&state).await?;
    read_file(&root, &path).await
}

/// Find project paths for the Files tab's filter box.
#[tauri::command]
pub async fn files_find(
    state: State<'_, Mnemo2State>,
    query: String,
) -> Result<Vec<FileEntry>, String> {
    let root = project_root(&state).await?;
    find_paths(&root, &query).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// Run Git in a test repository and fail loudly when it refuses.
    fn git_in(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Write a fixture file, creating its directory.
    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("fixture directory");
        }
        std::fs::write(path, contents).expect("fixture file");
    }

    /// A committed Git project: a tracked file, a tracked directory, an
    /// untracked file and an ignored build directory.
    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        git_in(root, &["init", "-q", "-b", "main"]);
        git_in(root, &["config", "user.name", "Test"]);
        git_in(root, &["config", "user.email", "test@example.invalid"]);
        write(root, ".gitignore", "dist/\n");
        write(root, "README.md", "# project\n");
        write(root, "src/main.rs", "fn main() {}\n");
        git_in(root, &["add", "-A"]);
        git_in(root, &["commit", "-qm", "base"]);
        write(root, "src/notes.txt", "scratch\n");
        write(root, "dist/bundle.js", "console.log(1);\n");
        dir
    }

    #[test]
    fn refuses_paths_outside_the_project() {
        let project = project();
        let root = project.path();
        for refused in [
            "..",
            "../outside.txt",
            "src/../../outside.txt",
            "/etc/passwd",
            "",
        ] {
            let outcome = resolve(root, refused);
            if refused.is_empty() {
                assert!(outcome.is_ok(), "the empty path is the project root");
                continue;
            }
            let error = outcome.expect_err(refused);
            assert!(error.contains("outside the project"), "{refused}: {error}");
        }
        let inside = resolve(root, "src/main.rs").expect("a project file resolves");
        let canonical_root = dunce::canonicalize(root).expect("canonical root");
        assert!(inside.starts_with(&canonical_root));
    }

    #[cfg(windows)]
    #[test]
    fn refuses_windows_absolute_paths() {
        let project = project();
        for refused in [
            r"C:\Windows\win.ini",
            r"C:Windows\win.ini",
            r"\\server\share\secret.txt",
            r"src\..\..\outside.txt",
        ] {
            let error = resolve(project.path(), refused).expect_err(refused);
            assert!(error.contains("outside the project"), "{refused}: {error}");
        }
    }

    #[tokio::test]
    async fn lists_staged_but_uncommitted_files() {
        let project = project();
        let root = project.path();
        write(root, "src/staged.rs", "fn staged() {}\n");
        write(root, "added/new.txt", "fresh\n");
        git_in(root, &["add", "src/staged.rs", "added/new.txt"]);

        let nested = list_dir(root, Some("src")).await.expect("src listing");
        assert!(
            nested.entries.iter().any(|entry| entry.name == "staged.rs"),
            "a staged file belongs to the project before it is committed: {:?}",
            nested.entries
        );

        let listing = list_dir(root, None).await.expect("root listing");
        let added = listing
            .entries
            .iter()
            .find(|entry| entry.name == "added")
            .expect("a staged directory appears");
        assert_eq!(added.kind, EntryKind::Dir);

        // A staged deletion drops out even though HEAD still holds the file.
        git_in(root, &["rm", "-q", "README.md"]);
        let after = list_dir(root, None).await.expect("root listing after removal");
        assert!(after.entries.iter().all(|entry| entry.name != "README.md"));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_symlink_that_escapes_the_project() {
        let outside = tempfile::tempdir().expect("outside directory");
        std::fs::write(outside.path().join("secret.txt"), "secret\n").expect("secret");
        let project = project();
        std::os::unix::fs::symlink(outside.path(), project.path().join("escape"))
            .expect("escape symlink");
        let error = resolve(project.path(), "escape/secret.txt")
            .expect_err("a symlink leaving the project must be refused");
        assert!(error.contains("outside the project"), "{error}");
    }

    #[tokio::test]
    async fn lists_one_level_and_honours_gitignore() {
        let project = project();
        let root = project.path();
        let listing = list_dir(root, None).await.expect("root listing");
        assert_eq!(listing.path, "");
        assert!(!listing.truncated);
        let rows: Vec<(&str, EntryKind)> = listing
            .entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind))
            .collect();
        // Directories first, then names; `dist/` is ignored and never listed.
        assert_eq!(
            rows,
            vec![
                ("src", EntryKind::Dir),
                (".gitignore", EntryKind::File),
                ("README.md", EntryKind::File),
            ]
        );
        assert_eq!(listing.entries[0].path, "src");

        let nested = list_dir(root, Some("src")).await.expect("nested listing");
        let nested_rows: Vec<(&str, EntryKind)> = nested
            .entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.kind))
            .collect();
        assert_eq!(
            nested_rows,
            vec![("main.rs", EntryKind::File), ("notes.txt", EntryKind::File)]
        );
        assert_eq!(nested.entries[0].path, "src/main.rs");
        // A trailing slash names the same directory.
        let same = list_dir(root, Some("src/")).await.expect("slash listing");
        assert_eq!(same.entries, nested.entries);
    }

    #[tokio::test]
    async fn refuses_listing_outside_the_project() {
        let project = project();
        let error = list_dir(project.path(), Some(".."))
            .await
            .expect_err("'..' must be refused");
        assert!(error.contains("outside the project"), "{error}");
    }

    #[tokio::test]
    async fn reads_text_and_flags_binary_and_large_files() {
        let project = project();
        let root = project.path();
        let text = read_file(root, "src/main.rs").await.expect("text read");
        assert_eq!(text.path, "src/main.rs");
        assert_eq!(text.text, "fn main() {}\n");
        assert_eq!(text.bytes, "fn main() {}
".len() as u64);
        assert!(!text.binary && !text.truncated);

        write(root, "logo.bin", "\u{0}\u{1}\u{2}binary");
        let binary = read_file(root, "logo.bin").await.expect("binary read");
        assert!(binary.binary);
        assert!(binary.text.is_empty());

        let filler = "x".repeat(MAX_FILE_BYTES as usize + 64);
        write(root, "big.txt", &filler);
        let large = read_file(root, "big.txt").await.expect("large read");
        assert!(large.truncated);
        assert_eq!(large.text.len(), MAX_FILE_BYTES as usize);
        assert_eq!(large.bytes, filler.len() as u64);

        let error = read_file(root, "src").await.expect_err("a directory is not a file");
        assert!(error.contains("not a file"), "{error}");
    }

    #[tokio::test]
    async fn finds_paths_by_substring_and_caps_the_list() {
        let project = project();
        let root = project.path();
        let found = find_paths(root, "NOTES").await.expect("find notes");
        assert_eq!(
            found,
            vec![FileEntry::new("src/notes.txt".to_string(), EntryKind::File)]
        );
        assert!(find_paths(root, "   ").await.expect("blank query").is_empty());
        assert!(find_paths(root, "bundle").await.expect("ignored file").is_empty());
    }
}
