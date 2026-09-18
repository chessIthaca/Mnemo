// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Shared SSE/stream plumbing for the provider clients.
//!
//! The byte-identical helpers both `openai.rs` and `anthropic.rs` need to
//! decode and debug their SSE streams (quality review MEDIUM, backlog
//! 4047c82f): they were copy-pasted between the two providers, and the
//! duplication had already begun to drift — `parse_data_url`'s
//! empty-media-type behavior diverged between the provider copy and
//! `src/backlog.rs`'s. Every future provider fix now lands here exactly
//! once.
//!
//! The shared HTTP-response error helpers (`provider_error`,
//! `check_response`) also live here (backlog 76efacba): the OpenAI client,
//! the Anthropic client, the vision client, and the `/models` discovery
//! fetchers all check responses the same way — one home, one 401/403
//! body-suppression contract.
//!
//! Deliberately NOT here: `parse_sse_buffer` and the payload parsers — the
//! OpenAI (`choices[]` chunks) and Anthropic (event-typed) protocols differ
//! materially, so those stay provider-local. Only the verified
//! byte-identical pieces are shared.

use crate::error::Error;

use super::{FinishReason, LlmEvent};

/// Truncate a raw stream-data string for inclusion in an error message.
/// Keeps the head (where the parse failure usually is) and notes how much was
/// elided, so the user sees enough to debug without flooding the transcript.
/// Backs up to a UTF-8 char boundary so the slice is always valid.
pub(crate) fn truncate_raw_stream(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let head = &s[..end];
    format!("{head}\n… ({} more bytes truncated)", s.len() - head.len())
}

/// Read a response header as a string, or `"(none)"` if absent. Used to
/// surface transport headers (content-encoding, etc.) in stream-decode errors.
pub(crate) fn header_str(response: &reqwest::Response, name: &str) -> String {
    response
        .headers()
        .get(name)
        .map(|v| v.to_str().unwrap_or("(non-ascii)").to_string())
        .unwrap_or_else(|| "(none)".to_string())
}

/// Walk an error's full `source()` chain and join the display of each layer
/// with " → ". reqwest's "error decoding response body" is a wrapper; the
/// actual reason (connection reset, unexpected EOF, hyper error, TLS error)
/// is one or more hops down. This surfaces the whole chain so a stream-decode
/// failure is debuggable instead of opaque.
pub(crate) fn error_chain(e: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut parts: Vec<String> = vec![e.to_string()];
    let mut current: Option<&(dyn std::error::Error + 'static)> = e.source();
    while let Some(cause) = current {
        parts.push(cause.to_string());
        current = cause.source();
    }
    parts.join(" → ")
}

/// Map a [`FinishReason`] to its snake_case wire form — the same strings the
/// providers send in `stop_reason` (Anthropic) / `choices[].finish_reason`
/// (OpenAI), and what serde's `rename_all = "snake_case"` on [`FinishReason`]
/// produces. Used by the trace log so the UI sees "tool_calls", not the Rust
/// Debug form.
pub(crate) fn finish_reason_label(reason: &FinishReason) -> String {
    match reason {
        FinishReason::Stop => "stop".to_string(),
        FinishReason::ToolCalls => "tool_calls".to_string(),
        FinishReason::Length => "length".to_string(),
        FinishReason::ContentFilter => "content_filter".to_string(),
        FinishReason::Other(s) => s.clone(),
    }
}

/// The result of processing one SSE line from the buffer.
#[derive(Debug)]
pub(crate) enum SseOutcome {
    /// A parsed LLM event.
    Event(LlmEvent),
    /// A line that looked like a `data:` payload but failed to parse as JSON.
    ParseError(String),
}

/// Parse a `data:<media_type>;base64,<data>` URL into `(media_type, data)`.
/// Returns `None` when the URL is not a base64 data URL (it is then treated
/// as a plain URL by the caller). The media type falls back to `image/png`
/// when the prefix is empty — the deliberately chosen contract (quality
/// review, backlog 4047c82f): an empty media type in a provider image
/// payload (e.g. an Anthropic source block) would be invalid on the wire, so
/// png is the sane fallback. NOTE: `src/backlog.rs` keeps its own
/// intentionally-divergent copy (empty stays empty — it feeds MIME→file
/// extension mapping, a different concern; do not "fix" it to match).
pub(crate) fn parse_data_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (meta, data) = rest.split_once(',')?;
    let media_type = meta.strip_suffix(";base64")?;
    if media_type.is_empty() {
        Some(("image/png".to_string(), data.to_string()))
    } else {
        Some((media_type.to_string(), data.to_string()))
    }
}

/// Truncate a string to `max` chars, appending an ellipsis if it was cut.
pub(crate) fn truncate_for_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}

/// Build the provider error for a failed HTTP response, suppressing the body
/// for auth-failure statuses (401/403).
///
/// For 401/403 a misconfigured or malicious server can echo the
/// `Authorization: Bearer {key}` header back in its response body, which would
/// then surface (truncated) in the error message shown in the UI. To avoid
/// leaking the key, the body is NEVER included for these statuses — only the
/// status code + a "check the API key" hint. For other failures, a truncated
/// body is included for diagnosis.
///
/// This is the pure decision extracted from [`check_response`] so it is
/// unit-testable without a live HTTP response.
pub(crate) fn provider_error(
    status: reqwest::StatusCode,
    body: &str,
    url: &str,
    label: &str,
) -> Error {
    if status.as_u16() == 401 || status.as_u16() == 403 {
        Error::Provider(format!(
            "{label}: HTTP {status} from {url} — unauthorized (check the API key)"
        ))
    } else {
        Error::Provider(format!(
            "{label}: HTTP {status} from {url} — {}",
            truncate_for_display(body, 500)
        ))
    }
}

/// Check an HTTP response for failure, suppressing the response body for
/// auth-failure statuses (401/403).
///
/// Returns `Ok(response)` on success, or `Err((status, body, error))` on
/// failure. The `(status, body)` pair is returned alongside the error so the
/// caller can log to a local provider trace — BUT the caller MUST suppress the
/// body for 401/403 in any trace that is surfaced to the UI (the provider
/// trace IS shown in the "Trace" tab). The body for 401/403 may echo the
/// `Authorization: Bearer {key}` header; it is never safe to display it.
pub(crate) async fn check_response(
    response: reqwest::Response,
    url: &str,
    label: &str,
) -> std::result::Result<reqwest::Response, (reqwest::StatusCode, String, Error)> {
    if response.status().is_success() {
        return Ok(response);
    }
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    Err((
        status,
        text.clone(),
        provider_error(status, &text, url, label),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The truncate_raw_stream tests moved from openai.rs and
    // parse_data_url_variants from anthropic.rs when the helpers were
    // extracted into this module (same names, same assertions — history
    // stays greppable).

    #[test]
    fn truncate_raw_stream_keeps_short_strings_intact() {
        assert_eq!(truncate_raw_stream("hello", 10), "hello");
    }

    #[test]
    fn truncate_raw_stream_truncates_long_strings() {
        let s = "a".repeat(100);
        let out = truncate_raw_stream(&s, 20);
        assert!(out.starts_with(&"a".repeat(20)));
        assert!(out.contains("more bytes truncated"));
    }

    #[test]
    fn truncate_raw_stream_backs_up_to_char_boundary() {
        // "é" is two bytes; cutting mid-char must back up, not panic.
        let s = "é".repeat(50);
        let out = truncate_raw_stream(&s, 21);
        // Should not panic and should contain the truncation note.
        assert!(out.contains("more bytes truncated"));
        // The head must be valid UTF-8 (all "é" chars, each 2 bytes).
        let head = out.split('\n').next().unwrap();
        assert!(head.chars().all(|c| c == 'é'));
    }

    #[test]
    fn parse_data_url_variants() {
        assert_eq!(
            parse_data_url("data:image/png;base64,AAAA"),
            Some(("image/png".to_string(), "AAAA".to_string()))
        );
        // Empty media type falls back to image/png.
        assert_eq!(
            parse_data_url("data:;base64,AAAA"),
            Some(("image/png".to_string(), "AAAA".to_string()))
        );
        // Non-data URLs are None.
        assert_eq!(parse_data_url("https://example.com/pic.jpg"), None);
        // Missing base64 marker is None.
        assert_eq!(parse_data_url("data:image/png,AAAA"), None);
    }

    // New — the label mapping had no direct test in either provider.
    #[test]
    fn finish_reason_label_maps_snake_case_and_passthrough() {
        assert_eq!(finish_reason_label(&FinishReason::Stop), "stop");
        assert_eq!(finish_reason_label(&FinishReason::ToolCalls), "tool_calls");
        assert_eq!(finish_reason_label(&FinishReason::Length), "length");
        assert_eq!(
            finish_reason_label(&FinishReason::ContentFilter),
            "content_filter"
        );
        assert_eq!(
            finish_reason_label(&FinishReason::Other("custom".to_string())),
            "custom"
        );
    }

    // The truncate_for_display + provider_error tests moved from openai.rs
    // when the HTTP-error helpers were extracted into this module (backlog
    // 76efacba; same names, same assertions — history stays greppable).

    #[test]
    fn truncate_for_display_short_kept_intact() {
        assert_eq!(truncate_for_display("hello", 10), "hello");
    }

    #[test]
    fn truncate_for_display_long_cut_with_ellipsis() {
        let out = truncate_for_display(&"a".repeat(50), 5);
        assert_eq!(out, "aaaaa…");
    }

    #[test]
    fn truncate_for_display_respects_char_boundaries() {
        // "é" is two bytes but one char; truncate on chars, not bytes.
        let out = truncate_for_display(&"é".repeat(10), 3);
        assert_eq!(out, "ééé…");
    }

    // --- A5: 401/403 body suppression (no key leakage in error strings) ----

    #[test]
    fn provider_error_401_suppresses_body() {
        // A 401 must NOT include the response body (it may echo the API key).
        let err = provider_error(
            reqwest::StatusCode::UNAUTHORIZED,
            "Bearer sk-secret-key-12345",
            "https://api.example.com/v1/chat/completions",
            "stream request",
        );
        let msg = match err {
            Error::Provider(m) => m,
            _ => panic!("expected Provider error"),
        };
        assert!(msg.contains("unauthorized"), "msg: {msg}");
        assert!(msg.contains("check the API key"), "msg: {msg}");
        // The body — which may contain the echoed key — must NOT appear.
        assert!(
            !msg.contains("sk-secret-key-12345"),
            "key leaked in 401 error: {msg}"
        );
        assert!(!msg.contains("Bearer"), "body leaked in 401 error: {msg}");
    }

    #[test]
    fn provider_error_403_suppresses_body() {
        let err = provider_error(
            reqwest::StatusCode::FORBIDDEN,
            "{\"error\":\"forbidden; token=sk-leaked\"}",
            "https://api.example.com/v1/models",
            "fetch models",
        );
        let msg = match err {
            Error::Provider(m) => m,
            _ => panic!("expected Provider error"),
        };
        assert!(msg.contains("unauthorized"), "msg: {msg}");
        assert!(!msg.contains("sk-leaked"), "key leaked in 403 error: {msg}");
        assert!(
            !msg.contains("forbidden; token"),
            "body leaked in 403 error: {msg}"
        );
    }

    #[test]
    fn provider_error_500_includes_truncated_body() {
        // A non-auth failure includes a truncated body for diagnosis.
        let long_body = "x".repeat(600);
        let err = provider_error(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            &long_body,
            "https://api.example.com/v1/chat/completions",
            "stream request",
        );
        let msg = match err {
            Error::Provider(m) => m,
            _ => panic!("expected Provider error"),
        };
        assert!(msg.contains("500"), "msg: {msg}");
        assert!(msg.contains("stream request"), "msg: {msg}");
        // The body is included (truncated to 500 chars + ellipsis).
        assert!(msg.contains("…"), "expected truncation ellipsis: {msg}");
    }

    #[test]
    fn provider_error_400_includes_body() {
        let err = provider_error(
            reqwest::StatusCode::BAD_REQUEST,
            "messages parameter is illegal",
            "https://api.example.com/v1/chat/completions",
            "stream request",
        );
        let msg = match err {
            Error::Provider(m) => m,
            _ => panic!("expected Provider error"),
        };
        assert!(msg.contains("400"), "msg: {msg}");
        assert!(msg.contains("messages parameter is illegal"), "msg: {msg}");
    }
}
