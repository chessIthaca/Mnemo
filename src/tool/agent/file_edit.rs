// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! `file_edit` — string-replace edits with diff-based approval.
//!
//! Takes an old/new string pair, applies the replacement, and the approval
//! prompt renders a unified diff (via the `similar` crate) so you see exactly
//! what changes before approving. An ops array (`ops`) applies several edits
//! atomically — compact line ops (i/b/d/r verbs, ranges, payloads) and
//! anchor items in order, one write, one combined diff; any failing op
//! aborts with the file untouched.
//!
//! Byte-exactness (backlog 838b6f4e): `old_string` matches byte-exact —
//! JS-escaped apostrophes (`\'`), backslashes (`\\`), and line endings are
//! literal. On a miss, the escape-normalization fallbacks (unescaped/
//! escaped variants, whitespace normalization) absorb the apostrophe trap
//! and say so in a success NOTE; a genuine miss reports the first
//! difference. If the anchor contains a quote, a backslash, or is >3
//! lines, prefer line-range mode (`lines`).
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

use crate::error::Result;
use crate::provider::{ApprovalPreview, ToolSchema};
use crate::tool::agent::edit_ops::{
    apply_ops, literal_splice, near_miss_diagnostic, parse_ops, validate_emission_artifacts,
    validate_op_items, with_fresh_read_nudge, EditOp, MatchOrigin, SpliceOutcome,
};
use crate::tool::agent::line_endings::{
    denormalize_literal_newlines, detect_line_ending, normalize_line_endings,
};
use crate::tool::agent::sandbox::Sandbox;
use crate::tool::{SafetyLevel, Tool, ToolCategory, ToolResult};

pub use crate::tool::agent::edit_ops::{compute_diff, EditItem};

/// Arguments for `file_edit`.
#[derive(Debug, Deserialize)]
pub struct FileEditArgs {
    pub path: String,
    /// The exact text to find (or a regex when `use_regex=true`). Required for
    /// string-matching mode; ignored when `lines` is set (line-range mode).
    /// Defaults to empty so line-range calls can omit it.
    #[serde(default, deserialize_with = "crate::tool::null_to_default")]
    pub old_string: String,
    /// The replacement text. Defaults to empty so multi-edit batch calls
    /// (`ops`) can omit it — the ops carry their own replacements. An
    /// empty `new_string` in single mode deletes `old_string`.
    #[serde(default, deserialize_with = "crate::tool::null_to_default")]
    pub new_string: String,
    #[serde(default, deserialize_with = "crate::tool::null_to_default")]
    pub replace_all: bool,
    /// Treat `old_string` as a Rust regular expression (default: false).
    /// Capture groups from the pattern can be referenced in `new_string`
    /// with `$1`, `$2`, … or `${name}` (regex-crate `Replacer` semantics).
    #[serde(default, deserialize_with = "crate::tool::null_to_default")]
    pub use_regex: bool,
    /// Replace at most N matches (vi-style `:s/…/…/` with a count). When
    /// `None`: `replace_all=false` replaces the first match only and
    /// `replace_all=true` replaces every match. When set, it takes precedence
    /// over `replace_all`.
    #[serde(default)]
    pub count: Option<usize>,
    /// Line-range edit mode (backlog 838b6f4e): `lines` is exactly
    /// `[start, end]` — 1-indexed, inclusive — and that range is replaced
    /// by `new_string`. No `old_string` matching is needed (an alternative
    /// to string matching that avoids whitespace-mismatch failures). One
    /// tuple parameter instead of a start/end pair: two independent
    /// optionals could be half-filled ("must both be set" — the observed
    /// failure class); a single array cannot. Mutually exclusive with
    /// `old_string`/`use_regex`/`replace_all`/`count`.
    #[serde(default)]
    pub lines: Option<Vec<usize>>,
    /// When true (literal mode only), locate `old_string` by normalizing
    /// whitespace — collapse runs of spaces/tabs to a single space and ignore
    /// trailing whitespace per line — so tab-vs-space or off-by-one-space
    /// mismatches still match. The replacement is spliced into the original
    /// content verbatim (surrounding whitespace preserved). Ignored in regex
    /// and line-range modes.
    #[serde(default, deserialize_with = "crate::tool::null_to_default")]
    pub fuzzy_whitespace: bool,
    /// Ops array (plan 2e27f896, extending the batch of plan be16ea36): the
    /// edits to apply IN ORDER — each element is either a compact line-op
    /// string or an anchor object `{old_string, new_string, count?,
    /// fuzzy_whitespace?}`. Compact ops: `{i|b|d|r}{N|N-M|N-}[:payload]`,
    /// 1-indexed inclusive — `i101:text` inserts after line 101 (`i0:` top,
    /// `i<line count>:` EOF), `b101:text` inserts before it, `d202-205`
    /// deletes (`d202` one line, `d100-` to EOF), `r102:text` replaces
    /// (`r100-120:text` a range; `r102:` leaves one empty line). Line numbers
    /// refer to the content as left by the preceding ops; an anchor must
    /// match exactly once unless it sets `count`. The result is written ONCE
    /// — any failing op aborts the whole array with NO write (the file stays
    /// byte-identical). Mutually exclusive with the single-edit fields: leave
    /// `old_string`/`new_string` empty and do not set `use_regex`/
    /// `replace_all`/`count`/`lines` (the array-level `fuzzy_whitespace` is
    /// each anchor's default). The legacy `edits` key is accepted as an alias.
    #[serde(default, alias = "edits")]
    pub ops: Option<Vec<serde_json::Value>>,
    /// Append mode (plan be16ea36 step 5): when true, `new_string` is added
    /// at EOF instead of replacing a match — no `old_string` is needed. The
    /// appended text starts on a fresh line (prefixed by the file's detected
    /// line ending when the file doesn't already end with one) and is
    /// re-emitted in the file's detected style; an empty file takes the
    /// caller's text verbatim (no style to preserve). Mutually exclusive
    /// with `ops`, line-range mode, `old_string`, and the matching knobs
    /// (`use_regex`/`replace_all`/`count`/`fuzzy_whitespace`). Errors when
    /// the file is missing — use `file_write` to create it.
    #[serde(default, deserialize_with = "crate::tool::null_to_default")]
    pub append: bool,
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
    /// Success-output notes (backlog 838b6f4e C): match-origin fallback
    /// notes ("matched via escape-normalization") — `execute` appends them
    /// to the result output so a fallback is never silent; the approval
    /// path ignores them.
    pub notes: Vec<String>,
}

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
    // Ops mode measures each element — the largest single emitted fragment
    // is what the advisory is about (anchor objects: their old/new pair;
    // compact line ops: the whole op string, payload included).
    if let Some(ops) = args.ops.as_deref() {
        let largest = ops
            .iter()
            .map(|value| match value {
                serde_json::Value::String(raw) => raw.len(),
                other => serde_json::from_value::<EditItem>(other.clone())
                    .map(|item| item.old_string.len().max(item.new_string.len()))
                    .unwrap_or(0),
            })
            .max()
            .unwrap_or(0);
        return (largest >= EMISSION_FRAGILITY_PAYLOAD_BYTES).then(|| {
            format!(
                "NOTE: large edit payload (largest op ~{largest} chars) — \
                 emission fragility has been observed near this size in long \
                 sessions; use smaller fragments, or file_write for a full-file \
                 replacement (backlog e8b39d72 H5)"
            )
        });
    }
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

/// Apply the edit to the file content, returning the new content + diff.
/// This does NOT write to disk — it's used to prepare the approval preview.
///
/// Dispatches to the multi-edit batch, line-range, regex, or literal
/// implementation depending on the args. The ops array (`ops`) takes
/// precedence (it is mutually exclusive with the single-edit fields);
/// line-range mode (`lines`) requires no `old_string`; otherwise the
/// `use_regex`/literal path runs.
pub fn prepare_edit(args: &FileEditArgs, content: &str) -> Result<PreparedEdit> {
    if args.ops.is_some() {
        prepare_edit_ops(args, content)
    } else if args.append {
        prepare_edit_append(args, content)
    } else if args.lines.is_some() {
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
                 lines: [start, end] (line range). If you meant the literal text \
                 'null' (dropped by the null-stringify defense), use batch mode \
                 (an ops anchor item's old_string is required, never dropped) or \
                 use_regex (e.g. nul[l])"
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

/// Line-range edit path: replace the `lines` range — `[start, end]`
/// (1-indexed, inclusive) — with `new_string`. No `old_string` matching is
/// needed — the caller identifies the region by line number (as shown by
/// `file_read`'s 1-indexed line prefixes), so whitespace/indentation
/// mismatch cannot cause a failure. The file's line-ending style is
/// preserved: `new_string` is normalized to the file's detected style
/// before insertion.
fn prepare_edit_lines(args: &FileEditArgs, content: &str) -> Result<PreparedEdit> {
    // Exactly [start, end] (backlog 838b6f4e): one tuple parameter — the
    // half-filled start/end pair ("must both be set") is unrepresentable.
    let bounds = args.lines.as_deref().unwrap_or(&[]);
    if bounds.len() != 2 {
        return Err(crate::error::Error::InvalidInput(format!(
            "lines must be exactly [start, end] — got {} element(s). Expected \
             arguments: {{\"path\": \"<file>\", \"lines\": [<1-indexed N>, \
             <1-indexed M>], \"new_string\": \"<replacement>\"}} — or use string \
             mode (old_string + new_string) instead",
            bounds.len()
        )));
    }
    let (start, end) = (bounds[0], bounds[1]);

    // 1-indexed: start must be >= 1, and end >= start.
    if start == 0 {
        return Err(crate::error::Error::InvalidInput(
            "lines[0] (start) must be >= 1 (1-indexed)".into(),
        ));
    }
    if end < start {
        return Err(crate::error::Error::InvalidInput(format!(
            "lines[1] (end, {end}) must be >= lines[0] (start, {start})"
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
            "lines[0] (start) {start} is past the end of the file ({} lines)",
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
        notes: Vec::new(),
    })
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
    let SpliceOutcome {
        new_content,
        origin,
    } = match literal_splice(
        content,
        &args.old_string,
        &args.new_string,
        args.fuzzy_whitespace,
        limit,
    )? {
        Some(outcome) => outcome,
        None => {
            // First-difference pointer (backlog 838b6f4e D): append the
            // near-miss diagnostic so a retry can be corrected in one
            // shot. The drift nudge (and its steering marker) stays the
            // error's backbone — appended, never replaced.
            let base = if args.fuzzy_whitespace {
                "old_string not found in file (fuzzy_whitespace)"
            } else {
                "old_string not found in file"
            };
            let detail = near_miss_diagnostic(content, &args.old_string)
                .map(|d| format!(" — {d}"))
                .unwrap_or_default();
            return Err(with_fresh_read_nudge(format!("{base}{detail}")));
        }
    };

    if new_content == content {
        // Review L5: reachable with a VARIANT match whose replacement
        // equals the matched text (old=`it's`, new=`it\'s`, file=`it\'s`)
        // — the anchor WAS found; say so instead of "not found".
        let reason = if origin == MatchOrigin::Exact {
            "old_string not found in file"
        } else {
            "the anchor matched via a fallback variant but the edit produced \
             no change — old_string and new_string are equivalent for the \
             matched text"
        };
        return Err(crate::error::Error::NotFound(reason.into()));
    }
    // The spliced replacement, re-emitted in the file's detected style —
    // recomputed here for the emission validator (literal_splice owns the
    // splice; the validator wants the same replacement text).
    let le = detect_line_ending(content);
    let new_string = denormalize_literal_newlines(&normalize_line_endings(&args.new_string, le));
    validate_emission_artifacts(&args.path, content, &new_string, &new_content)?;
    let diff = compute_diff(&args.path, content, &new_content);
    let notes = origin
        .note()
        .map(|n| vec![n.to_string()])
        .unwrap_or_default();
    Ok(PreparedEdit {
        path: args.path.clone(),
        diff,
        new_content,
        notes,
    })
}

/// Ops path (plan 2e27f896; the batch of plan be16ea36 step 4): apply every
/// op — compact line ops and anchor objects alike — to the in-memory content
/// IN ORDER and return ONE PreparedEdit for the combined result. The caller
/// writes once, so any failing op aborts the whole array with the file
/// untouched on disk (atomicity). Line numbers refer to the content as left
/// by the preceding ops; each anchor must match exactly once unless it sets
/// `count` (which widens the match to the first N occurrences) — an
/// ambiguous anchor without a count is rejected, and every error names the
/// op's 1-based index. Emission artifacts are validated per op and once on
/// the combined brace delta (a legitimate two-step array can be temporarily
/// unbalanced mid-apply); the diff is one combined diff.
fn prepare_edit_ops(args: &FileEditArgs, content: &str) -> Result<PreparedEdit> {
    let ops = validate_ops(args)?;
    let (new_content, notes) = apply_ops(&args.path, content, &ops, args.fuzzy_whitespace)?;
    let diff = compute_diff(&args.path, content, &new_content);
    Ok(PreparedEdit {
        path: args.path.clone(),
        diff,
        new_content,
        notes,
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
    if args.ops.is_some() {
        return Err(crate::error::Error::InvalidInput(
            "append is mutually exclusive with ops".into(),
        ));
    }
    if args.lines.is_some() {
        return Err(crate::error::Error::InvalidInput(
            "append is mutually exclusive with line-range mode (lines)".into(),
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
        notes: Vec::new(),
    })
}

/// The ops mode's argument validation (plan 2e27f896, extending plan
/// be16ea36 step 4's batch validation): `ops` is mutually exclusive with
/// every single-edit field — the ops carry their own replacements — and must
/// be non-empty. The element forms and the anchor item-level checks are the
/// shared engine's (`edit_ops::parse_ops` / `edit_ops::validate_op_items`).
/// Returns the parsed ops on success.
fn validate_ops(args: &FileEditArgs) -> Result<Vec<EditOp>> {
    let values = args
        .ops
        .as_deref()
        .ok_or_else(|| crate::error::Error::InvalidInput("ops is required".into()))?;
    if values.is_empty() {
        return Err(crate::error::Error::InvalidInput(
            "ops is empty — provide at least one op".into(),
        ));
    }
    if !args.old_string.is_empty() {
        return Err(crate::error::Error::InvalidInput(
            "ops is mutually exclusive with old_string — the anchor items carry their \
             own old_string"
                .into(),
        ));
    }
    if !args.new_string.is_empty() {
        return Err(crate::error::Error::InvalidInput(
            "ops is mutually exclusive with new_string — the anchor items carry their \
             own new_string"
                .into(),
        ));
    }
    if args.use_regex {
        return Err(crate::error::Error::InvalidInput(
            "ops is mutually exclusive with use_regex — anchor items are literal \
             matches"
                .into(),
        ));
    }
    if args.replace_all {
        return Err(crate::error::Error::InvalidInput(
            "ops is mutually exclusive with replace_all — set count on the anchor \
             items instead"
                .into(),
        ));
    }
    if args.count.is_some() {
        return Err(crate::error::Error::InvalidInput(
            "ops is mutually exclusive with count — set it on the anchor items instead"
                .into(),
        ));
    }
    if args.lines.is_some() {
        return Err(crate::error::Error::InvalidInput(
            "ops is mutually exclusive with line-range mode (lines)".into(),
        ));
    }
    if args.append {
        return Err(crate::error::Error::InvalidInput(
            "ops is mutually exclusive with append — an append has no anchors".into(),
        ));
    }
    let ops = parse_ops(values)?;
    validate_op_items(&ops)?;
    Ok(ops)
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
        notes: Vec::new(),
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
               exact content or the edit fails (escape/whitespace differences are \
               auto-absorbed with a NOTE): JS-escaped apostrophes (\\'), backslashes \
               (\\\\), and line endings are literal. If the anchor contains a quote, a \
               backslash, or is >3 lines, prefer the ops array with a compact line op. Six modes: \
              LITERAL (default, first \
              occurrence), REGEX (use_regex), LINE-RANGE (lines: [start, end] — no \
              old_string needed, so whitespace mismatches cannot bite), FUZZY \
             (fuzzy_whitespace, literal only), OPS (ops — an atomic ops array \
             applied in order with ONE write; any failing op aborts the whole \
             array with the file untouched on disk; an element is a compact \
             line op like 'd202-205' or an anchor object), and APPEND (append — add \
             new_string at EOF on a fresh line; no old_string needed; the file must \
             exist — use file_write to create it). OPS and APPEND are mutually \
             exclusive with every single-edit field. replace_all / count=N widen a \
             literal or regex match. An invalid regex is rejected with an actionable \
             error — match semantics never change silently before an edit.",
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the file, relative to the project root."},
                    "old_string": {"type": ["string", "null"], "description": "The exact text to find, or a regex when use_regex=true."},
                    "new_string": {"type": ["string", "null"], "description": "The replacement text; supports $1/${name} capture refs when use_regex=true. Required in single mode (empty deletes old_string); omit in ops mode — the ops carry their own replacements. A replacement of exactly 'null' is dropped by the transport's null-stringify defense (empty = deletion) — use batch mode or include surrounding context."},
                    "replace_all": {"type": "boolean", "description": "Replace all occurrences (default: false)."},
                    "use_regex": {"type": "boolean", "description": "Treat old_string as a Rust regex (default: false). In the replacement (new_string), use real newlines — a literal backslash-n is inserted as-is, not converted to a newline."},
                    "count": {"type": "integer", "description": "Replace at most N matches (vi-style count); overrides replace_all."},
                    "lines": {"type": "array", "items": {"type": "integer"}, "description": "Line-range mode: exactly [start, end] — 1-indexed, inclusive; use the numbers shown by file_read. That range is replaced by new_string — old_string, replace_all, use_regex, count and fuzzy_whitespace are all ignored in this mode. One tuple, so the pair can never be half-filled."},
                    "fuzzy_whitespace": {"type": "boolean", "description": "Literal mode only: locate old_string with whitespace normalized — runs of spaces/tabs collapse to one space, trailing whitespace ignored — catching tab-vs-space and off-by-one mismatches. The replacement is spliced in verbatim (default: false)."},
                    "ops": {"type": "array", "description": "Ops array: the edits to apply IN ORDER, written ONCE — any failing op aborts the whole array with every file untouched on disk. A compact line op is a string: {i|b|d|r}{N|N-M|N-}[:payload], 1-indexed inclusive — 'i101:text' inserts after line 101 ('i0:' top, 'i<line count>:' EOF), 'b101:text' inserts before line 101, 'd202-205' deletes ('d202' one line, 'd100-' to EOF), 'r102:text' replaces a line ('r100-120:text' a range; 'r102:' leaves one empty line). Line numbers refer to the content as left by the preceding ops. An anchor object {old_string, new_string, count?, fuzzy_whitespace?} is also an element; its anchor must match exactly once unless it sets count. Mutually exclusive with old_string/new_string/use_regex/replace_all/count/lines.", "items": {"anyOf": [{"type": "string"}, {"type": "object", "properties": {"old_string": {"type": "string", "description": "The exact text to find (EOL-agnostic matching)."}, "new_string": {"type": "string", "description": "The replacement text."}, "count": {"type": "integer", "description": "Replace at most N occurrences of this item's old_string (default 1)."}, "fuzzy_whitespace": {"type": "boolean", "description": "Whitespace-tolerant matching for this item (default: the array-level fuzzy_whitespace)."}}, "required": ["old_string", "new_string"]}]}},
                    "append": {"type": "boolean", "description": "Append mode: add new_string at EOF (on a fresh line, in the file's detected line-ending style) instead of replacing — no old_string needed. Mutually exclusive with ops/line-range/old_string and the matching knobs. Errors when the file is missing — use file_write to create it (default: false)."}
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
            Err(e) => return ToolResult::error(crate::tool::error_message::sanitize_arguments_error(self.name(), &e)),
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

        // No-op guard (backlog 838b6f4e D): a single-mode LITERAL edit whose
        // old_string equals new_string replaces text with itself — reject it
        // BEFORE any file I/O with the exact message, instead of the generic
        // identical-strings error from deep inside the splice. Regex mode is
        // exempt (a pattern can equal its replacement text while still
        // changing the content); the normalized-equal guards (whitespace/
        // EOL) stay in the splice — only the byte-identical case is
        // knowable pre-read.
        if args.ops.is_none()
            && !args.append
            && args.lines.is_none()
            && !args.use_regex
            && !args.old_string.is_empty()
            && args.old_string == args.new_string
        {
            return ToolResult::error(
                "no-op edit: old_string equals new_string — nothing to change",
            );
        }

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
            for note in &prepared.notes {
                output.push('\n');
                output.push_str(note);
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

    #[test]
    fn strict_mode_shape_all_keys_null_optionals_deserializes() {
        // Plan 21118961 regression (review HIGH 1): strict mode forces
        // EVERY schema key present, with `null` as the sanctioned "no
        // value" for the optional ones. The non-Option optional fields
        // must accept null (null_to_default), not just absence —
        // line-range mode (old_string/new_string null) and batch mode
        // are the advertised primary paths that would otherwise break.
        let args: FileEditArgs = serde_json::from_value(serde_json::json!({
            "path": "a.txt",
            "old_string": null,
            "new_string": null,
            "replace_all": null,
            "use_regex": null,
            "fuzzy_whitespace": null,
            "count": null,
            "lines": [1, 2],
            "ops": null,
            "append": null
        }))
        .expect("the strict-mode shape must deserialize");
        assert_eq!(args.path, "a.txt");
        assert_eq!(args.old_string, "");
        assert_eq!(args.new_string, "");
        assert!(!args.replace_all);
        assert!(!args.use_regex);
        assert!(!args.fuzzy_whitespace);
        assert!(!args.append);
        assert_eq!(args.count, None);
        assert!(args.ops.is_none());
        assert_eq!(args.lines, Some(vec![1, 2]));
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
            lines: None,
            fuzzy_whitespace: false,
            ops: None,
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
            lines: None,
            fuzzy_whitespace: false,
            ops: None,
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
            lines: Some(vec![start, end]),
            fuzzy_whitespace: false,
            ops: None,
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
        // The in-splice guard still rejects direct prepare_edit calls (the
        // approval-preview path bypasses execute's early no-op check).
        let args = lit_args("a.txt", "same", "same", false);
        let result = prepare_edit(&args, "same content");
        assert!(result.is_err());
    }

    /// Backlog 838b6f4e D: a single-mode literal edit whose old_string
    /// equals new_string is rejected BEFORE any file I/O with the exact
    /// message — not the generic identical-strings error from deep inside
    /// the splice. Regex mode is exempt: a pattern can equal its
    /// replacement text while still changing the content (a capture-free
    /// replacement of a capture-bearing match).
    #[tokio::test]
    async fn no_op_edit_rejected_early_with_clear_message() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "same content\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "path": "a.txt",
                "old_string": "same",
                "new_string": "same",
            }))
            .await;
        assert!(!result.success, "expected error, got: {}", result.output);
        assert!(
            result.output.contains("no-op edit: old_string equals new_string"),
            "must carry the exact message: {}",
            result.output
        );
        // The rejection happens pre-read — the file is untouched.
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "same content\n"
        );
    }

    /// Regex mode is exempt from the no-op guard (backlog 838b6f4e D): a
    /// pattern can equal its replacement text while still changing the
    /// content — a capture-free replacement of a capture-bearing match.
    #[test]
    fn regex_equal_pattern_and_replacement_is_not_a_no_op() {
        let args = re_args("a.txt", "(a)", "(a)", false, None);
        let prepared = prepare_edit(&args, "a\n").unwrap();
        assert_eq!(prepared.new_content, "(a)\n");
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
        args.lines = Some(vec![9, 9]);
        let err = prepare_edit(&args, "a\nb\nc\n").unwrap_err().to_string();
        assert!(err.contains(EDIT_STALE_READ_MARK), "past EOF: {err}");
        // Empty file.
        let mut args = lit_args("a.txt", "", "x", false);
        args.lines = Some(vec![1, 1]);
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

    // ---- line-range mode (lines: [start, end]) ----

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
        assert!(msg.contains("lines[1]"), "msg: {msg}");
    }

    #[test]
    fn prepare_edit_lines_one_element_array_errors() {
        // A 1-element `lines` array is the new shape of the observed
        // malformed-call class (the old start_line-without-end_line — live
        // incident: 5 consecutive identical failures); the error must carry
        // the exact expected arguments so the model can self-correct on the
        // first attempt.
        let mut args = line_args("a.txt", 2, 3, "X");
        args.lines = Some(vec![2]);
        let result = prepare_edit(&args, "a\nb\nc\n");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("exactly [start, end]"), "msg: {msg}");
        assert!(msg.contains("Expected arguments"), "msg: {msg}");
        assert!(msg.contains("\"lines\""), "msg: {msg}");
    }

    #[test]
    fn prepare_edit_lines_zero_start_errors() {
        let args = line_args("a.txt", 0, 2, "X");
        let result = prepare_edit(&args, "a\nb\nc\n");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("lines[0]"), "msg: {msg}");
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
    fn prepare_edit_lines_three_element_array_errors() {
        // A 3-element `lines` array is likewise rejected — exactly two.
        let mut args = line_args("a.txt", 1, 1, "X");
        args.lines = Some(vec![1, 2, 3]);
        let result = prepare_edit(&args, "a\nb\n");
        assert!(result.is_err());
        let msg = format!("{}", result.unwrap_err());
        assert!(msg.contains("exactly [start, end]"), "msg: {msg}");
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
                "lines": [2, 3],
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
            lines: None,
            fuzzy_whitespace: true,
            ops: None,
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
            lines: None,
            fuzzy_whitespace: true, // ignored in regex mode
            ops: None,
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

    // ---- ops mode (compact line ops + anchor items) ----

    /// Build batch-mode args with empty single-edit fields.
    fn batch_args(path: &str, items: Vec<EditItem>) -> FileEditArgs {
        FileEditArgs {
            path: path.into(),
            old_string: String::new(),
            new_string: String::new(),
            replace_all: false,
            use_regex: false,
            count: None,
            lines: None,
            fuzzy_whitespace: false,
            ops: Some(items.iter().map(|i| serde_json::to_value(i).unwrap()).collect()),
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
                "ops": [
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

    #[tokio::test]
    async fn batch_mode_with_stringified_null_old_new_applies_the_batch() {
        // Backlog 9118714a: the transport stringifies JSON null for string
        // params into the literal string "null" — a batch call carrying
        // old_string:"null"/new_string:"null" tripped the false
        // "ops is mutually exclusive with old_string" error, making batch
        // mode unusable through the transport. The dispatch seam drops the
        // artifact from OPTIONAL properties; composed here with the tool
        // exactly as the seam composes them, the batch applies. The per-item
        // old_string/new_string are REQUIRED inside the item schema, so the
        // defense never touches them.
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        let tool = make_tool(dir.path());
        let mut args = json!({
            "path": "a.rs",
            "ops": [
                {"old_string": "fn a() {}", "new_string": "fn a2() {}"},
                {"old_string": "fn b() {}", "new_string": "fn b2() {}"}
            ],
            "old_string": "null",
            "new_string": "null"
        });
        crate::tool::drop_stringified_nulls(&tool.schema().parameters, &mut args);
        let result = tool.execute(args).await;
        assert!(result.success, "{}", result.output);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.rs")).unwrap(),
            "fn a2() {}\nfn b2() {}\n"
        );
    }

    #[test]
    fn file_edit_optional_string_params_advertise_nullable() {
        // Backlog 9118714a: optional string params advertise
        // ["string", "null"] so explicit JSON null is legal end-to-end —
        // the spawn_agent.model precedent mirrored onto this tool.
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let params = tool.schema().parameters;
        for field in ["old_string", "new_string"] {
            let ty = &params["properties"][field]["type"];
            assert!(
                ty.as_array()
                    .is_some_and(|t| t.contains(&json!("string")) && t.contains(&json!("null"))),
                "{field} must advertise [\"string\", \"null\"], got: {ty}"
            );
        }
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
        // Mutual exclusivity: ops + old_string / use_regex / line-range.
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
        args.lines = Some(vec![1, 1]);
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
            lines: None,
            fuzzy_whitespace: false,
            ops: None,
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
        args.ops = Some(vec![serde_json::to_value(EditItem {
            old_string: "x".into(),
            new_string: "y".into(),
            count: None,
            fuzzy_whitespace: None,
        })
        .unwrap()]);
        assert!(prepare_edit(&args, "x\n").is_err());

        let mut args = append_args("a.txt", "tail");
        args.lines = Some(vec![1, 1]);
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

    #[test]
    fn ops_compact_verbs_and_anchors_apply_through_file_edit() {
        let mut args = batch_args("a.txt", vec![]);
        args.ops = Some(vec![
            json!("d1"),
            json!({"old_string": "c", "new_string": "C"}),
        ]);
        let prepared = prepare_edit(&args, "a\nb\nc\n").expect("ops apply");
        assert_eq!(prepared.new_content, "b\nC\n");
        assert!(prepared.diff.contains("--- a.txt"), "{}", prepared.diff);
    }

    #[test]
    fn ops_legacy_edits_alias_still_deserializes() {
        // The advertised name is `ops`; the pre-rename `edits` key keeps
        // working as a serde alias (old habits, stored transcripts).
        let args: FileEditArgs = serde_json::from_value(json!({
            "path": "a.txt",
            "edits": [{"old_string": "a", "new_string": "b"}]
        }))
        .expect("the legacy edits key must stay accepted");
        assert_eq!(args.ops.as_ref().map(|v| v.len()), Some(1));
    }

    #[test]
    fn ops_schema_advertises_the_compact_grammar() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let params = tool.schema().parameters;
        let ops = &params["properties"]["ops"];
        assert!(ops.is_object(), "ops must be advertised: {params}");
        assert!(
            params["properties"].get("edits").is_none(),
            "the legacy edits key must not be advertised: {params}"
        );
        let text = ops.to_string();
        for needle in ["i101:text", "d202-205", "r102", "inserts after line 101"] {
            assert!(text.contains(needle), "{needle} missing from the ops schema");
        }
        assert!(
            ops["items"]["anyOf"].is_array(),
            "items must accept both forms"
        );
    }

    #[test]
    fn ops_rejects_conflicting_single_edit_fields() {
        let mut args = batch_args("a.txt", vec![]);
        args.ops = Some(vec![json!("d1")]);
        args.old_string = "x".into();
        let err = prepare_edit(&args, "a\n").unwrap_err().to_string();
        assert!(
            err.contains("ops is mutually exclusive with old_string"),
            "{err}"
        );
    }

    #[test]
    fn ops_items_survive_strict_normalization() {
        // file_edit is a STRICT_TOOLS member: the polymorphic ops items must
        // normalize idempotently — both branches of the union stay, and the
        // anchor branch gets the strict treatment (required + no extras).
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let normalized = crate::provider::strict::normalize_for_strict(&tool.schema().parameters);
        let again = crate::provider::strict::normalize_for_strict(&normalized);
        assert_eq!(normalized, again, "strict normalization must be idempotent");
        let items = &normalized["properties"]["ops"]["items"];
        let branches = items["anyOf"].as_array().expect("the union must survive");
        assert_eq!(branches.len(), 2, "{items}");
        let anchor = branches
            .iter()
            .find(|b| b.get("properties").is_some())
            .expect("the anchor branch must survive");
        assert_eq!(anchor["additionalProperties"], json!(false));
        let required = anchor["required"]
            .as_array()
            .expect("required must be forced");
        assert!(required.contains(&json!("old_string")));
        assert!(required.contains(&json!("new_string")));
    }

    #[test]
    fn large_payload_note_covers_ops_payloads() {
        let big = "x".repeat(EMISSION_FRAGILITY_PAYLOAD_BYTES);
        let mut args = batch_args("a.txt", vec![]);
        args.ops = Some(vec![json!(format!("r1:{big}"))]);
        let note = large_payload_note(&args).expect("note at threshold for an op");
        assert!(note.contains("largest op"), "{note}");
        let mut args = batch_args("a.txt", vec![]);
        args.ops = Some(vec![json!({"old_string": big, "new_string": "y"})]);
        let note = large_payload_note(&args).expect("note at threshold for an anchor item");
        // Ops mode reports the largest element either way (string or object).
        assert!(note.contains("largest op"), "{note}");
        let mut args = batch_args("a.txt", vec![]);
        args.ops = Some(vec![json!("d1")]);
        assert!(large_payload_note(&args).is_none());
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
            "ops": [{"old_string": "", "new_string": "x"}]
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

    // ---- escape-normalization fallback (backlog 838b6f4e C) ----

    /// The apostrophe trap: the file holds a JS-escaped \' where the needle
    /// has a plain ' — the escaped variant matches and the success output
    /// carries the NOTE (never silent).
    #[test]
    fn escape_fallback_file_escaped_needle_plain_matches_with_note() {
        let args = lit_args("a.txt", "it's here", "it was here", false);
        let prepared = prepare_edit(&args, "line\nit\\'s here\n").unwrap();
        assert_eq!(prepared.new_content, "line\nit was here\n");
        assert_eq!(prepared.notes.len(), 1, "notes: {:?}", prepared.notes);
        assert!(
            prepared.notes[0].contains("escape-normalization"),
            "note: {}",
            prepared.notes[0]
        );
    }

    /// The mirror direction: the needle carries the JS escape, the file is
    /// plain — the unescaped variant matches.
    #[test]
    fn escape_fallback_needle_escaped_file_plain_matches_with_note() {
        let args = lit_args("a.txt", "it\\'s here", "it was here", false);
        let prepared = prepare_edit(&args, "line\nit's here\n").unwrap();
        assert_eq!(prepared.new_content, "line\nit was here\n");
        assert_eq!(prepared.notes.len(), 1, "notes: {:?}", prepared.notes);
        assert!(
            prepared.notes[0].contains("escape-normalization"),
            "note: {}",
            prepared.notes[0]
        );
    }

    /// A whitespace-run difference matches automatically (fuzzy semantics
    /// applied on miss) with the NOTE — the opt-in flag is no longer the
    /// only route.
    #[test]
    fn whitespace_fallback_matches_with_note() {
        let args = lit_args("a.txt", "fn  foo()", "fn bar()", false);
        let prepared = prepare_edit(&args, "fn\tfoo()\n").unwrap();
        assert_eq!(prepared.new_content, "fn bar()\n");
        assert_eq!(prepared.notes.len(), 1, "notes: {:?}", prepared.notes);
        assert!(
            prepared.notes[0].contains("whitespace normalization"),
            "note: {}",
            prepared.notes[0]
        );
    }

    /// An exact match stays silent — the NOTE fires only on fallbacks.
    #[test]
    fn exact_match_carries_no_fallback_note() {
        let args = lit_args("a.txt", "it's here", "it was here", false);
        let prepared = prepare_edit(&args, "line\nit's here\n").unwrap();
        assert_eq!(prepared.new_content, "line\nit was here\n");
        assert!(prepared.notes.is_empty(), "notes: {:?}", prepared.notes);
    }

    /// A batch item's escape fallback gets a per-item NOTE naming the
    /// item's index.
    #[test]
    fn batch_item_escape_fallback_gets_per_item_note() {
        let args = batch_args(
            "a.txt",
            vec![
                EditItem {
                    old_string: "it's here".into(),
                    new_string: "it was here".into(),
                    count: None,
                    fuzzy_whitespace: None,
                },
                EditItem {
                    old_string: "plain".into(),
                    new_string: "PLAIN".into(),
                    count: None,
                    fuzzy_whitespace: None,
                },
            ],
        );
        let prepared = prepare_edit(&args, "it\\'s here\nplain\n").unwrap();
        assert_eq!(prepared.new_content, "it was here\nPLAIN\n");
        assert_eq!(prepared.notes.len(), 1, "notes: {:?}", prepared.notes);
        assert!(
            prepared.notes[0].contains("edit 1/2"),
            "note: {}",
            prepared.notes[0]
        );
        assert!(
            prepared.notes[0].contains("escape-normalization"),
            "note: {}",
            prepared.notes[0]
        );
    }

    /// End to end: the fallback NOTE rides the tool's success output.
    #[tokio::test]
    async fn escape_fallback_note_rides_the_success_output() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "it\\'s here\n").unwrap();
        let tool = make_tool(dir.path());
        let result = tool
            .execute(json!({
                "path": "a.txt",
                "old_string": "it's here",
                "new_string": "it was here",
            }))
            .await;
        assert!(result.success, "output: {}", result.output);
        assert!(
            result.output.contains("escape-normalization"),
            "the fallback must be visible: {}",
            result.output
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "it was here\n"
        );
    }

    /// The miss error carries the first-difference pointer (backlog
    /// 838b6f4e D): a near-miss anchor reports how far it matched and
    /// where the file diverges — appended to the drift nudge, never
    /// replacing it.
    #[test]
    fn miss_error_carries_first_difference_pointer() {
        let args = lit_args("a.txt", "let placeholder_count = 1;", "X", false);
        let err = prepare_edit(&args, "let placeholder_count = 2;\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("Re-read the file"), "nudge intact: {err}");
        assert!(err.contains("matched 24/26 chars"), "pointer: {err}");
        assert!(err.contains("first difference at char 25"), "position: {err}");
        assert!(err.contains("file has '2"), "file side: {err}");
        assert!(err.contains("you sent '1"), "needle side: {err}");
    }

    /// HIGH 1 (review 2026-09-19, plan 04a195de): OpenAI's strict-mode
    /// JSON-Schema subset rejects `minItems`/`maxItems` outright — a 400
    /// on the whole tools array — and file_edit is a STRICT_TOOLS member
    /// whose schema `normalize_for_strict` passes through verbatim (it
    /// only widens types and forces required/additionalProperties). The
    /// advertised schema must therefore stay inside the supported set.
    #[test]
    fn advertised_schema_carries_no_strict_unsupported_keywords() {
        let dir = tempdir().unwrap();
        let tool = make_tool(dir.path());
        let schema = serde_json::to_value(tool.schema()).unwrap();
        assert_no_unsupported_keywords(&schema);
    }

    /// Recursive walker for the strict-legality check: no `minItems`/
    /// `maxItems` object keys anywhere in the schema (description strings
    /// may mention them; only keys are checked).
    fn assert_no_unsupported_keywords(value: &serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                for key in map.keys() {
                    assert!(
                        key != "minItems" && key != "maxItems",
                        "strict-mode-unsupported keyword '{key}' in the advertised schema"
                    );
                }
                for v in map.values() {
                    assert_no_unsupported_keywords(v);
                }
            }
            serde_json::Value::Array(items) => {
                for v in items {
                    assert_no_unsupported_keywords(v);
                }
            }
            _ => {}
        }
    }

    /// LOW 2 (review 2026-09-19): a batch item with old_string ==
    /// new_string — the observed incident shape — is rejected pre-write
    /// (the file is never touched) with the exact no-op message naming
    /// the item.
    #[test]
    fn batch_no_op_item_rejected_with_clear_message() {
        let args = batch_args(
            "a.txt",
            vec![
                EditItem {
                    old_string: "keep".into(),
                    new_string: "KEEP".into(),
                    count: None,
                    fuzzy_whitespace: None,
                },
                EditItem {
                    old_string: "same".into(),
                    new_string: "same".into(),
                    count: None,
                    fuzzy_whitespace: None,
                },
            ],
        );
        let err = prepare_edit(&args, "keep\nsame\n").unwrap_err().to_string();
        assert!(
            err.contains("no-op edit: edit 2/2 has old_string == new_string"),
            "msg: {err}"
        );
    }

    /// LOW 5 (review 2026-09-19): a variant match whose replacement equals
    /// the matched text reports "produced no change", not "not found".
    #[test]
    fn variant_match_no_change_reports_no_change_not_not_found() {
        let args = lit_args("a.txt", "it's", "it\\'s", false);
        let err = prepare_edit(&args, "it\\'s\n").unwrap_err().to_string();
        assert!(err.contains("produced no change"), "msg: {err}");
        assert!(
            !err.contains("old_string not found in file"),
            "the anchor WAS found (via the escaped variant): {err}"
        );
    }
}
