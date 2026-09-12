// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Shared line-ending helpers.
//!
//! The file tools (`file_edit`, `file_write`, `file_append`) match and write
//! content in a line-ending-agnostic way: the caller's strings may use either
//! `\n` (LF) or `\r\n` (CRLF), and the tool normalizes them to the *existing
//! file's* style so the file's endings are preserved on write. This avoids the
//! "LF will be replaced by CRLF" churn and mixed-ending files that arise when
//! an LF `content` overwrites a CRLF file (a common stumble on Windows repos).

use std::path::Path;

/// Detect the dominant line-ending style of a string.
///
/// Majority vote (backlog #48, 2026-04-20): returns `"\r\n"` (Windows/CRLF)
/// when CRLF-terminated lines outnumber OR tie the bare-LF lines, and `"\n"`
/// (Unix/LF) when bare-LF lines strictly outnumber CRLF lines or the content
/// has no line endings at all. The vote keeps a mostly-LF file with one
/// stray CRLF detectable as LF, so multi-line needles normalized to the
/// file's style still match the file's dominant reality (the previous
/// any-CRLF-wins rule flipped such files to CRLF and every LF-spanning
/// `old_string` missed). Ties keep the historical any-CRLF behavior so
/// existing callers and tests are unaffected.
pub fn detect_line_ending(content: &str) -> &'static str {
    let crlf = content.matches("\r\n").count();
    let bare_lf = content.matches('\n').count() - crlf;
    if crlf > 0 && bare_lf <= crlf {
        "\r\n"
    } else {
        "\n"
    }
}

/// Detect the line-ending style of an existing file by reading a **bounded
/// prefix** (8 KB), without loading the whole file into memory.
///
/// Returns:
/// - `Some("\r\n")` if any CRLF byte sequence is found in the prefix,
/// - `Some("\n")` if the prefix has content but no CRLF,
/// - `None` if the file is missing, empty, or unreadable — in which case the
///   caller should write its content verbatim (there is no style to preserve).
///
/// Line-ending style is consistent within a file, so scanning a prefix is
/// sufficient. This matters for `file_append`, which is documented for writing
/// large files in chunks — reading a whole multi-hundred-MB file just to
/// detect its endings would be disproportionate. Treating an empty existing
/// file as `None` (write as-is) also keeps it consistent with the
/// non-existent-file path, so a zero-byte file doesn't silently convert CRLF
/// content to LF.
pub fn detect_line_ending_path(path: &Path) -> Option<&'static str> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; 8192];
    let n = file.read(&mut buf).ok()?;
    if n == 0 {
        return None;
    }
    let prefix = &buf[..n];
    // Scan for a CRLF byte sequence (0x0D 0x0A). ASCII-safe on raw bytes, so
    // this works even for non-UTF-8 files (though appending text to a binary
    // file is already a caller error).
    if prefix.windows(2).any(|w| w == b"\r\n") {
        Some("\r\n")
    } else {
        Some("\n")
    }
}

/// Normalize all line endings in `s` to the target style `le`.
///
/// First collapses `\r\n` and lone `\r` to `\n`, then — if the target is CRLF —
/// expands `\n` back to `\r\n`. This lets us match a caller-supplied string
/// that was written with LF against a file that uses CRLF (and vice versa),
/// and preserves the file's own endings when writing back.
pub fn normalize_line_endings(s: &str, le: &str) -> String {
    // Collapse all line endings to LF first.
    let lf = s.replace("\r\n", "\n").replace('\r', "\n");
    if le == "\r\n" {
        lf.replace('\n', "\r\n")
    } else {
        lf
    }
}

/// Convert literal `\n` (the two characters backslash + n) to real newlines
/// when the string contains literal `\n` but no real newlines.
///
/// Safety net for model double-escaping: under long-session degradation the
/// model sometimes emits `\\n` (JSON double-escape) instead of `\n` (JSON
/// newline escape) in tool-call arguments. serde_json correctly parses `\\n`
/// to the two characters `\` + `n`, which pass through `normalize_line_endings`
/// untouched (it only handles real 0x0A/0x0D). In the regex replacement path
/// this is especially harmful: the `regex` crate treats `\n` in a replacement
/// `&str` as literal characters (not an escape), so the two bytes `\` + `n`
/// are inserted verbatim into the file.
///
/// The heuristic guard (only normalize when no real newlines are present)
/// preserves intentional literal `\n` in strings that already have real
/// newlines — e.g. a Rust source line `let s = "hello\nworld"` sent as part
/// of a multi-line `new_string` keeps its literal `\n` because the surrounding
/// lines provide real newlines. The common corruption case (the ENTIRE
/// `new_string` has literal `\n` instead of real newlines) is caught because
/// there are no real newlines to trigger the guard.
///
/// See BUG memory 9777844a for the full root-cause analysis.
pub fn denormalize_literal_newlines(s: &str) -> String {
    let has_real_newlines = s.contains('\n');
    let has_literal_newlines = s.contains("\\n");
    if has_literal_newlines && !has_real_newlines {
        s.replace("\\n", "\n")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_lf() {
        assert_eq!(detect_line_ending("a\nb\n"), "\n");
    }

    #[test]
    fn detect_crlf() {
        assert_eq!(detect_line_ending("a\r\nb\r\n"), "\r\n");
    }

    #[test]
    fn detect_mixed_treats_as_crlf() {
        // 1 CRLF line vs 1 bare-LF line → tie → CRLF (the tie-break
        // preserves the historical any-CRLF-wins rule).
        assert_eq!(detect_line_ending("a\r\nb\nc"), "\r\n");
    }

    #[test]
    fn detect_mixed_majority_lf() {
        // 3 bare-LF lines vs 1 CRLF line → LF wins: a stray CRLF must not
        // flip the whole file's style (backlog #48 trip-hazard).
        assert_eq!(detect_line_ending("a\r\nb\nc\nd\n"), "\n");
    }

    #[test]
    fn detect_mixed_majority_crlf() {
        // 3 CRLF lines vs 1 bare-LF line → CRLF wins.
        assert_eq!(detect_line_ending("a\r\nb\r\nc\r\nd\n"), "\r\n");
    }

    #[test]
    fn detect_no_newlines_is_lf() {
        // No line endings at all → LF (the historical no-CRLF default).
        assert_eq!(detect_line_ending("abc"), "\n");
    }

    #[test]
    fn normalize_lf_to_crlf() {
        assert_eq!(normalize_line_endings("a\nb\n", "\r\n"), "a\r\nb\r\n");
    }

    #[test]
    fn normalize_crlf_to_lf() {
        assert_eq!(normalize_line_endings("a\r\nb\r\n", "\n"), "a\nb\n");
    }

    #[test]
    fn normalize_lone_cr() {
        // Lone \r (old Mac) collapses to LF, then to CRLF if target is CRLF.
        assert_eq!(normalize_line_endings("a\rb\r", "\r\n"), "a\r\nb\r\n");
        assert_eq!(normalize_line_endings("a\rb\r", "\n"), "a\nb\n");
    }

    #[test]
    fn normalize_already_target_is_idempotent() {
        assert_eq!(normalize_line_endings("a\nb\n", "\n"), "a\nb\n");
        assert_eq!(normalize_line_endings("a\r\nb\r\n", "\r\n"), "a\r\nb\r\n");
    }

    #[test]
    fn denormalize_converts_literal_backslash_n_to_real_newlines() {
        // The common corruption case: the ENTIRE new_string has literal \n
        // (backslash + n) instead of real newlines. All are converted.
        assert_eq!(denormalize_literal_newlines("a\\nb\\nc"), "a\nb\nc");
    }

    #[test]
    fn denormalize_preserves_real_newlines() {
        // When real newlines are present, literal \n is left alone — it may
        // be intentional (e.g. inside a string literal in source code).
        assert_eq!(denormalize_literal_newlines("a\nb\\nc\n"), "a\nb\\nc\n");
    }

    #[test]
    fn denormalize_noop_without_literal_backslash_n() {
        // No literal \n → no-op (returns the same string).
        assert_eq!(denormalize_literal_newlines("a\nb\nc"), "a\nb\nc");
    }

    #[test]
    fn denormalize_handles_multiple_literal_newlines() {
        // Multiple literal \n are all converted.
        assert_eq!(denormalize_literal_newlines("x\\ny\\nz\\n"), "x\ny\nz\n");
    }

    // ---- detect_line_ending_path ----

    #[test]
    fn detect_path_crlf_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "line one\r\nline two\r\n").unwrap();
        assert_eq!(detect_line_ending_path(&p), Some("\r\n"));
    }

    #[test]
    fn detect_path_lf_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "line one\nline two\n").unwrap();
        assert_eq!(detect_line_ending_path(&p), Some("\n"));
    }

    #[test]
    fn detect_path_empty_file_is_none() {
        // An empty existing file has no style to preserve → None (write as-is),
        // consistent with a non-existent file.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::write(&p, "").unwrap();
        assert_eq!(detect_line_ending_path(&p), None);
    }

    #[test]
    fn detect_path_missing_file_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nope.txt");
        assert_eq!(detect_line_ending_path(&p), None);
    }

    #[test]
    fn detect_path_reads_only_a_prefix() {
        // A file larger than the 8 KB prefix whose CRLF only appears late would
        // be misdetected as LF — but line-ending style is consistent within a
        // file, so a CRLF file has CRLF in its first bytes. Verify a large CRLF
        // file is still detected as CRLF.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.txt");
        let line = "x".repeat(200) + "\r\n";
        let content = line.repeat(1000); // ~200 KB, CRLF throughout
        std::fs::write(&p, &content).unwrap();
        assert_eq!(detect_line_ending_path(&p), Some("\r\n"));
    }
}
