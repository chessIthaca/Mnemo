// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Shared handling for model-supplied regex patterns.
//!
//! Model-written patterns fail in two recurring ways:
//!
//! 1. **Compile errors** — a literal containing regex metacharacters
//!    (`handleClick(`) fails `Regex::new` and the whole tool call errors out,
//!    burning a retry round-trip. Read tools recover by retrying the pattern
//!    as escaped literal text ([`compile_with_fallback`] with
//!    `allow_fallback: true`); edit tools instead get an actionable error,
//!    because silently changing match semantics right before a mutation could
//!    edit the wrong text.
//! 2. **Line endings / multi-line patterns** — per-line scanning can never
//!    match a pattern containing `\n`, and CRLF files would need `\r\n`.
//!    [`match_lines`] scans per-line when possible (`str::lines()` strips the
//!    `\r\n` terminator, so single-line patterns are CRLF-safe by
//!    construction) and switches to whole-content matching on LF-normalized
//!    text when the pattern can span lines.

use regex::Regex;

/// A compiled pattern plus an optional note the caller should surface in its
/// output (set when a broken regex was retried as literal text).
#[derive(Debug)]
pub struct CompiledPattern {
    /// The compiled regex — the literal-escaped fallback when the original
    /// pattern failed to compile.
    pub regex: Regex,
    /// Human-readable note that the fallback fired; `None` on a clean compile.
    pub fallback_note: Option<String>,
}

/// Compile a model-supplied `pattern`.
///
/// * `literal: true` — the pattern is escaped, so it always compiles.
/// * `literal: false`, `allow_fallback: true` (read tools) — a broken regex
///   is retried as escaped literal text and the fallback note is set.
/// * `literal: false`, `allow_fallback: false` (edit tools) — a broken regex
///   is a hard error that suggests literal mode; never silently change match
///   semantics before a mutation.
pub fn compile_with_fallback(
    pattern: &str,
    literal: bool,
    allow_fallback: bool,
) -> Result<CompiledPattern, String> {
    if literal {
        let re = Regex::new(&regex::escape(pattern))
            .map_err(|e| format!("invalid literal pattern: {e}"))?;
        return Ok(CompiledPattern {
            regex: re,
            fallback_note: None,
        });
    }
    match Regex::new(pattern) {
        Ok(re) => Ok(CompiledPattern {
            regex: re,
            fallback_note: None,
        }),
        Err(e) if allow_fallback => {
            let re = Regex::new(&regex::escape(pattern))
                .map_err(|e2| format!("invalid pattern (literal retry also failed: {e2}): {e}"))?;
            Ok(CompiledPattern {
                regex: re,
                fallback_note: Some(format!("invalid regex ({e}); matched literally instead")),
            })
        }
        Err(e) => Err(format!(
            "invalid regex: {e} — if you meant literal text, retry with literal matching \
             (literal: true / use_regex: false)"
        )),
    }
}

/// One line-level match: a 1-based line number plus the line's text with the
/// line terminator stripped.
#[derive(Debug)]
pub struct LineMatch {
    /// 1-based line number in the original content.
    pub line: usize,
    /// The matched line's text (for a whole-content match, the line where the
    /// match starts).
    pub text: String,
}

/// Match `re` against file `content`, returning one [`LineMatch`] per match.
///
/// `raw_pattern` is the pre-compile pattern string: when it can span lines
/// (contains a real newline or a `\n` escape), per-line scanning could never
/// match, so the content is normalized CRLF→LF and matched whole instead.
/// Line numbers are unaffected by normalization — only `\r` bytes are
/// removed, so the line count is unchanged.
pub fn match_lines(content: &str, re: &Regex, raw_pattern: &str) -> Vec<LineMatch> {
    let may_span_lines = raw_pattern.contains('\n') || raw_pattern.contains("\\n");
    if !may_span_lines {
        return content
            .lines()
            .enumerate()
            .filter(|(_, l)| re.is_match(l))
            .map(|(i, l)| LineMatch {
                line: i + 1,
                text: l.to_string(),
            })
            .collect();
    }
    let normalized = content.replace("\r\n", "\n");
    // Byte offset of every line start, for match-offset → line-number mapping.
    let line_starts: Vec<usize> = std::iter::once(0)
        .chain(normalized.match_indices('\n').map(|(i, _)| i + 1))
        .collect();
    re.find_iter(&normalized)
        .map(|m| {
            let idx = line_starts.partition_point(|&s| s <= m.start()) - 1;
            let start = line_starts[idx];
            let end = normalized[start..]
                .find('\n')
                .map(|e| start + e)
                .unwrap_or(normalized.len());
            LineMatch {
                line: idx + 1,
                text: normalized[start..end].to_string(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_pattern_always_compiles() {
        let c = compile_with_fallback("a(b", true, false).unwrap();
        assert!(c.fallback_note.is_none());
        assert!(c.regex.is_match("x a(b y"));
    }

    #[test]
    fn broken_regex_falls_back_to_literal_for_read_tools() {
        let c = compile_with_fallback("handleClick(", false, true).unwrap();
        let note = c.fallback_note.expect("fallback note set");
        assert!(note.contains("matched literally"));
        assert!(c.regex.is_match("foo handleClick( bar"));
        assert!(!c.regex.is_match("handleClickX"));
    }

    #[test]
    fn broken_regex_is_actionable_error_for_edit_tools() {
        let err = compile_with_fallback("handleClick(", false, false).unwrap_err();
        assert!(err.contains("invalid regex"), "names the problem: {err}");
        assert!(err.contains("literal"), "suggests literal mode: {err}");
    }

    #[test]
    fn valid_regex_passes_through_without_note() {
        let c = compile_with_fallback(r"fn\s+\w+", false, true).unwrap();
        assert!(c.fallback_note.is_none());
        assert!(c.regex.is_match("fn  main"));
    }

    #[test]
    fn single_line_pattern_matches_crlf_file() {
        // str::lines() strips the \r\n terminator, so $ anchors work on CRLF.
        let c = compile_with_fallback("bar$", false, false).unwrap();
        let hits = match_lines("foo\r\nbar\r\nbaz\n", &c.regex, "bar$");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 2);
        assert_eq!(hits[0].text, "bar");
    }

    #[test]
    fn multi_line_pattern_matches_crlf_file() {
        // The defect: a pattern with \n can never match in a per-line scan.
        // Whole-content matching on LF-normalized text fixes it; the reported
        // line number is where the match starts.
        let c = compile_with_fallback("foo\nbar", false, false).unwrap();
        let hits = match_lines("x\r\nfoo\r\nbar\r\ny\n", &c.regex, "foo\nbar");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 2);
        assert_eq!(hits[0].text, "foo");
    }

    #[test]
    fn multi_line_pattern_matches_lf_file() {
        let c = compile_with_fallback("foo\nbar", false, false).unwrap();
        let hits = match_lines("foo\nbar\n", &c.regex, "foo\nbar");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 1);
    }

    #[test]
    fn literal_pattern_with_real_newline_also_spans_lines() {
        let c = compile_with_fallback("foo\nbar", true, false).unwrap();
        let hits = match_lines("foo\r\nbar\r\n", &c.regex, "foo\nbar");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line, 1);
    }
}
