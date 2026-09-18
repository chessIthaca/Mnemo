// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `file_edit` — string-replace edits with diff-based approval.
//!
//! Takes an old/new string pair, applies the replacement, and the approval
//! prompt renders a unified diff (via the `similar` crate) so you see exactly
//! what changes before approving. A multi-edit batch (`edits`) applies
//! several old/new pairs atomically — in order, one write, one combined
//! diff; any failing item aborts with the file untouched.
//!
//! Hardening (backlog e8b39d72, completing 714196da's freshness contract):
//! every drift-class failure carries the fresh-read nudge
//! (`EDIT_STALE_READ_MARK`) and — per-agent — arms the gate that intercepts
//! the NEXT blind edit (any failure with zero reads is the contract, not
//! timeout-after-three). Emission-artifact validation rejects the observed
//! decay classes pre-write (markdown headings in code files, sentinel
//! whole-values, duplicated adjacent doc lines, a delimiter-balance
//! regression) with a re-emit/smaller-fragments message — deliberately not
//! the drift nudge. Large payloads (~700+ chars) carry an advisory
//! emission-fragility NOTE on success.

use std::path::Path;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;

use crate::agent::steering_stats::EDIT_STALE_READ_MARK;
use similar::TextDiff;

use crate::error::Result;
use crate::provider::{ApprovalPreview, ToolSchema};
use crate::tool::agent::line_endings::{
    denormalize_literal_newlines, detect_line_ending, normalize_line_endings,
};
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

/// Arguments for `file_edit`.
#[derive(Debug, Deserialize)]
pub struct FileEditArgs {
    pub path: String,
    /// The exact text to find (or a regex when `use_regex=true`). Required for
    /// string-matching mode; ignored when `start_line` + `end_line` are set
    /// (line-range mode). Defaults to empty so line-range calls can omit it.
    #[serde(default)]
    pub old_string: String,
    /// The replacement text. Defaults to empty so multi-edit batch calls
    /// (`edits`) can omit it — the items carry their own replacements. An
    /// empty `new_string` in single mode deletes `old_string`.
    #[serde(default)]
    pub new_string: String,
    #[serde(default)]
    pub replace_all: bool,
    /// Treat `old_string` as a Rust regular expression (default: false).
    /// Capture groups from the pattern can be referenced in `new_string`
    /// with `$1`, `$2`, … or `${name}` (regex-crate `Replacer` semantics).
    #[serde(default)]
    pub use_regex: bool,
    /// Replace at most N matches (vi-style `:s/…/…/` with a count). When
    /// `None`: `replace_all=false` replaces the first match only and
    /// `replace_all=true` replaces every match. When set, it takes precedence
    /// over `replace_all`.
    #[serde(default)]
    pub count: Option<usize>,
    /// 1-indexed inclusive start line for line-range edit mode. When both
    /// `start_line` and `end_line` are set, those lines are replaced by
    /// `new_string` — no `old_string` matching is needed (an alternative to
    /// string matching that avoids whitespace-mismatch failures). Mutually
    /// exclusive with `old_string`/`use_regex`/`replace_all`/`count`.
    #[serde(default)]
    pub start_line: Option<usize>,
    /// 1-indexed inclusive end line for line-range edit mode (see
    /// `start_line`). Both must be set together; setting only one is an error.
    #[serde(default)]
    pub end_line: Option<usize>,
    /// When true (literal mode only), locate `old_string` by normalizing
    /// whitespace — collapse runs of spaces/tabs to a single space and ignore
    /// trailing whitespace per line — so tab-vs-space or off-by-one-space
    /// mismatches still match. The replacement is spliced into the original
    /// content verbatim (surrounding whitespace preserved). Ignored in regex
    /// and line-range modes.
    #[serde(default)]
    pub fuzzy_whitespace: bool,
    /// Atomic multi-edit batch (plan be16ea36 step 4): every item is applied
    /// to the in-memory content IN ORDER and the result is written ONCE — any
    /// failing item aborts the whole batch with NO write (the file stays
    /// byte-identical). Each item's anchor must match exactly once in the
    /// content as left by the preceding items unless the item sets `count`.
    /// Mutually exclusive with the single-edit fields: leave `old_string`/
    /// `new_string` empty and do not set `use_regex`/`replace_all`/`count`/
    /// `start_line`/`end_line` (the batch-level `fuzzy_whitespace` is each
    /// item's default).
    #[serde(default)]
    pub edits: Option<Vec<EditItem>>,
    /// Append mode (plan be16ea36 step 5): when true, `new_string` is added
    /// at EOF instead of replacing a match — no `old_string` is needed. The
    /// appended text starts on a fresh line (prefixed by the file's detected
    /// line ending when the file doesn't already end with one) and is
    /// re-emitted in the file's detected style; an empty file takes the
    /// caller's text verbatim (no style to preserve). Mutually exclusive
    /// with `edits`, line-range mode, `old_string`, and the matching knobs
    /// (`use_regex`/`replace_all`/`count`/`fuzzy_whitespace`). Errors when
    /// the file is missing — use `file_write` to create it.
    #[serde(default)]
    pub append: bool,
}

/// One edit in a multi-edit batch (file_edit's `edits` array, plan be16ea36
/// step 4): replace `old_string` with `new_string` — `count` widens the match
/// to the first N occurrences (default 1; required when the anchor matches
/// more than once), `fuzzy_whitespace` enables whitespace-tolerant matching
/// for THIS item (default: the batch-level `fuzzy_whitespace`).
#[derive(Debug, Clone, Deserialize)]
pub struct EditItem {
    /// The exact text to find (EOL-agnostic matching, as in single mode).
    pub old_string: String,
    /// The replacement text.
    pub new_string: String,
    /// Replace at most N occurrences of this item's `old_string` (default 1).
    #[serde(default)]
    pub count: Option<usize>,
    /// Whitespace-tolerant matching for this item (default: the batch-level
    /// `fuzzy_whitespace`).
    #[serde(default)]
    pub fuzzy_whitespace: Option<bool>,
}

/// The `file_edit` tool.
pub struct FileEditTool {
    sandbox: Sandbox,
}

impl FileEditTool {
    pub fn new(sandbox: Sandbox) -> Self {
        Self { sandbox }
    }
}

/// The result of preparing an edit (before applying): the diff + new content.
#[derive(Debug, Clone)]
pub struct PreparedEdit {
    pub path: String,
    pub diff: String,
    pub new_content: String,
}

/// Compute the unified diff between old and new content.
pub fn compute_diff(path: &str, old: &str, new: &str) -> String {
    let diff = TextDiff::from_lines(old, new);
    let mut out = String::new();
    out.push_str(&format!("--- {path}\n+++ {path}\n"));
    for hunk in diff.unified_diff().context_radius(3).iter_hunks() {
        out.push_str(&hunk.to_string());
    }
    out
}

/// The fresh-read nudge appended to drift-class edit failures (backlog
/// 714196da): the content has drifted from the agent's last read, so the
/// error steers to a re-read instead of inviting a blind retry from memory.
/// The "Re-read the file" substring is the EditStaleRead steering marker
/// ([`crate::agent::steering_stats`]) — part of the output contract; the
/// dispatch funnel's stale-read gate counts and intercepts on it. The tail is
/// one line: the error text IS the result output's first line, the marker
/// surface.
fn with_fresh_read_nudge(msg: impl std::fmt::Display) -> crate::error::Error {
    crate::error::Error::NotFound(format!(
        "{msg} — the content has likely drifted from your last read. \
         {EDIT_STALE_READ_MARK} (read_files, this exact path) and retry with the \
         exact current text; do not edit without a fresh read."
    ))
}

/// Extensions treated as code files for the markdown-heading artifact check
/// (backlog e8b39d72 H2): a '## ' line start inside these is an emission
/// artifact, not a markdown heading. Markdown files (.md) are excluded by
/// design.
const ARTIFACT_CHECK_EXTENSIONS: [&str; 6] = ["rs", "ts", "tsx", "js", "json", "toml"];

/// H5 advisory threshold (backlog e8b39d72): the payload size at which
/// emission decay was observed in the 2026-12-20 incident (~700+ chars of
/// dense tokenized source in a single edit). At or above this, the success
/// result carries a fragility NOTE — advisory only, never a rejection.
const EMISSION_FRAGILITY_PAYLOAD_BYTES: usize = 700;

/// H5 advisory (backlog e8b39d72): when the edit payload is at/above
/// [`EMISSION_FRAGILITY_PAYLOAD_BYTES`], the success result carries a NOTE
/// steering toward smaller fragments / `file_write` full-file replacement.
/// Advisory only — the edit still applies.
fn large_payload_note(args: &FileEditArgs) -> Option<String> {
    let (o, n) = (args.old_string.len(), args.new_string.len());
    let larger = o.max(n);
    (larger >= EMISSION_FRAGILITY_PAYLOAD_BYTES).then(|| {
        format!(
            "NOTE: large edit payload (old_string ~{o} chars, new_string ~{n} chars) — \
             emission fragility has been observed near this size in long sessions; \
             use smaller fragments, or file_write for a full-file replacement \
             (backlog e8b39d72 H5)"
        )
    })
}

/// Backlog e8b39d72 H2: pre-write structural validation of a prepared edit —
/// rejects the emission-decay artifact classes observed in the 2026-12-20
/// incident (see .coding/knowledge/bug/2026-12-20-code-bearing-tool-payload-
/// emissions-degrade-unde.md) BEFORE the edit is applied:
///
/// - markdown-heading lines ('## ' at a line start) inside non-markdown code
///   files (Rust has no markdown headings — the '///' → '##' mangling
///   artifact),
/// - sentinel whole-values (the replacement being exactly 'unused' /
///   'placeholder' — the fragment-replacement artifact),
/// - adjacent byte-identical doc-comment lines ('///'/'//!' — the
///   duplication artifact),
/// - a delimiter-balance smoke-parse for `.rs` files (the truncation
///   artifact; the scoped lexer skips strings, raw strings, chars, and
///   comments so legit brace-bearing literals cannot false-positive).
///
/// These are EMISSION-ARTIFACT rejections, deliberately NOT the drift-class
/// stale-read failure ([`with_fresh_read_nudge`]/`EditStaleRead`): a re-read
/// cannot fix a corrupted emission, so the error steers to re-emit (smaller
/// fragments / `file_write`) instead.
fn validate_emission_artifacts(
    path: &str,
    content: &str,
    new_string: &str,
    new_content: &str,
) -> crate::error::Result<()> {
    validate_emission_artifacts_lines(path, new_string)?;
    validate_emission_artifacts_braces(path, content, new_content)
}

/// The line-scoped emission-artifact checks — (a) markdown-heading lines
/// inside non-markdown code files, (b) sentinel whole-values, (c) adjacent
/// byte-identical doc-comment lines — scoped to ONE replacement text. The
/// single paths run these on the one replacement; the batch path runs them
/// PER ITEM (review L2: the items' join would weaken the sentinel
/// exact-match — a longer join escapes it — and false-positive the
/// dup-doc-line check across item boundaries, whose boundary lines are
/// adjacent in the join but not in the spliced result).
fn validate_emission_artifacts_lines(path: &str, new_string: &str) -> crate::error::Result<()> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let code_file = ARTIFACT_CHECK_EXTENSIONS.contains(&ext.as_str());

    // (a) Markdown-heading lines inside non-markdown code files.
    if code_file {
        for (n, line) in new_string.lines().enumerate() {
            if line.starts_with("## ") {
                return Err(artifact_rejection(format!(
                    "markdown-heading line at new_string line {}: '{line}' — \
                     this is the '///' → '##' mangling artifact; this file type \
                     has no markdown headings",
                    n + 1
                )));
            }
        }
    }

    // (b) Sentinel whole-values: the trimmed replacement is EXACTLY one of
    // the observed placeholder tokens (the fragment-replacement artifact).
    let trimmed = new_string.trim();
    if trimmed == "unused" || trimmed == "placeholder" {
        return Err(artifact_rejection(format!(
            "sentinel-shaped replacement value '{trimmed}' — the \
             fragment-replacement artifact: a real replacement must carry code"
        )));
    }

    // (c) Adjacent byte-identical doc-comment lines (the duplication
    // artifact).
    let lines: Vec<&str> = new_string.lines().collect();
    for pair in lines.windows(2) {
        if pair[0] == pair[1]
            && !pair[0].trim().is_empty()
            && (pair[0].starts_with("///") || pair[0].starts_with("//!"))
        {
            return Err(artifact_rejection(format!(
                "duplicated adjacent doc line: '{}' appears twice back-to-back \
                 — the duplication artifact",
                pair[0]
            )));
        }
    }
    Ok(())
}

/// The delimiter-balance emission-artifact check (d) — delta-scoped to
/// (content, new_content), `.rs` only. The batch path runs it ONCE on the
/// combined result (the delta's right scope); truncation leaves unbalanced
/// braces, and a file that was unbalanced before the edit is not this
/// edit's fault.
fn validate_emission_artifacts_braces(
    path: &str,
    content: &str,
    new_content: &str,
) -> crate::error::Result<()> {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext == "rs" && rust_brace_deficit(new_content) > rust_brace_deficit(content) {
        return Err(artifact_rejection(
            "unbalanced delimiters after the edit (the brace/bracket/paren \
             deficit grew) — the truncation artifact: the payload was cut off",
        ));
    }
    Ok(())
}

/// The emission-artifact rejection error (backlog e8b39d72 H2): deliberately
/// NOT the drift-class stale-read failure — a re-read cannot fix a corrupted
/// emission, so the message steers to re-emit (smaller fragments / file_write
/// full-file replacement) instead, and is deliberately NOT counted by the
/// EditStaleRead steering marker.
fn artifact_rejection(detail: impl std::fmt::Display) -> crate::error::Error {
    crate::error::Error::InvalidInput(format!(
        "edit rejected: emission artifact detected ({detail}) — the payload \
         looks corrupted (model emission decay; see \
         .coding/knowledge/bug/2026-12-20-code-bearing-tool-payload-emissions-\
         degrade-unde.md). Do not blind-retry: re-emit carefully in SMALLER \
         fragments, or use file_write for full-file replacement. (This is not \
         a content-drift failure — a re-read will not help.)"
    ))
}

/// The net unbalance of braces/brackets/parens in `source` — a minimal Rust
/// lexer: skips `//` and (nestable) `/* */` comments, plain strings with
/// escapes, `r"..."`/`r#"..."#` raw strings (any `#` count), and
/// `'c'`/`'\\x'`/`'\\u{..}'` char literals; lifetimes/labels do not open a
/// char context. Returns the count of unclosed openers plus mismatched
/// closers (0 = balanced; `usize::MAX` = an unterminated raw string).
fn rust_brace_deficit(source: &str) -> usize {
    let bytes = source.as_bytes();
    let mut i = 0usize;
    let mut stack: Vec<u8> = Vec::new();
    let mut deficit = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'/' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if i + 1 < bytes.len() && bytes[i + 1] == b'*' => {
                let mut depth = 1usize;
                i += 2;
                while i < bytes.len() && depth > 0 {
                    if i + 1 < bytes.len() && bytes[i] == b'/' && bytes[i + 1] == b'*' {
                        depth += 1;
                        i += 2;
                    } else if i + 1 < bytes.len() && bytes[i] == b'*' && bytes[i + 1] == b'/' {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'\\' => i += 2,
                        b'"' => {
                            i += 1;
                            break;
                        }
                        _ => i += 1,
                    }
                }
            }
            b'\'' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'\'' {
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                } else if i + 2 < bytes.len() && bytes[i + 2] == b'\'' {
                    i += 3;
                } else {
                    i += 1;
                    while i < bytes.len()
                        && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                    {
                        i += 1;
                    }
                }
            }
            b'r'
                if i + 1 < bytes.len()
                    && (bytes[i + 1] == b'"' || bytes[i + 1] == b'#') =>
            {
                let mut hashes = 0usize;
                let mut j = i + 1;
                while j < bytes.len() && bytes[j] == b'#' {
                    hashes += 1;
                    j += 1;
                }
                if j < bytes.len() && bytes[j] == b'"' {
                    let mut closing = Vec::with_capacity(hashes + 1);
                    closing.push(b'"');
                    closing.resize(closing.len() + hashes, b'#');
                    match source[j + 1..].find(std::str::from_utf8(&closing).expect("ascii")) {
                        Some(pos) => i = j + 1 + pos + closing.len(),
                        None => return usize::MAX,
                    }
                } else {
                    i += 1;
                    while i < bytes.len()
                        && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                    {
                        i += 1;
                    }
                }
            }
            b'{' | b'[' | b'(' => {
                stack.push(bytes[i]);
                i += 1;
            }
            b'}' | b']' | b')' => {
                let open = match bytes[i] {
                    b'}' => b'{',
                    b']' => b'[',
                    _ => b'(',
                };
                match stack.pop() {
                    Some(c) if c == open => {}
                    _ => deficit += 1,
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    deficit + stack.len()
}

/// Apply the edit to the file content, returning the new content + diff.
/// This does NOT write to disk — it's used to prepare the approval preview.
///
/// Dispatches to the multi-edit batch, line-range, regex, or literal
/// implementation depending on the args. The batch (`edits`) takes
/// precedence (it is mutually exclusive with the single-edit fields);
/// line-range mode (`start_line` + `end_line`) requires no `old_string`;
/// otherwise the `use_regex`/literal path runs.
pub fn prepare_edit(args: &FileEditArgs, content: &str) -> Result<PreparedEdit> {
    if args.edits.is_some() {
        prepare_edit_batch(args, content)
    } else if args.append {
        prepare_edit_append(args, content)
    } else if args.start_line.is_some() || args.end_line.is_some() {
        prepare_edit_lines(args, content)
    } else {
        // String-matching mode (literal or regex) requires a non-empty
        // old_string. An empty needle would insert new_string between every
        // character of the file (Rust's `str::replace` / `replacen` with an
        // empty pattern matches at every position) — silent corruption. The
        // line-range path above doesn't use old_string, so it's exempt.
        if args.old_string.is_empty() {
            return Err(crate::error::Error::InvalidInput(
                "old_string is empty — provide old_string (string matching) or \
                 start_line + end_line (line range)"
                    .into(),
            ));
        }
        if args.use_regex {
            prepare_edit_regex(args, content)
        } else {
            prepare_edit_literal(args, content)
        }
    }
}

/// Line-range edit path: replace lines `[start_line, end_line]` (1-indexed,
/// inclusive) with `new_string`. No `old_string` matching is needed — the
/// caller identifies the region by line number (as shown by `file_read`'s
/// 1-indexed line prefixes), so whitespace/indentation mismatch cannot cause
/// a failure. The file's line-ending style is preserved: `new_string` is
/// normalized to the file's detected style before insertion.
fn prepare_edit_lines(args: &FileEditArgs, content: &str) -> Result<PreparedEdit> {
    // Both bounds must be set together — setting only one is a caller error.
    let start_line = match (args.start_line, args.end_line) {
        (Some(s), Some(e)) => (s, e),
        _ => {
            return Err(crate::error::Error::InvalidInput(
                "start_line and end_line must both be set (line-range mode). Expected \
                 arguments: {\"path\": \"<file>\", \"start_line\": <1-indexed N>, \
                 \"end_line\": <1-indexed M>, \"new_string\": \"<replacement>\"} — \
                 or use string mode (old_string + new_string) instead".into(),
            ));
        }
    };
    let (start, end) = start_line;

    // 1-indexed: start_line must be >= 1, and end_line >= start_line.
    if start == 0 {
        return Err(crate::error::Error::InvalidInput(
            "start_line must be >= 1 (1-indexed)".into(),
        ));
    }
    if end < start {
        return Err(crate::error::Error::InvalidInput(format!(
            "end_line ({end}) must be >= start_line ({start})"
        )));
    }

    // Split into lines. Rust's `str::lines()` splits on \n (dropping the
    // terminator) and does NOT yield a trailing empty element for a trailing
    // newline, so "a\nb\nc\n" → ["a","b","c"] (3 lines).
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Err(with_fresh_read_nudge("no lines to replace (file is empty)"));
    }

    // Convert to 0-indexed. Clamp end to the file's length so an over-large
    // end_line replaces through the last line rather than erroring (mirrors
    // how an editor behaves when you select past EOF).
    let start_idx = start - 1;
    let end_idx = (end - 1).min(lines.len() - 1);
    if start_idx >= lines.len() {
        return Err(with_fresh_read_nudge(format!(
            "start_line {start} is past the end of the file ({} lines)",
            lines.len()
        )));
    }

    // Normalize new_string to the file's line-ending style so CRLF is preserved
    // (mirrors the literal path's handling).
    let le = detect_line_ending(content);
    let new_string = denormalize_literal_newlines(&normalize_line_endings(
        &args.new_string, le,
    ));

    // Rebuild as parts joined by the file's LE: the prefix lines, the
    // replacement (new_string, inserted verbatim), then the suffix lines.
    // This preserves multi-line new_string and the file's line-ending style.
    let mut parts: Vec<String> = Vec::with_capacity(lines.len() + 1);
    for line in &lines[..start_idx] {
        parts.push((*line).to_string());
    }
    parts.push(new_string.clone());
    for line in &lines[end_idx + 1..] {
        parts.push((*line).to_string());
    }
    let mut new_content = parts.join(le);

    // Preserve the original's trailing newline (if any). `lines()` drops the
    // terminator, so a file "a\nb\n" → ["a","b"] and join gives "a\nb" — we
    // re-append the trailing LE so the file's shape is preserved. CRLF files
    // end with '\n' too (the LF half of "\r\n"), so this check covers both.
    if content.ends_with('\n') {
        new_content.push_str(le);
    }

    if new_content == content {
        return Err(crate::error::Error::InvalidInput(
            "line-range edit produced no change (new_string equals the replaced lines)".into(),
        ));
    }

    validate_emission_artifacts(&args.path, content, &new_string, &new_content)?;
    let diff = compute_diff(&args.path, content, &new_content);
    Ok(PreparedEdit {
        path: args.path.clone(),
        diff,
        new_content,
    })
}

/// Normalize whitespace for fuzzy matching: strip trailing whitespace from
/// each line and collapse runs of ASCII whitespace (space/tab) within a line
/// to a single space. Newlines are preserved (line-ending normalization
/// already happened upstream via `normalize_line_endings`). This lets a
/// caller's `old_string` match despite tab-vs-space or extra-space
/// differences — the *match* is fuzzy, but the replacement is spliced into
/// the original content verbatim.
///
/// Returns the normalized string AND, for each byte of the normalized string,
/// the byte offset it came from in the original (used to map a normalized
/// match range back to original byte offsets for the verbatim splice). For a
/// collapsed whitespace run, the emitted space is tagged with the offset of
/// the *first* byte of the run.
fn normalize_ws_with_offsets(s: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(s.len());
    let mut offsets: Vec<usize> = Vec::with_capacity(s.len());
    let mut abs = 0usize; // running absolute byte offset in `s`
    for line in s.split_inclusive('\n') {
        let line_len = line.len();
        let (body, has_nl) = match line.strip_suffix('\n') {
            Some(b) => (b, true),
            None => (line, false),
        };
        let body_start = abs;
        // Collapse runs of ASCII whitespace within the line to a single space.
        let mut in_ws = false;
        for (rel_off, ch) in body.char_indices() {
            let byte_off = body_start + rel_off;
            if ch == ' ' || ch == '\t' {
                if !in_ws {
                    out.push(' ');
                    offsets.push(byte_off);
                    in_ws = true;
                }
                // else: part of the run — skip (not emitted).
            } else {
                // Push one offset entry PER BYTE of the emitted char, so the
                // offset map stays aligned with `out` at the byte level (a
                // multi-byte char like é contributes multiple bytes to `out`
                // but is one char — without this, the map would be too short
                // and byte-index lookups would misalign).
                out.push(ch);
                for _ in 0..ch.len_utf8() {
                    offsets.push(byte_off);
                }
                in_ws = false;
            }
        }
        // Strip trailing whitespace: if the last emitted char is a space (from
        // a collapse), remove it and its offset.
        if out.ends_with(' ') {
            out.pop();
            offsets.pop();
        }
        // Emit the newline (if any), tagged with its absolute offset.
        if has_nl {
            let nl_off = body_start + body.len();
            out.push('\n');
            offsets.push(nl_off);
        }
        abs += line_len;
    }
    (out, offsets)
}

/// Find `needle` in `haystack` using whitespace-normalized comparison, and
/// return the byte range `(start, end)` of the match in the **original**
/// `haystack` (not the normalized one). This lets us locate a match despite
/// whitespace differences while splicing the replacement into the original
/// content (preserving the file's exact surrounding whitespace).
///
/// Returns `None` if the normalized needle is not found. The first match is
/// returned; callers loop for `count`/`replace_all`.
///
/// The match region covers the original bytes that the normalized needle
/// "represents": internal whitespace (e.g. a tab between two words) is
/// included (it was collapsed to a space in the normalized view), but
/// trailing whitespace stripped during normalization is NOT included — it
/// stays in the file, untouched, outside the splice.
fn fuzzy_find(haystack: &str, needle: &str) -> Option<(usize, usize)> {
    let (norm_hay, hay_offsets) = normalize_ws_with_offsets(haystack);
    let (norm_needle, _) = normalize_ws_with_offsets(needle);
    if norm_needle.is_empty() {
        return None;
    }
    let n_start = norm_hay.find(&norm_needle)?;
    let n_end = n_start + norm_needle.len();

    // Map the normalized match range back to original byte offsets. The start
    // is the original offset of the first matched normalized byte.
    let orig_start = *hay_offsets.get(n_start)?;

    // The end is the byte immediately AFTER the last matched normalized
    // byte's original char. We use the last match byte (n_end-1), NOT the next
    // normalized byte (n_end), because trailing whitespace stripped during
    // normalization sits between them — using n_end would wrongly consume it.
    // The offset map is byte-aligned (one entry per normalized byte, with
    // multi-byte chars repeating their offset), so hay_offsets[n_end-1] is the
    // original offset of the last matched byte; adding that char's UTF-8 length
    // gives the exclusive end bound.
    let last_orig_off = *hay_offsets.get(n_end - 1)?;
    let char_len = haystack[last_orig_off..]
        .chars()
        .next()
        .map_or(1, |c| c.len_utf8());
    let orig_end = last_orig_off + char_len;
    Some((orig_start, orig_end))
}

/// The literal edit's matching+splicing core, shared by the single-edit path
/// and the multi-edit batch (plan be16ea36 step 4): EOL-agnostic matching on
/// BOTH sides (the content is projected to LF with a byte-offset map back to
/// the original; the needle is normalized to LF), then `new_string` —
/// re-emitted in the file's detected (majority) ending style — is spliced
/// into the ORIGINAL bytes at the mapped offsets, at most `limit`
/// non-overlapping matches (fuzzy-whitespace matching when `fuzzy`).
/// Returns `None` when the anchor matches nothing. Deliberately NO emission
/// validation and NO diff here: the single path adds both for its one edit,
/// while the batch path validates the COMBINED result once (a legitimate
/// two-step batch can be temporarily unbalanced mid-batch) and emits one
/// combined diff.
fn literal_splice(
    content: &str,
    old_string: &str,
    new_string: &str,
    fuzzy: bool,
    limit: usize,
) -> Result<Option<String>> {
    let le = detect_line_ending(content);
    let new_string = denormalize_literal_newlines(&normalize_line_endings(new_string, le));
    // Matching happens in LF space on BOTH sides — an ending-style difference
    // between the needle and the file can never cause a miss.
    let old_lf = normalize_line_endings(old_string, "\n");
    let new_lf = normalize_line_endings(&new_string, "\n");

    if old_lf == new_lf {
        return Err(crate::error::Error::InvalidInput(
            "old_string and new_string are identical".into(),
        ));
    }
    // In fuzzy mode, also reject when old/new differ only in whitespace —
    // they'd normalize to the same string and the edit would be a no-op
    // (or, worse, replace a region with whitespace-equivalent text).
    if fuzzy && normalize_ws_with_offsets(&old_lf).0 == normalize_ws_with_offsets(&new_lf).0 {
        return Err(crate::error::Error::InvalidInput(
            "old_string and new_string are identical after whitespace normalization".into(),
        ));
    }

    // LF projection of the content + a byte-offset map back into the
    // original: matches are found in EOL-agnostic space, then spliced into
    // the original bytes so untouched regions keep their exact endings.
    let (content_lf, lf_offsets) = normalize_lf_with_offsets(content);
    let mut out = String::with_capacity(content.len());
    let mut lf_pos = 0usize; // cursor in the LF projection
    let mut orig_pos = 0usize; // cursor in the original content
    let mut replaced = 0usize;
    while replaced < limit {
        let found = if fuzzy {
            fuzzy_find(&content_lf[lf_pos..], &old_lf).map(|(s, e)| (lf_pos + s, lf_pos + e))
        } else {
            content_lf[lf_pos..]
                .find(&old_lf)
                .map(|i| (lf_pos + i, lf_pos + i + old_lf.len()))
        };
        let Some((m_start, m_end)) = found else {
            break;
        };
        let (o_start, o_end) = map_lf_region_to_original(&lf_offsets, content, m_start, m_end);
        out.push_str(&content[orig_pos..o_start]);
        out.push_str(&new_string);
        orig_pos = o_end;
        lf_pos = m_end;
        replaced += 1;
    }
    if replaced == 0 {
        return Ok(None);
    }
    out.push_str(&content[orig_pos..]);
    Ok(Some(out))
}

/// Literal (exact-string) edit path — EOL-agnostic matching (plan be16ea36
/// step 2): the file content AND the caller's `old_string` are normalized to
/// LF for matching, so a needle matches regardless of which ending style each
/// of its lines carries — a mixed-ending file whose needle spans both styles
/// no longer false-drifts on verified-identical text, and `replace_all`
/// reaches every occurrence instead of only detected-style ones. The
/// replacement is spliced into the ORIGINAL content at the mapped byte
/// offsets (untouched bytes keep their exact endings), with `new_string`
/// re-emitted in the file's detected (majority) ending style.
fn prepare_edit_literal(args: &FileEditArgs, content: &str) -> Result<PreparedEdit> {
    let limit = if args.replace_all {
        usize::MAX
    } else {
        args.count.unwrap_or(1).max(1)
    };
    let new_content = match literal_splice(
        content,
        &args.old_string,
        &args.new_string,
        args.fuzzy_whitespace,
        limit,
    )? {
        Some(nc) => nc,
        None => {
            return Err(with_fresh_read_nudge(if args.fuzzy_whitespace {
                "old_string not found in file (fuzzy_whitespace)"
            } else {
                "old_string not found in file"
            }))
        }
    };

    if new_content == content {
        return Err(crate::error::Error::NotFound(
            "old_string not found in file".into(),
        ));
    }
    // The spliced replacement, re-emitted in the file's detected style —
    // recomputed here for the emission validator (literal_splice owns the
    // splice; the validator wants the same replacement text).
    let le = detect_line_ending(content);
    let new_string = denormalize_literal_newlines(&normalize_line_endings(&args.new_string, le));
    validate_emission_artifacts(&args.path, content, &new_string, &new_content)?;
    let diff = compute_diff(&args.path, content, &new_content);
    Ok(PreparedEdit {
        path: args.path.clone(),
        diff,
        new_content,
    })
}

/// Multi-edit batch path (plan be16ea36 step 4): apply every item to the
/// in-memory content IN ORDER and return ONE PreparedEdit for the combined
/// result — the caller writes once, so any failing item aborts the whole
/// batch with the file untouched on disk (atomicity). Each item's anchor
/// must match exactly once in the content as left by the preceding items
/// unless the item sets `count` (which widens the match to the first N
/// occurrences) — an ambiguous anchor without a count is rejected. The
/// error names the item's 1-based index and the first line of its
/// old_string. Emission artifacts are validated ONCE on the combined
/// result (a legitimate two-step batch can be temporarily unbalanced
/// mid-batch); the diff is one combined diff.
fn prepare_edit_batch(args: &FileEditArgs, content: &str) -> Result<PreparedEdit> {
    let items = validate_batch_args(args)?;
    let mut current = content.to_string();
    for (idx, item) in items.iter().enumerate() {
        let fuzzy = item.fuzzy_whitespace.unwrap_or(args.fuzzy_whitespace);
        // Ambiguity guard: without an explicit count the anchor must match
        // exactly once in the content as left by the preceding items.
        if item.count.is_none() {
            let occurrences = count_matches(&current, &item.old_string, fuzzy);
            if occurrences > 1 {
                return Err(crate::error::Error::InvalidInput(format!(
                    "edit {}/{} is ambiguous: its old_string ('{}') matches \
                     {occurrences} times — pass count on the item to replace \
                     more than one occurrence",
                    idx + 1,
                    items.len(),
                    anchor_excerpt(&item.old_string)
                )));
            }
        }
        let limit = item.count.unwrap_or(1).max(1);
        match literal_splice(&current, &item.old_string, &item.new_string, fuzzy, limit)? {
            Some(next) if next != current => current = next,
            Some(_) => {
                return Err(batch_item_error(
                    idx,
                    items.len(),
                    &item.old_string,
                    crate::error::Error::NotFound(
                        "the item produced no change (new_string equals the matched \
                         region after line-ending normalization)"
                            .into(),
                    ),
                ))
            }
            None => {
                return Err(batch_item_error(
                    idx,
                    items.len(),
                    &item.old_string,
                    with_fresh_read_nudge("old_string not found in file"),
                ))
            }
        }
    }
    // Per-item line-scoped emission checks (review L2): the items' join
    // would weaken the sentinel exact-match (a longer join escapes it) and
    // false-positive the dup-doc-line check across item boundaries (two
    // items' boundary lines are adjacent in the join but not in the spliced
    // result).
    for item in items {
        validate_emission_artifacts_lines(&args.path, &item.new_string)?;
    }
    // One combined brace-delta check for the whole batch (delta-scoped, so
    // the combined result is the right scope).
    validate_emission_artifacts_braces(&args.path, content, &current)?;
    let diff = compute_diff(&args.path, content, &current);
    Ok(PreparedEdit {
        path: args.path.clone(),
        diff,
        new_content: current,
    })
}

/// Append mode (plan be16ea36 step 5): `new_string` is added at EOF — no
/// anchor matching. The appended text starts on a fresh line: when the file
/// has content and doesn't already end with a line ending, the file's
/// detected line ending is prefixed; a file that already ends with a line
/// ending (or is empty) gets no extra prefix, so no spurious blank line.
/// The appended text is re-emitted in the file's detected style (an empty
/// file takes the caller's text verbatim — there is no style to preserve,
/// matching `detect_line_ending_path`'s None convention). The caller must
/// have left `old_string` and the matching knobs unset (validated here);
/// execute() steers a missing file to `file_write`.
fn prepare_edit_append(args: &FileEditArgs, content: &str) -> Result<PreparedEdit> {
    if args.edits.is_some() {
        return Err(crate::error::Error::InvalidInput(
            "append is mutually exclusive with edits".into(),
        ));
    }
    if args.start_line.is_some() || args.end_line.is_some() {
        return Err(crate::error::Error::InvalidInput(
            "append is mutually exclusive with line-range mode (start_line/end_line)"
                .into(),
        ));
    }
    if !args.old_string.is_empty() {
        return Err(crate::error::Error::InvalidInput(
            "append is mutually exclusive with old_string — there is nothing to \
             match in append mode"
                .into(),
        ));
    }
    if args.use_regex {
        return Err(crate::error::Error::InvalidInput(
            "append is mutually exclusive with use_regex — there is nothing to \
             match in append mode"
                .into(),
        ));
    }
    if args.replace_all {
        return Err(crate::error::Error::InvalidInput(
            "append is mutually exclusive with replace_all — there is nothing to \
             match in append mode"
                .into(),
        ));
    }
    if args.count.is_some() {
        return Err(crate::error::Error::InvalidInput(
            "append is mutually exclusive with count — there is nothing to match \
             in append mode"
                .into(),
        ));
    }
    if args.fuzzy_whitespace {
        return Err(crate::error::Error::InvalidInput(
            "append is mutually exclusive with fuzzy_whitespace — there is \
             nothing to match in append mode"
                .into(),
        ));
    }
    if args.new_string.is_empty() {
        return Err(crate::error::Error::InvalidInput(
            "new_string is empty — there is nothing to append".into(),
        ));
    }

    let le = detect_line_ending(content);
    let appended = if content.is_empty() {
        // Empty file: no style to preserve — write the caller's text
        // verbatim (consistent with detect_line_ending_path's None).
        args.new_string.clone()
    } else {
        let text = denormalize_literal_newlines(&normalize_line_endings(&args.new_string, le));
        // Start on a fresh line unless the file already ends with one
        // (both "\n" and "\r\n" end with '\n').
        if content.ends_with('\n') {
            text
        } else {
            format!("{le}{text}")
        }
    };
    let mut new_content = String::with_capacity(content.len() + appended.len());
    new_content.push_str(content);
    new_content.push_str(&appended);

    validate_emission_artifacts(&args.path, content, &appended, &new_content)?;
    let diff = compute_diff(&args.path, content, &new_content);
    Ok(PreparedEdit {
        path: args.path.clone(),
        diff,
        new_content,
    })
}

/// The batch mode's argument validation (plan be16ea36 step 4): `edits` is
/// mutually exclusive with every single-edit field — the items carry their
/// own anchors — and must be non-empty. Returns the items on success.
fn validate_batch_args(args: &FileEditArgs) -> Result<&[EditItem]> {
    let items = args
        .edits
        .as_deref()
        .ok_or_else(|| crate::error::Error::InvalidInput("edits is required".into()))?;
    if items.is_empty() {
        return Err(crate::error::Error::InvalidInput(
            "edits is empty — provide at least one item".into(),
        ));
    }
    if !args.old_string.is_empty() {
        return Err(crate::error::Error::InvalidInput(
            "edits is mutually exclusive with old_string — the items carry their own \
             old_string"
                .into(),
        ));
    }
    if !args.new_string.is_empty() {
        return Err(crate::error::Error::InvalidInput(
            "edits is mutually exclusive with new_string — the items carry their own \
             new_string"
                .into(),
        ));
    }
    if args.use_regex {
        return Err(crate::error::Error::InvalidInput(
            "edits is mutually exclusive with use_regex — batch items are literal \
             matches"
                .into(),
        ));
    }
    if args.replace_all {
        return Err(crate::error::Error::InvalidInput(
            "edits is mutually exclusive with replace_all — set count on the items \
             instead"
                .into(),
        ));
    }
    if args.count.is_some() {
        return Err(crate::error::Error::InvalidInput(
            "edits is mutually exclusive with count — set it on the items instead"
                .into(),
        ));
    }
    if args.start_line.is_some() || args.end_line.is_some() {
        return Err(crate::error::Error::InvalidInput(
            "edits is mutually exclusive with line-range mode (start_line/end_line)"
                .into(),
        ));
    }
    if args.append {
        return Err(crate::error::Error::InvalidInput(
            "edits is mutually exclusive with append — an append has no anchors"
                .into(),
        ));
    }
    // Every item needs a non-empty anchor (review H1): an empty old_string
    // would match everywhere (the empty-needle hazard the single mode
    // rejects) and used to reach a subtraction underflow in the EOL-agnostic
    // core — reject it here with a clean error naming the item.
    for (idx, item) in items.iter().enumerate() {
        if item.old_string.is_empty() {
            return Err(crate::error::Error::InvalidInput(format!(
                "edit {}/{} has an empty old_string — every batch item needs an \
                 anchor (an empty anchor matches everywhere and would corrupt \
                 the file)",
                idx + 1,
                items.len()
            )));
        }
    }
    Ok(items)
}

/// Count non-overlapping matches of `needle` in `content` using the same
/// EOL-agnostic (and optionally fuzzy-whitespace) matching as the literal
/// splice — the batch ambiguity guard's counter (plan be16ea36 step 4).
fn count_matches(content: &str, needle: &str, fuzzy: bool) -> usize {
    let (content_lf, _) = normalize_lf_with_offsets(content);
    let needle_lf = normalize_line_endings(needle, "\n");
    if needle_lf.is_empty() {
        return 0;
    }
    let mut count = 0usize;
    let mut pos = 0usize;
    while pos <= content_lf.len() {
        let found = if fuzzy {
            fuzzy_find(&content_lf[pos..], &needle_lf).map(|(s, e)| (pos + s, pos + e))
        } else {
            content_lf[pos..]
                .find(&needle_lf)
                .map(|i| (pos + i, pos + i + needle_lf.len()))
        };
        match found {
            Some((_, end)) if end > pos => {
                count += 1;
                pos = end;
            }
            _ => break,
        }
    }
    count
}

/// A batch item's failure (plan be16ea36 step 4): names the item's 1-based
/// index, the batch size, and the anchor excerpt, preserving the source
/// error's class (NotFound stays drift-class so the stale-read gate arms;
/// everything else is a caller error).
fn batch_item_error(
    index: usize,
    total: usize,
    old_string: &str,
    source: crate::error::Error,
) -> crate::error::Error {
    let prefix = format!(
        "edit {}/{} failed (anchor: '{}')",
        index + 1,
        total,
        anchor_excerpt(old_string)
    );
    match source {
        crate::error::Error::NotFound(msg) => {
            crate::error::Error::NotFound(format!("{prefix}: {msg}"))
        }
        other => crate::error::Error::InvalidInput(format!("{prefix}: {other}")),
    }
}

/// The anchor excerpt for batch error messages: the first line of the item's
/// old_string, truncated to keep the error readable (plan be16ea36 step 4 —
/// the error names the anchor index and the first line of its old_string).
fn anchor_excerpt(old_string: &str) -> String {
    let first = old_string.lines().next().unwrap_or("");
    const MAX: usize = 100;
    if first.chars().count() <= MAX {
        first.to_string()
    } else {
        let truncated: String = first.chars().take(MAX).collect();
        format!("{truncated}…")
    }
}

/// Project `s` to LF-only line endings, returning the projection plus, for
/// each byte of the projection, the byte offset in `s` it came from (one
/// entry per byte, so byte-index lookups stay aligned; a multi-byte char
/// repeats its offset). A "\r\n" pair projects to one '\n' tagged with the
/// '\n' byte's offset (the '\r' has no image); a lone '\r' projects to '\n'
/// tagged with its own offset. This is the matching substrate for the
/// EOL-agnostic literal path: an LF-normalized needle matches the projection
/// regardless of the file's per-line ending style.
fn normalize_lf_with_offsets(s: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(s.len());
    let mut offsets: Vec<usize> = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\r' if i + 1 < bytes.len() && bytes[i + 1] == b'\n' => {
                out.push('\n');
                offsets.push(i + 1);
                i += 2;
            }
            b'\r' => {
                out.push('\n');
                offsets.push(i);
                i += 1;
            }
            _ => {
                let ch = s[i..].chars().next().expect("i is a char boundary");
                out.push(ch);
                for _ in 0..ch.len_utf8() {
                    offsets.push(i);
                }
                i += ch.len_utf8();
            }
        }
    }
    (out, offsets)
}

/// Map a match region `[m_start, m_end)` in the LF projection back to byte
/// offsets in the original content. The start extends back across the '\r'
/// when the match opens at the '\n' of a "\r\n" pair (the needle's leading
/// line ending represents the whole pair); the end is exclusive, one past
/// the original char carrying the last matched byte.
fn map_lf_region_to_original(
    lf_offsets: &[usize],
    content: &str,
    m_start: usize,
    m_end: usize,
) -> (usize, usize) {
    // Defensive (review H1): an empty match region has no bytes to map —
    // the callers reject empty anchors upstream; clamp to the map's bounds
    // instead of underflowing on `m_end - 1`.
    if m_end == 0 {
        let pos = lf_offsets.get(m_start).copied().unwrap_or(content.len());
        return (pos, pos);
    }
    let bytes = content.as_bytes();
    let mut o_start = lf_offsets[m_start];
    if bytes[o_start] == b'\n' && o_start > 0 && bytes[o_start - 1] == b'\r' {
        o_start -= 1;
    }
    let last = lf_offsets[m_end - 1];
    let char_len = content[last..].chars().next().map_or(1, |c| c.len_utf8());
    (o_start, last + char_len)
}

/// Regex edit path: `old_string` is a Rust regex; `new_string` may reference
/// capture groups (`$1`, `${name}`). Line endings of the pattern and
/// replacement are normalized to the file's style so `\n` in a pattern still
/// matches CRLF files. When a multi-line pattern fails to match, the wrapper
/// retries once with the flipped ending (mixed-ending files, backlog #48).
fn prepare_edit_regex(args: &FileEditArgs, content: &str) -> Result<PreparedEdit> {
    let le = detect_line_ending(content);
    match prepare_edit_regex_with_le(args, content, le) {
        Err(crate::error::Error::NotFound(_)) if args.old_string.contains('\n') => {
            let alt = if le == "\n" { "\r\n" } else { "\n" };
            prepare_edit_regex_with_le(args, content, alt)
        }
        other => other,
    }
}

/// The regex edit body for ONE line-ending style `le`.
fn prepare_edit_regex_with_le(
    args: &FileEditArgs,
    content: &str,
    le: &str,
) -> Result<PreparedEdit> {
    let pattern = normalize_line_endings(&args.old_string, le);
    let replacement = denormalize_literal_newlines(&normalize_line_endings(
        &args.new_string, le,
    ));

    // Shared compile helper with allow_fallback=false: a mutation tool must
    // never silently change match semantics — a broken regex is a hard error
    // that points at literal mode instead (context names the offending
    // argument, review B2).
    let re = crate::tool::agent::pattern::compile_with_fallback(&pattern, false, false)
        .map_err(|e| crate::error::Error::InvalidInput(format!("in old_string: {e}")))?
        .regex;

    let limit = match args.count {
        Some(n) => n.max(1),
        None if args.replace_all => 0, // 0 = unlimited for `replacen`
        None => 1,
    };
    let new_content = re
        .replacen(content, limit, replacement.as_str())
        .into_owned();

    if new_content == content {
        return Err(with_fresh_read_nudge(
            "regex old_string matched nothing (or produced no change)",
        ));
    }
    validate_emission_artifacts(&args.path, content, &replacement, &new_content)?;
    let diff = compute_diff(&args.path, content, &new_content);
    Ok(PreparedEdit {
        path: args.path.clone(),
        diff,
        new_content,
    })
}

#[async_trait]
impl Tool for FileEditTool {
    fn name(&self) -> &str {
        "file_edit"
    }

    fn category(&self) -> ToolCategory {
        ToolCategory::Agent
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema::new(
            "file_edit",
            "Replace old_string with new_string in a file; generates a unified diff for \
             approval. Read the file first (read_files) — old_string must match the file's \
             exact content or the edit fails. Six modes: LITERAL (default, first \
             occurrence), REGEX (use_regex), LINE-RANGE (start_line + end_line — no \
             old_string needed, so whitespace mismatches cannot bite), FUZZY \
             (fuzzy_whitespace, literal only), BATCH (edits — an atomic multi-edit \
             array applied in order with ONE write; any failing item aborts the whole \
             batch with the file untouched on disk), and APPEND (append — add \
             new_string at EOF on a fresh line; no old_string needed; the file must \
             exist — use file_write to create it). BATCH and APPEND are mutually \
             exclusive with every single-edit field. replace_all / count=N widen a \
             literal or regex match. An invalid regex is rejected with an actionable \
             error — match semantics never change silently before an edit.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the file, relative to the project root."},
                    "old_string": {"type": "string", "description": "The exact text to find, or a regex when use_regex=true."},
                    "new_string": {"type": "string", "description": "The replacement text; supports $1/${name} capture refs when use_regex=true. Required in single mode (empty deletes old_string); omit in batch mode — the edits items carry their own replacements."},
                    "replace_all": {"type": "boolean", "description": "Replace all occurrences (default: false)."},
                    "use_regex": {"type": "boolean", "description": "Treat old_string as a Rust regex (default: false). In the replacement (new_string), use real newlines — a literal backslash-n is inserted as-is, not converted to a newline."},
                    "count": {"type": "integer", "description": "Replace at most N matches (vi-style count); overrides replace_all."},
                    "start_line": {"type": "integer", "description": "1-indexed inclusive start line; use the numbers shown by file_read. Set with end_line to replace that range with new_string — old_string, replace_all, use_regex, count and fuzzy_whitespace are all ignored in this mode. Both must be set together."},
                    "end_line": {"type": "integer", "description": "1-indexed inclusive end line (see start_line); clamped to the file length."},
                    "fuzzy_whitespace": {"type": "boolean", "description": "Literal mode only: locate old_string with whitespace normalized — runs of spaces/tabs collapse to one space, trailing whitespace ignored — catching tab-vs-space and off-by-one mismatches. The replacement is spliced in verbatim (default: false)."},
                    "edits": {"type": "array", "description": "Atomic multi-edit batch: every item is applied IN ORDER and the result is written ONCE — any failing item aborts the whole batch with the file untouched. Each item: {old_string, new_string, count?, fuzzy_whitespace?}; an item's anchor must match exactly once in the content as left by the preceding items unless it sets count. Mutually exclusive with old_string/new_string/use_regex/replace_all/count/start_line/end_line.", "items": {"type": "object", "properties": {"old_string": {"type": "string", "description": "The exact text to find (EOL-agnostic matching)."}, "new_string": {"type": "string", "description": "The replacement text."}, "count": {"type": "integer", "description": "Replace at most N occurrences of this item's old_string (default 1)."}, "fuzzy_whitespace": {"type": "boolean", "description": "Whitespace-tolerant matching for this item (default: the batch-level fuzzy_whitespace)."}}, "required": ["old_string", "new_string"]}},
                    "append": {"type": "boolean", "description": "Append mode: add new_string at EOF (on a fresh line, in the file's detected line-ending style) instead of replacing — no old_string needed. Mutually exclusive with edits/line-range/old_string and the matching knobs. Errors when the file is missing — use file_write to create it (default: false)."}
                },
                "required": ["path"]
            }),
        )
    }

    fn safety(&self) -> SafetyLevel {
        SafetyLevel::NeedsApproval
    }

    fn approval_preview(&self, args: &serde_json::Value) -> Option<ApprovalPreview> {
        let args: FileEditArgs = serde_json::from_value(args.clone()).ok()?;
        self.prepare_for_approval(&args).ok()
    }

    async fn execute(&self, args: serde_json::Value) -> ToolResult {
        let args: FileEditArgs = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolResult::error(format!("invalid arguments: {e}")),
        };
        // (backlog 1db26c95) Restore boundary-token markers to the raw
        // tokens in BOTH edit strings: the agent reconstructs old_string
        // from its escaped view of the file (without the restore it fails
        // to match the raw token on disk — the 2026-12-24 incident's
        // failed old_string match), and new_string must land the raw
        // token, not the marker. The request layer does the escaping (the
        // serving layer strips/maps the raw token from input). In regex
        // mode old_string is a PATTERN — the raw token's metacharacters
        // (the GLM token's `|` alternation) would silently change match
        // semantics — so its markers restore to a regex-QUOTED form; the
        // replacement (new_string) is not a pattern, plain restore
        // (review L4).
        let args = FileEditArgs {
            old_string: if args.use_regex {
                crate::provider::boundary::restore_boundary_tokens_regex_quoted(&args.old_string)
            } else {
                crate::provider::boundary::restore_boundary_tokens(&args.old_string)
            },
            new_string: crate::provider::boundary::restore_boundary_tokens(&args.new_string),
            ..args
        };

        // Offload the blocking validate + read + write onto the blocking pool
        // (Perf H1). The pure work (`prepare_edit`, `is_protected_write_target`)
        // runs inside the closure too because it consumes the content/validated
        // path produced there — keeping the exact error ordering (validate →
        // protected → read → prepare → write) identical to the pre-wrap path.
        let sandbox = self.sandbox.clone();
        tokio::task::spawn_blocking(move || {
            let validated = match sandbox.validate(Path::new(&args.path)) {
                Ok(p) => p,
                Err(e) => return ToolResult::error(format!("path validation failed: {e}")),
            };

            // Directory guard (backlog #89 sibling of file_read's): editing a
            // dir path must name the path kind, not surface the raw Windows
            // "Access is denied. (os error 5)" from read_to_string(dir).
            if validated.is_dir() {
                return ToolResult::error(format!(
                    "'{}' is a directory, not a file — read_files on '{}' returns its listing",
                    args.path, args.path
                ));
            }

            // Reject edits to protected paths (.coding state/bookkeeping or
            // the .git control plane) with the shared refusal message.
            // file_edit does NOT use the creation
            // ladder (it edits existing files — mkdir for a non-existent file
            // then read_to_string would fail confusingly).
            if let Err(e) = sandbox.refuse_if_protected(&validated) {
                return ToolResult::error(format!("{e}"));
            }

            let content = match std::fs::read_to_string(&validated) {
                Ok(c) => c,
                Err(e) if args.append && e.kind() == std::io::ErrorKind::NotFound => {
                    // Append mode requires an existing file — steer to
                    // file_write, which creates it (plan be16ea36 step 5).
                    return ToolResult::error(format!(
                        "cannot append: '{}' does not exist — use file_write \
                         (mode \"overwrite\") to create it first",
                        args.path
                    ));
                }
                Err(e) => return ToolResult::error(format!("failed to read '{}': {e}", args.path)),
            };

            let prepared = match prepare_edit(&args, &content) {
                Ok(p) => p,
                Err(e) => return ToolResult::error(format!("{e}")),
            };

            // Write the new content.
            if let Err(e) = std::fs::write(&validated, &prepared.new_content) {
                return ToolResult::error(format!("failed to write '{}': {e}", args.path));
            }

            // H5 advisory (backlog e8b39d72): a large payload gets a NOTE on
            // the success result — never a rejection.
            let mut output = format!("edited {}", args.path);
            if let Some(note) = large_payload_note(&args) {
                output.push('\n');
                output.push_str(&note);
            }
            ToolResult {
                success: true,
                output,
                data: Some(json!({"diff": prepared.diff})),
            }
        })
        .await
        .unwrap_or_else(|e| ToolResult::error(format!("file edit task failed: {e}")))
    }
}

/// Soft cap on approval-preview diff size (chars). Mirrors file_write (M3).
const PREVIEW_DIFF_CHAR_BUDGET: usize = 120_000;

impl FileEditTool {
    /// Prepare an edit for approval preview (without applying). Returns the
    /// diff and new content so the UI can show it before the user approves.
    /// Does not write to disk.
    pub fn prepare_for_approval(&self, args: &FileEditArgs) -> Result<ApprovalPreview> {
        let validated = self.sandbox.validate(Path::new(&args.path))?;
        // Same directory guard as execute — the preview path must fail with
        // the clear path-kind message, not a raw io error (backlog #89).
        if validated.is_dir() {
            return Err(crate::error::Error::Tool(format!(
                "'{}' is a directory, not a file — read_files on '{}' returns its listing",
                args.path, args.path
            )));
        }
        self.sandbox.refuse_if_protected(&validated)?;
        let content = std::fs::read_to_string(&validated)?;
        let prepared = prepare_edit(args, &content)?;
        let diff = if prepared.diff.chars().count() > PREVIEW_DIFF_CHAR_BUDGET {
            let mut d: String = prepared
                .diff
                .chars()
                .take(PREVIEW_DIFF_CHAR_BUDGET)
                .collect();
            d.push_str("\n… [preview truncated for size] …\n");
            d
        } else {
            prepared.diff
        };
        Ok(ApprovalPreview::Diff {
            path: validated,
            diff,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_tool(dir: &Path) -> FileEditTool {
        FileEditTool::new(Sandbox::new(dir).unwrap())
    }

    /// Build literal-mode args (the common case for existing tests).
    fn lit_args(path: &str, old: &str, new: &str, replace_all: bool) -> FileEditArgs {
        FileEditArgs {
            path: path.into(),
            old_string: old.into(),
            new_string: new.into(),
            replace_all,
            use_regex: false,
            count: None,
            start_line: None,
            end_line: None,
            fuzzy_whitespace: false,
            edits: None,
            append: false,
        }
    }

    /// Build regex-mode args.
    fn re_args(
        path: &str,
        pat: &str,
        new: &str,
        replace_all: bool,
        count: Option<usize>,
    ) -> FileEditArgs {
        FileEditArgs {
            path: path.into(),
            old_string: pat.into(),
            new_string: new.into(),
            replace_all,
            use_regex: true,
            count,
            start_line: None,
            end_line: None,
            fuzzy_whitespace: false,
            edits: None,
            append: false,
        }
    }

    /// Build line-range-mode args.
    fn line_args(path: &str, start: usize, end: usize, new: &str) -> FileEditArgs {
        FileEditArgs {
            path: path.into(),
            old_string: String::new(),
            new_string: new.into(),
            replace_all: false,
            use_regex: false,
            count: None,
            start_line: Some(start),
            end_line: Some(end),
            fuzzy_whitespace: false,
            edits: None,
            append: false,
        }
    }

    #[test]
    fn compute_diff_shows_changes() {
        let diff = compute_diff("a.rs", "hello\nworld", "hello\nrust");
        assert!(diff.contains("--- a.rs"));
        assert!(diff.contains("+++ a.rs"));
        assert!(diff.contains("-world"));
        assert!(diff.contains("+rust"));
    }

    #[test]
    fn prepare_for_approval_is_pure_and_returns_diff() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "hello world\n").unwrap();
        let tool = make_tool(dir.path());
        let args = lit_args("a.txt", "world", "rust", false);
        let preview = tool.prepare_for_approval(&args).unwrap();
        match preview {
            ApprovalPreview::Diff { path: p, diff } => {
                assert!(p.ends_with("a.txt"));
                assert!(diff.contains("-world") || diff.contains("world"));
                assert!(diff.contains("rust") || diff.contains("+rust"));
            }
            other => panic!("expected Diff preview, got {other:?}"),
        }
        // Must not have written the file.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello world\n");
    }

    #[test]
    fn approval_preview_trait_hook_deserializes_args() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), "aa\n").unwrap();
        let tool = make_tool(dir.path());
        let preview = tool
            .approval_preview(&json!({
                "path": "b.txt",
                "old_string": "aa",
                "new_string": "bb"
            }))
            .expect("preview");
        assert!(matches!(preview, ApprovalPreview::Diff { .. }));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "aa\n"
        );
    }

    #[tokio::test]
    async fn file_edit_restores_boundary_markers_in_old_and_new_string() {
        // Backlog 1db26c95: BOTH edit strings restore the boundary-token
        // marker — the old_string (the agent reconstructs it from its
        // escaped view of the file; without the restore it fails to match
        // the raw token on disk — the 2026-12-24 incident's failed
        // old_string match) and the new_string (the written content must
        // carry the raw token, not the marker). Transport-safe: the marker
        // and the raw token are built from escapes — raw angle-bracket tag
        // text is stripped in text transports.
        let endoftext = "\u{3c}|endoftext|\u{3e}";
        let marker = format!(
            "\u{27e6}raw:{}\u{27e7}",
            endoftext
                .chars()
                .map(|c| format!("\\u{:04x}", c as u32))
                .collect::<String>()
        );
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("cfg.toml"),
            format!("stop = [\"{endoftext}\"]\n"),
        )
        .unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "path": "cfg.toml",
                "old_string": format!("stop = [\"{marker}\"]"),
                "new_string": format!("stop = [\"{marker}\", \"x\"]")
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("cfg.toml")).unwrap(),
            format!("stop = [\"{endoftext}\", \"x\"]\n"),
            "both old_string (matching) and new_string (writing) must \
             restore the marker to the raw boundary token"
        );
    }

    #[tokio::test]
    async fn regex_mode_restores_boundary_markers_quoted() {
        // Backlog 1db26c95, review L4: in regex mode the restored
        // old_string is a PATTERN — the raw token's `|` alternation would
        // silently match a tiny wrong fragment (here `<`), so markers
        // restore to a regex-QUOTED form that matches the raw token
        // literally. Transport-safe: built from escapes — raw
        // angle-bracket tag text is stripped in text transports.
        let endoftext = "\u{3c}|endoftext|\u{3e}";
        let marker = format!(
            "\u{27e6}raw:{}\u{27e7}",
            endoftext
                .chars()
                .map(|c| format!("\\u{:04x}", c as u32))
                .collect::<String>()
        );
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join("r.txt"),
            format!("before{endoftext}after"),
        )
        .unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "path": "r.txt",
                "old_string": marker,
                "new_string": "REPLACED",
                "use_regex": true
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("r.txt")).unwrap(),
            "beforeREPLACEDafter",
            "the quoted restore must match the whole raw token, not an \
             alternation fragment"
        );
    }

    #[tokio::test]
    async fn editing_a_directory_path_returns_directory_hint() {
        // Regression (backlog #89 sibling): file_edit on a directory hit
        // read_to_string(dir) → on Windows "Access is denied. (os error 5)"
        // instead of a clear "is a directory" error naming the path kind.
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("plans")).unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "plans", "old_string": "a", "new_string": "b"}))
            .await;
        assert!(!result.success, "expected error, got: {}", result.output);
        assert!(
            result.output.contains("is a directory, not a file"),
            "must name the path kind: {}",
            result.output
        );
    }

    #[test]
    fn prepare_edit_replaces_first_occurrence() {
        let args = lit_args("a.txt", "foo", "bar", false);
        let prepared = prepare_edit(&args, "foo and foo again").unwrap();
        assert_eq!(prepared.new_content, "bar and foo again");
    }

    #[test]
    fn prepare_edit_replace_all() {
        let args = lit_args("a.txt", "foo", "bar", true);
        let prepared = prepare_edit(&args, "foo and foo again").unwrap();
        assert_eq!(prepared.new_content, "bar and bar again");
    }

    #[test]
    fn prepare_edit_count_replaces_first_n() {
        let mut args = lit_args("a.txt", "foo", "bar", false);
        args.count = Some(2);
        let prepared = prepare_edit(&args, "foo foo foo").unwrap();
        assert_eq!(prepared.new_content, "bar bar foo");
    }

    /// Backlog #48 regression: a mostly-LF file with one stray CRLF used to
    /// detect as CRLF (any-CRLF-wins), so a multi-line LF needle missed.
    /// Majority-vote detection now reads the file as LF and the edit lands.
    #[test]
    fn mixed_majority_lf_file_matches_lf_old_string() {
        let args = lit_args("a.txt", "line2\nline3", "line2\nLINE3", false);
        let prepared = prepare_edit(&args, "intro\r\nline2\nline3\n").unwrap();
        assert_eq!(prepared.new_content, "intro\r\nline2\nLINE3\n");
    }

    /// Majority-CRLF mixed files keep working (guard for the vote direction).
    #[test]
    fn mixed_majority_crlf_file_matches_crlf_old_string() {
        let args = lit_args("a.txt", "b\r\nc", "b\r\nC", false);
        let prepared = prepare_edit(&args, "a\r\nb\r\nc\n").unwrap();
        assert_eq!(prepared.new_content, "a\r\nb\r\nC\n");
    }

    /// A needle spanning the file's MINORITY lines (the CRLF head a,b of an
    /// LF-majority file — c,d,e are bare-LF) matches: matching is EOL-agnostic
    /// (both sides normalized to LF), so no flipped-retry ladder is needed.
    /// The replacement is re-emitted in the file's DETECTED (majority) style;
    /// untouched bytes keep their original endings (plan be16ea36 step 2).
    #[test]
    fn mixed_file_needle_spanning_minority_lines_matches() {
        // LF majority (c,d,e tail) with a CRLF head (a,b).
        let args = lit_args("a.txt", "a\nb", "A\nB", false);
        let prepared = prepare_edit(&args, "a\r\nb\r\nc\nd\ne\n").unwrap();
        assert_eq!(prepared.new_content, "A\nB\r\nc\nd\ne\n");
    }

    /// The mirrored direction (review LOW-3): a CRLF-MAJORITY file whose
    /// needle spans the bare-LF minority lines. EOL-agnostic matching finds
    /// the span; the replacement is re-emitted in the file's detected (CRLF)
    /// style (plan be16ea36 step 2).
    #[test]
    fn mixed_majority_crlf_file_needle_spanning_bare_lf_lines_matches() {
        // CRLF majority (x,y,z,w) with a bare-LF tail (d,e).
        let args = lit_args("a.txt", "d\ne", "D\nE", false);
        let prepared = prepare_edit(&args, "x\r\ny\r\nz\r\nw\r\nd\ne\n").unwrap();
        assert_eq!(prepared.new_content, "x\r\ny\r\nz\r\nw\r\nD\r\nE\n");
    }

    /// No false positives: a needle that is genuinely absent from a
    /// mixed-ending file must still error after both attempts.
    #[test]
    fn mixed_file_needle_still_absent_errors() {
        let args = lit_args("a.txt", "zzz\nqqq", "x\ny", false);
        let result = prepare_edit(&args, "a\r\nb\nc\n");
        assert!(
            matches!(result, Err(crate::error::Error::NotFound(_))),
            "expected NotFound, got {result:?}"
        );
    }

    // ---- RED (plan be16ea36 step 1a): false drift on EOL-mismatched text ----
    // The needle below is the file's text modulo line endings (what an agent
    // reconstructs from a read, where endings are invisible), so every edit
    // here must APPLY. Current code fails each with old_string-not-found.

    /// A multi-line needle spanning BOTH ending styles of a mixed file misses
    /// under BOTH flipped attempts today: the CRLF normalization stumbles on
    /// the file's bare-LF line, the LF retry on its CRLF lines — a false
    /// drift error on verified-identical text.
    #[test]
    fn mixed_eol_file_multiline_old_string_applies() {
        // CRLF majority (first, third) with one bare-LF line (second).
        let args = lit_args(
            "a.txt",
            "first\nsecond\nthird",
            "FIRST\nSECOND\nTHIRD",
            false,
        );
        let prepared = prepare_edit(&args, "first\r\nsecond\nthird\r\n").expect(
            "the needle is the file's text modulo line endings — the edit must apply",
        );
        // The replaced region is re-emitted in the file's detected (majority)
        // style; untouched bytes keep their original endings.
        assert_eq!(prepared.new_content, "FIRST\r\nSECOND\r\nTHIRD\r\n");
    }

    /// replace_all on a mixed-ending file must replace EVERY occurrence of
    /// the text regardless of which ending style each occurrence carries —
    /// today only detected-style occurrences match (a silent partial edit,
    /// same root cause as the false drift).
    #[test]
    fn mixed_eol_replace_all_is_eol_agnostic() {
        // One CRLF occurrence + one bare-LF occurrence of the same text.
        let args = lit_args("a.txt", "x\ny", "X\nY", true);
        let prepared = prepare_edit(&args, "x\r\ny\nx\ny\n")
            .expect("every occurrence must be replaced regardless of ending style");
        assert_eq!(prepared.new_content, "X\nY\nX\nY\n");
    }

    /// fuzzy_whitespace must stay EOL-agnostic: a CRLF file whose line
    /// carries trailing whitespace defeats the per-line trailing-strip today
    /// (the strip pops a collapsed space only while it is the line's LAST
    /// emitted char — the '\r' emitted before the '\n' blocks it), so the LF
    /// needle misses. The LF-file mirror of this exact scenario matches.
    #[test]
    fn fuzzy_crlf_interior_trailing_whitespace_matches() {
        let args = fuzzy_args("a.txt", "foo\nbar", "foo\nBAR", false);
        let prepared = prepare_edit(&args, "foo  \r\nbar\r\n")
            .expect("trailing whitespace before CRLF must be ignored by fuzzy matching");
        // Interior trailing whitespace inside the match span is consumed
        // (mirrors the LF-file behavior); the file's CRLF style is kept.
        assert_eq!(prepared.new_content, "foo\r\nBAR\r\n");
    }

    /// The mixed-file false drift through fuzzy mode: same needle/content as
    /// [`mixed_eol_file_multiline_old_string_applies`] with fuzzy_whitespace
    /// enabled.
    #[test]
    fn fuzzy_mixed_eol_multiline_old_string_applies() {
        let args = fuzzy_args(
            "a.txt",
            "first\nsecond\nthird",
            "FIRST\nSECOND\nTHIRD",
            false,
        );
        let prepared = prepare_edit(&args, "first\r\nsecond\nthird\r\n").expect(
            "fuzzy matching must be EOL-agnostic — the edit must apply",
        );
        assert_eq!(prepared.new_content, "FIRST\r\nSECOND\r\nTHIRD\r\n");
    }

    #[test]
    fn prepare_edit_identical_strings_error() {
        let args = lit_args("a.txt", "same", "same", false);
        let result = prepare_edit(&args, "same content");
        assert!(result.is_err());
    }

    #[test]
    fn prepare_edit_old_not_found_error() {
        let args = lit_args("a.txt", "missing", "present", false);
        let result = prepare_edit(&args, "no match here");
        assert!(result.is_err());
    }

    /// Backlog 714196da: drift-class edit failures must steer the agent to a
    /// fresh read — the error text carries the re-read instruction (the
    /// EditStaleRead steering marker), so a blind retry from memory is never
    /// the only signal. Schema-mistake errors (identical strings, mode
    /// exclusivity) intentionally do NOT carry it — those are caller errors,
    /// not content-drift signals.
    #[test]
    fn drift_class_errors_carry_fresh_read_nudge() {
        use crate::agent::steering_stats::EDIT_STALE_READ_MARK;
        // Literal miss.
        let args = lit_args("a.txt", "zzz\nqqq", "x\ny", false);
        let err = prepare_edit(&args, "a\r\nb\nc\n").unwrap_err().to_string();
        assert!(err.contains(EDIT_STALE_READ_MARK), "literal miss: {err}");
        // Fuzzy miss.
        let mut args = lit_args("a.txt", "zzz\nqqq", "x\ny", false);
        args.fuzzy_whitespace = true;
        let err = prepare_edit(&args, "a\r\nb\nc\n").unwrap_err().to_string();
        assert!(err.contains(EDIT_STALE_READ_MARK), "fuzzy miss: {err}");
        // Regex miss.
        let mut args = lit_args("a.txt", "zzz", "x", false);
        args.use_regex = true;
        let err = prepare_edit(&args, "abc\n").unwrap_err().to_string();
        assert!(err.contains(EDIT_STALE_READ_MARK), "regex miss: {err}");
        // Line-range past EOF.
        let mut args = lit_args("a.txt", "", "x", false);
        args.start_line = Some(9);
        args.end_line = Some(9);
        let err = prepare_edit(&args, "a\nb\nc\n").unwrap_err().to_string();
        assert!(err.contains(EDIT_STALE_READ_MARK), "past EOF: {err}");
        // Empty file.
        let mut args = lit_args("a.txt", "", "x", false);
        args.start_line = Some(1);
        args.end_line = Some(1);
        let err = prepare_edit(&args, "").unwrap_err().to_string();
        assert!(err.contains(EDIT_STALE_READ_MARK), "empty file: {err}");
    }

    /// H5 (backlog e8b39d72): the emission-fragility NOTE fires at the
    /// observed ~700-char threshold on success results, and stays silent for
    /// smaller payloads.
    #[test]
    fn large_payload_note_fires_at_threshold() {
        let big = "x".repeat(EMISSION_FRAGILITY_PAYLOAD_BYTES);
        let args = lit_args("a.rs", "old", &big, false);
        let note = large_payload_note(&args).expect("note at threshold");
        assert!(note.contains("large edit payload"), "{note}");
        assert!(note.contains("emission fragility"), "{note}");
        // Just below the threshold: no note.
        let small = "x".repeat(EMISSION_FRAGILITY_PAYLOAD_BYTES - 1);
        let args = lit_args("a.rs", "old", &small, false);
        assert!(large_payload_note(&args).is_none());
    }

    /// Backlog e8b39d72 H2: emission-artifact validation — the observed decay
    /// classes are rejected pre-write with the escape-hatch message, and legit
    /// content passes. These are NOT drift-class failures (a re-read cannot
    /// fix a corrupted emission), so they must NOT carry the stale-read nudge.
    #[test]
    fn emission_artifacts_are_rejected_pre_write() {
        // (a) '## ' line start inside a .rs file (the '///' → '##' mangling).
        let args = lit_args("a.rs", "fn old() {}", "## boundary\nfn new() {}", false);
        let err = prepare_edit(&args, "fn old() {}").unwrap_err().to_string();
        assert!(err.contains("emission artifact"), "markdown: {err}");
        assert!(err.contains("markdown-heading"), "{err}");
        assert!(!err.contains("Re-read the file"), "not a drift failure: {err}");
        // (b) Sentinel whole-value (the fragment-replacement artifact).
        let args = lit_args("a.rs", "fn old() {}", "placeholder", false);
        let err = prepare_edit(&args, "fn old() {}").unwrap_err().to_string();
        assert!(err.contains("sentinel-shaped"), "{err}");
        // (c) Adjacent byte-identical doc lines (the duplication artifact).
        let args = lit_args(
            "a.rs",
            "/// Does a thing.",
            "/// Does a thing.\n/// Does a thing.\nfn f() {}",
            false,
        );
        let err = prepare_edit(&args, "/// Does a thing.\nfn f() {}")
            .unwrap_err()
            .to_string();
        assert!(err.contains("duplicated adjacent doc line"), "{err}");
        // (d) Truncation: an edit that grows the delimiter deficit.
        let args = lit_args("a.rs", "fn old() { /* keep */ }", "fn new() {", false);
        let err = prepare_edit(&args, "fn old() { /* keep */ }\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("unbalanced delimiters"), "{err}");
    }

    /// Backlog e8b39d72 H2: the validation must not false-positive on legit
    /// content — markdown headings in .md files, placeholder as a substring,
    /// brace-bearing string literals, non-code extensions, and balanced
    /// additions all pass.
    #[test]
    fn emission_artifact_validation_allows_legit_content() {
        // A '## ' line in a MARKDOWN file is legit.
        let args = lit_args("README.md", "old", "## Heading\nnew", false);
        assert!(prepare_edit(&args, "old\n").is_ok());
        // 'placeholder' as a SUBSTRING (not a whole-value) is legit.
        let args = lit_args("a.rs", "old", "let placeholder_count = 1;", false);
        assert!(prepare_edit(&args, "old\n").is_ok());
        // A '## ' sequence mid-line (inside a string) is not a line start.
        let args = lit_args("a.rs", "old", "let s = \"x ## y\";", false);
        assert!(prepare_edit(&args, "old\n").is_ok());
        // Non-code extensions skip the heading check entirely.
        let args = lit_args("notes.txt", "old", "## Note\nnew", false);
        assert!(prepare_edit(&args, "old\n").is_ok());
        // Brace-bearing string literals in a balanced addition pass the
        // smoke-parse (the lexer skips string contents).
        let args = lit_args(
            "a.rs",
            "old",
            "fn new() { let x = \"{oops\"; let y = r#\"{raw\"#; }",
            false,
        );
        assert!(prepare_edit(&args, "old\n").is_ok());
        // A pre-existing unbalanced file is not this edit's fault (delta
        // scoping): the balance deficit does not grow.
        let args = lit_args("a.rs", "fn old() {", "fn old() { /* still */", false);
        let broken = "fn old() {\n"; // already unbalanced
        assert!(prepare_edit(&args, broken).is_ok());
    }

    #[test]
    fn crlf_file_matches_lf_old_string() {
        // A CRLF file should match an old_string written with LF endings.
        let args = lit_args("a.txt", "line one\nline two", "line one\nLINE TWO", false);
        let content = "line one\r\nline two\r\n";
        let prepared = prepare_edit(&args, content).unwrap();
        // The file's CRLF endings must be preserved.
        assert_eq!(prepared.new_content, "line one\r\nLINE TWO\r\n");
    }

    #[test]
    fn lf_file_matches_crlf_old_string() {
        // An LF file should match an old_string written with CRLF endings.
        let args = lit_args(
            "a.txt",
            "line one\r\nline two",
            "line one\r\nLINE TWO",
            false,
        );
        let content = "line one\nline two\n";
        let prepared = prepare_edit(&args, content).unwrap();
        // The file's LF endings must be preserved.
        assert_eq!(prepared.new_content, "line one\nLINE TWO\n");
    }

    #[test]
    fn crlf_file_preserves_crlf_after_edit() {
        // After editing a CRLF file, the result must still use CRLF throughout.
        let args = lit_args("a.txt", "foo\nbar", "foo\nbaz\nqux", false);
        let content = "foo\r\nbar\r\n";
        let prepared = prepare_edit(&args, content).unwrap();
        assert_eq!(prepared.new_content, "foo\r\nbaz\r\nqux\r\n");
        // No bare LF should remain in the output.
        assert!(
            !prepared.new_content.contains('\n')
                || prepared.new_content.matches("\r\n").count()
                    == prepared.new_content.matches('\n').count(),
            "all newlines must be CRLF"
        );
    }

    #[test]
    fn crlf_file_replace_all_with_lf_strings() {
        // replace_all should also normalize line endings.
        let args = lit_args("a.txt", "x\ny", "z\nw", true);
        let content = "x\r\ny\r\nx\r\ny\r\n";
        let prepared = prepare_edit(&args, content).unwrap();
        assert_eq!(prepared.new_content, "z\r\nw\r\nz\r\nw\r\n");
    }

    // ---- regex mode ----

    #[test]
    fn regex_replaces_first_match_by_default() {
        let args = re_args("a.txt", r"\d+", "N", false, None);
        let prepared = prepare_edit(&args, "a1 b2 c3").unwrap();
        assert_eq!(prepared.new_content, "aN b2 c3");
    }

    #[test]
    fn regex_replace_all() {
        let args = re_args("a.txt", r"\d+", "N", true, None);
        let prepared = prepare_edit(&args, "a1 b2 c3").unwrap();
        assert_eq!(prepared.new_content, "aN bN cN");
    }

    #[test]
    fn regex_count_replaces_first_n() {
        let args = re_args("a.txt", r"\d+", "N", false, Some(2));
        let prepared = prepare_edit(&args, "a1 b2 c3").unwrap();
        assert_eq!(prepared.new_content, "aN bN c3");
    }

    #[test]
    fn regex_capture_groups() {
        // vi-style: swap two words using capture groups ($1/$2).
        let args = re_args("a.txt", r"(\w+) (\w+)", "$2 $1", false, None);
        let prepared = prepare_edit(&args, "hello world").unwrap();
        assert_eq!(prepared.new_content, "world hello");
    }

    #[test]
    fn regex_no_match_errors() {
        let args = re_args("a.txt", r"zzz+", "x", false, None);
        assert!(prepare_edit(&args, "no match here").is_err());
    }

    #[test]
    fn regex_invalid_pattern_errors() {
        let args = re_args("a.txt", r"(unclosed", "x", false, None);
        let err = prepare_edit(&args, "anything").unwrap_err();
        // The error must be actionable: point at literal mode. A mutation
        // tool never silently falls back — wrong-text edits would result.
        let msg = err.to_string();
        assert!(msg.contains("invalid regex"), "names the problem: {msg}");
        assert!(msg.contains("literal"), "suggests literal mode: {msg}");
    }

    #[test]
    fn regex_crlf_file_lf_pattern() {
        // An LF `\n` in the pattern must match a CRLF file's line ending.
        let args = re_args("a.txt", "one\ntwo", "one\nTWO", false, None);
        let content = "one\r\ntwo\r\n";
        let prepared = prepare_edit(&args, content).unwrap();
        assert_eq!(prepared.new_content, "one\r\nTWO\r\n");
    }

    #[test]
    fn regex_anchored_line_start() {
        // vi-style `^` anchor: prepend a comment marker to the fn line.
        let args = re_args("a.txt", r"(?m)^fn ", "pub fn ", true, None);
        let content = "fn a() {}\nfn b() {}\n";
        let prepared = prepare_edit(&args, content).unwrap();
        assert_eq!(prepared.new_content, "pub fn a() {}\npub fn b() {}\n");
    }

    #[tokio::test]
    async fn edits_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello world").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "path": "a.txt",
                "old_string": "world",
                "new_string": "rust",
            }))
            .await;
        assert!(result.success);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "hello rust"
        );
        assert!(result.data.unwrap()["diff"]
            .as_str()
            .unwrap()
            .contains("+hello rust"));
    }

    #[tokio::test]
    async fn path_traversal_rejected() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "path": "../etc/passwd",
                "old_string": "a",
                "new_string": "b",
            }))
            .await;
        assert!(!result.success);
    }

    // ---- line-range mode (start_line + end_line) ----

    #[test]
    fn prepare_edit_lines_replaces_range() {
        // Replace lines 2-3 of "a\nb\nc\nd\n" with "X" → "a\nX\nd\n".
        let args = line_args("a.txt", 2, 3, "X");
        let prepared = prepare_edit(&args, "a\nb\nc\nd\n").unwrap();
        assert_eq!(prepared.new_content, "a\nX\nd\n");
    }

    #[test]
    fn prepare_edit_lines_replaces_single_line() {
        // start == end replaces exactly one line.
        let args = line_args("a.txt", 2, 2, "REPLACED");
        let prepared = prepare_edit(&args, "a\nb\nc\n").unwrap();
        assert_eq!(prepared.new_content, "a\nREPLACED\nc\n");
    }

    #[test]
    fn prepare_edit_lines_clamps_end() {
        // end_line beyond the file length clamps to the last line.
        let args = line_args("a.txt", 2, 99, "X");
        let prepared = prepare_edit(&args, "a\nb\nc\n").unwrap();
        // Lines 2..end (b, c) replaced with X → "a\nX\n".
        assert_eq!(prepared.new_content, "a\nX\n");
    }

    #[test]
    fn prepare_edit_lines_replaces_through_last_line() {
        // Replacing the final line preserves the trailing newline.
        let args = line_args("a.txt", 3, 3, "Z");
        let prepared = prepare_edit(&args, "a\nb\nc\n").unwrap();
        assert_eq!(prepared.new_content, "a\nb\nZ\n");
    }

    #[test]
    fn prepare_edit_lines_start_gt_end_errors() {
        let args = line_args("a.txt", 3, 2, "X");
        let result = prepare_edit(&args, "a\nb\nc\n");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("end_line"), "msg: {msg}");
    }

    #[test]
    fn prepare_edit_lines_one_bound_only_errors() {
        // Setting only one of the two line-range bounds is the observed
        // malformed-call shape (start_line without end_line — live incident:
        // 5 consecutive identical failures); the error must carry the exact
        // expected arguments so the model can self-correct on the first
        // attempt.
        let mut args = line_args("a.txt", 2, 3, "X");
        args.end_line = None;
        let result = prepare_edit(&args, "a\nb\nc\n");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("must both be set"), "msg: {msg}");
        assert!(msg.contains("Expected arguments"), "msg: {msg}");
        assert!(msg.contains("\"end_line\""), "msg: {msg}");
    }

    #[test]
    fn prepare_edit_lines_zero_start_errors() {
        let args = line_args("a.txt", 0, 2, "X");
        let result = prepare_edit(&args, "a\nb\nc\n");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("start_line"), "msg: {msg}");
    }

    #[test]
    fn prepare_edit_lines_start_past_end_of_file_errors() {
        // start_line beyond the last line is an error (nothing to replace).
        let args = line_args("a.txt", 5, 6, "X");
        let result = prepare_edit(&args, "a\nb\nc\n");
        assert!(result.is_err());
    }

    #[test]
    fn prepare_edit_lines_empty_file_errors() {
        let args = line_args("a.txt", 1, 1, "X");
        let result = prepare_edit(&args, "");
        assert!(result.is_err());
    }

    #[test]
    fn prepare_edit_lines_only_one_bound_errors() {
        // Setting only start_line (not end_line) is an error.
        let mut args = line_args("a.txt", 1, 1, "X");
        args.end_line = None;
        let result = prepare_edit(&args, "a\nb\n");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("both"), "msg: {msg}");
    }

    #[test]
    fn prepare_edit_lines_crlf_preserved() {
        // A CRLF file: new_string written with LF is normalized to CRLF.
        let args = line_args("a.txt", 2, 3, "X\nY");
        let content = "a\r\nb\r\nc\r\nd\r\n";
        let prepared = prepare_edit(&args, content).unwrap();
        assert_eq!(prepared.new_content, "a\r\nX\r\nY\r\nd\r\n");
        // No bare LF should remain.
        assert!(
            prepared.new_content.matches("\r\n").count()
                == prepared.new_content.matches('\n').count(),
            "all newlines must be CRLF"
        );
    }

    #[test]
    fn prepare_edit_lines_multiline_new_string() {
        // new_string spanning multiple lines replaces the range cleanly.
        let args = line_args("a.txt", 1, 1, "alpha\nbeta");
        let prepared = prepare_edit(&args, "old\nkeep\n").unwrap();
        assert_eq!(prepared.new_content, "alpha\nbeta\nkeep\n");
    }

    #[test]
    fn prepare_edit_lines_diff_renders() {
        // The diff must contain -/+ lines for the replaced range.
        let args = line_args("a.txt", 2, 2, "REPLACED");
        let prepared = prepare_edit(&args, "a\nb\nc\n").unwrap();
        assert!(
            prepared.diff.contains("-b"),
            "diff should show removed line: {}",
            prepared.diff
        );
        assert!(
            prepared.diff.contains("+REPLACED"),
            "diff should show added line: {}",
            prepared.diff
        );
    }

    #[test]
    fn prepare_edit_lines_no_change_errors() {
        // If new_string equals the replaced lines, it's a no-op → error.
        let args = line_args("a.txt", 2, 2, "b");
        let result = prepare_edit(&args, "a\nb\nc\n");
        assert!(result.is_err());
    }

    #[test]
    fn prepare_edit_empty_old_string_errors() {
        // An empty old_string in string-matching mode must error, not insert
        // new_string between every character (Rust's str::replace with an empty
        // needle matches at every position — silent corruption). Line-range
        // mode is exempt (it doesn't use old_string).
        let mut args = lit_args("a.txt", "", "X", false);
        let result = prepare_edit(&args, "a\nb\nc\n");
        assert!(result.is_err(), "empty old_string must error");
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("old_string is empty"), "msg: {msg}");

        // use_regex with an empty pattern must also error.
        args.use_regex = true;
        let result = prepare_edit(&args, "a\nb\nc\n");
        assert!(result.is_err(), "empty regex pattern must error");
    }

    #[tokio::test]
    async fn edits_file_by_line_range() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "a\nb\nc\nd\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "path": "a.txt",
                "start_line": 2,
                "end_line": 3,
                "new_string": "X",
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "a\nX\nd\n"
        );
        // The diff in the result data should show the change.
        let data = result.data.unwrap();
        let diff = data["diff"].as_str().unwrap();
        assert!(diff.contains("-b") || diff.contains("-c"));
        assert!(diff.contains("+X"));
    }

    // ---- fuzzy_whitespace mode ----

    /// Build literal-mode args with fuzzy_whitespace=true.
    fn fuzzy_args(path: &str, old: &str, new: &str, replace_all: bool) -> FileEditArgs {
        FileEditArgs {
            path: path.into(),
            old_string: old.into(),
            new_string: new.into(),
            replace_all,
            use_regex: false,
            count: None,
            start_line: None,
            end_line: None,
            fuzzy_whitespace: true,
            edits: None,
            append: false,
        }
    }

    #[test]
    fn fuzzy_matches_tab_vs_space() {
        // Content uses a tab; old_string uses a space. Fuzzy matches.
        let args = fuzzy_args("a.txt", "fn foo()", "fn bar()", false);
        let prepared = prepare_edit(&args, "fn\tfoo()\n").unwrap();
        assert_eq!(prepared.new_content, "fn bar()\n");
    }

    #[test]
    fn fuzzy_matches_extra_trailing_space() {
        // Content has trailing spaces the old_string omits. Fuzzy matches the
        // non-whitespace region; the trailing spaces are OUTSIDE the match, so
        // they're preserved (the splice only replaces "let x = 1;").
        let args = fuzzy_args("a.txt", "let x = 1;", "let y = 2;", false);
        let prepared = prepare_edit(&args, "let x = 1;   \n").unwrap();
        assert_eq!(prepared.new_content, "let y = 2;   \n");
    }

    #[test]
    fn fuzzy_collapses_runs() {
        // Multiple spaces collapse to one for matching.
        let args = fuzzy_args("a.txt", "a b c", "X", false);
        let prepared = prepare_edit(&args, "a    b     c\n").unwrap();
        assert_eq!(prepared.new_content, "X\n");
    }

    #[test]
    fn fuzzy_preserves_surrounding_whitespace() {
        // Bytes OUTSIDE the match (indentation + trailing) are preserved.
        let args = fuzzy_args("a.txt", "fn foo()", "BAR", false);
        let prepared = prepare_edit(&args, "  fn\tfoo()  \n").unwrap();
        assert_eq!(prepared.new_content, "  BAR  \n");
    }

    #[test]
    fn fuzzy_count_replaces_first_n() {
        // Replace the first 2 of 3 matches.
        let mut args = fuzzy_args("a.txt", "foo", "bar", false);
        args.count = Some(2);
        let prepared = prepare_edit(&args, "foo  foo  foo\n").unwrap();
        assert_eq!(prepared.new_content, "bar  bar  foo\n");
    }

    #[test]
    fn fuzzy_replace_all() {
        let args = fuzzy_args("a.txt", "foo", "bar", true);
        let prepared = prepare_edit(&args, "foo  foo  foo\n").unwrap();
        assert_eq!(prepared.new_content, "bar  bar  bar\n");
    }

    #[test]
    fn fuzzy_no_match_errors() {
        let args = fuzzy_args("a.txt", "zzz", "x", false);
        let result = prepare_edit(&args, "no match here\n");
        assert!(result.is_err());
    }

    #[test]
    fn fuzzy_identical_normalized_errors() {
        // old and new normalize to the same thing → identical-strings guard.
        let args = fuzzy_args("a.txt", "a  b", "a b", false);
        let result = prepare_edit(&args, "a  b\n");
        assert!(result.is_err());
    }

    #[test]
    fn fuzzy_crlf_file() {
        // CRLF content, LF old_string, fuzzy → matches + CRLF preserved.
        let args = fuzzy_args("a.txt", "fn foo()", "fn bar()", false);
        let content = "fn\tfoo()\r\nnext\r\n";
        let prepared = prepare_edit(&args, content).unwrap();
        assert_eq!(prepared.new_content, "fn bar()\r\nnext\r\n");
    }

    #[test]
    fn fuzzy_ignored_in_regex_mode() {
        // use_regex=true + fuzzy=true: fuzzy is ignored, regex path runs.
        let args = FileEditArgs {
            path: "a.txt".into(),
            old_string: r"\d+".into(),
            new_string: "N".into(),
            replace_all: false,
            use_regex: true,
            count: None,
            start_line: None,
            end_line: None,
            fuzzy_whitespace: true, // ignored in regex mode
            edits: None,
            append: false,
        };
        let prepared = prepare_edit(&args, "a1 b2\n").unwrap();
        assert_eq!(prepared.new_content, "aN b2\n");
    }

    #[tokio::test]
    async fn fuzzy_edits_file_end_to_end() {
        let dir = tempdir().unwrap();
        // File uses a tab where the edit specifies a space.
        std::fs::write(dir.path().join("a.txt"), "fn\tmain() {}\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "path": "a.txt",
                "old_string": "fn main() {}",
                "new_string": "pub fn main() {}",
                "fuzzy_whitespace": true,
            }))
            .await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "pub fn main() {}\n"
        );
    }

    #[test]
    fn fuzzy_multibyte_utf8_in_match() {
        // A multi-byte UTF-8 char (é = 2 bytes) inside the matched region.
        // The offset mapping uses char.len_utf8() for the end bound — verify
        // it doesn't split the char or miscompute the splice region.
        let args = fuzzy_args("a.txt", "café au lait", "coffee", false);
        let prepared = prepare_edit(&args, "café\t au  lait\n").unwrap();
        assert_eq!(prepared.new_content, "coffee\n");
    }

    #[test]
    fn fuzzy_match_at_end_of_file() {
        // Match spanning to the very last byte (no trailing newline). The end
        // bound uses hay_offsets[n_end-1] + char_len; when n_end ==
        // norm_hay.len() this must still resolve correctly.
        let args = fuzzy_args("a.txt", "fn\tfoo()", "fn bar()", false);
        let prepared = prepare_edit(&args, "fn\tfoo()").unwrap();
        assert_eq!(prepared.new_content, "fn bar()");
    }

    #[test]
    fn fuzzy_preserves_leading_whitespace() {
        // Indentation before the match is preserved (outside the splice).
        let args = fuzzy_args("a.txt", "let x = 1;", "let y = 2;", false);
        let prepared = prepare_edit(&args, "    let x = 1;\n").unwrap();
        assert_eq!(prepared.new_content, "    let y = 2;\n");
    }

    #[test]
    fn fuzzy_internal_tab_included_in_splice() {
        // A tab BETWEEN two words is internal to the match — it's included in
        // the splice region (replaced), not preserved. "fn\tfoo()" matching
        // "fn foo()" → the whole "fn\tfoo()" is replaced by new_string.
        let args = fuzzy_args("a.txt", "fn foo()", "pub fn bar()", false);
        let prepared = prepare_edit(&args, "  fn\tfoo()  \n").unwrap();
        // Indent + trailing preserved; the "fn\tfoo()" region replaced.
        assert_eq!(prepared.new_content, "  pub fn bar()  \n");
    }

    // ---- multi-edit batch mode (edits) ----

    /// Build batch-mode args with empty single-edit fields.
    fn batch_args(path: &str, items: Vec<EditItem>) -> FileEditArgs {
        FileEditArgs {
            path: path.into(),
            old_string: String::new(),
            new_string: String::new(),
            replace_all: false,
            use_regex: false,
            count: None,
            start_line: None,
            end_line: None,
            fuzzy_whitespace: false,
            edits: Some(items),
            append: false,
        }
    }

    #[test]
    fn multi_edit_batch_applies_all_items_in_order() {
        // Three anchors applied IN ORDER: item 2's anchor ("fn a2() {}") is
        // CREATED by item 1, so a parallel/out-of-order application could
        // never produce this result.
        let args = batch_args(
            "a.rs",
            vec![
                EditItem {
                    old_string: "fn a() {}".into(),
                    new_string: "fn a2() {}".into(),
                    count: None,
                    fuzzy_whitespace: None,
                },
                EditItem {
                    old_string: "fn a2() {}".into(),
                    new_string: "fn a3() {}".into(),
                    count: None,
                    fuzzy_whitespace: None,
                },
                EditItem {
                    old_string: "fn b() {}".into(),
                    new_string: "fn b2() {}".into(),
                    count: None,
                    fuzzy_whitespace: None,
                },
            ],
        );
        let prepared = prepare_edit(&args, "fn a() {}\nfn b() {}\n").unwrap();
        assert_eq!(prepared.new_content, "fn a3() {}\nfn b2() {}\n");
    }

    #[tokio::test]
    async fn multi_edit_batch_is_atomic_on_failure() {
        // Item 2's anchor doesn't exist → the whole batch fails and the file
        // on disk stays byte-identical (NO write), with the error naming the
        // failing item's index and anchor.
        let dir = tempdir().unwrap();
        let original = "fn a() {}\nfn b() {}\n";
        std::fs::write(dir.path().join("a.rs"), original).unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "path": "a.rs",
                "edits": [
                    {"old_string": "fn a() {}", "new_string": "fn a2() {}"},
                    {"old_string": "fn missing() {}", "new_string": "fn x() {}"}
                ]
            }))
            .await;
        assert!(!result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            original
        );
        assert!(result.output.contains("edit 2/2"), "{}", result.output);
        assert!(result.output.contains("fn missing()"), "{}", result.output);
    }

    #[test]
    fn multi_edit_batch_rejects_ambiguous_anchor_without_count() {
        // "x" matches twice → rejected unless the item sets count.
        let args = batch_args(
            "a.txt",
            vec![EditItem {
                old_string: "x".into(),
                new_string: "y".into(),
                count: None,
                fuzzy_whitespace: None,
            }],
        );
        let err = prepare_edit(&args, "x\nx\n").unwrap_err();
        assert!(err.to_string().contains("ambiguous"), "{}", err);
        // With count: Some(2) the same batch succeeds, replacing both.
        let args = batch_args(
            "a.txt",
            vec![EditItem {
                old_string: "x".into(),
                new_string: "y".into(),
                count: Some(2),
                fuzzy_whitespace: None,
            }],
        );
        let prepared = prepare_edit(&args, "x\nx\n").unwrap();
        assert_eq!(prepared.new_content, "y\ny\n");
    }

    #[test]
    fn multi_edit_batch_renders_one_combined_diff() {
        // ONE diff covering every item's change (not one diff per item).
        let args = batch_args(
            "a.rs",
            vec![
                EditItem {
                    old_string: "fn a() {}".into(),
                    new_string: "fn a2() {}".into(),
                    count: None,
                    fuzzy_whitespace: None,
                },
                EditItem {
                    old_string: "fn b() {}".into(),
                    new_string: "fn b2() {}".into(),
                    count: None,
                    fuzzy_whitespace: None,
                },
            ],
        );
        let prepared = prepare_edit(&args, "fn a() {}\nfn b() {}\n").unwrap();
        assert!(prepared.diff.contains("-fn a() {}"), "{}", prepared.diff);
        assert!(prepared.diff.contains("+fn a2() {}"), "{}", prepared.diff);
        assert!(prepared.diff.contains("-fn b() {}"), "{}", prepared.diff);
        assert!(prepared.diff.contains("+fn b2() {}"), "{}", prepared.diff);
    }

    #[test]
    fn multi_edit_batch_rejects_single_edit_fields() {
        // Mutual exclusivity: edits + old_string / use_regex / line-range.
        let mut args = batch_args(
            "a.txt",
            vec![EditItem {
                old_string: "x".into(),
                new_string: "y".into(),
                count: None,
                fuzzy_whitespace: None,
            }],
        );
        args.old_string = "z".into();
        assert!(prepare_edit(&args, "x\n").is_err());

        let mut args = batch_args(
            "a.txt",
            vec![EditItem {
                old_string: "x".into(),
                new_string: "y".into(),
                count: None,
                fuzzy_whitespace: None,
            }],
        );
        args.use_regex = true;
        assert!(prepare_edit(&args, "x\n").is_err());

        let mut args = batch_args(
            "a.txt",
            vec![EditItem {
                old_string: "x".into(),
                new_string: "y".into(),
                count: None,
                fuzzy_whitespace: None,
            }],
        );
        args.start_line = Some(1);
        args.end_line = Some(1);
        assert!(prepare_edit(&args, "x\n").is_err());
    }

    // ---- append mode ----

    /// Build append-mode args with every other field at its default.
    fn append_args(path: &str, new_string: &str) -> FileEditArgs {
        FileEditArgs {
            path: path.into(),
            old_string: String::new(),
            new_string: new_string.into(),
            replace_all: false,
            use_regex: false,
            count: None,
            start_line: None,
            end_line: None,
            fuzzy_whitespace: false,
            edits: None,
            append: true,
        }
    }

    #[test]
    fn append_mode_adds_text_at_eof_in_file_style() {
        // CRLF file already ending in a newline: no extra prefix (no blank
        // line), appended block re-emitted in CRLF.
        let args = append_args("a.txt", "x\ny");
        let prepared = prepare_edit(&args, "a\r\nb\r\n").unwrap();
        assert_eq!(prepared.new_content, "a\r\nb\r\nx\r\ny");

        // LF file WITHOUT a trailing newline: the detected ending prefixes
        // the appended text so it starts on a fresh line.
        let prepared = prepare_edit(&args, "a\nb").unwrap();
        assert_eq!(prepared.new_content, "a\nb\nx\ny");

        // Empty file: no style to preserve — the caller's text verbatim.
        let prepared = prepare_edit(&args, "").unwrap();
        assert_eq!(prepared.new_content, "x\ny");
    }

    #[tokio::test]
    async fn append_mode_errors_when_file_missing() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({"path": "missing.txt", "append": true, "new_string": "hi"}))
            .await;
        assert!(!result.success, "{}", result.output);
        assert!(result.output.contains("file_write"), "{}", result.output);
    }

    #[test]
    fn append_mode_diff_is_pure_addition() {
        let args = append_args("a.txt", "tail");
        let prepared = prepare_edit(&args, "a\nb\n").unwrap();
        assert!(prepared.diff.contains("+tail"), "{}", prepared.diff);
        // No removal lines (the "---" file header aside).
        assert!(
            !prepared
                .diff
                .lines()
                .any(|l| l.starts_with('-') && !l.starts_with("---")),
            "{}",
            prepared.diff
        );
    }

    #[test]
    fn append_mode_rejects_conflicting_fields() {
        let mut args = append_args("a.txt", "tail");
        args.old_string = "x".into();
        assert!(prepare_edit(&args, "x\n").is_err());

        let mut args = append_args("a.txt", "tail");
        args.edits = Some(vec![EditItem {
            old_string: "x".into(),
            new_string: "y".into(),
            count: None,
            fuzzy_whitespace: None,
        }]);
        assert!(prepare_edit(&args, "x\n").is_err());

        let mut args = append_args("a.txt", "tail");
        args.start_line = Some(1);
        args.end_line = Some(1);
        assert!(prepare_edit(&args, "x\n").is_err());

        let mut args = append_args("a.txt", "tail");
        args.use_regex = true;
        assert!(prepare_edit(&args, "x\n").is_err());
    }

    #[test]
    fn multi_edit_batch_rejects_empty_anchor_with_clean_error() {
        // Review H1: an empty-anchor batch item used to reach a
        // subtraction-underflow panic in the EOL-agnostic core (an empty
        // needle matches at position 0 → map_lf_region_to_original(0, 0) →
        // lf_offsets[m_end - 1] underflow). Rejected with a clean
        // InvalidInput naming the item index instead.
        let args = batch_args(
            "a.txt",
            vec![EditItem {
                old_string: String::new(),
                new_string: "x".into(),
                count: None,
                fuzzy_whitespace: None,
            }],
        );
        let err = prepare_edit(&args, "content\n").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("empty old_string"), "{msg}");
        assert!(msg.contains("edit 1/1"), "{msg}");
    }

    #[tokio::test]
    async fn multi_edit_batch_empty_anchor_is_contained_on_the_approval_path() {
        // Review H1: the approval path (approval_preview →
        // prepare_for_approval → prepare_edit) runs synchronously on the
        // agent-loop task — a panic there would unwind the session mid-turn.
        // The empty-anchor item must surface as "no preview" (the Err mapped
        // through .ok()), not a panic.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "content\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool.approval_preview(&json!({
            "path": "a.txt",
            "edits": [{"old_string": "", "new_string": "x"}]
        }));
        assert!(
            result.is_none(),
            "expected the rejected batch to produce no preview, got {result:?}"
        );
    }

    #[test]
    fn multi_edit_batch_emission_checks_are_per_item() {
        // Review L2: the line-scoped artifact checks run per item's
        // new_string — a sentinel-shaped item is rejected even when other
        // items join around it (the join used to escape the exact-match).
        let args = batch_args(
            "a.rs",
            vec![
                EditItem {
                    old_string: "fn a() {}".into(),
                    new_string: "placeholder".into(),
                    count: None,
                    fuzzy_whitespace: None,
                },
                EditItem {
                    old_string: "fn b() {}".into(),
                    new_string: "fn b2() {}".into(),
                    count: None,
                    fuzzy_whitespace: None,
                },
            ],
        );
        let err = prepare_edit(&args, "fn a() {}\nfn b() {}\n").unwrap_err();
        assert!(err.to_string().contains("sentinel-shaped"), "{}", err);

        // Boundary doc lines shared between adjacent items are adjacent in
        // the JOIN but not in the spliced result (a non-adjacent item sits
        // between them) — the batch is accepted (the join used to
        // false-positive the dup-doc-line check).
        let args = batch_args(
            "a.rs",
            vec![
                EditItem {
                    old_string: "fn a() {}".into(),
                    new_string: "fn a2() {}\n/// foo".into(),
                    count: None,
                    fuzzy_whitespace: None,
                },
                EditItem {
                    old_string: "fn c() {}".into(),
                    new_string: "/// foo\nfn c2() {}".into(),
                    count: None,
                    fuzzy_whitespace: None,
                },
            ],
        );
        let prepared = prepare_edit(&args, "fn a() {}\nfn b() {}\nfn c() {}\n").unwrap();
        assert!(
            prepared
                .new_content
                .contains("/// foo\nfn b() {}\n/// foo"),
            "{}",
            prepared.new_content
        );
    }
}
