// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Boundary-token round-trip escaping (backlog 1db26c95).
//!
//! The serving layer strips/maps LLM special tokens (e.g. GLM's
//! end-of-text tag) from request input — live-verified 2027-01-08 (echo
//! test: the model perceived the token position as "a blank line"; a
//! regex search matched the raw token in 11 source lines while every
//! displayed line showed it stripped) — so the model's textual view of a
//! tool result carrying the raw token shows an empty string, and a
//! read-then-write round-trip silently corrupts the file (incident
//! 2026-12-24, plan 7383a4d8: reads showed the stop literal as an empty
//! string, a reconstructed old_string failed to match).
//!
//! The fix: [`escape_boundary_tokens`] replaces each configured boundary
//! token (the same `stop_boundary_strings` the request stop list uses)
//! with a visible, deterministic, reversible marker in TOOL-RESULT
//! messages at the request-building layer — the model can see and re-emit
//! the marker — and the file tools call [`restore_boundary_tokens`] on
//! written content so the round-trip is faithful.
//!
//! Marker form: `⟦raw:ESCAPED⟧` where ESCAPED renders EVERY char of the
//! token as `\uXXXX` (min-4-width lowercase hex). The escape is a SINGLE
//! pass over the original content (longest token match at each position)
//! — emitted markers are never rescanned, so no configured token can be
//! escaped inside a marker and the escape→restore round trip is an
//! identity for ANY token set, even pathological tokens that overlap the
//! marker's own alphabet (`raw:`, hex digits, the wrappers).
//! [`restore_boundary_tokens`] strictly parses `⟦raw:…⟧` inners as
//! consecutive `\u`-prefixed hex groups; a malformed marker is left
//! untouched.
//!
//! Collision caveat: a file that legitimately contains `⟦raw:…⟧` text is
//! rewritten with the un-escaped token — accepted (the wrapper is
//! app-invented; vanishingly unlikely in real content).
//!
//! Related: the client-side stream guard (super::openai::guard) cuts
//! boundary tokens from the model's OUTPUT stream — the output-side
//! counterpart of this input-side escaping.

/// The marker wrapper open (U+27E6 MATHEMATICAL LEFT WHITE SQUARE BRACKET).
const MARKER_OPEN: char = '\u{27e6}';
/// The marker wrapper close (U+27E7 MATHEMATICAL RIGHT WHITE SQUARE BRACKET).
const MARKER_CLOSE: char = '\u{27e7}';
/// The marker prefix: `⟦raw:`.
const MARKER_PREFIX: &str = "\u{27e6}raw:";
/// The marker suffix: `⟧`.
const MARKER_SUFFIX: &str = "\u{27e7}";

/// Escape every configured boundary token in `content` to the visible
/// marker form `⟦raw:\uXXXX…⟧` (backlog 1db26c95).
///
/// Applied to TOOL-RESULT message content at the request-building layer:
/// the serving layer strips/maps the raw token from request input, so the
/// model's textual view would show an empty string — the marker is pure
/// ASCII the model CAN see and re-emit. The escape is a SINGLE pass over
/// the original content, taking the longest token match at each position
/// (a token that prefixes another never shadows it) and never rescanning
/// emitted markers — no configured token can be escaped inside a marker,
/// keeping the escape→restore round trip an identity for ANY token set
/// (review L2: the old sequential per-token replace could nest markers
/// for tokens overlapping the marker alphabet). Empty tokens and empty
/// content are no-ops; content without any token hit is returned
/// unchanged (a clone).
pub fn escape_boundary_tokens(content: &str, tokens: &[String]) -> String {
    if content.is_empty() {
        return content.to_string();
    }
    let mut ordered: Vec<&str> = tokens
        .iter()
        .filter(|t| !t.is_empty())
        .map(|t| t.as_str())
        .collect();
    if ordered.is_empty() {
        return content.to_string();
    }
    // Longest-first so the longest token matching at a position wins.
    ordered.sort_by_key(|t| std::cmp::Reverse(t.len()));
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while !rest.is_empty() {
        if let Some(token) = ordered.iter().find(|t| rest.starts_with(*t)) {
            out.push_str(&marker_for(token));
            rest = &rest[token.len()..];
        } else {
            // No token matches here — emit one char (UTF-8 safe) and
            // advance.
            let ch = rest.chars().next().expect("rest is non-empty");
            out.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }
    out
}

/// Render one token as its marker: every char as `\uXXXX` (min-4-width
/// lowercase hex) between the wrappers.
fn marker_for(token: &str) -> String {
    format!(
        "{MARKER_PREFIX}{}{MARKER_SUFFIX}",
        token
            .chars()
            .map(|c| format!("\\u{:04x}", c as u32))
            .collect::<String>()
    )
}

/// Restore boundary-token markers (`⟦raw:\uXXXX…⟧`) in `content` to the
/// raw tokens (backlog 1db26c95).
///
/// Called by the file tools on written content (file_write `content`,
/// file_append `content` — including file_write's mode="append"
/// delegation — and file_edit `old_string` + `new_string`) so a
/// read-then-write round-trip is faithful: the agent saw the marker (the
/// request layer escaped the raw token), re-emits it verbatim, and the
/// write lands the raw token. The inner must parse STRICTLY as
/// consecutive `\u`-prefixed hex groups — a malformed marker is left
/// untouched (never corrupts surrounding text).
pub fn restore_boundary_tokens(content: &str) -> String {
    restore_impl(content, false)
}

/// Restore boundary-token markers in `content` to regex-QUOTED raw tokens
/// (backlog 1db26c95) — for PATTERN text (file_edit's `use_regex`
/// `old_string`).
///
/// The plain restore would land the raw token in the pattern, where its
/// regex metacharacters (the GLM token's `|` alternation) silently change
/// match semantics — a restored marker must keep matching the raw token
/// LITERALLY, so each restored token is wrapped in [`regex::escape`]
/// (review L4). Replacement text (`new_string`) is not a pattern — use
/// the plain [`restore_boundary_tokens`] there.
pub fn restore_boundary_tokens_regex_quoted(content: &str) -> String {
    restore_impl(content, true)
}

/// The shared marker-restore scan; `quote` emits each restored token
/// regex-escaped instead of raw.
fn restore_impl(content: &str, quote: bool) -> String {
    if !content.contains(MARKER_PREFIX) {
        return content.to_string();
    }
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find(MARKER_PREFIX) {
        let inner_start = start + MARKER_PREFIX.len();
        let Some(inner_end_rel) = rest[inner_start..].find(MARKER_CLOSE) else {
            // Unterminated marker — the remainder stays as-is.
            break;
        };
        let inner_end = inner_start + inner_end_rel;
        if let Some(raw) = unescape_marker_inner(&rest[inner_start..inner_end]) {
            out.push_str(&rest[..start]);
            if quote {
                out.push_str(&regex::escape(&raw));
            } else {
                out.push_str(&raw);
            }
            rest = &rest[inner_end + MARKER_SUFFIX.len()..];
        } else {
            // Malformed inner — keep the prefix literally and continue
            // scanning after it.
            out.push_str(&rest[..inner_start]);
            rest = &rest[inner_start..];
        }
    }
    out.push_str(rest);
    out
}

/// Strictly parse a marker inner as consecutive `\u`-prefixed hex groups
/// (4..=6 hex digits each — the escape pads to at least 4 (`{:04x}`) and
/// a scalar value is at most U+10FFFF, 6 digits; the parser mirrors the
/// escape's emission exactly, review L3). Returns `None` on any
/// deviation — the caller leaves the marker untouched.
fn unescape_marker_inner(inner: &str) -> Option<String> {
    if inner.is_empty() {
        return None;
    }
    let mut out = String::with_capacity(inner.len() / 4);
    let bytes = inner.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 >= bytes.len() || bytes[i + 1] != b'u' {
            return None;
        }
        i += 2;
        let start = i;
        while i < bytes.len() && (bytes[i] as char).is_ascii_hexdigit() && i - start < 6 {
            i += 1;
        }
        // The escape pads every char to at least 4 hex digits (`{:04x}`)
        // and never emits more than 6 (U+10FFFF); anything outside
        // 4..=6 is malformed by construction.
        if i - start < 4 {
            return None;
        }
        let cp = u32::from_str_radix(&inner[start..i], 16).ok()?;
        out.push(char::from_u32(cp)?);
    }
    Some(out)
}

// The wrappers are referenced by the doc comments above; keep them used.
const _: (char, char) = (MARKER_OPEN, MARKER_CLOSE);

#[cfg(test)]
mod tests {
    use super::*;

    /// The GLM end-of-text token, built from escapes (transport-safe —
    /// raw angle-bracket tag text is stripped in text transports).
    fn endoftext() -> String {
        "\u{3c}|endoftext|\u{3e}".to_string()
    }

    #[test]
    fn escape_restore_round_trip_is_identity() {
        let tokens = vec![endoftext()];
        let content = format!("stop = [\"{}\"]\nplain line\n", endoftext());
        let escaped = escape_boundary_tokens(&content, &tokens);
        assert_ne!(escaped, content, "the token must be escaped");
        assert!(
            !escaped.contains(&endoftext()),
            "no raw token in the escaped form: {escaped:?}"
        );
        assert!(
            escaped.contains(MARKER_PREFIX),
            "the marker is present: {escaped:?}"
        );
        let restored = restore_boundary_tokens(&escaped);
        assert_eq!(restored, content, "the round trip is identity");
    }

    #[test]
    fn marker_never_contains_the_raw_token() {
        let tokens = vec![endoftext()];
        let escaped = escape_boundary_tokens(&endoftext(), &tokens);
        assert!(!escaped.contains(&endoftext()));
        // The marker is the full \u rendering of every char.
        assert_eq!(
            escaped,
            "\u{27e6}raw:\\u003c\\u007c\\u0065\\u006e\\u0064\\u006f\\u0066\\u0074\\u0065\\u0078\\u0074\\u007c\\u003e\u{27e7}"
        );
    }

    #[test]
    fn no_tokens_or_no_hits_is_unchanged() {
        let content = "plain content";
        assert_eq!(escape_boundary_tokens(content, &[]), content);
        assert_eq!(escape_boundary_tokens(content, &[endoftext()]), content);
        assert_eq!(restore_boundary_tokens(content), content);
        // Empty content and empty tokens are no-ops.
        assert_eq!(escape_boundary_tokens("", &[endoftext()]), "");
        assert_eq!(
            escape_boundary_tokens("x", &["".to_string()]),
            "x",
            "an empty configured token must not match everything"
        );
    }

    #[test]
    fn escape_is_idempotent() {
        // The marker contains no raw token, so re-escaping is a no-op —
        // the request layer re-serializes the (unchanged) Message every
        // request.
        let tokens = vec![endoftext()];
        let once = escape_boundary_tokens(&format!("a {} b", endoftext()), &tokens);
        let twice = escape_boundary_tokens(&once, &tokens);
        assert_eq!(once, twice);
    }

    #[test]
    fn malformed_markers_untouched() {
        // Not \u-hex groups — left as-is.
        assert_eq!(
            restore_boundary_tokens("\u{27e6}raw:hello\u{27e7}"),
            "\u{27e6}raw:hello\u{27e7}"
        );
        // A truncated group is malformed.
        assert_eq!(
            restore_boundary_tokens("\u{27e6}raw:\\u003\u{27e7}"),
            "\u{27e6}raw:\\u003\u{27e7}"
        );
        // An unterminated marker is left as-is.
        assert_eq!(
            restore_boundary_tokens("\u{27e6}raw:\\u003c"),
            "\u{27e6}raw:\\u003c"
        );
        // A valid marker next to a malformed one: only the valid one
        // restores.
        assert_eq!(
            restore_boundary_tokens("\u{27e6}raw:\\u0041\u{27e7}\u{27e6}raw:zz\u{27e7}"),
            "A\u{27e6}raw:zz\u{27e7}"
        );
    }

    #[test]
    fn multiple_tokens_and_longest_first() {
        let user_tag = "\u{3c}|user|\u{3e}".to_string();
        // endoftext contains "|user|"-adjacent text? No — but a
        // shortest-first order would partially escape a token that is a
        // substring of another; longest-first avoids it.
        let tokens = vec![user_tag.clone(), endoftext()];
        let content = format!("{} and {}", endoftext(), user_tag);
        let escaped = escape_boundary_tokens(&content, &tokens);
        assert!(!escaped.contains(&endoftext()));
        assert!(!escaped.contains(&user_tag));
        assert_eq!(restore_boundary_tokens(&escaped), content);
    }

    #[test]
    fn astral_chars_round_trip() {
        // Chars above the BMP take 5 hex digits — the variable-width
        // parse must handle them (a fixed 4-width parse would corrupt).
        let token = "\u{1f600}\u{3c}".to_string();
        let content = format!("emoji {} token", token);
        let escaped = escape_boundary_tokens(&content, &[token.clone()]);
        assert!(!escaped.contains(&token));
        assert_eq!(restore_boundary_tokens(&escaped), content);
    }

    #[test]
    fn pathological_tokens_do_not_nest_markers() {
        // Review L2: a token overlapping the marker alphabet (here "a",
        // which occurs in `raw:` and in hex digits) must not be escaped
        // INSIDE a previously emitted marker — the old sequential
        // per-token replace nested markers and broke the round trip; the
        // single-pass escape never rescans emitted output.
        let endoftext = endoftext();
        let tokens = vec![endoftext.clone(), "a".to_string()];
        let content = format!("stop = [\"{endoftext}\"] and a plain a");
        let escaped = escape_boundary_tokens(&content, &tokens);
        // The endoftext marker is intact — "a" did not split it.
        let expected_marker = "\u{27e6}raw:\\u003c\\u007c\\u0065\\u006e\\u0064\\u006f\\u0066\\u0074\\u0065\\u0078\\u0074\\u007c\\u003e\u{27e7}";
        assert!(
            escaped.contains(expected_marker),
            "the endoftext marker must stay intact, got: {escaped:?}"
        );
        assert_eq!(
            restore_boundary_tokens(&escaped),
            content,
            "the round trip must be identity with a pathological token set"
        );
    }

    #[test]
    fn pathological_single_tokens_round_trip() {
        // The single-pass escape keeps the round trip an identity for ANY
        // token, including ones that occur inside marker text.
        for token in ["a", "raw:", "\u{27e7}", "\\u003c", "0f"] {
            let token = token.to_string();
            let content = format!("x{token}y and z {token} tail");
            let escaped = escape_boundary_tokens(&content, &[token.clone()]);
            assert_eq!(
                restore_boundary_tokens(&escaped),
                content,
                "round trip must be identity for token {token:?}"
            );
        }
    }

    #[test]
    fn groups_longer_than_six_digits_are_malformed() {
        // Review L3: the escape emits at most 6 hex digits (U+10FFFF) —
        // an 8-digit group is not something it can produce and is left
        // untouched (the parser mirrors the escape's emission).
        assert_eq!(
            restore_boundary_tokens("\u{27e6}raw:\\u00000041\u{27e7}"),
            "\u{27e6}raw:\\u00000041\u{27e7}"
        );
        // 4..=6 digit groups are the escape's emission range.
        assert_eq!(restore_boundary_tokens("\u{27e6}raw:\\u0041\u{27e7}"), "A");
        assert_eq!(
            restore_boundary_tokens("\u{27e6}raw:\\u1f600\u{27e7}"),
            "\u{1f600}"
        );
        assert_eq!(
            restore_boundary_tokens("\u{27e6}raw:\\u10ffff\u{27e7}"),
            "\u{10ffff}"
        );
    }

    #[test]
    fn restore_regex_quoted_quotes_metacharacters() {
        // Review L4: file_edit's use_regex old_string restores markers to
        // a regex-QUOTED form — the raw token's `|` alternation must not
        // change match semantics.
        let token = endoftext();
        let escaped = escape_boundary_tokens(&token, &[token.clone()]);
        let quoted = restore_boundary_tokens_regex_quoted(&escaped);
        assert_eq!(quoted, regex::escape(&token));
        // The plain restore still lands the raw token.
        assert_eq!(restore_boundary_tokens(&escaped), token);
        // The quoted form compiles to a pattern matching the WHOLE raw
        // token literally (the unquoted form would be alternation).
        let re = regex::Regex::new(&quoted).unwrap();
        assert_eq!(re.find(&token).unwrap().as_str(), token);
    }
}
