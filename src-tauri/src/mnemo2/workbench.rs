// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Read-only Git and screenshot data for Mnemo2's native workbench.

use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use mnemo::app::worktrees::{agent_branch, agent_short_id, current_branch, is_pool_checkout};
use serde::Serialize;

use crate::mnemo2::phase3::{git_value, tip};
use crate::mnemo2::team::WorkerRecord;

const MAX_FILES: usize = 300;
const MAX_FILE_LINES: usize = 1_000;
const MAX_TOTAL_LINES: usize = 12_000;
const MAX_LINE_BYTES: usize = 8 * 1024;
const MAX_UNTRACKED_BYTES: u64 = 1024 * 1024;
const MAX_SCREENSHOT_BYTES: u64 = 20 * 1024 * 1024;

/// The sub-directory of a checkout the offscreen browser writes its
/// screenshots into; the same path under the team's state dir is the archive.
const SCREENSHOT_DIR: &str = ".coding/browser/screenshots";

/// The sub-directory (beside `team.json`) holding one screenshot archive
/// folder per worker id.
const SCREENSHOT_ARCHIVE_DIR: &str = "screenshots";

/// The bytes every PNG file starts with.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// The answer for a screenshot neither a live checkout nor the archive holds:
/// the worker merged before Mnemo kept screenshots, so there is nothing left
/// to open.
const SCREENSHOT_NO_LONGER_AVAILABLE: &str = "This screenshot is no longer available: the worker's checkout was released before Mnemo kept screenshots.";

/// The image formats a tool card can show, by lower-cased extension.
const TOOL_IMAGE_TYPES: [(&str, &str); 6] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
    ("bmp", "image/bmp"),
];

/// One rendered unified-diff line with its original line numbers.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DiffLine {
    /// `add`, `remove`, `context`, or `hunk`.
    #[serde(rename = "type")]
    pub kind: String,
    /// The line content, without its leading diff marker.
    pub text: String,
    /// The old-file line number, when applicable.
    pub old_line: Option<u32>,
    /// The new-file line number, when applicable.
    pub new_line: Option<u32>,
}

/// A changed file and its bounded textual preview.
#[derive(Clone, Debug, Serialize)]
pub struct DiffFile {
    /// The post-image repository-relative path.
    pub path: String,
    /// The old path for deletions and renames.
    pub old_path: Option<String>,
    /// Number of added lines in the actual delta, including omitted lines.
    pub additions: u64,
    /// Number of removed lines in the actual delta, including omitted lines.
    pub deletions: u64,
    /// Textual hunks. Empty for binary or empty files.
    pub lines: Vec<DiffLine>,
    /// Whether the file is binary rather than textual.
    pub binary: bool,
    /// Whether this file's preview omitted content.
    pub truncated: bool,
    /// Whether the path is a credential/key file whose contents must be withheld.
    #[serde(default)]
    pub sensitive: bool,
}

impl DiffFile {
    fn new(path: String) -> Self {
        Self {
            path,
            old_path: None,
            additions: 0,
            deletions: 0,
            lines: Vec::new(),
            binary: false,
            truncated: false,
            sensitive: false,
        }
    }
}

/// A real worker delta. Merged workers retain their reviewed historical delta.
#[derive(Clone, Debug, Serialize)]
pub struct DiffSnapshot {
    /// Worker owning this delta.
    pub worker_id: String,
    /// Worker's branch.
    pub branch: String,
    /// Lead branch into which the worker integrates.
    pub target: String,
    /// Base commit used for the comparison.
    pub base: String,
    /// Exact worker tip used for the comparison.
    pub tip: String,
    /// Bounded file previews.
    pub files: Vec<DiffFile>,
    /// Totals across the whole delta, including omitted previews.
    pub total_files: u64,
    /// Total added lines, including omitted previews.
    pub total_additions: u64,
    /// Total removed lines, including omitted previews.
    pub total_deletions: u64,
    /// Whether any file or content was omitted.
    pub truncated: bool,
}

#[derive(Default)]
struct ParsedDiff {
    files: Vec<DiffFile>,
    total_files: u64,
    total_additions: u64,
    total_deletions: u64,
    truncated: bool,
}

fn git_path(raw: &str) -> String {
    if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
        let bytes = raw.as_bytes();
        let mut decoded = Vec::new();
        let mut index = 1;
        while index < bytes.len() - 1 {
            if bytes[index] != b'\\' {
                decoded.push(bytes[index]);
                index += 1;
                continue;
            }
            index += 1;
            if index >= bytes.len() - 1 {
                break;
            }
            match bytes[index] {
                b'0'..=b'7' => {
                    let mut value = 0u16;
                    let mut digits = 0;
                    while digits < 3 && index < bytes.len() - 1 {
                        let digit = bytes[index];
                        if !(b'0'..=b'7').contains(&digit) {
                            break;
                        }
                        value = value * 8 + u16::from(digit - b'0');
                        index += 1;
                        digits += 1;
                    }
                    decoded.push(value as u8);
                    continue;
                }
                b't' => decoded.push(b'\t'),
                b'n' => decoded.push(b'\n'),
                b'r' => decoded.push(b'\r'),
                other => decoded.push(other),
            }
            index += 1;
        }
        String::from_utf8_lossy(&decoded).into_owned()
    } else {
        raw.to_string()
    }
}

fn prefixed_path(raw: &str) -> Option<String> {
    let path = git_path(raw);
    (path != "/dev/null").then(|| {
        path.strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))
            .unwrap_or(&path)
            .to_string()
    })
}

fn diff_header_path(line: &str) -> String {
    let rest = line.strip_prefix("diff --git ").unwrap_or("");
    if rest.starts_with('"') {
        let mut escaped = false;
        let mut end = None;
        for (index, ch) in rest.char_indices().skip(1) {
            if ch == '"' && !escaped {
                end = Some(index);
                break;
            }
            escaped = ch == '\\' && !escaped;
            if ch != '\\' {
                escaped = false;
            }
        }
        if let Some(end) = end {
            return prefixed_path(&rest[..=end]).unwrap_or_default();
        }
    }
    rest.rsplit_once(" b/")
        .and_then(|(_, path)| prefixed_path(path))
        .unwrap_or_default()
}

fn hunk_start(text: &str) -> Option<(u32, u32)> {
    let mut fields = text.split_whitespace();
    let old = fields.next()?.strip_prefix('-')?;
    let new = fields.next()?.strip_prefix('+')?;
    let old = old.split(',').next()?.parse().ok()?;
    let new = new.split(',').next()?.parse().ok()?;
    Some((old, new))
}

fn short_text(text: &str) -> (String, bool) {
    if text.len() <= MAX_LINE_BYTES {
        return (text.to_string(), false);
    }
    let mut end = MAX_LINE_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}…", &text[..end]), true)
}

fn parse_unified_diff(patch: &str) -> ParsedDiff {
    let mut parsed = ParsedDiff::default();
    let mut current: Option<DiffFile> = None;
    let mut rendered_lines = 0usize;
    let mut old_line = 0u32;
    let mut new_line = 0u32;
    let finish = |current: &mut Option<DiffFile>, parsed: &mut ParsedDiff| {
        let Some(mut file) = current.take() else {
            return;
        };
        file.sensitive |= sensitive_path(Path::new(&file.path))
            || file
                .old_path
                .as_deref()
                .is_some_and(|path| sensitive_path(Path::new(path)));
        if file.sensitive {
            file.lines.clear();
            file.truncated = true;
            parsed.truncated = true;
        }
        parsed.total_files += 1;
        parsed.total_additions += file.additions;
        parsed.total_deletions += file.deletions;
        if parsed.files.len() < MAX_FILES {
            parsed.files.push(file);
        } else {
            parsed.truncated = true;
        }
    };
    for raw in patch.lines() {
        if raw.starts_with("diff --git ") {
            finish(&mut current, &mut parsed);
            current = Some(DiffFile::new(diff_header_path(raw)));
            continue;
        }
        let Some(file) = current.as_mut() else {
            continue;
        };
        if let Some(path) = raw.strip_prefix("rename from ") {
            file.old_path = Some(git_path(path));
            continue;
        }
        if let Some(path) = raw.strip_prefix("rename to ") {
            file.path = git_path(path);
            continue;
        }
        if let Some(raw_path) = raw.strip_prefix("--- ") {
            if let Some(path) = prefixed_path(raw_path) {
                file.old_path = Some(path);
            }
            continue;
        }
        if let Some(raw_path) = raw.strip_prefix("+++ ") {
            if let Some(path) = prefixed_path(raw_path) {
                file.path = path;
            }
            continue;
        }
        if raw.starts_with("Binary files ") || raw == "GIT binary patch" {
            file.binary = true;
            continue;
        }
        let (kind, text) = if let Some(hunk) = raw.strip_prefix("@@") {
            if let Some((old, new)) = hunk_start(hunk) {
                old_line = old;
                new_line = new;
            }
            ("hunk", raw)
        } else if let Some(text) = raw.strip_prefix('+') {
            file.additions += 1;
            ("add", text)
        } else if let Some(text) = raw.strip_prefix('-') {
            file.deletions += 1;
            ("remove", text)
        } else if let Some(text) = raw.strip_prefix(' ') {
            ("context", text)
        } else if raw.starts_with("\\ No newline") {
            ("hunk", raw)
        } else {
            continue;
        };
        let old = match kind {
            "context" | "remove" => {
                let line = old_line;
                old_line = old_line.saturating_add(1);
                Some(line)
            }
            _ => None,
        };
        let new = match kind {
            "context" | "add" => {
                let line = new_line;
                new_line = new_line.saturating_add(1);
                Some(line)
            }
            _ => None,
        };
        let (text, shortened) = short_text(text);
        if shortened {
            file.truncated = true;
            parsed.truncated = true;
        }
        if file.lines.len() < MAX_FILE_LINES && rendered_lines < MAX_TOTAL_LINES {
            file.lines.push(DiffLine {
                kind: kind.to_string(),
                text,
                old_line: old,
                new_line: new,
            });
            rendered_lines += 1;
        } else {
            file.truncated = true;
            parsed.truncated = true;
        }
    }
    finish(&mut current, &mut parsed);
    parsed
}

fn checked_sha(value: &str) -> Result<String, String> {
    if matches!(value.len(), 40 | 64) && value.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(value.to_string())
    } else {
        Err(format!("invalid commit id in the worker ledger: '{value}'"))
    }
}

fn validate_worker_identity(worker: &WorkerRecord) -> Result<(), String> {
    if worker.branch != agent_branch(&worker.id)
        || worker.id.is_empty()
        || !worker.id.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Err("the worker ledger has an invalid branch identity".to_string());
    }
    Ok(())
}

fn checkout_path(root: &Path, worker: &WorkerRecord) -> Result<PathBuf, String> {
    validate_worker_identity(worker)?;
    let root = dunce::canonicalize(root)
        .map_err(|err| format!("could not resolve the project root: {err}"))?;
    if is_pool_checkout(&worker.worktree) {
        let metadata = std::fs::symlink_metadata(&worker.worktree)
            .map_err(|err| format!("worker checkout could not be resolved: {err}"))?;
        if pool_is_link_or_junction(&metadata) || !metadata.is_dir() {
            return Err("the worker pool checkout is not a real directory".to_string());
        }
        let recorded = dunce::canonicalize(&worker.worktree)
            .map_err(|err| format!("worker checkout could not be resolved: {err}"))?;
        let worktrees = dunce::canonicalize(root.join(".worktrees"))
            .map_err(|err| format!("worker checkout could not be resolved: {err}"))?;
        if recorded.parent() != Some(worktrees.as_path()) || !is_pool_checkout(&recorded) {
            return Err("the worker checkout is outside its registered worktree".to_string());
        }
        return Ok(recorded);
    }
    let expected = root
        .join(".worktrees")
        .join(format!("agent-{}", agent_short_id(&worker.id)));
    let expected = dunce::canonicalize(&expected)
        .map_err(|err| format!("worker '{}' checkout is unavailable: {err}", worker.name))?;
    let recorded = dunce::canonicalize(&worker.worktree)
        .map_err(|err| format!("worker checkout could not be resolved: {err}"))?;
    if recorded != expected {
        return Err("the worker checkout is outside its registered worktree".to_string());
    }
    Ok(expected)
}

#[cfg(windows)]
fn pool_is_link_or_junction(metadata: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt as _;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn pool_is_link_or_junction(metadata: &std::fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

async fn read_diff(checkout: &Path, base: &str, tip: Option<&str>) -> Result<ParsedDiff, String> {
    let base = checked_sha(base)?;
    let tip = tip.map(checked_sha).transpose()?;
    let mut args = vec![
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--no-textconv",
        "--unified=3",
        &base,
    ];
    if let Some(tip) = tip.as_deref() {
        args.push(tip);
    }
    args.push("--");
    let patch = git_value(checkout, &args).await?;
    Ok(parse_unified_diff(&patch))
}

fn read_regular_file(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    use std::io::Read as _;

    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // The unix extension's `custom_flags` takes `i32` — only the Windows
        // extension above takes `u32`.
        const O_NOFOLLOW: i32 = 0x20000;
        options.custom_flags(O_NOFOLLOW);
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        const O_NOFOLLOW: i32 = 0x100;
        options.custom_flags(O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|err| format!("could not open {}: {err}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|err| format!("could not inspect {}: {err}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() > limit {
        return Err(format!("{} exceeds the preview size limit", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("could not read {}: {err}", path.display()))?;
    if bytes.len() as u64 > limit {
        return Err(format!("{} exceeds the preview size limit", path.display()));
    }
    Ok(bytes)
}

fn sensitive_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    name.starts_with(".env")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.ends_with(".pfx")
        || name.ends_with(".p12")
        || matches!(
            name.as_str(),
            "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519"
        )
}

fn read_untracked(checkout: &Path, relative: &str) -> Result<DiffFile, String> {
    let relative_path = Path::new(relative);
    if relative_path.is_absolute()
        || relative_path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err("Git returned an unsafe untracked path".to_string());
    }
    let checkout_root = dunce::canonicalize(checkout)
        .map_err(|err| format!("could not resolve the worker checkout: {err}"))?;
    let path = checkout_root.join(relative_path);
    let metadata = std::fs::symlink_metadata(&path)
        .map_err(|err| format!("could not inspect untracked file '{relative}': {err}"))?;
    let canonical = dunce::canonicalize(&path)
        .map_err(|err| format!("could not resolve untracked file '{relative}': {err}"))?;
    if !canonical.starts_with(&checkout_root) {
        return Err("an untracked file resolves outside the worker checkout".to_string());
    }
    let mut file = DiffFile::new(relative.to_string());
    let sensitive = sensitive_path(relative_path);
    file.sensitive = sensitive;
    file.truncated = sensitive;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        file.binary = true;
        return Ok(file);
    }
    if metadata.len() > MAX_UNTRACKED_BYTES {
        file.truncated = true;
        return Ok(file);
    }
    let bytes = read_regular_file(&canonical, MAX_UNTRACKED_BYTES)
        .map_err(|err| format!("could not read untracked file '{relative}': {err}"))?;
    let Ok(text) = std::str::from_utf8(&bytes) else {
        file.binary = true;
        return Ok(file);
    };
    if sensitive {
        file.additions = text.lines().count() as u64;
        return Ok(file);
    }
    file.lines.push(DiffLine {
        kind: "hunk".to_string(),
        text: "@@ new untracked file @@".to_string(),
        old_line: None,
        new_line: None,
    });
    for (index, line) in text.lines().enumerate() {
        file.additions += 1;
        if file.lines.len() >= MAX_FILE_LINES {
            file.truncated = true;
            continue;
        }
        let (text, shortened) = short_text(line);
        file.truncated |= shortened;
        file.lines.push(DiffLine {
            kind: "add".to_string(),
            text,
            old_line: None,
            new_line: Some((index + 1) as u32),
        });
    }
    Ok(file)
}

/// Read the selected worker's real branch and working-tree delta.
pub async fn worker_diff(root: &Path, worker: &WorkerRecord) -> Result<DiffSnapshot, String> {
    validate_worker_identity(worker)?;
    let (target, base, worker_tip, checkout) = if let Some(merge) = &worker.merge {
        let commit = checked_sha(&merge.commit)?;
        let base = git_value(root, &["rev-parse", &format!("{commit}^1")]).await?;
        let second_parent = git_value(root, &["rev-parse", &format!("{commit}^2")]).await?;
        if second_parent != merge.worker_tip {
            return Err("the merge record does not match its reviewed worker tip".to_string());
        }
        (merge.target.clone(), base, merge.worker_tip.clone(), None)
    } else {
        let checkout = if worker.status == crate::mnemo2::team::WorkerStatus::Cancelled
            && worker.worktree.as_os_str().is_empty()
        {
            None
        } else {
            Some(checkout_path(root, worker)?)
        };
        let target = current_branch(root)?;
        let worker_tip = match &checkout {
            Some(checkout) => tip(checkout).await?,
            None => {
                git_value(
                    root,
                    &[
                        "rev-parse",
                        &format!("refs/heads/{}^{{commit}}", worker.branch),
                    ],
                )
                .await?
            }
        };
        let base = git_value(root, &["merge-base", "HEAD", &worker_tip]).await?;
        (target, base, worker_tip, checkout)
    };
    let mut parsed = read_diff(
        checkout.as_deref().unwrap_or(root),
        &base,
        checkout.is_none().then_some(worker_tip.as_str()),
    )
    .await?;
    if let Some(checkout) = checkout {
        let untracked = git_value(
            &checkout,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )
        .await?;
        for relative in untracked.split('\0').filter(|path| !path.is_empty()) {
            let file = tokio::task::spawn_blocking({
                let checkout = checkout.clone();
                let relative = relative.to_string();
                move || read_untracked(&checkout, &relative)
            })
            .await
            .map_err(|err| format!("untracked-file task failed: {err}"))??;
            parsed.total_files += 1;
            parsed.total_additions += file.additions;
            parsed.truncated |= file.truncated;
            if parsed.files.len() < MAX_FILES {
                parsed.files.push(file);
            } else {
                parsed.truncated = true;
            }
        }
    }
    Ok(DiffSnapshot {
        worker_id: worker.id.clone(),
        branch: worker.branch.clone(),
        target,
        base: checked_sha(&base)?,
        tip: checked_sha(&worker_tip)?,
        files: parsed.files,
        total_files: parsed.total_files,
        total_additions: parsed.total_additions,
        total_deletions: parsed.total_deletions,
        truncated: parsed.truncated,
    })
}

/// Read one genuine worker screenshot as a data URL, never an arbitrary file.
///
/// The worker's checkout is read first, while the record still holds one. Once
/// that checkout was released, `archive` (see [`screenshot_archive_dir`])
/// holds the copy Mnemo kept at release time; every path and format guard
/// applies to both bases.
pub async fn worker_screenshot(
    root: &Path,
    worker: &WorkerRecord,
    relative: &str,
    archive: Option<&Path>,
) -> Result<String, String> {
    let path = Path::new(relative);
    if !is_worker_screenshot_path(relative) {
        return Err("not a worker screenshot path".to_string());
    }
    validate_worker_identity(worker)?;
    let archive_relative = path
        .strip_prefix(Path::new(SCREENSHOT_DIR))
        .map_err(|_| "not a worker screenshot path".to_string())?;
    // The live checkout first: a worker that still holds one shows the file it
    // has now, even when an older copy also sits in the archive.
    if let Ok(checkout) = checkout_path(root, worker) {
        let screenshot = checkout.join(path);
        if std::fs::symlink_metadata(&screenshot).is_ok() {
            return read_screenshot(&checkout, screenshot, "the worker checkout").await;
        }
    }
    if let Some(archive) = archive {
        let screenshot = archive.join(archive_relative);
        if std::fs::symlink_metadata(&screenshot).is_ok() {
            return read_screenshot(archive, screenshot, "the worker screenshot archive").await;
        }
    }
    Err(SCREENSHOT_NO_LONGER_AVAILABLE.to_string())
}

/// Whether a path a tool reported names a screenshot: a relative path under
/// `.coding/browser/screenshots`, plain components only, ending in `.png`.
pub(crate) fn is_worker_screenshot_path(relative: &str) -> bool {
    let path = Path::new(relative);
    path.starts_with(Path::new(SCREENSHOT_DIR))
        && path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && path.extension().and_then(|ext| ext.to_str()) == Some("png")
}

/// The folder that holds one worker's archived screenshots, inside the team's
/// state dir.
///
/// `None` for an id that cannot safely name a folder: the archive is only ever
/// written for a validated id, so an invalid one cannot have one.
pub(crate) fn screenshot_archive_dir(state_dir: &Path, worker_id: &str) -> Option<PathBuf> {
    if worker_id.len() < 8 || !worker_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(state_dir.join(SCREENSHOT_ARCHIVE_DIR).join(worker_id))
}

/// Read a screenshot as a data URL, enforcing every path and format guard
/// against `base`.
async fn read_screenshot(
    base: &Path,
    screenshot: PathBuf,
    outside: &'static str,
) -> Result<String, String> {
    let base = base.to_path_buf();
    let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let base = dunce::canonicalize(&base)
            .map_err(|err| format!("could not resolve the screenshot folder: {err}"))?;
        let canonical = dunce::canonicalize(&screenshot)
            .map_err(|err| format!("could not resolve the screenshot: {err}"))?;
        if !canonical.starts_with(&base) {
            return Err(format!("the screenshot is outside {outside}"));
        }
        let metadata = std::fs::symlink_metadata(&canonical)
            .map_err(|err| format!("could not inspect the screenshot: {err}"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("the screenshot is not a regular file".to_string());
        }
        if metadata.len() > MAX_SCREENSHOT_BYTES {
            return Err("the screenshot exceeds the 20 MiB preview limit".to_string());
        }
        let bytes = read_regular_file(&canonical, MAX_SCREENSHOT_BYTES)
            .map_err(|err| format!("could not read the screenshot: {err}"))?;
        if !bytes.starts_with(&PNG_SIGNATURE) {
            return Err("the screenshot is not a PNG file".to_string());
        }
        Ok(bytes)
    })
    .await
    .map_err(|err| format!("screenshot task failed: {err}"))??;
    Ok(format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

/// Copy a worker's screenshots out of its checkout into `archive`.
///
/// Every regular PNG file under `<checkout>/.coding/browser/screenshots` is
/// copied to the same relative path under `archive`. Links (symlinks and
/// Windows junctions), files over the preview limit and files that do not
/// start with the PNG signature are skipped. The copy is idempotent: an
/// existing archive file of the same size is kept. Returns how many files the
/// worker's archive holds afterwards; 0 when there is no screenshot folder.
pub(crate) async fn archive_worker_screenshots(
    checkout: &Path,
    archive: &Path,
) -> Result<usize, String> {
    let checkout = checkout.to_path_buf();
    let archive = archive.to_path_buf();
    tokio::task::spawn_blocking(move || copy_screenshots(&checkout, &archive))
        .await
        .map_err(|err| format!("screenshot archive task failed: {err}"))?
}

/// Distinguishes the temp files of concurrent archive copies.
static SCREENSHOT_COPY_SEQUENCE: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn copy_screenshots(checkout: &Path, archive: &Path) -> Result<usize, String> {
    let source = checkout.join(SCREENSHOT_DIR);
    let metadata = match std::fs::symlink_metadata(&source) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(format!("could not inspect {}: {err}", source.display())),
    };
    if !metadata.is_dir() || pool_is_link_or_junction(&metadata) {
        return Ok(0);
    }
    let mut archived = 0;
    copy_screenshot_dir(&source, &source, archive, &mut archived)?;
    Ok(archived)
}

/// Copy every PNG under `dir` (itself under `root`) to the same relative path
/// under `archive`, counting what the archive holds.
fn copy_screenshot_dir(
    root: &Path,
    dir: &Path,
    archive: &Path,
    archived: &mut usize,
) -> Result<(), String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|err| format!("could not read {}: {err}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("could not read {}: {err}", dir.display()))?;
        let path = entry.path();
        // `symlink_metadata` never follows a link, so a symlink or junction is
        // skipped instead of copied out of the screenshot folder.
        let metadata = std::fs::symlink_metadata(&path)
            .map_err(|err| format!("could not inspect {}: {err}", path.display()))?;
        if pool_is_link_or_junction(&metadata) {
            continue;
        }
        if metadata.is_dir() {
            copy_screenshot_dir(root, &path, archive, archived)?;
            continue;
        }
        let is_png = path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("png"));
        if !metadata.is_file() || !is_png || metadata.len() > MAX_SCREENSHOT_BYTES {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| format!("{} is outside the screenshot folder", path.display()))?;
        let bytes = read_regular_file(&path, MAX_SCREENSHOT_BYTES)
            .map_err(|err| format!("could not read {}: {err}", path.display()))?;
        if !bytes.starts_with(&PNG_SIGNATURE) {
            continue;
        }
        copy_archived_png(&bytes, &archive.join(relative))?;
        *archived += 1;
    }
    Ok(())
}

/// Write one archived PNG through a temp file and a rename, keeping an
/// existing copy of the same size.
fn copy_archived_png(bytes: &[u8], target: &Path) -> Result<(), String> {
    if let Ok(existing) = std::fs::symlink_metadata(target) {
        if existing.is_file() && existing.len() == bytes.len() as u64 {
            return Ok(());
        }
    }
    let parent = target
        .parent()
        .ok_or_else(|| format!("{} has no parent folder", target.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|err| format!("could not create {}: {err}", parent.display()))?;
    let sequence = SCREENSHOT_COPY_SEQUENCE
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let temp = parent.join(format!(".archive-{}-{sequence}.tmp", std::process::id()));
    std::fs::write(&temp, bytes)
        .map_err(|err| format!("could not write {}: {err}", temp.display()))?;
    let renamed = std::fs::rename(&temp, target).or_else(|_| {
        // Windows cannot rename over an existing file: drop the stale copy (or
        // link) and retry once.
        let _ = std::fs::remove_file(target);
        std::fs::rename(&temp, target)
    });
    if let Err(err) = renamed {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("could not archive {}: {err}", target.display()));
    }
    Ok(())
}

/// Remove the screenshot files Git does not track from a checkout, once their
/// archive copy exists.
///
/// `git clean` is scoped to the screenshot folder and only ever deletes
/// untracked (or ignored) files there, so a tracked screenshot survives. A
/// folder without its own `.git` is skipped: Git would otherwise walk up to
/// the project root and clean the lead's checkout instead.
pub(crate) async fn clean_checkout_screenshots(checkout: &Path) -> Result<(), String> {
    let checkout = checkout.to_path_buf();
    tokio::task::spawn_blocking(move || {
        if !checkout.join(SCREENSHOT_DIR).is_dir() || !checkout.join(".git").exists() {
            return Ok(());
        }
        let output = std::process::Command::new("git")
            .args(["clean", "-fdx", "--", SCREENSHOT_DIR])
            .current_dir(&checkout)
            .output()
            .map_err(|err| format!("could not run git clean in {}: {err}", checkout.display()))?;
        if !output.status.success() {
            return Err(format!(
                "git clean failed in {}: {}",
                checkout.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(())
    })
    .await
    .map_err(|err| format!("screenshot clean task failed: {err}"))?
}

/// Keep a worker's screenshots before its checkout is released or removed.
///
/// The files are copied into the team's state dir and only then dropped from
/// the checkout, so the next pool reuse cannot wipe the only copy - and the
/// release no longer refuses over work Git does not track. Best effort on
/// purpose: an archive failure is logged and the release goes ahead as
/// before, with the screenshots still in the checkout.
pub(crate) async fn keep_worker_screenshots(
    worker_id: &str,
    checkout: &Path,
    state_dir: &Path,
) {
    let Some(archive) = screenshot_archive_dir(state_dir, worker_id) else {
        return;
    };
    match archive_worker_screenshots(checkout, &archive).await {
        Ok(archived) => {
            if archived > 0 {
                eprintln!(
                    "mnemo2: archived {archived} screenshot(s) of {worker_id} in {}",
                    archive.display()
                );
            }
            if let Err(error) = clean_checkout_screenshots(checkout).await {
                eprintln!(
                    "mnemo2: could not remove the archived screenshots from {}: {error}",
                    checkout.display()
                );
            }
        }
        Err(error) => eprintln!(
            "mnemo2: could not archive the screenshots of {worker_id} from {}: {error}",
            checkout.display()
        ),
    }
}

/// Read an image a worker's tool call reported, as a `data:` URL for the tool
/// card.
///
/// A screenshot path resolves like [`worker_screenshot`]: the checkout first,
/// then the archive kept when that checkout was released. Every other path
/// keeps resolving against the checkout alone.
pub async fn worker_tool_image(
    root: &Path,
    worker: &WorkerRecord,
    requested: &str,
    archive: Option<&Path>,
) -> Result<String, String> {
    let normalized = requested.replace('\\', "/");
    if is_worker_screenshot_path(&normalized) {
        return worker_screenshot(root, worker, &normalized, archive).await;
    }
    let checkout = worker_checkout(root, worker)?;
    read_tool_image(checkout, requested).await
}

/// The MIME type an image file's extension names, or None for anything the
/// tool card cannot render inline.
pub fn tool_image_mime(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    TOOL_IMAGE_TYPES
        .iter()
        .find(|(known, _)| *known == extension.as_str())
        .map(|(_, mime)| *mime)
}

/// The checkout a worker's tool images live in (its registered worktree).
pub fn worker_checkout(root: &Path, worker: &WorkerRecord) -> Result<PathBuf, String> {
    checkout_path(root, worker)
}

/// Read an image a tool call took or looked at, as a `data:` URL for the tool
/// card. `checkout` is the project root for the lead or the worker's worktree;
/// `requested` is the path the call reported, relative to it or absolute.
pub async fn read_tool_image(checkout: PathBuf, requested: &str) -> Result<String, String> {
    let root = dunce::canonicalize(&checkout)
        .map_err(|err| format!("could not resolve the project root: {err}"))?;
    // A tool may report a Windows path; every component check below reads the
    // forward-slash form so both spellings take the same route.
    let requested = requested.replace('\\', "/");
    let requested = Path::new(&requested);
    let path = if requested.is_absolute() {
        let absolute = dunce::canonicalize(requested)
            .map_err(|err| format!("could not resolve the image: {err}"))?;
        if !absolute.starts_with(&root) {
            return Err("the image is outside the project".to_string());
        }
        absolute
    } else {
        if requested
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err("not a project image path".to_string());
        }
        if matches!(requested.components().next(), Some(Component::Normal(name)) if name == std::ffi::OsStr::new(".git")) {
            return Err("not a project image path".to_string());
        }
        checkout.join(requested)
    };
    if sensitive_path(&path) {
        return Err("not a project image path".to_string());
    }
    let mime = tool_image_mime(&path).ok_or_else(|| "not an image file".to_string())?;
    let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let canonical = dunce::canonicalize(&path)
            .map_err(|err| format!("could not resolve the image: {err}"))?;
        if !canonical.starts_with(&root) {
            return Err("the image is outside the project".to_string());
        }
        let metadata = std::fs::symlink_metadata(&canonical)
            .map_err(|err| format!("could not inspect the image: {err}"))?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("the image is not a regular file".to_string());
        }
        if metadata.len() > MAX_SCREENSHOT_BYTES {
            return Err("the image exceeds the 20 MiB preview limit".to_string());
        }
        read_regular_file(&canonical, MAX_SCREENSHOT_BYTES)
            .map_err(|err| format!("could not read the image: {err}"))
    })
    .await
    .map_err(|err| format!("image task failed: {err}"))??;
    Ok(format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_hunks_renames_and_binary_paths() {
        let patch = concat!(
            "diff --git a/old name.rs b/new name.rs\n",
            "similarity index 80%\n",
            "rename from old name.rs\n",
            "rename to new name.rs\n",
            "--- a/old name.rs\n",
            "+++ b/new name.rs\n",
            "@@ -1,2 +1,2 @@\n",
            " context\n",
            "-old\n",
            "+new\n",
            "diff --git \"a/caf\\303\\251.png\" \"b/caf\\303\\251.png\"\n",
            "Binary files \"a/caf\\303\\251.png\" and \"b/caf\\303\\251.png\" differ\n",
        );
        let parsed = parse_unified_diff(patch);
        assert!(!parsed.truncated);
        assert_eq!(parsed.files.len(), 2);
        assert_eq!(parsed.files[0].path, "new name.rs");
        assert_eq!(parsed.files[0].old_path.as_deref(), Some("old name.rs"));
        assert_eq!(parsed.files[0].additions, 1);
        assert_eq!(parsed.files[0].deletions, 1);
        assert_eq!(parsed.files[0].lines[2].old_line, Some(2));
        assert_eq!(parsed.files[0].lines[2].new_line, None);
        assert_eq!(parsed.files[0].lines[3].new_line, Some(2));
        assert_eq!(parsed.files[1].path, "café.png");
        assert!(parsed.files[1].binary);
        assert_eq!(
            (
                parsed.total_files,
                parsed.total_additions,
                parsed.total_deletions
            ),
            (2, 1, 1)
        );
    }

    #[test]
    fn large_diffs_are_explicitly_truncated_without_losing_real_counts() {
        let additions = 700_000;
        let patch = format!(
            "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -0,0 +1,{additions} @@\n{}",
            "+line\n".repeat(additions)
        );
        assert!(patch.len() > 4 * 1024 * 1024);
        let parsed = parse_unified_diff(&patch);
        assert!(parsed.truncated);
        assert_eq!(parsed.files[0].additions, additions as u64);
        assert_eq!(parsed.total_additions, additions as u64);
        assert_eq!(parsed.files[0].lines.len(), MAX_FILE_LINES);
    }

    #[test]
    fn sensitive_diff_keeps_real_counts_without_content() {
        let patch = concat!(
            "diff --git a/.env b/.env\n",
            "--- a/.env\n",
            "+++ b/.env\n",
            "@@ -0,0 +1 @@\n",
            "+MNEMO2_TEST_SECRET=do-not-render\n",
        );
        let parsed = parse_unified_diff(patch);
        let file = &parsed.files[0];
        assert!(file.sensitive);
        assert!(file.truncated);
        assert!(file.lines.is_empty());
        assert_eq!(file.additions, 1);
        assert_eq!(parsed.total_additions, 1);
        let serialized = serde_json::to_string(&parsed.files).expect("serialized diff");
        assert!(!serialized.contains("MNEMO2_TEST_SECRET"));
        assert!(!serialized.contains("do-not-render"));

        let checkout = tempfile::tempdir().expect("checkout");
        std::fs::write(
            checkout.path().join(".env"),
            "FIRST=do-not-render\nSECOND=also-hidden\n",
        )
        .expect("untracked sensitive file");
        let file = read_untracked(checkout.path(), ".env").expect("untracked sensitive diff");
        assert!(file.sensitive);
        assert!(file.truncated);
        assert!(file.lines.is_empty());
        assert_eq!(file.additions, 2);
        let serialized = serde_json::to_string(&file).expect("serialized untracked diff");
        assert!(!serialized.contains("do-not-render"));
        assert!(!serialized.contains("also-hidden"));
    }

    fn git(root: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git process");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn worker(root: &Path, checkout: &Path) -> WorkerRecord {
        serde_json::from_value(serde_json::json!({
            "id": "abcd1234ffff",
            "name": "Worker abcd1234",
            "brief": "Change file.txt and keep scope",
            "model": null,
            "model_id": "test-model",
            "branch": "wt/agent-abcd1234",
            "worktree": checkout,
            "status": "in_review",
            "generation": 1,
            "transcript": { "messages": [] },
            "pending": null,
            "used_tokens": 0,
            "max_tokens": 1000,
            "checks": [{ "name": "test", "status": "passed", "tip": "pending" }],
            "error": null,
            "_root": root,
        }))
        .expect("worker record")
    }

    #[tokio::test]
    async fn reads_real_active_and_merged_deltas_and_guarded_screenshots() {
        let repo = tempfile::tempdir().expect("repo");
        let root = repo.path();
        git(root, &["init", "-b", "wt/test"]);
        git(root, &["config", "user.name", "Test"]);
        git(root, &["config", "user.email", "test@example.invalid"]);
        std::fs::write(root.join("file.txt"), "base\n").expect("base file");
        git(root, &["add", "file.txt"]);
        git(root, &["commit", "-m", "base"]);
        let checkout = root.join(".worktrees/agent-abcd1234");
        git(
            root,
            &[
                "worktree",
                "add",
                "-b",
                "wt/agent-abcd1234",
                checkout.to_str().expect("checkout path"),
                "wt/test",
            ],
        );
        std::fs::write(checkout.join("file.txt"), "base\nworker\n").expect("worker edit");
        git(&checkout, &["add", "file.txt"]);
        git(&checkout, &["commit", "-m", "worker change"]);
        std::fs::write(checkout.join("notes.txt"), "uncommitted\n").expect("untracked");
        let screenshot = checkout.join(".coding/browser/screenshots/test.png");
        std::fs::create_dir_all(screenshot.parent().expect("screenshot directory"))
            .expect("screenshot directory");
        let png = b"\x89PNG\r\n\x1a\nreal test image";
        std::fs::write(&screenshot, png).expect("screenshot");
        let mut worker = worker(root, &checkout);

        let active = worker_diff(root, &worker).await.expect("active diff");
        assert_eq!(active.target, "wt/test");
        assert_eq!(active.total_files, 3);
        assert!(active.files.iter().any(|file| file.path == "notes.txt"));
        assert!(active
            .files
            .iter()
            .any(|file| file.path == "file.txt" && file.additions == 1));
        let image = worker_screenshot(root, &worker, ".coding/browser/screenshots/test.png", None)
            .await
            .expect("screenshot data");
        assert!(image.starts_with("data:image/png;base64,"));
        assert!(worker_screenshot(root, &worker, "../file.txt", None)
            .await
            .is_err());

        let worker_tip = active.tip.clone();
        git(
            root,
            &[
                "merge",
                "--no-ff",
                "wt/agent-abcd1234",
                "-m",
                "merge worker",
            ],
        );
        let merge_commit = tip(root).await.expect("merge commit");
        worker.status = crate::mnemo2::team::WorkerStatus::Merged;
        worker.merge = Some(crate::mnemo2::phase3::MergeRecord {
            commit: merge_commit,
            worker_tip,
            branch: worker.branch.clone(),
            target: "wt/test".to_string(),
            checks: vec!["test".to_string()],
            review_summary: "Reviewed".to_string(),
            review_risk: None,
            review_evidence: None,
            files: 1,
            additions: 1,
            deletions: 0,
            worker_model: None,
            worker_tokens: 0,
            cost_micros: None,
            merged_at_ms: 1,
            revert_commit: None,
        });
        git(
            root,
            &[
                "worktree",
                "remove",
                "--force",
                checkout.to_str().expect("checkout"),
            ],
        );
        let merged = worker_diff(root, &worker)
            .await
            .expect("historical merged diff");
        assert_eq!(merged.total_files, 1);
        assert_eq!(merged.files[0].path, "file.txt");
        assert_eq!(merged.total_additions, 1);
        assert!(!checkout.exists());
    }

    #[test]
    fn tool_image_mime_by_extension() {
        assert_eq!(tool_image_mime(Path::new("a/b.PNG")), Some("image/png"));
        assert_eq!(tool_image_mime(Path::new("x.jpeg")), Some("image/jpeg"));
        assert_eq!(tool_image_mime(Path::new("x.txt")), None);
    }

    /// A canonical tempdir standing in for a project root.
    fn canonical_tempdir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("checkout");
        let root = dunce::canonicalize(dir.path()).expect("canonical checkout");
        (dir, root)
    }

    #[tokio::test]
    async fn read_tool_image_returns_a_data_url() {
        let (_dir, root) = canonical_tempdir();
        std::fs::create_dir_all(root.join("shots")).expect("shots");
        std::fs::write(root.join("shots/a.png"), b"\x89PNG\r\n\x1a\nimage").expect("png");
        let data = read_tool_image(root, "shots/a.png")
            .await
            .expect("data url");
        assert!(data.starts_with("data:image/png;base64,"));
    }

    #[tokio::test]
    async fn read_tool_image_accepts_an_absolute_path_inside() {
        let (_dir, root) = canonical_tempdir();
        let image = root.join("shot.png");
        std::fs::write(&image, b"\x89PNG\r\n\x1a\nimage").expect("png");
        let data = read_tool_image(root, image.to_str().expect("absolute path"))
            .await
            .expect("data url");
        assert!(data.starts_with("data:image/png;base64,"));
    }

    #[tokio::test]
    async fn read_tool_image_refuses_parent_paths() {
        let (_dir, root) = canonical_tempdir();
        assert!(read_tool_image(root, "../x.png").await.is_err());
    }

    #[tokio::test]
    async fn read_tool_image_refuses_git_and_secrets() {
        let (_dir, root) = canonical_tempdir();
        std::fs::create_dir_all(root.join(".git")).expect(".git");
        std::fs::write(root.join(".git/x.png"), b"\x89PNG\r\n\x1a\nimage").expect("git png");
        std::fs::write(root.join(".env.png"), b"\x89PNG\r\n\x1a\nimage").expect("secret png");
        assert!(read_tool_image(root.clone(), ".git/x.png").await.is_err());
        assert!(read_tool_image(root, ".env.png").await.is_err());
    }

    #[tokio::test]
    async fn read_tool_image_refuses_outside_absolute() {
        let (_dir, root) = canonical_tempdir();
        let (_outside_dir, outside) = canonical_tempdir();
        let image = outside.join("x.png");
        std::fs::write(&image, b"\x89PNG\r\n\x1a\nimage").expect("png");
        assert!(read_tool_image(root, image.to_str().expect("absolute path"))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn read_tool_image_refuses_non_images() {
        let (_dir, root) = canonical_tempdir();
        std::fs::write(root.join("notes.txt"), b"text").expect("notes");
        assert!(read_tool_image(root, "notes.txt").await.is_err());
    }
    #[test]
    fn checkout_path_accepts_a_recorded_pool_checkout() {
        let root = tempfile::tempdir().expect("workspace");
        let pool = root.path().join(".worktrees/pool-1");
        std::fs::create_dir_all(&pool).expect("pool checkout");
        let record = worker(root.path(), &pool);

        let resolved = checkout_path(root.path(), &record).expect("pool checkout path");

        assert_eq!(resolved, dunce::canonicalize(pool).expect("canonical pool"));
    }

    /// Decode a `data:image/png;base64,` URL back to its bytes.
    fn decode_png(data: &str) -> Vec<u8> {
        let payload = data
            .strip_prefix("data:image/png;base64,")
            .expect("png data url");
        base64::engine::general_purpose::STANDARD
            .decode(payload)
            .expect("base64 payload")
    }

    /// A root and its legacy checkout folder, the layout `checkout_path`
    /// derives from a worker record.
    fn legacy_checkout() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().expect("workspace");
        let checkout = root.path().join(".worktrees/agent-abcd1234");
        std::fs::create_dir_all(&checkout).expect("checkout");
        (root, checkout)
    }

    #[tokio::test]
    async fn archive_copies_pngs_and_skips_links_large_and_non_png_files() {
        let (checkout_dir, checkout) = canonical_tempdir();
        let (_archive_dir, archive) = canonical_tempdir();
        let shots = checkout.join(SCREENSHOT_DIR);
        std::fs::create_dir_all(shots.join("nested")).expect("screenshot folders");
        std::fs::write(shots.join("shot-1.png"), b"\x89PNG\r\n\x1a\nfirst").expect("png");
        std::fs::write(shots.join("nested/shot-2.png"), b"\x89PNG\r\n\x1a\nsecond")
            .expect("nested png");
        std::fs::write(shots.join("notes.txt"), b"not a png").expect("text");
        std::fs::write(shots.join("fake.png"), b"GIF89a not a png").expect("fake png");
        let large = std::fs::File::create(shots.join("large.png")).expect("large png");
        large
            .set_len(MAX_SCREENSHOT_BYTES + 1)
            .expect("sparse large png");
        // A directory link must be skipped, not followed (following it would
        // loop back into the checkout forever).
        let link = checkout_dir
            .path()
            .join(".coding")
            .join("browser")
            .join("screenshots")
            .join("loop");
        #[cfg(unix)]
        std::os::unix::fs::symlink(checkout_dir.path(), &link).expect("directory symlink");
        #[cfg(windows)]
        {
            let output = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(checkout_dir.path())
                .output()
                .expect("mklink process");
            assert!(
                output.status.success(),
                "mklink failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let archived = archive_worker_screenshots(&checkout, &archive)
            .await
            .expect("archive screenshots");
        assert_eq!(archived, 2, "only genuine PNG files are archived");
        assert_eq!(
            std::fs::read(archive.join("shot-1.png")).expect("archived png"),
            b"\x89PNG\r\n\x1a\nfirst"
        );
        assert_eq!(
            std::fs::read(archive.join("nested/shot-2.png")).expect("archived nested png"),
            b"\x89PNG\r\n\x1a\nsecond"
        );
        assert!(!archive.join("notes.txt").exists());
        assert!(!archive.join("fake.png").exists());
        assert!(!archive.join("large.png").exists());
        assert!(!archive.join("loop").exists());

        // Idempotent: a second run keeps the archived copies.
        assert_eq!(
            archive_worker_screenshots(&checkout, &archive)
                .await
                .expect("second archive"),
            2
        );
        // A checkout without a screenshot folder archives nothing.
        let (_empty_dir, empty) = canonical_tempdir();
        let (_other_dir, other_archive) = canonical_tempdir();
        assert_eq!(
            archive_worker_screenshots(&empty, &other_archive)
                .await
                .expect("no screenshot folder"),
            0
        );
    }

    #[tokio::test]
    async fn a_screenshot_in_a_held_checkout_is_read_from_the_checkout() {
        let (root, checkout) = legacy_checkout();
        let shots = checkout.join(SCREENSHOT_DIR);
        std::fs::create_dir_all(&shots).expect("checkout screenshots");
        std::fs::write(shots.join("shot.png"), b"\x89PNG\r\n\x1a\nfrom checkout")
            .expect("checkout png");
        let archive = root.path().join("archive");
        std::fs::create_dir_all(&archive).expect("archive");
        std::fs::write(archive.join("shot.png"), b"\x89PNG\r\n\x1a\nfrom archive")
            .expect("archived png");
        let record = worker(root.path(), &checkout);

        let data = worker_screenshot(
            root.path(),
            &record,
            ".coding/browser/screenshots/shot.png",
            Some(&archive),
        )
        .await
        .expect("held checkout screenshot");

        assert_eq!(decode_png(&data), b"\x89PNG\r\n\x1a\nfrom checkout");
    }

    #[tokio::test]
    async fn a_released_workers_missing_screenshot_says_it_is_no_longer_available() {
        let (root, _checkout) = legacy_checkout();
        let archive = root.path().join("archive");
        std::fs::create_dir_all(&archive).expect("archive");
        let mut record = worker(root.path(), &root.path().join(".worktrees/agent-abcd1234"));
        record.status = crate::mnemo2::team::WorkerStatus::Merged;
        record.worktree = PathBuf::new();

        let error = worker_screenshot(
            root.path(),
            &record,
            ".coding/browser/screenshots/shot.png",
            Some(&archive),
        )
        .await
        .expect_err("a screenshot merged before the archive existed is gone");

        assert_eq!(error, SCREENSHOT_NO_LONGER_AVAILABLE);
    }

    #[tokio::test]
    async fn a_merged_workers_screenshot_thumbnail_still_loads() {
        let (root, _checkout) = legacy_checkout();
        let archive = root.path().join("archive");
        std::fs::create_dir_all(&archive).expect("archive");
        std::fs::write(archive.join("shot.png"), b"\x89PNG\r\n\x1a\nthumb")
            .expect("archived png");
        let mut record = worker(root.path(), &root.path().join(".worktrees/agent-abcd1234"));
        record.status = crate::mnemo2::team::WorkerStatus::Merged;
        record.worktree = PathBuf::new();

        let data = worker_tool_image(
            root.path(),
            &record,
            ".coding/browser/screenshots/shot.png",
            Some(&archive),
        )
        .await
        .expect("the tool card's screenshot thumbnail loads from the archive");

        assert_eq!(decode_png(&data), b"\x89PNG\r\n\x1a\nthumb");
    }
}
