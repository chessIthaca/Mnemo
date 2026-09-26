// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `edit_ops` — the shared op engine for anchor and line-targeted edits.
//!
//! Home of the machinery both `file_edit` and `multi_edit` share: the
//! EOL-agnostic literal match/splice core (with the escape-normalization
//! fallbacks and near-miss diagnostics), the emission-artifact validators,
//! and the unified-diff helper. An anchor edit behaves identically whichever
//! tool carries it, because both route through here.

use std::path::Path;

use serde::{Deserialize, Serialize};
use similar::TextDiff;

use crate::agent::steering_stats::EDIT_STALE_READ_MARK;
use crate::error::Result;
use crate::tool::agent::line_endings::{
    denormalize_literal_newlines, detect_line_ending, normalize_line_endings,
};

/// One anchor edit in an ops array (file_edit's `ops` field, plan be16ea36
/// step 4): replace `old_string` with `new_string` — `count` widens the match
/// to the first N occurrences (default 1; required when the anchor matches
/// more than once), `fuzzy_whitespace` enables whitespace-tolerant matching
/// for THIS item (default: the array-level `fuzzy_whitespace`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct EditItem {
    /// The exact text to find (EOL-agnostic matching, as in single mode).
    pub old_string: String,
    /// The replacement text.
    pub new_string: String,
    /// Replace at most N occurrences of this item's `old_string` (default 1).
    #[serde(default)]
    pub count: Option<usize>,
    /// Whitespace-tolerant matching for this item (default: the array-level
    /// `fuzzy_whitespace`).
    #[serde(default)]
    pub fuzzy_whitespace: Option<bool>,
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
pub(crate) fn with_fresh_read_nudge(msg: impl std::fmt::Display) -> crate::error::Error {
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
pub(crate) fn validate_emission_artifacts(
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
pub(crate) fn validate_emission_artifacts_lines(
    path: &str,
    new_string: &str,
) -> crate::error::Result<()> {
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
pub(crate) fn validate_emission_artifacts_braces(
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
pub(crate) fn normalize_ws_with_offsets(s: &str) -> (String, Vec<usize>) {
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
pub(crate) fn fuzzy_find(haystack: &str, needle: &str) -> Option<(usize, usize)> {
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

/// How a literal anchor was matched (backlog 838b6f4e C): exactly, or via
/// one of the fallbacks that absorb the observed apostrophe trap — a
/// transport that JS-escapes quotes in the agent's view of the file (or
/// the reverse) used to hard-fail byte-exact matching. The non-Exact
/// origins ride the success output as a NOTE so a fallback is never
/// silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MatchOrigin {
    Exact,
    /// The needle carried JS escapes (\' → ') the file does not.
    Unescaped,
    /// The file carries JS escapes the needle does not (' → \').
    Escaped,
    /// Whitespace runs differ — fuzzy semantics applied automatically.
    WhitespaceNormalized,
}

impl MatchOrigin {
    /// The success-output NOTE for this origin (None for Exact — an exact
    /// match is the silent default).
    pub(crate) fn note(self) -> Option<&'static str> {
        match self {
            MatchOrigin::Exact => None,
            MatchOrigin::Unescaped => Some(
                "NOTE: matched via escape-normalization — old_string carried \
                 JS-escaped quotes (\\') the file does not; the unescaped \
                 variant was applied",
            ),
            MatchOrigin::Escaped => Some(
                "NOTE: matched via escape-normalization — the file carries \
                 JS-escaped quotes (\\') where old_string has plain ones; the \
                 escaped variant was applied",
            ),
            MatchOrigin::WhitespaceNormalized => Some(
                "NOTE: matched via whitespace normalization — runs of \
                 spaces/tabs differ between old_string and the file; fuzzy \
                 semantics were applied automatically",
            ),
        }
    }
}

/// Unescape the JS quote escapes (backlog 838b6f4e C): `\'` → `'` and
/// `\\` → `\`. Conservative — any other backslash sequence passes through
/// untouched (a literal `\d` stays `\d`).
fn unescape_js(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some('\'') | Some('\\') => {
                    out.push(chars.next().expect("peeked"));
                }
                _ => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The mirror direction: `'` → `\'` and `\` → `\\`.
fn escape_js(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\'' => out.push_str("\\'"),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape a short display snippet for the near-miss diagnostic (backlog
/// 838b6f4e D): newlines, tabs, and carriage returns become their escapes
/// so the message stays on one line.
fn escape_display(s: &str) -> String {
    s.replace('\n', "\\n").replace('\r', "\\r").replace('\t', "\\t")
}

/// The first-difference miss diagnostic (backlog 838b6f4e D): on a genuine
/// literal miss, find the position in the LF-projected content sharing the
/// longest common prefix with the needle (>= 8 chars) and report how far it
/// matched and where the two diverge — turning a blind retry loop into a
/// one-shot fix. Returns None when no position shares the threshold prefix
/// (nothing useful to say). Char-based (not bytes) so the reported position
/// and snippets are display-accurate.
pub(crate) fn near_miss_diagnostic(content: &str, old_string: &str) -> Option<String> {
    const MIN_PREFIX: usize = 8;
    let (content_lf, _) = normalize_lf_with_offsets(content);
    let needle: Vec<char> = normalize_line_endings(old_string, "\n").chars().collect();
    let hay: Vec<char> = content_lf.chars().collect();
    if needle.len() < MIN_PREFIX {
        return None;
    }
    // Work budget (review L3): bound the worst case — long runs of the
    // needle's first char (minified/generated content) times a long
    // needle would otherwise degrade to unbounded O(n·m) on the blocking
    // pool. Bail past the budget and report the best found so far.
    const MAX_COMPARED: usize = 10_000_000;
    let mut compared = 0usize;
    let mut best_start = usize::MAX;
    let mut best_len = 0usize;
    'scan: for start in 0..hay.len() {
        if hay[start] != needle[0] {
            continue;
        }
        let mut len = 0usize;
        while start + len < hay.len() && len < needle.len() && hay[start + len] == needle[len] {
            len += 1;
            compared += 1;
            if compared > MAX_COMPARED {
                break 'scan;
            }
        }
        if len > best_len {
            best_len = len;
            best_start = start;
        }
    }
    if best_len < MIN_PREFIX {
        return None;
    }
    let file_next: String = hay[best_start + best_len..].iter().take(4).collect();
    let sent_next: String = needle[best_len..].iter().take(4).collect();
    Some(format!(
        "near-miss: matched {}/{} chars; first difference at char {}: file \
         has '{}', you sent '{}'",
        best_len,
        needle.len(),
        best_len + 1,
        escape_display(&file_next),
        escape_display(&sent_next)
    ))
}

/// Resolve the effective needle for a literal match (backlog 838b6f4e C):
/// the exact LF-normalized needle when it occurs in the content; on a
/// miss, the unescaped variant, then the escaped variant, then
/// whitespace-normalized matching (when not already fuzzy). Returns the
/// needle to splice with and how it was chosen — a miss everywhere
/// returns the original needle with `Exact` (the callers report the miss;
/// `Exact`'s absent NOTE keeps that path unchanged).
pub(crate) fn resolve_match_variant(
    content_lf: &str,
    old_lf: &str,
    fuzzy: bool,
) -> (String, MatchOrigin) {
    if content_lf.contains(old_lf) {
        return (old_lf.to_string(), MatchOrigin::Exact);
    }
    let unescaped = unescape_js(old_lf);
    if unescaped != old_lf && content_lf.contains(&unescaped) {
        return (unescaped, MatchOrigin::Unescaped);
    }
    let escaped = escape_js(old_lf);
    if escaped != old_lf && content_lf.contains(&escaped) {
        return (escaped, MatchOrigin::Escaped);
    }
    if !fuzzy && fuzzy_find(content_lf, old_lf).is_some() {
        return (old_lf.to_string(), MatchOrigin::WhitespaceNormalized);
    }
    (old_lf.to_string(), MatchOrigin::Exact)
}

/// The literal splice's outcome (backlog 838b6f4e C): the new content plus
/// how the anchor was matched — the non-Exact origins become success-output
/// NOTEs.
pub(crate) struct SpliceOutcome {
    pub(crate) new_content: String,
    pub(crate) origin: MatchOrigin,
}

/// The literal edit's matching+splicing core, shared by the single-edit path
/// and the multi-edit batch (plan be16ea36 step 4): EOL-agnostic matching on
/// BOTH sides (the content is projected to LF with a byte-offset map back to
/// the original; the needle is normalized to LF), then `new_string` —
/// re-emitted in the file's detected (majority) ending style — is spliced
/// into the ORIGINAL bytes at the mapped offsets, at most `limit`
/// non-overlapping matches (fuzzy-whitespace matching when `fuzzy`). On an
/// exact miss, the escape-normalization fallbacks are tried first
/// (backlog 838b6f4e C — see [`resolve_match_variant`]); the chosen
/// origin rides the outcome for the success NOTE. Returns `None` when the
/// anchor matches nothing (after the fallbacks). Deliberately NO emission
/// validation and NO diff here: the single path adds both for its one edit,
/// while the batch path validates the COMBINED result once (a legitimate
/// two-step batch can be temporarily unbalanced mid-batch) and emits one
/// combined diff.
pub(crate) fn literal_splice(
    content: &str,
    old_string: &str,
    new_string: &str,
    fuzzy: bool,
    limit: usize,
) -> Result<Option<SpliceOutcome>> {
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
    // Variant resolution (backlog 838b6f4e C): on an exact miss, the
    // escape-normalized variants and whitespace-normalized matching absorb
    // the apostrophe trap before the loop gives up.
    let (old_lf, origin) = resolve_match_variant(&content_lf, &old_lf, fuzzy);
    let fuzzy = fuzzy || origin == MatchOrigin::WhitespaceNormalized;
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
    Ok(Some(SpliceOutcome {
        new_content: out,
        origin,
    }))
}

/// Count non-overlapping matches of `needle` in `content` using the same
/// EOL-agnostic (and optionally fuzzy-whitespace) matching as the literal
/// splice — the batch ambiguity guard's counter (plan be16ea36 step 4).
pub(crate) fn count_matches(content: &str, needle: &str, fuzzy: bool) -> usize {
    let (content_lf, _) = normalize_lf_with_offsets(content);
    let needle_lf = normalize_line_endings(needle, "\n");
    if needle_lf.is_empty() {
        return 0;
    }
    // Variant-aware (backlog 838b6f4e C): count with the same effective
    // needle the splice will use, so a variant-matched anchor's ambiguity
    // is still caught.
    let (needle_lf, origin) = resolve_match_variant(&content_lf, &needle_lf, fuzzy);
    let fuzzy = fuzzy || origin == MatchOrigin::WhitespaceNormalized;
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
pub(crate) fn batch_item_error(
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
pub(crate) fn anchor_excerpt(old_string: &str) -> String {
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
pub(crate) fn normalize_lf_with_offsets(s: &str) -> (String, Vec<usize>) {
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
pub(crate) fn map_lf_region_to_original(
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

/// One op in an ops array: either the compact line-op string form
/// (`'i101:text'`, `'d202-205'`, `'r102:x'`) or an anchor-based [`EditItem`]
/// object. One array carries both forms, so a single field covers compact
/// line-targeted ops and byte-exact anchor edits.
#[derive(Debug)]
pub(crate) enum EditOp {
    Line(LineOp),
    Anchor(EditItem),
}

/// The verbs of the compact line-op grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LineVerb {
    /// `i101:text` — insert the payload as new line(s) AFTER line 101;
    /// `i0:` inserts at the very top, `i<line count>:` appends at EOF.
    InsertAfter,
    /// `b101:text` — insert the payload BEFORE line 101 (`b1:` = top).
    InsertBefore,
    /// `d202-205` — delete lines 202-205 inclusive (`d202` single line,
    /// `d100-` through EOF).
    Delete,
    /// `r102:text` — replace the line (or the range `r100-120:text`) with
    /// the payload; the payload may be empty (one empty line).
    Replace,
}

/// The line span a compact op covers: one line, a bounded range, or
/// everything through EOF (`d100-`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Span {
    Single,
    Through(usize),
    ToEof,
}

/// A parsed compact line op — `{verb}{N|N-M|N-}[:payload]`, 1-indexed
/// inclusive, matching the `lines` tuple's semantics. Multi-line payloads
/// ride JSON newline escapes.
#[derive(Debug, Clone)]
pub(crate) struct LineOp {
    /// The original op text, echoed in error messages.
    pub(crate) raw: String,
    pub(crate) verb: LineVerb,
    /// The anchor line (1-indexed; `i0` is the one zero special: top of file).
    pub(crate) start: usize,
    pub(crate) span: Span,
    /// The text after the FIRST ':' — verbatim (only `r` may be empty).
    pub(crate) payload: Option<String>,
}

/// Parse the raw ops array into [`EditOp`]s: a string element is a compact
/// line op, an object element an anchor [`EditItem`]. Errors name the
/// element's index and its text and repeat the grammar, so a bad op never
/// needs a guessing round-trip.
pub(crate) fn parse_ops(values: &[serde_json::Value]) -> Result<Vec<EditOp>> {
    let mut ops = Vec::with_capacity(values.len());
    for (idx, value) in values.iter().enumerate() {
        match value {
            serde_json::Value::String(raw) => ops.push(parse_line_op(raw, idx)?),
            serde_json::Value::Object(_) => {
                let item: EditItem = serde_json::from_value(value.clone()).map_err(|e| {
                    crate::error::Error::InvalidInput(format!(
                        "ops[{idx}] is not a valid anchor item ({e}) — expected \
                         {{old_string, new_string, count?, fuzzy_whitespace?}}"
                    ))
                })?;
                ops.push(EditOp::Anchor(item));
            }
            _ => {
                return Err(crate::error::Error::InvalidInput(format!(
                    "ops[{idx}] is not a valid op — expected a compact op string \
                     (e.g. 'd202-205', 'i101:text', 'r102:x') or an anchor object \
                     {{old_string, new_string}}"
                )))
            }
        }
    }
    Ok(ops)
}

/// The compact-op error prefix: names the element, echoes the op, and
/// repeats the grammar, so every rejection is self-explanatory.
fn op_error(idx: usize, raw: &str, detail: impl std::fmt::Display) -> crate::error::Error {
    crate::error::Error::InvalidInput(format!(
        "ops[{idx}] '{raw}': {detail} — compact ops are \
         {{i|b|d|r}}{{N|N-M|N-}}[:payload], 1-indexed inclusive \
         (e.g. 'i101:text' insert after, 'b101:text' insert before, \
         'd202-205' delete, 'r102:text' replace)"
    ))
}

/// A drift-class compact-op rejection — kept `NotFound` so the stale-read
/// gate arms, exactly like the anchor path's misses.
fn op_drift_error(idx: usize, raw: &str, msg: impl std::fmt::Display) -> crate::error::Error {
    crate::error::Error::NotFound(format!("ops[{idx}] '{raw}': {msg}"))
}

/// Parse one compact line op: `{verb}{range}[:{payload}]`, split on the
/// FIRST ':' (the payload keeps any further colons and rides verbatim).
/// Insert verbs take a single line number; delete takes no payload; a
/// replace's payload may be empty (one empty line).
fn parse_line_op(raw: &str, idx: usize) -> Result<EditOp> {
    let mut chars = raw.chars();
    let verb_ch = chars
        .next()
        .ok_or_else(|| op_error(idx, raw, "the op is empty"))?;
    let verb = match verb_ch {
        'i' => LineVerb::InsertAfter,
        'b' => LineVerb::InsertBefore,
        'd' => LineVerb::Delete,
        'r' => LineVerb::Replace,
        other => {
            return Err(op_error(
                idx,
                raw,
                format!(
                    "unknown verb '{other}' — expected i (insert after), \
                     b (insert before), d (delete), or r (replace)"
                ),
            ))
        }
    };
    let rest = &raw[verb_ch.len_utf8()..];
    let (range_part, payload) = match rest.split_once(':') {
        Some((range, payload)) => (range, Some(payload.to_string())),
        None => (rest, None),
    };
    match verb {
        LineVerb::Delete => {
            if payload.is_some() {
                return Err(op_error(
                    idx,
                    raw,
                    "a delete takes no payload — use 'd202-205', for example",
                ));
            }
        }
        LineVerb::InsertAfter | LineVerb::InsertBefore => match payload.as_deref() {
            None => {
                return Err(op_error(
                    idx,
                    raw,
                    "missing ':' and the text to insert — use 'i101:text' or 'b101:text'",
                ))
            }
            Some("") => {
                return Err(op_error(
                    idx,
                    raw,
                    "the insert payload is empty — provide the text after ':'",
                ))
            }
            Some(_) => {}
        },
        LineVerb::Replace => {
            if payload.is_none() {
                return Err(op_error(
                    idx,
                    raw,
                    "missing ':' — a replace takes the replacement text after it \
                     (it may be empty: 'r102:' leaves one empty line)",
                ));
            }
        }
    }
    let (start, span) = parse_line_range(range_part, idx, raw)?;
    if matches!(verb, LineVerb::InsertAfter | LineVerb::InsertBefore) && span != Span::Single {
        return Err(op_error(
            idx,
            raw,
            "insert takes a single line number, not a range — use 'i101:text'",
        ));
    }
    if start == 0 && !matches!(verb, LineVerb::InsertAfter) {
        return Err(op_error(
            idx,
            raw,
            "line numbers are 1-indexed (only 'i0:' is special: top of file)",
        ));
    }
    Ok(EditOp::Line(LineOp {
        raw: raw.to_string(),
        verb,
        start,
        span,
        payload,
    }))
}

/// Parse a compact op's range: `N` | `N-M` | `N-` (a trailing '-' means
/// through EOF). Returns the 1-indexed start and the span.
fn parse_line_range(range: &str, idx: usize, raw: &str) -> Result<(usize, Span)> {
    if range.is_empty() {
        return Err(op_error(idx, raw, "missing the line number"));
    }
    let (start_part, end_part) = match range.split_once('-') {
        Some((start, end)) => (start, Some(end)),
        None => (range, None),
    };
    let start = parse_line_number(start_part, idx, raw)?;
    let span = match end_part {
        None => Span::Single,
        Some("") => Span::ToEof,
        Some(end) => {
            let end_line = parse_line_number(end, idx, raw)?;
            if end_line < start {
                return Err(op_error(
                    idx,
                    raw,
                    format!("the range end {end_line} is before its start {start}"),
                ));
            }
            Span::Through(end_line)
        }
    };
    Ok((start, span))
}

/// Parse one decimal line number (digits only).
fn parse_line_number(part: &str, idx: usize, raw: &str) -> Result<usize> {
    if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
        return Err(op_error(
            idx,
            raw,
            format!("'{part}' is not a line number (digits only)"),
        ));
    }
    part.parse::<usize>()
        .map_err(|_| op_error(idx, raw, format!("'{part}' is too large for a line number")))
}

/// The engine's item-level validation (moved from file_edit's batch
/// validation, plan be16ea36 step 4): every ANCHOR op needs a non-empty
/// `old_string` and a real change — an empty anchor matches everywhere
/// (the empty-needle hazard the single mode rejects) and used to reach a
/// subtraction underflow in the EOL-agnostic core; and the observed incident
/// shape (old_string == new_string) is rejected pre-write. Line ops need
/// neither here — their bounds are checked against the content at apply
/// time.
pub(crate) fn validate_op_items(ops: &[EditOp]) -> Result<()> {
    for (idx, op) in ops.iter().enumerate() {
        let EditOp::Anchor(item) = op else { continue };
        if item.old_string.is_empty() {
            return Err(crate::error::Error::InvalidInput(format!(
                "edit {}/{} has an empty old_string — every batch item needs an \
                 anchor (an empty anchor matches everywhere and would corrupt \
                 the file)",
                idx + 1,
                ops.len()
            )));
        }
        if item.old_string == item.new_string {
            return Err(crate::error::Error::InvalidInput(format!(
                "no-op edit: edit {}/{} has old_string == new_string — nothing \
                 to change",
                idx + 1,
                ops.len()
            )));
        }
    }
    Ok(())
}

/// Apply every op IN ORDER to the in-memory content and return the combined
/// new content plus success notes. The caller writes ONCE, so any failing op
/// aborts the whole array with every file untouched on disk (atomicity).
///
/// Anchor ops behave exactly as `file_edit`'s batch always has (ambiguity
/// guard, escape-normalization fallbacks, drift-class misses with the
/// first-difference pointer, per-item success notes). Line ops are matched
/// against the content AS LEFT by the preceding ops — the same contract the
/// anchors have, so a mixed array's line numbers reflect every earlier op.
/// Emission artifacts are validated per op on its payload and once on the
/// combined result's brace delta; the diff itself is the caller's job.
pub(crate) fn apply_ops(
    path: &str,
    content: &str,
    ops: &[EditOp],
    fuzzy_default: bool,
) -> Result<(String, Vec<String>)> {
    let le = detect_line_ending(content);
    let total = ops.len();
    let mut current = content.to_string();
    let mut notes: Vec<String> = Vec::new();
    for (idx, op) in ops.iter().enumerate() {
        match op {
            EditOp::Anchor(item) => {
                let fuzzy = item.fuzzy_whitespace.unwrap_or(fuzzy_default);
                // Ambiguity guard: without an explicit count the anchor must
                // match exactly once in the content as left by the preceding
                // ops.
                if item.count.is_none() {
                    let occurrences = count_matches(&current, &item.old_string, fuzzy);
                    if occurrences > 1 {
                        return Err(crate::error::Error::InvalidInput(format!(
                            "edit {}/{} is ambiguous: its old_string ('{}') matches \
                             {occurrences} times — pass count on the item to replace \
                             more than one occurrence",
                            idx + 1,
                            total,
                            anchor_excerpt(&item.old_string)
                        )));
                    }
                }
                let limit = item.count.unwrap_or(1).max(1);
                match literal_splice(&current, &item.old_string, &item.new_string, fuzzy, limit)? {
                    Some(SpliceOutcome {
                        new_content: next,
                        origin,
                    }) if next != current => {
                        if let Some(note) = origin.note() {
                            notes.push(format!("edit {}/{total}: {note}", idx + 1));
                        }
                        current = next;
                    }
                    Some(_) => {
                        return Err(batch_item_error(
                            idx,
                            total,
                            &item.old_string,
                            crate::error::Error::NotFound(
                                "the item produced no change (new_string equals the matched \
                                 region after line-ending normalization)"
                                    .into(),
                            ),
                        ))
                    }
                    None => {
                        // First-difference pointer (backlog 838b6f4e D), scoped
                        // to the content as left by the preceding ops.
                        let detail = near_miss_diagnostic(&current, &item.old_string)
                            .map(|d| format!(" — {d}"))
                            .unwrap_or_default();
                        return Err(batch_item_error(
                            idx,
                            total,
                            &item.old_string,
                            with_fresh_read_nudge(format!(
                                "old_string not found in file{detail}"
                            )),
                        ));
                    }
                }
            }
            EditOp::Line(line_op) => {
                current = apply_line_op(&current, &le, line_op, idx)?;
            }
        }
    }
    // Per-op line-scoped emission checks (review L2): the ops' join would
    // weaken the sentinel exact-match (a longer join escapes it) and
    // false-positive the dup-doc-line check across op boundaries (two ops'
    // boundary lines are adjacent in the join but not in the spliced
    // result).
    for op in ops {
        match op {
            EditOp::Anchor(item) => validate_emission_artifacts_lines(path, &item.new_string)?,
            EditOp::Line(line_op) => {
                if let Some(payload) = line_op.payload.as_deref() {
                    if !payload.is_empty() {
                        validate_emission_artifacts_lines(path, payload)?;
                    }
                }
            }
        }
    }
    // One combined brace-delta check for the whole array (delta-scoped, so
    // the combined result is the right scope).
    validate_emission_artifacts_braces(path, content, &current)?;
    if current == content {
        return Err(crate::error::Error::InvalidInput(
            "the ops produced no change — the result equals the current content".into(),
        ));
    }
    Ok((current, notes))
}

/// Apply one compact line op to `content` (the running result) and return
/// the new content. Line numbers are 1-indexed inclusive against `content`
/// as it stands; bounds mirror the `lines` tuple's semantics — a range end
/// clamps to the last line (a start beyond EOF is a drift-class error).
fn apply_line_op(content: &str, le: &str, op: &LineOp, idx: usize) -> Result<String> {
    let lines: Vec<&str> = content.lines().collect();
    let count = lines.len();
    let payload_lines: Vec<String> = match op.payload.as_deref() {
        Some(payload) => {
            let normalized = denormalize_literal_newlines(&normalize_line_endings(payload, le));
            normalized.split(le).map(|line| line.to_string()).collect()
        }
        None => Vec::new(),
    };
    match op.verb {
        LineVerb::InsertAfter => {
            if op.start > count {
                return Err(op_drift_error(
                    idx,
                    &op.raw,
                    with_fresh_read_nudge(format!(
                        "line {} is past the end of the file ({count} lines) — \
                         nothing to insert after",
                        op.start
                    )),
                ));
            }
            let mut parts: Vec<String> = Vec::with_capacity(count + payload_lines.len());
            parts.extend(lines[..op.start].iter().map(|line| line.to_string()));
            parts.extend(payload_lines);
            parts.extend(lines[op.start..].iter().map(|line| line.to_string()));
            Ok(join_lines(parts, le, content))
        }
        LineVerb::InsertBefore => {
            if count == 0 {
                return Err(op_error(
                    idx,
                    &op.raw,
                    "the file is empty — use 'i0:<text>' to insert its first line",
                ));
            }
            if op.start > count {
                return Err(op_drift_error(
                    idx,
                    &op.raw,
                    with_fresh_read_nudge(format!(
                        "line {} is past the end of the file ({count} lines) — \
                         nothing to insert before",
                        op.start
                    )),
                ));
            }
            let mut parts: Vec<String> = Vec::with_capacity(count + payload_lines.len());
            parts.extend(lines[..op.start - 1].iter().map(|line| line.to_string()));
            parts.extend(payload_lines);
            parts.extend(lines[op.start - 1..].iter().map(|line| line.to_string()));
            Ok(join_lines(parts, le, content))
        }
        LineVerb::Delete => {
            if count == 0 {
                return Err(op_drift_error(
                    idx,
                    &op.raw,
                    with_fresh_read_nudge("no lines to delete (the file is empty)"),
                ));
            }
            if op.start > count {
                return Err(op_drift_error(
                    idx,
                    &op.raw,
                    with_fresh_read_nudge(format!(
                        "line {} is past the end of the file ({count} lines)",
                        op.start
                    )),
                ));
            }
            let end_idx = span_end_index(&op.span, op.start, count);
            let mut parts: Vec<String> = Vec::with_capacity(count);
            parts.extend(lines[..op.start - 1].iter().map(|line| line.to_string()));
            parts.extend(lines[end_idx + 1..].iter().map(|line| line.to_string()));
            Ok(join_lines(parts, le, content))
        }
        LineVerb::Replace => {
            if count == 0 {
                return Err(op_drift_error(
                    idx,
                    &op.raw,
                    with_fresh_read_nudge("no lines to replace (the file is empty)"),
                ));
            }
            if op.start > count {
                return Err(op_drift_error(
                    idx,
                    &op.raw,
                    with_fresh_read_nudge(format!(
                        "line {} is past the end of the file ({count} lines)",
                        op.start
                    )),
                ));
            }
            let end_idx = span_end_index(&op.span, op.start, count);
            let mut parts: Vec<String> = Vec::with_capacity(count + payload_lines.len());
            parts.extend(lines[..op.start - 1].iter().map(|line| line.to_string()));
            parts.extend(payload_lines);
            parts.extend(lines[end_idx + 1..].iter().map(|line| line.to_string()));
            let next = join_lines(parts, le, content);
            if next == content {
                return Err(op_error(
                    idx,
                    &op.raw,
                    "produced no change — the payload equals the replaced line(s)",
                ));
            }
            Ok(next)
        }
    }
}

/// The 0-based index of the last line a span covers. The start is already
/// validated to be within the file; a bounded end clamps to the last line.
fn span_end_index(span: &Span, start: usize, count: usize) -> usize {
    match span {
        Span::Single => start - 1,
        Span::Through(end) => (*end).min(count) - 1,
        Span::ToEof => count - 1,
    }
}

/// Rebuild the file from its line parts: join with the detected ending and
/// re-append the trailing newline only when the original carried one AND
/// lines remain — deleting every line yields empty content, not a lone
/// newline.
fn join_lines(parts: Vec<String>, le: &str, original: &str) -> String {
    let mut out = parts.join(le);
    if !parts.is_empty() && original.ends_with('\n') {
        out.push_str(le);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Parse an ops array literal and apply it to `content`.
    fn apply(
        path: &str,
        content: &str,
        values: &[serde_json::Value],
    ) -> Result<(String, Vec<String>)> {
        let ops = parse_ops(values)?;
        apply_ops(path, content, &ops, false)
    }

    fn apply_ok(content: &str, values: &[serde_json::Value]) -> String {
        apply("a.txt", content, values).expect("ops should apply").0
    }

    fn apply_err(path: &str, content: &str, values: &[serde_json::Value]) -> String {
        apply(path, content, values)
            .expect_err("ops should fail")
            .to_string()
    }

    fn line_ops(raw: &[&str]) -> Vec<serde_json::Value> {
        raw.iter().map(|s| json!(s)).collect()
    }

    fn line(op: &EditOp) -> &LineOp {
        match op {
            EditOp::Line(line) => line,
            EditOp::Anchor(_) => panic!("expected a line op"),
        }
    }

    #[test]
    fn parse_covers_every_verb_and_span() {
        let values = line_ops(&[
            "i101:text",
            "b5:x",
            "d202",
            "d202-205",
            "d100-",
            "r102:a:b",
            "r100-120:t",
        ]);
        let ops = parse_ops(&values).expect("parse");
        let expected = [
            (LineVerb::InsertAfter, 101, Span::Single),
            (LineVerb::InsertBefore, 5, Span::Single),
            (LineVerb::Delete, 202, Span::Single),
            (LineVerb::Delete, 202, Span::Through(205)),
            (LineVerb::Delete, 100, Span::ToEof),
            (LineVerb::Replace, 102, Span::Single),
            (LineVerb::Replace, 100, Span::Through(120)),
        ];
        for (op, (verb, start, span)) in ops.iter().zip(expected) {
            let op = line(op);
            assert_eq!(op.verb, verb);
            assert_eq!(op.start, start);
            assert_eq!(op.span, span);
        }
        assert_eq!(line(&ops[0]).payload.as_deref(), Some("text"));
        // The payload splits on the FIRST ':' and keeps every later colon.
        assert_eq!(line(&ops[5]).payload.as_deref(), Some("a:b"));
    }

    #[test]
    fn parse_accepts_anchor_objects_in_the_same_array() {
        let values = vec![json!("d1"), json!({"old_string": "a", "new_string": "b"})];
        let ops = parse_ops(&values).expect("parse");
        assert!(matches!(&ops[0], EditOp::Line(_)));
        match &ops[1] {
            EditOp::Anchor(item) => {
                assert_eq!(item.old_string, "a");
                assert_eq!(item.new_string, "b");
            }
            EditOp::Line(_) => panic!("expected an anchor"),
        }
    }

    #[test]
    fn parse_errors_name_the_element_and_the_grammar() {
        for (raw, needle) in [
            ("x102:text", "unknown verb"),
            ("", "the op is empty"),
            ("d202:x", "takes no payload"),
            ("i101", "missing ':'"),
            ("i101:", "payload is empty"),
            ("r102", "missing ':'"),
            ("d0", "1-indexed"),
            ("d5-2", "before its start"),
            ("dx", "not a line number"),
            ("i1-2:x", "single line number"),
            ("d99999999999999999999999", "too large"),
        ] {
            let err = parse_ops(&line_ops(&[raw])).unwrap_err().to_string();
            assert!(err.contains("ops[0]"), "{raw}: {err}");
            assert!(err.contains(needle), "{raw}: {err}");
            assert!(err.contains("compact ops are"), "{raw}: {err}");
        }
    }

    #[test]
    fn parse_rejects_unsupported_element_kinds() {
        let err = parse_ops(&[json!(7)]).unwrap_err().to_string();
        assert!(err.contains("ops[0]"), "{err}");
        assert!(err.contains("not a valid op"), "{err}");
        let err = parse_ops(&[json!({"new_string": "b"})]).unwrap_err().to_string();
        assert!(err.contains("not a valid anchor item"), "{err}");
    }

    #[test]
    fn insert_after_before_top_and_eof() {
        assert_eq!(apply_ok("a\nb\nc\n", &line_ops(&["i1:X"])), "a\nX\nb\nc\n");
        assert_eq!(apply_ok("a\nb\nc\n", &line_ops(&["b2:X"])), "a\nX\nb\nc\n");
        assert_eq!(apply_ok("a\nb\nc\n", &line_ops(&["i0:X"])), "X\na\nb\nc\n");
        assert_eq!(apply_ok("a\nb\nc\n", &line_ops(&["b1:X"])), "X\na\nb\nc\n");
        assert_eq!(apply_ok("a\nb\nc\n", &line_ops(&["i3:X"])), "a\nb\nc\nX\n");
        // A file without a trailing newline keeps its shape on an EOF insert.
        assert_eq!(apply_ok("a\nb\nc", &line_ops(&["i3:X"])), "a\nb\nc\nX");
        // A multi-line payload inserts one line per split.
        assert_eq!(apply_ok("a\nb\n", &line_ops(&["i1:X\nY"])), "a\nX\nY\nb\n");
    }

    #[test]
    fn insert_into_empty_file_and_before_requires_lines() {
        assert_eq!(apply_ok("", &line_ops(&["i0:text"])), "text");
        assert_eq!(apply_ok("", &line_ops(&["i0:a\nb"])), "a\nb");
        // `b` has nothing to insert before in an empty file — steered to i0.
        let err = apply_err("a.txt", "", &line_ops(&["b1:x"]));
        assert!(err.contains("the file is empty"), "{err}");
        assert!(err.contains("i0"), "{err}");
        // A usage error, not a drift-class failure: no fresh-read nudge.
        assert!(!err.contains(EDIT_STALE_READ_MARK), "{err}");
    }

    #[test]
    fn past_eof_errors_are_drift_class_and_name_the_op() {
        for raw in ["i5:x", "b5:x", "d5", "d5-", "r5:x", "r5-9:x"] {
            let err = apply_err("a.txt", "a\nb\n", &line_ops(&[raw]));
            assert!(err.contains("ops[0]"), "{raw}: {err}");
            assert!(
                err.contains("past the end of the file (2 lines)"),
                "{raw}: {err}"
            );
            assert!(err.contains(EDIT_STALE_READ_MARK), "{raw}: {err}");
        }
    }

    #[test]
    fn delete_single_range_to_eof_and_clamp() {
        assert_eq!(apply_ok("a\nb\nc\n", &line_ops(&["d2"])), "a\nc\n");
        assert_eq!(apply_ok("a\nb\nc\nd\n", &line_ops(&["d2-3"])), "a\nd\n");
        assert_eq!(apply_ok("a\nb\nc\nd\n", &line_ops(&["d2-"])), "a\n");
        // A range end past EOF clamps to the last line (mirrors `lines`).
        assert_eq!(apply_ok("a\nb\nc\nd\n", &line_ops(&["d2-99"])), "a\n");
        // Deleting the last line of a trailing-newline file keeps one.
        assert_eq!(apply_ok("a\nb\n", &line_ops(&["d2"])), "a\n");
        // Deleting every line yields empty content, not a lone newline.
        assert_eq!(apply_ok("a\nb\n", &line_ops(&["d1-"])), "");
        assert_eq!(apply_ok("a\nb", &line_ops(&["d1-2"])), "");
    }

    #[test]
    fn replace_single_range_multiline_and_empty_payload() {
        assert_eq!(apply_ok("a\nb\nc\n", &line_ops(&["r2:X"])), "a\nX\nc\n");
        assert_eq!(
            apply_ok("a\nb\nc\nd\n", &line_ops(&["r2-3:X\nY"])),
            "a\nX\nY\nd\n"
        );
        // An empty payload replaces the line with one empty line.
        assert_eq!(apply_ok("a\nb\nc\n", &line_ops(&["r2:"])), "a\n\nc\n");
        // An open-ended replace runs through EOF; a file without a trailing
        // newline keeps its shape.
        assert_eq!(apply_ok("a\nb\nc\n", &line_ops(&["r2-:X"])), "a\nX\n");
        assert_eq!(apply_ok("a\nb", &line_ops(&["r2:X"])), "a\nX");
    }

    #[test]
    fn replace_and_delete_on_empty_file_are_drift_class() {
        for raw in ["d1", "d1-", "r1:x"] {
            let err = apply_err("a.txt", "", &line_ops(&[raw]));
            assert!(err.contains("file is empty"), "{raw}: {err}");
            assert!(err.contains(EDIT_STALE_READ_MARK), "{raw}: {err}");
        }
    }

    #[test]
    fn crlf_files_keep_their_style() {
        assert_eq!(apply_ok("a\r\nb\r\nc\r\n", &line_ops(&["d2"])), "a\r\nc\r\n");
        assert_eq!(
            apply_ok("a\r\nb\r\nc\r\n", &line_ops(&["i1:X"])),
            "a\r\nX\r\nb\r\nc\r\n"
        );
        // A multi-line payload is re-emitted in the file's detected style.
        assert_eq!(
            apply_ok("a\r\nb\r\n", &line_ops(&["r2:X\nY"])),
            "a\r\nX\r\nY\r\n"
        );
    }

    #[test]
    fn payload_literal_backslash_n_is_denormalized() {
        // The double-escape safety net (mirrors line-range mode): a payload
        // with no real newlines but literal backslash-n text is denormalized.
        assert_eq!(
            apply_ok("a\nb\nc\n", &line_ops(&[r"r2:X\nY"])),
            "a\nX\nY\nc\n"
        );
    }

    #[test]
    fn no_change_replace_and_net_no_change_array_are_rejected() {
        let err = apply_err("a.txt", "a\nb\nc\n", &line_ops(&["r2:b"]));
        assert!(err.contains("produced no change"), "{err}");
        // Compensating ops that net out are rejected too.
        let err = apply_err("a.txt", "a\nb\nc\n", &line_ops(&["i1:X", "d2"]));
        assert!(err.contains("the ops produced no change"), "{err}");
    }

    #[test]
    fn mixed_anchor_and_line_ops_apply_in_order() {
        let values = vec![
            json!({"old_string": "fn a() {}", "new_string": "fn a2() {}"}),
            json!("d1"),
        ];
        assert_eq!(apply_ok("fn a() {}\nx\ny\n", &values), "x\ny\n");
    }

    #[test]
    fn line_numbers_refer_to_the_running_content() {
        // The second op sees the first op's result: d1 removes 'a', so the
        // old line 2 ('b') is now line 1.
        assert_eq!(apply_ok("a\nb\nc\n", &line_ops(&["d1", "d1"])), "c\n");
    }

    #[test]
    fn anchor_guards_still_apply_inside_ops() {
        let err = apply_err(
            "a.txt",
            "x\nx\n",
            &[json!({"old_string": "x", "new_string": "y"})],
        );
        assert!(err.contains("is ambiguous"), "{err}");
        let err = apply_err(
            "a.txt",
            "hello\n",
            &[json!({"old_string": "hullo", "new_string": "x"})],
        );
        assert!(err.contains("old_string not found in file"), "{err}");
        assert!(err.contains(EDIT_STALE_READ_MARK), "{err}");
    }

    #[test]
    fn validate_op_items_rejects_empty_and_no_op_anchors() {
        let ops = parse_ops(&[json!({"old_string": "", "new_string": "x"})]).expect("parse");
        let err = validate_op_items(&ops).unwrap_err().to_string();
        assert!(err.contains("empty old_string"), "{err}");
        let ops = parse_ops(&[json!({"old_string": "a", "new_string": "a"})]).expect("parse");
        let err = validate_op_items(&ops).unwrap_err().to_string();
        assert!(err.contains("old_string == new_string"), "{err}");
        // Line ops are unaffected by the anchor item checks.
        let ops = parse_ops(&line_ops(&["d1"])).expect("parse");
        assert!(validate_op_items(&ops).is_ok());
    }

    #[test]
    fn emission_artifact_checks_cover_line_payloads() {
        let err = apply_err("a.rs", "fn main() {}\n", &line_ops(&["i1:## heading"]));
        assert!(err.contains("emission artifact"), "{err}");
        let err = apply_err("a.rs", "fn main() {}\n", &line_ops(&["i1:if x {"]));
        assert!(err.contains("unbalanced delimiters"), "{err}");
    }
}
