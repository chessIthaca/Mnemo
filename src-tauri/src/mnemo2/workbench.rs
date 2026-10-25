// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! Read-only Git and screenshot data for Mnemo2's native workbench.

use std::path::{Component, Path, PathBuf};

use base64::Engine as _;
use mnemo::app::worktrees::{agent_branch, agent_short_id, current_branch};
use serde::Serialize;

use crate::mnemo2::phase3::{git_value, tip};
use crate::mnemo2::team::WorkerRecord;

const MAX_FILES: usize = 300;
const MAX_FILE_LINES: usize = 1_000;
const MAX_TOTAL_LINES: usize = 12_000;
const MAX_LINE_BYTES: usize = 8 * 1024;
const MAX_UNTRACKED_BYTES: u64 = 1024 * 1024;
const MAX_SCREENSHOT_BYTES: u64 = 20 * 1024 * 1024;

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
        let (kind, text) = if raw.starts_with("@@") {
            if let Some((old, new)) = hunk_start(&raw[2..]) {
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
        const O_NOFOLLOW: u32 = 0x20000;
        options.custom_flags(O_NOFOLLOW);
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        const O_NOFOLLOW: u32 = 0x100;
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
pub async fn worker_screenshot(
    root: &Path,
    worker: &WorkerRecord,
    relative: &str,
) -> Result<String, String> {
    let path = Path::new(relative);
    let expected = Path::new(".coding/browser/screenshots");
    if !path.starts_with(expected)
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        || path.extension().and_then(|ext| ext.to_str()) != Some("png")
    {
        return Err("not a worker screenshot path".to_string());
    }
    let checkout = checkout_path(root, worker)?;
    let screenshot = checkout.join(path);
    let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
        let canonical = dunce::canonicalize(&screenshot)
            .map_err(|err| format!("could not resolve the screenshot: {err}"))?;
        if !canonical.starts_with(&checkout) {
            return Err("the screenshot is outside the worker checkout".to_string());
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
        if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
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
        let image = worker_screenshot(root, &worker, ".coding/browser/screenshots/test.png")
            .await
            .expect("screenshot data");
        assert!(image.starts_with("data:image/png;base64,"));
        assert!(worker_screenshot(root, &worker, "../file.txt")
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
}
