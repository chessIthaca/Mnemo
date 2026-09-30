//! Repo-wide encoding guard: no source file under the scanned roots may
//! carry a double-encoded (mojibake) sequence.
//!
//! Two damage classes are covered — the two observed in this repo:
//!
//! - class 1: a UTF-8 file read as Windows-1252 and re-saved as UTF-8. Six
//!   fixed needles: the em dash, arrow, box-drawing dash, en dash,
//!   greater-or-equal, and CJK-hint sequences (the same set the
//!   `image_tools_strings_carry_no_mojibake` module guard uses).
//! - class 2: a UTF-8 file read as CP437, re-saved, then read as
//!   Windows-1252 and re-saved again. Which character leads the damage
//!   depends on the damaged original's first UTF-8 byte: 0xE2-led text
//!   (general punctuation — the family found in src/agent/tests.rs on
//!   2027-01-11: em dash, arrow, double arrow — and CJK) surfaces as
//!   U+00CE followed by a non-ASCII character, while 0xC2/0xC3-led text
//!   (Latin-1-range letters and symbols) and 0xF0-led emoji surface as
//!   U+00E2 followed by a non-ASCII character. Both leads are checked
//!   (they cover this codebase's repertoire), and overlaps with the fixed
//!   needles below are de-duplicated by offset.
//!
//! Needles are written as Unicode escape sequences on purpose: this file
//! must never contain a raw needle, or the scan would flag itself. The walk
//! skips build output, dependencies, vendored sources, and `.coding/**`
//! records (which quote mojibake deliberately).

use std::path::{Path, PathBuf};

/// Roots scanned, relative to the repo root.
const ROOTS: &[&str] = &["src", "src-tauri/src", "frontend/src", "tests"];

/// Directory names skipped anywhere in the walk.
const SKIP_DIRS: &[&str] = &[
    "target",
    "node_modules",
    ".git",
    "dist",
    ".worktrees",
    "vendor",
    ".coding",
];

/// File extensions treated as text sources.
const TEXT_EXTS: &[&str] = &["rs", "ts", "tsx", "js", "jsx", "css", "json", "toml", "md"];

/// Class-1 needles: (label, sequence).
const CLASS1: &[(&str, &str)] = &[
    ("cp1252 em dash", "\u{00E2}\u{20AC}\u{201D}"),
    ("cp1252 arrow", "\u{00E2}\u{2020}\u{2019}"),
    ("cp1252 box drawing", "\u{00E2}\u{201D}\u{20AC}"),
    ("cp1252 en dash", "\u{00E2}\u{20AC}\u{201C}"),
    ("cp1252 greater-or-equal", "\u{00E2}\u{2030}\u{00A5}"),
    (
        "cp1252 cjk hint",
        "\u{00E4}\u{00B8}\u{00AD}\u{00E6}\u{2013}\u{2021}",
    ),
];

/// Class-2 signature leads, as UTF-8: U+00CE (the damage of 0xE2-led
/// originals — general punctuation and CJK) and U+00E2 (the damage of
/// 0xC2/0xC3-led originals — Latin-1-range letters and symbols — and of
/// 0xF0-led emoji). A lead counts only when followed by a non-ASCII byte.
const CLASS2_LEADS: &[&[u8]] = &[&[0xC3, 0x8E], &[0xC3, 0xA2]];

/// Collect every text file under `dir`, skipping [`SKIP_DIRS`].
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if SKIP_DIRS.contains(&name.as_str()) {
                continue;
            }
            collect_files(&path, out);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if TEXT_EXTS.contains(&ext) {
                out.push(path);
            }
        }
    }
}

/// All mojibake hits in `bytes` as (offset, label), earliest first.
fn mojibake_hits(bytes: &[u8]) -> Vec<(usize, String)> {
    let mut hits: Vec<(usize, String)> = Vec::new();
    for (label, needle) in CLASS1 {
        let n = needle.as_bytes();
        let mut off = 0;
        while let Some(pos) = bytes[off..].windows(n.len()).position(|w| w == n) {
            let at = off + pos;
            if hits.iter().all(|(seen, _)| *seen != at) {
                hits.push((at, (*label).to_string()));
            }
            off = at + 1;
        }
    }
    for lead in CLASS2_LEADS {
        let mut off = 0;
        while let Some(pos) = bytes[off..].windows(lead.len()).position(|w| w == *lead) {
            let at = off + pos;
            let non_ascii_next = bytes.get(at + lead.len()).is_some_and(|b| *b >= 0x80);
            if non_ascii_next && hits.iter().all(|(seen, _)| *seen != at) {
                hits.push((at, "cp437+cp1252 double-mangle".to_string()));
            }
            off = at + 1;
        }
    }
    hits.sort();
    hits
}

/// Every scanned source file is free of both mojibake families, and the
/// walk is non-vacuous (anti-vacuous file floor + per-root presence).
#[test]
fn repo_source_carries_no_mojibake() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut scanned = 0usize;
    let mut findings = Vec::new();
    for rel in ROOTS {
        let mut files = Vec::new();
        collect_files(&root.join(rel), &mut files);
        assert!(
            !files.is_empty(),
            "no files found under {rel} — the walk is broken"
        );
        for file in files {
            let bytes = std::fs::read(&file)
                .unwrap_or_else(|e| panic!("encoding guard cannot read {}: {e}", file.display()));
            if bytes.is_empty() {
                continue;
            }
            if bytes.iter().take(8192).any(|b| *b == 0) {
                continue; // binary, out of scope
            }
            scanned += 1;
            for (at, label) in mojibake_hits(&bytes) {
                let line = bytes[..at].iter().filter(|b| **b == b'\n').count() + 1;
                findings.push(format!(
                    "{}:{} carries {label} mojibake",
                    file.strip_prefix(root).unwrap_or(&file).display(),
                    line
                ));
            }
        }
    }
    assert!(
        scanned >= 100,
        "only {scanned} text files scanned — the walk is too narrow"
    );
    assert!(
        findings.is_empty(),
        "double-encoded mojibake found ({} finding(s)):\n{}",
        findings.len(),
        findings.join("\n")
    );
}
