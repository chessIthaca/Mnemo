// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Tests for the OpenAI-compatible client and its submodules.
//!
//! Moved out of openai.rs when the file was split into submodules (backlog
//! 76efacba) — same tests, same names; `use super::*` reaches the openai
//! module root exactly as before.

use super::*;
use super::sse::*;
use crate::provider::Role;
use crate::provider::sse_util::SseOutcome;
use crate::provider::stream::{bound_repetition_buffer, detect_repetition};
use crate::provider::{ContentPart, ImageUrl, MessageContent, ToolCall};

#[test]
fn detect_repetition_true_for_repeated_window() {
    // A 200-char segment repeated 3× → detected.
    let segment = "x".repeat(200);
    let text = format!("{segment}{segment}{segment}");
    assert!(detect_repetition(&text, 200, 3));
}

#[test]
fn detect_repetition_false_for_normal_prose() {
    // Normal varied text → no repetition.
    let text = "The quick brown fox jumps over the lazy dog. \
                Pack my box with five dozen liquor jugs. \
                How vexingly quick daft zebras jump!";
    assert!(!detect_repetition(&text, 200, 3));
}

#[test]
fn detect_repetition_false_when_too_short() {
    // Text shorter than window × threshold → false.
    let text = "x".repeat(599); // 200 × 3 = 600 needed
    assert!(!detect_repetition(&text, 200, 3));
}

#[test]
fn detect_repetition_true_with_prefix_before_repeats() {
    // Repetition at the TAIL with unrelated text before it → detected.
    let prefix = "Here is some preamble text that is not repeated. ";
    let segment = "loop body ".repeat(20); // 200 chars
    let text = format!("{prefix}{segment}{segment}{segment}");
    assert!(detect_repetition(&text, 200, 3));
}

#[test]
fn detect_repetition_false_when_only_two_repeats() {
    // Two repeats (below threshold of 3) → false.
    let segment = "x".repeat(200);
    let text = format!("{segment}{segment}");
    assert!(!detect_repetition(&text, 200, 3));
}

#[test]
fn detect_repetition_handles_multibyte_utf8_without_panic() {
    // Multi-byte UTF-8 (é = 2 bytes): 300 × "é" + "a" = 601 bytes.
    // The scan is byte-wise (no str slicing), so mid-char offsets are safe:
    // the trailing 'a' breaks every candidate period → false, no panic.
    // (Pre-any-period this exercised the removed char-boundary bail —
    // the exact trigger from the reviewer's HIGH 1.)
    let text = format!("{}a", "é".repeat(300));
    assert!(!detect_repetition(&text, 200, 3));
    // Also verify a genuine repetition with multi-byte content is detected:
    // "é" is 2 bytes, so a 200-byte window = 100 "é" chars.
    // 300 "é" chars = 600 bytes = 3× window (the p=2 period also fires).
    let segment = "é".repeat(100); // 200 bytes
    let text = format!("{segment}{segment}{segment}");
    assert!(detect_repetition(&text, 200, 3));
}

#[test]
fn detect_repetition_fires_on_exit_note_loop_with_74_byte_period() {
    // Regression (2027-01-11 deepseek exit-note loop): the model repeated
    // this exact 69-char / 74-byte unit 366× (the em-dash came through as
    // mojibake — U+00E2 U+20AC U+201D). The old suffix check required the
    // accumulated tail to be exactly 200-byte-periodic; 74 does not divide
    // 200, so the guard could never fire and the loop ran 64.5 s / 6,194
    // tokens until the user cancelled. Any-period detection must catch it.
    let unit = "Exit note says 'DeepSeek temporarily disabled \u{00E2}\u{20AC}\u{201D} exit notes staged. ";
    assert_eq!(unit.chars().count(), 69);
    assert_eq!(unit.len(), 74);
    // 10 repetitions (740 bytes) — a genuine degenerate loop.
    assert!(detect_repetition(&unit.repeat(10), 200, 3));
    // 8 repetitions (592 bytes) stay below window × threshold (600) → no fire.
    assert!(!detect_repetition(&unit.repeat(8), 200, 3));
}

#[test]
fn bound_repetition_buffer_returns_short_text_unchanged() {
    // Below the cap → returned as-is (no truncation, no allocation).
    let text = "x".repeat(100);
    let out = bound_repetition_buffer(text.clone(), 600, 2048);
    assert_eq!(out, text);
}

#[test]
fn bound_repetition_buffer_truncates_to_tail_when_over_cap() {
    // 3000 bytes, cap 2048, needed 600 → result is the last 600 bytes.
    let text = "abcdefgh".repeat(375); // 3000 bytes
    assert_eq!(text.len(), 3000);
    let out = bound_repetition_buffer(text.clone(), 600, 2048);
    assert_eq!(out.len(), 600);
    // The tail must be preserved exactly.
    assert_eq!(out, &text[text.len() - 600..]);
}

#[test]
fn bound_repetition_buffer_preserves_detection_semantics() {
    // After truncation, detect_repetition must still fire on a genuine
    // repetition at the tail — the truncated buffer's tail is identical
    // to the full buffer's tail.
    let segment = "x".repeat(200);
    let prefix = "p".repeat(2000); // push past the cap
    let text = format!("{prefix}{segment}{segment}{segment}");
    assert!(detect_repetition(&text, 200, 3));
    let bounded = bound_repetition_buffer(text.clone(), 600, 2048);
    assert!(bounded.len() <= 2048);
    assert!(detect_repetition(&bounded, 200, 3));
}

#[test]
fn bound_repetition_buffer_stays_bounded_across_many_appends() {
    // Simulate the stream loop: append many deltas, bound after each.
    // The buffer must never exceed the cap (+ one delta) regardless of
    // how much text the model emits.
    let cap = 2048;
    let needed = 600;
    let mut buf = String::new();
    // 10 000 bytes of varied content — far beyond the cap.
    for i in 0..1000 {
        buf.push_str(&format!("delta-{i:04} ")); // ~11 bytes each
        buf = bound_repetition_buffer(buf, needed, cap);
    }
    // The buffer never grew past cap + a single delta's worth.
    assert!(buf.len() <= cap + 64, "buf.len() = {}", buf.len());
    // And it still holds at least `needed` bytes for the next check.
    assert!(buf.len() >= needed);
}

#[test]
fn bound_repetition_buffer_handles_multibyte_without_panic() {
    // "é" is 2 bytes; needed=600 is even so it lands on a boundary, but
    // needed=601 would land mid-char — the boundary guard must back up.
    let text = "é".repeat(1500); // 3000 bytes
    let out = bound_repetition_buffer(text.clone(), 601, 2048);
    // Must not panic and must be valid UTF-8 (to_string guarantees it).
    assert!(out.len() <= 602); // backed up at most one byte
    assert!(out.chars().all(|c| c == 'é'));
}

#[test]
fn parse_sse_buffer_handles_multi_line_chunk() {
    // A single chunk containing multiple complete SSE lines must be split
    // and each parsed. This is the path the drain() fix targets — the old
    // to_string() copy was O(lines²) here.
    let mut buffer = String::new();
    buffer.push_str("data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n");
    buffer.push_str("data: {\"choices\":[{\"delta\":{\"content\":\"!\"}}]}\n");

    let outcomes = parse_sse_buffer(&mut buffer);
    // Two text deltas.
    let texts: Vec<&str> = outcomes
        .iter()
        .filter_map(|o| match o {
            SseOutcome::Event(LlmEvent::TextDelta { text }) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, vec!["Hi", "!"]);
    // The buffer is fully drained (both lines had trailing newlines).
    assert!(buffer.is_empty());
}

#[test]
fn parse_sse_buffer_retains_incomplete_trailing_line() {
    // A line without a trailing newline is incomplete — it must stay in
    // the buffer for the next chunk (SSE lines are newline-delimited).
    let mut buffer = String::from(
        "data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\n\
         data: {\"choices\":[{\"delta\":{\"content\":\"partial",
    );
    let outcomes = parse_sse_buffer(&mut buffer);
    // Only the first (complete) line was parsed. It yields a TextDelta
    // plus a RawAssistantDelta (Rule 1 raw capture).
    assert_eq!(outcomes.len(), 2);
    // The incomplete trailing line remains in the buffer.
    assert_eq!(
        buffer,
        "data: {\"choices\":[{\"delta\":{\"content\":\"partial"
    );
}

#[test]
fn parse_sse_buffer_appends_next_chunk_correctly() {
    // Simulate two chunks arriving: the first ends mid-line, the second
    // completes it. The retained partial + the new bytes must parse as one
    // event (not two, not zero).
    let mut buffer = String::from("data: {\"choices\":[{\"delta\":{\"content\":\"Hel");
    // First parse — nothing complete yet.
    let outcomes = parse_sse_buffer(&mut buffer);
    assert!(outcomes.is_empty());
    assert_eq!(buffer, "data: {\"choices\":[{\"delta\":{\"content\":\"Hel");

    // Second chunk arrives, completing the line.
    buffer.push_str("lo\"}}]}\n");
    let outcomes = parse_sse_buffer(&mut buffer);
    // TextDelta + RawAssistantDelta (Rule 1 raw capture).
    assert_eq!(outcomes.len(), 2);
    match &outcomes[0] {
        SseOutcome::Event(LlmEvent::TextDelta { text }) => assert_eq!(text, "Hello"),
        other => panic!("expected TextDelta, got {other:?}"),
    }
    assert!(buffer.is_empty());
}

#[test]
fn parse_sse_buffer_skips_comments_and_empty_lines() {
    let mut buffer = String::from(
        ": this is a comment\n\n\
         data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n",
    );
    let outcomes = parse_sse_buffer(&mut buffer);
    // TextDelta + RawAssistantDelta (Rule 1 raw capture).
    assert_eq!(outcomes.len(), 2);
    assert!(buffer.is_empty());
}

#[test]
fn parse_sse_buffer_handles_done_sentinel() {
    let mut buffer = String::from(
        "data: {\"choices\":[{\"delta\":{\"content\":\"end\"}}]}\n\
         data: [DONE]\n",
    );
    let outcomes = parse_sse_buffer(&mut buffer);
    // [DONE] is skipped — the one text delta plus its RawAssistantDelta
    // (Rule 1 raw capture).
    assert_eq!(outcomes.len(), 2);
    assert!(buffer.is_empty());
}

#[test]
fn parse_sse_buffer_reports_parse_errors() {
    let mut buffer = String::from("data: {not valid json}\n");
    let outcomes = parse_sse_buffer(&mut buffer);
    assert_eq!(outcomes.len(), 1);
    match &outcomes[0] {
        SseOutcome::ParseError(msg) => {
            assert!(msg.contains("failed to parse SSE chunk"));
            assert!(msg.contains("not valid json"));
        }
        other => panic!("expected ParseError, got {other:?}"),
    }
    assert!(buffer.is_empty());
}

#[test]
fn parse_sse_buffer_drains_without_copying_large_buffers() {
    // Stress the drain path with many lines in one chunk — verifies the
    // in-place drain handles a large buffer without the O(n²) re-copy.
    let mut buffer = String::new();
    for i in 0..200 {
        buffer.push_str(&format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{i}\"}}}}]}}\n"
        ));
    }
    let outcomes = parse_sse_buffer(&mut buffer);
    // Each line yields a TextDelta + a RawAssistantDelta (Rule 1 raw
    // capture) → 400 outcomes.
    assert_eq!(outcomes.len(), 400);
    // All 200 lines drained.
    assert!(buffer.is_empty());
    // Verify the last text delta parsed correctly (every even index is a
    // TextDelta; 2*199 = 398).
    match &outcomes[398] {
        SseOutcome::Event(LlmEvent::TextDelta { text }) => assert_eq!(text, "199"),
        other => panic!("expected TextDelta, got {other:?}"),
    }
}

// --- parse_sse_chunk unit tests (canned JSON → LlmEvent) ---

#[test]
fn parse_sse_chunk_text_delta() {
    let json = serde_json::json!({
        "choices": [{"delta": {"content": "Hello"}}]
    });
    let events = parse_sse_chunk(json);
    // TextDelta + RawAssistantDelta (Rule 1 raw capture).
    assert_eq!(events.len(), 2);
    match &events[0] {
        LlmEvent::TextDelta { text } => assert_eq!(text, "Hello"),
        other => panic!("expected TextDelta, got {other:?}"),
    }
}

#[test]
fn parse_sse_chunk_multiple_choices() {
    // Each choice's delta is taken and moved independently (mem-perf
    // review LOW 6): two choices -> two TextDelta + two RawAssistantDelta
    // events, each raw delta carrying only its own choice's payload.
    let json = serde_json::json!({
        "choices": [
            {"delta": {"content": "Hello"}},
            {"delta": {"content": " world"}}
        ]
    });
    let events = parse_sse_chunk(json);
    assert_eq!(events.len(), 4);
    match &events[0] {
        LlmEvent::TextDelta { text } => assert_eq!(text, "Hello"),
        other => panic!("expected TextDelta, got {other:?}"),
    }
    match &events[1] {
        LlmEvent::RawAssistantDelta { delta } => {
            assert_eq!(delta.get("content"), Some(&serde_json::json!("Hello")));
        }
        other => panic!("expected RawAssistantDelta, got {other:?}"),
    }
    match &events[2] {
        LlmEvent::TextDelta { text } => assert_eq!(text, " world"),
        other => panic!("expected TextDelta, got {other:?}"),
    }
    match &events[3] {
        LlmEvent::RawAssistantDelta { delta } => {
            assert_eq!(delta.get("content"), Some(&serde_json::json!(" world")));
        }
        other => panic!("expected RawAssistantDelta, got {other:?}"),
    }
}

#[test]
fn parse_sse_chunk_empty_content_is_skipped() {
    // An empty content string should not emit a TextDelta (avoids no-op
    // events in the stream).
    let json = serde_json::json!({
        "choices": [{"delta": {"content": ""}}]
    });
    let events = parse_sse_chunk(json);
    // No TextDelta for empty content, but the verbatim delta is still
    // captured as a RawAssistantDelta (Rule 1 raw — the merge skips the
    // empty content, so it contributes nothing to the raw message).
    assert!(events
        .iter()
        .all(|e| !matches!(e, LlmEvent::TextDelta { .. })));
    assert_eq!(events.len(), 1);
}

#[test]
fn parse_sse_chunk_reasoning_content() {
    let json = serde_json::json!({
        "choices": [{"delta": {"reasoning_content": "thinking..."}}]
    });
    let events = parse_sse_chunk(json);
    // ReasoningDelta + RawAssistantDelta (Rule 1 raw capture).
    assert_eq!(events.len(), 2);
    match &events[0] {
        LlmEvent::ReasoningDelta { text } => assert_eq!(text, "thinking..."),
        other => panic!("expected ReasoningDelta, got {other:?}"),
    }
}

#[test]
fn parse_sse_chunk_reasoning_field_alias() {
    // Regression (traces 2026-08-22): Ollama's OpenAI-compatible endpoint
    // streams thinking in `delta.reasoning` (not `reasoning_content`) —
    // deepseek-v4-flash sends `{"content":"","reasoning":"…"}`, so before
    // this fallback the whole thinking phase produced zero events.
    let json = serde_json::json!({
        "choices": [{"delta": {"content": "", "reasoning": "thinking..."}}]
    });
    let events = parse_sse_chunk(json);
    // ReasoningDelta + RawAssistantDelta (Rule 1 raw capture).
    assert_eq!(events.len(), 2);
    match &events[0] {
        LlmEvent::ReasoningDelta { text } => assert_eq!(text, "thinking..."),
        other => panic!("expected ReasoningDelta, got {other:?}"),
    }
}

#[test]
fn parse_sse_chunk_reasoning_content_wins_over_reasoning_alias() {
    // Both fields present → the canonical `reasoning_content` wins.
    let json = serde_json::json!({
        "choices": [{"delta": {"reasoning_content": "primary", "reasoning": "alias"}}]
    });
    let events = parse_sse_chunk(json);
    // ReasoningDelta + RawAssistantDelta (Rule 1 raw capture).
    assert_eq!(events.len(), 2);
    match &events[0] {
        LlmEvent::ReasoningDelta { text } => assert_eq!(text, "primary"),
        other => panic!("expected ReasoningDelta, got {other:?}"),
    }
}

// ---- ThinkTagFilter: inline <think>…</think> reasoning blocks ----

/// Drain the filter over a sequence of content fragments, collecting all
/// emitted events (including the stream-end flush).
fn think_filter_all(fragments: &[&str]) -> Vec<LlmEvent> {
    let mut f = ThinkTagFilter::new();
    let mut out = Vec::new();
    for frag in fragments {
        out.extend(f.feed(frag));
    }
    out.extend(f.finish());
    out
}

/// Render an event stream as "R:text" / "T:text" pairs for compact
/// assertions.
fn think_event_kinds(events: &[LlmEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| match e {
            LlmEvent::ReasoningDelta { text } => format!("R:{text}"),
            LlmEvent::TextDelta { text } => format!("T:{text}"),
            other => panic!("unexpected event {other:?}"),
        })
        .collect()
}

#[test]
fn think_filter_basic_block() {
    // The canonical R1/Ollama shape: tags and both channels in one delta.
    let events = think_filter_all(&["<think>hidden</think>shown"]);
    assert_eq!(think_event_kinds(&events), ["R:hidden", "T:shown"]);
}

#[test]
fn think_filter_split_open_and_close_tags() {
    // Tags split across chunk boundaries must still be recognized.
    let events = think_filter_all(&["<th", "ink>abc</th", "ink>def"]);
    assert_eq!(think_event_kinds(&events), ["R:abc", "T:def"]);
}

#[test]
fn think_filter_leading_whitespace_before_open_tag() {
    let events = think_filter_all(&["  \n", "<think>x</think>y"]);
    assert_eq!(think_event_kinds(&events), ["R:x", "T:y"]);
}

#[test]
fn think_filter_no_tag_passes_through() {
    let events = think_filter_all(&["Hello", " world"]);
    assert_eq!(think_event_kinds(&events), ["T:Hello", "T: world"]);
}

#[test]
fn think_filter_literal_tag_mid_answer_passes_through() {
    // Position-0 rule: a `<think>` appearing after the answer started is
    // literal text, not a reasoning block.
    let events = think_filter_all(&["foo <th", "ink> bar"]);
    assert_eq!(think_event_kinds(&events), ["T:foo <th", "T:ink> bar"]);
}

#[test]
fn think_filter_unclosed_block_flushes_as_reasoning() {
    // The model thought but never answered: the held-back partial close
    // tag flushes as reasoning too.
    let events = think_filter_all(&["<think>abc</th"]);
    assert_eq!(think_event_kinds(&events), ["R:abc", "R:</th"]);
}

#[test]
fn think_filter_undecided_prefix_flushes_as_text() {
    // Stream ends while still undecided — it was never a think block.
    let events = think_filter_all(&["<th"]);
    assert_eq!(think_event_kinds(&events), ["T:<th"]);
}

#[test]
fn think_filter_empty_block_then_answer() {
    // An empty think block emits no ReasoningDelta (the no-empty-deltas
    // invariant); the answer still follows as text.
    let events = think_filter_all(&["<think></think>answer"]);
    assert_eq!(think_event_kinds(&events), ["T:answer"]);
}

#[test]
fn parse_sse_chunk_tool_call_start_and_arg_delta() {
    // The first tool-call delta carries id + name (→ ToolCallStart) and
    // an arguments fragment (→ ToolCallArgumentDelta).
    let json = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "function": {"name": "file_read", "arguments": "{\"path\":"}
                }]
            }
        }]
    });
    let events = parse_sse_chunk(json);
    // ToolCallStart + ToolCallArgumentDelta + RawAssistantDelta (Rule 1).
    assert_eq!(events.len(), 3);
    match &events[0] {
        LlmEvent::ToolCallStart { index, id, name } => {
            assert_eq!(*index, 0);
            assert_eq!(id, "call_1");
            assert_eq!(name, "file_read");
        }
        other => panic!("expected ToolCallStart, got {other:?}"),
    }
    match &events[1] {
        LlmEvent::ToolCallArgumentDelta { index, fragment } => {
            assert_eq!(*index, 0);
            assert_eq!(fragment, "{\"path\":");
        }
        other => panic!("expected ToolCallArgumentDelta, got {other:?}"),
    }
}

#[test]
fn parse_sse_chunk_tool_call_arg_only_delta() {
    // Subsequent deltas carry only an arguments fragment (no id/name) —
    // should emit only a ToolCallArgumentDelta, no ToolCallStart.
    let json = serde_json::json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 1,
                    "function": {"arguments": "\"test.txt\"}"}
                }]
            }
        }]
    });
    let events = parse_sse_chunk(json);
    // ToolCallArgumentDelta + RawAssistantDelta (Rule 1 raw capture).
    assert_eq!(events.len(), 2);
    match &events[0] {
        LlmEvent::ToolCallArgumentDelta { index, fragment } => {
            assert_eq!(*index, 1);
            assert_eq!(fragment, "\"test.txt\"}");
        }
        other => panic!("expected ToolCallArgumentDelta, got {other:?}"),
    }
}

#[test]
fn parse_sse_chunk_captures_message_level_thought_signature_in_raw() {
    // Gemini streams thought_signature at the delta/assistant-message level.
    // It is now captured verbatim in RawAssistantDelta (Rule 1 raw echo
    // supersedes the old ProviderMeta allowlist).
    let json = serde_json::json!({
        "choices": [{"delta": {"thought_signature": "opaque_sig"}}]
    });
    let events = parse_sse_chunk(json);
    // Only RawAssistantDelta (no text, no tool calls, no ProviderMeta).
    assert_eq!(events.len(), 1);
    match &events[0] {
        LlmEvent::RawAssistantDelta { delta } => {
            assert_eq!(
                delta.get("thought_signature"),
                Some(&serde_json::json!("opaque_sig")),
            );
        }
        other => panic!("expected RawAssistantDelta, got {other:?}"),
    }
}

#[test]
fn parse_sse_chunk_captures_per_tool_call_thought_signature_in_raw() {
    // Gemini attaches thought_signature to functionCall entries. It is now
    // captured verbatim in RawAssistantDelta (Rule 1 raw echo supersedes
    // the old ProviderMeta allowlist).
    let json = serde_json::json!({
        "choices": [{"delta": {"tool_calls": [{
            "index": 0,
            "id": "call_1",
            "function": {"name": "read", "arguments": "{}"},
            "thought_signature": "call_sig"
        }]}}]
    });
    let events = parse_sse_chunk(json);
    // ToolCallStart + ToolCallArgumentDelta + RawAssistantDelta.
    assert_eq!(events.len(), 3);
    let raw = events.iter().find_map(|e| match e {
        LlmEvent::RawAssistantDelta { delta } => Some(delta),
        _ => None,
    });
    let raw = raw.expect("RawAssistantDelta should be emitted");
    let tc = &raw["tool_calls"][0];
    assert_eq!(
        tc.get("thought_signature"),
        Some(&serde_json::json!("call_sig")),
    );
    assert_eq!(tc["function"]["name"], serde_json::json!("read"));
}

#[test]
fn parse_sse_chunk_finish_reason_stop() {
    let json = serde_json::json!({
        "choices": [{"finish_reason": "stop"}]
    });
    let events = parse_sse_chunk(json);
    assert_eq!(events.len(), 1);
    match &events[0] {
        LlmEvent::Finish { reason } => {
            assert_eq!(*reason, crate::provider::FinishReason::Stop);
        }
        other => panic!("expected Finish, got {other:?}"),
    }
}

#[test]
fn parse_sse_chunk_finish_reason_tool_calls() {
    let json = serde_json::json!({
        "choices": [{"finish_reason": "tool_calls"}]
    });
    let events = parse_sse_chunk(json);
    assert_eq!(events.len(), 1);
    match &events[0] {
        LlmEvent::Finish { reason } => {
            assert_eq!(*reason, crate::provider::FinishReason::ToolCalls);
        }
        other => panic!("expected Finish, got {other:?}"),
    }
}

#[test]
fn parse_sse_chunk_finish_reason_other() {
    // An unrecognized finish_reason maps to FinishReason::Other.
    let json = serde_json::json!({
        "choices": [{"finish_reason": "content_filter"}]
    });
    let events = parse_sse_chunk(json);
    assert_eq!(events.len(), 1);
    match &events[0] {
        LlmEvent::Finish { reason } => {
            assert_eq!(*reason, crate::provider::FinishReason::ContentFilter);
        }
        other => panic!("expected Finish, got {other:?}"),
    }
}

#[test]
fn parse_sse_chunk_usage() {
    let json = serde_json::json!({
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "completion_tokens_details": {"reasoning_tokens": 20},
            "prompt_tokens_details": {"cached_tokens": 80}
        }
    });
    let events = parse_sse_chunk(json);
    assert_eq!(events.len(), 1);
    match &events[0] {
        LlmEvent::Usage {
            prompt_tokens,
            completion_tokens,
            reasoning_tokens,
            cached_tokens,
            ttft_ms,
            generation_ms,
        } => {
            assert_eq!(*prompt_tokens, 100);
            assert_eq!(*completion_tokens, 50);
            assert_eq!(*reasoning_tokens, 20);
            assert_eq!(*cached_tokens, 80);
            // parse_sse_chunk has no clock — timing is None here and
            // enriched by the stream loop before the event is sent.
            assert_eq!(*ttft_ms, None);
            assert_eq!(*generation_ms, None);
        }
        other => panic!("expected Usage, got {other:?}"),
    }
}

#[test]
fn parse_sse_chunk_usage_without_cached_details() {
    // When prompt_tokens_details is absent, cached_tokens defaults to 0.
    let json = serde_json::json!({
        "usage": {"prompt_tokens": 10, "completion_tokens": 5}
    });
    let events = parse_sse_chunk(json);
    assert_eq!(events.len(), 1);
    match &events[0] {
        LlmEvent::Usage { cached_tokens, .. } => assert_eq!(*cached_tokens, 0),
        other => panic!("expected Usage, got {other:?}"),
    }
}

#[test]
fn parse_sse_chunk_usage_without_reasoning_details() {
    // When completion_tokens_details is absent, reasoning_tokens defaults to 0.
    let json = serde_json::json!({
        "usage": {"prompt_tokens": 10, "completion_tokens": 5}
    });
    let events = parse_sse_chunk(json);
    assert_eq!(events.len(), 1);
    match &events[0] {
        LlmEvent::Usage {
            reasoning_tokens, ..
        } => assert_eq!(*reasoning_tokens, 0),
        other => panic!("expected Usage, got {other:?}"),
    }
}

#[test]
fn parse_sse_chunk_combined_text_and_finish() {
    // A single chunk can carry both a text delta and a finish reason.
    let json = serde_json::json!({
        "choices": [{"delta": {"content": "done"}, "finish_reason": "stop"}]
    });
    let events = parse_sse_chunk(json);
    // TextDelta + Finish + RawAssistantDelta (Rule 1 raw capture).
    assert_eq!(events.len(), 3);
    assert!(matches!(events[0], LlmEvent::TextDelta { .. }));
    assert!(matches!(events[1], LlmEvent::Finish { .. }));
}

#[test]
fn parse_sse_chunk_empty_choices() {
    // A chunk with no choices (e.g. a usage-only final chunk) emits only
    // the usage event, if present.
    let json = serde_json::json!({"choices": []});
    let events = parse_sse_chunk(json);
    assert!(events.is_empty());
}

#[test]
fn parse_sse_chunk_no_choices_field() {
    let json = serde_json::json!({"usage": {"prompt_tokens": 1, "completion_tokens": 1}});
    let events = parse_sse_chunk(json);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0], LlmEvent::Usage { .. }));
}

#[test]
fn client_builds_request_with_tools() {
    let client = OpenAiClient::new(OpenAiClientConfig::test_default());
    let tools = vec![ToolSchema::new(
        "file_read",
        "read a file",
        serde_json::json!({"type": "object"}),
    )];
    let body = client
        .build_request_json(
            &[Message::user_text("hi")],
            &tools,
            Some(ToolChoice::Auto),
        )
        .unwrap();
    assert_eq!(body["model"], "test-model");
    assert_eq!(body["stream"], true);
    assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    assert_eq!(body["tool_choice"], "auto");
}

#[test]
fn local_client_omits_tool_choice() {
    let client = OpenAiClient::new(OpenAiClientConfig {
        kind: ProviderKind::Local,
        ..OpenAiClientConfig::test_default()
    });
    let body = client
        .build_request_json(
            &[Message::user_text("hi")],
            &[],
            Some(ToolChoice::Auto), // requested, but caps say no
        )
        .unwrap();
    // Local caps → tool_choice omitted (null, not "auto").
    assert!(body.get("tool_choice").map(|v| v.is_null()).unwrap_or(true));
}

#[test]
fn build_request_json_omits_strict_when_none() {
    // The json! macro serializes None as null, which litellm/vertex
    // rejects. When strict is None, the field must be absent entirely.
    let client = OpenAiClient::new(OpenAiClientConfig::test_default());
    let tools = vec![ToolSchema::new(
        "file_read",
        "read a file",
        serde_json::json!({"type": "object"}),
    )];
    let body = client
        .build_request_json(
            &[Message::user_text("hi")],
            &tools,
            None,
        )
        .unwrap();
    let tools_arr = body["tools"].as_array().unwrap();
    assert_eq!(tools_arr.len(), 1);
    let function = &tools_arr[0]["function"];
    // strict must be absent (not null) when None.
    assert!(
        function.get("strict").is_none(),
        "strict should be omitted when None, got: {:?}",
        function.get("strict")
    );
}

#[test]
fn build_request_json_includes_strict_when_some() {
    let client = OpenAiClient::new(OpenAiClientConfig::test_default());
    let mut schema = ToolSchema::new(
        "file_read",
        "read a file",
        serde_json::json!({"type": "object"}),
    );
    schema.strict = Some(true);
    let body = client
        .build_request_json(
            &[Message::user_text("hi")],
            &[schema],
            None,
        )
        .unwrap();
    let function = &body["tools"][0]["function"];
    assert_eq!(function["strict"], serde_json::json!(true));
}

#[test]
fn estimate_prompt_tokens_counts_content_and_tools() {
    let msgs = vec![Message::user_text("hello world")]; // 11 chars
    // 1 message * 4 overhead + 11 chars / 4 = 4 + 2 = 6.
    assert_eq!(
        crate::provider::estimate_prompt_tokens(&msgs, &[]),
        4 + 11 / 4
    );
    let tools = vec![ToolSchema::new(
        "file_read",
        "read a file",
        serde_json::json!({"type": "object"}),
    )];
    let est = crate::provider::estimate_prompt_tokens(&msgs, &tools);
    // Tool chars: "file_read"(9) + "read a file"(11) + '{"type":"object"}'(18) = 38.
    // Total chars = 11 + 38 = 49. 49/4 = 12. + 4 overhead = 16.
    assert_eq!(est, 4 + (11 + 9 + 11 + 18) / 4);
}

#[test]
fn max_completion_tokens_uncapped_when_prompt_small() {
    // Default OpenAI caps: 128K context, 32K output. A tiny prompt
    // leaves plenty of room → max_completion_tokens = 32_000 (uncapped).
    let client = OpenAiClient::new(OpenAiClientConfig::test_default());
    let body = client
        .build_request_json(
            &[Message::user_text("hi")],
            &[],
            None,
        )
        .unwrap();
    assert_eq!(body["max_completion_tokens"], serde_json::json!(32_000));
}

#[test]
fn max_completion_tokens_capped_at_sane_ceiling() {
    // A large-context endpoint (1M context) with a huge output budget
    // (131_072) would send 128K output tokens — wasteful for coding
    // tasks. The sane ceiling (32K) kicks in even though the R9
    // context-window cap never triggers (1M − tiny prompt − 1024 ≫ 131_072).
    let client = OpenAiClient::new(OpenAiClientConfig {
        max_context: Some(1_000_000),
        max_output_tokens: Some(131_072),
        ..OpenAiClientConfig::test_default()
    });
    let body = client
        .build_request_json(
            &[Message::user_text("hi")],
            &[],
            None,
        )
        .unwrap();
    assert_eq!(
        body["max_completion_tokens"],
        serde_json::json!(32_000),
        "sane ceiling should cap 131_072 → 32_000"
    );
}

#[test]
fn max_completion_tokens_capped_when_prompt_large() {
    // Simulate a near-full context: max_context = 10_000, max_output = 8_000.
    // A 6_000-char prompt (~1_500 tokens est) should cap output to
    // min(8000, 10000 - 1500 - 1024) = min(8000, 7476) = 7476.
    let client = OpenAiClient::new(OpenAiClientConfig {
        max_context: Some(10_000),
        max_output_tokens: Some(8_000),
        ..OpenAiClientConfig::test_default()
    });
    let big_text = "x".repeat(6_000);
    let body = client
        .build_request_json(
            &[Message::user_text(big_text)],
            &[],
            None,
        )
        .unwrap();
    let mct = body["max_completion_tokens"].as_u64().unwrap();
    // prompt_est = 4 + 6000/4 = 4 + 1500 = 1504.
    // raw cap = min(8000, 10000 - 1504 - 1024) = min(8000, 7472) = 7472.
    // Quantized to 2048-token bucket = (7472 / 2048) * 2048 = 6144.
    assert_eq!(
        mct, 6144,
        "should be capped to fit context window and quantized to 2048-token bucket"
    );
}

#[test]
fn max_completion_tokens_quantization_prevents_per_turn_jitter() {
    // Two consecutive turns with slightly different prompt sizes (e.g. +80 chars
    // from a new tool result) produce the EXACT same max_completion_tokens when
    // in the same 2048-token bucket, preventing proxy-level cache busts.
    let client = OpenAiClient::new(OpenAiClientConfig {
        max_context: Some(10_000),
        max_output_tokens: Some(8_000),
        ..OpenAiClientConfig::test_default()
    });
    // Turn A: 6000 chars -> prompt_est 1504 -> remaining budget 7472 -> quantized 6144
    let body_a = client
        .build_request_json(&[Message::user_text("x".repeat(6_000))], &[], None)
        .unwrap();
    // Turn B: 6300 chars (+300 chars) -> prompt_est 1579 -> remaining budget 7397 -> quantized 6144
    let body_b = client
        .build_request_json(&[Message::user_text("x".repeat(6_300))], &[], None)
        .unwrap();
    assert_eq!(
        body_a["max_completion_tokens"],
        body_b["max_completion_tokens"],
        "max_completion_tokens must remain identical across nearby turns to preserve cache"
    );
    assert_eq!(body_a["max_completion_tokens"], serde_json::json!(6144));
}

#[test]
fn max_completion_tokens_floors_at_1024_when_prompt_exceeds_context() {
    // Prompt larger than max_context → cap would go negative → floors at 1024.
    let client = OpenAiClient::new(OpenAiClientConfig {
        max_context: Some(1_000),
        max_output_tokens: Some(8_000),
        ..OpenAiClientConfig::test_default()
    });
    let big_text = "x".repeat(10_000);
    let body = client
        .build_request_json(
            &[Message::user_text(big_text)],
            &[],
            None,
        )
        .unwrap();
    let mct = body["max_completion_tokens"].as_u64().unwrap();
    assert_eq!(
        mct, 1024,
        "should floor at 1024 when prompt exceeds context"
    );
}

#[test]
fn build_request_json_includes_reasoning_effort_when_set() {
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "glm-5.2".into(),
        reasoning_effort: Some("max".into()),
        ..OpenAiClientConfig::test_default()
    });
    let body = client
        .build_request_json(
            &[Message::user_text("hi")],
            &[],
            None,
        )
        .unwrap();
    assert_eq!(body["reasoning_effort"], serde_json::json!("max"));
}

#[test]
fn build_request_json_carries_per_model_configured_effort() {
    // Backlog 5b099aef: a per-model `reasoning_effort` (the per-model row
    // dropdown) flows through the shared construction path —
    // effective_reasoning_effort_for → openai_client_config → the request
    // body — so the persisted per-model default lands on the wire.
    use crate::config::{Config, Endpoint, ModelSpec};
    use crate::provider::client_factory::openai_client_config;

    let ep = Endpoint {
        name: "gateway".into(),
        base_url: "https://gateway.example.com/v1/".into(),
        models: vec![ModelSpec {
            id: "glm".into(),
            reasoning_effort: Some("low".into()),
            ..ModelSpec::test_default()
        }],
        reasoning_effort: Some("high".into()),
        ..Endpoint::test_default()
    };
    let config = Config::default();
    let client = OpenAiClient::new(openai_client_config(
        &config,
        &ep,
        "glm",
        false,
        ep.effective_reasoning_effort_for(Some("glm")),
    ));
    let body = client
        .build_request_json(&[Message::user_text("hi")], &[], None)
        .unwrap();
    assert_eq!(
        body["reasoning_effort"],
        serde_json::json!("low"),
        "the per-model configured effort must land on the wire, got: {:?}",
        body.get("reasoning_effort")
    );
}

#[test]
fn build_request_json_maps_off_effort_to_none_for_deepseek() {
    // Regression (DeepSeek instant-400): "off" is not in DeepSeek's
    // reasoning_effort enum (none|minimal|low|medium|high|xhigh|max) — a
    // literal "off" is rejected with a non-retryable HTTP 400. The
    // harness's "off" must encode as the enum's explicit thinking-off
    // value "none" for DeepSeek-family models, never reach the wire
    // verbatim.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4-flash".into(),
        reasoning_effort: Some("off".into()),
        ..OpenAiClientConfig::test_default()
    });
    let body = client
        .build_request_json(&[Message::user_text("hi")], &[], None)
        .unwrap();
    assert_eq!(
        body["reasoning_effort"],
        serde_json::json!("none"),
        "DeepSeek-family models must encode \"off\" as \"none\", got: {:?}",
        body.get("reasoning_effort")
    );
}

#[test]
fn build_request_json_omits_off_effort_for_non_deepseek() {
    // Non-DeepSeek providers have no "none" variant: the harness's "off"
    // omits the field entirely (never a literal "off" on the wire).
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "gpt-4o".into(),
        reasoning_effort: Some("off".into()),
        ..OpenAiClientConfig::test_default()
    });
    let body = client
        .build_request_json(&[Message::user_text("hi")], &[], None)
        .unwrap();
    assert!(
        body.get("reasoning_effort").is_none(),
        "\"off\" must omit the field for non-DeepSeek providers, got: {:?}",
        body.get("reasoning_effort")
    );
}

#[test]
fn build_request_json_off_wire_config_overrides_policy() {
    // The endpoint/model `reasoning_effort_off_wire` config encodes "off"
    // regardless of the model's name — the escape hatch for renamed/aliased/
    // fine-tuned DeepSeek models the name-based policy can't see.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "qwen3-max".into(),
        reasoning_effort: Some("off".into()),
        reasoning_effort_off_wire: Some("none".into()),
        ..OpenAiClientConfig::test_default()
    });
    let body = client
        .build_request_json(&[Message::user_text("hi")], &[], None)
        .unwrap();
    assert_eq!(
        body["reasoning_effort"],
        serde_json::json!("none"),
        "a configured reasoning_effort_off_wire must encode \"off\" regardless of model name, got: {:?}",
        body.get("reasoning_effort")
    );
}

#[test]
fn build_request_json_off_wire_config_beats_deepseek_policy() {
    // When both the config and the name-based policy would apply, the
    // explicit config wins — the user's setting is authoritative over the
    // zero-config default.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4-flash".into(),
        reasoning_effort: Some("off".into()),
        reasoning_effort_off_wire: Some("disabled".into()),
        ..OpenAiClientConfig::test_default()
    });
    let body = client
        .build_request_json(&[Message::user_text("hi")], &[], None)
        .unwrap();
    assert_eq!(
        body["reasoning_effort"],
        serde_json::json!("disabled"),
        "an explicit reasoning_effort_off_wire must override the policy default, got: {:?}",
        body.get("reasoning_effort")
    );
}

#[test]
fn build_request_json_omits_reasoning_effort_when_none() {
    // "off" maps to None — the field must be absent entirely so endpoints
    // that don't accept it never see it.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "glm-5.2".into(),
        ..OpenAiClientConfig::test_default()
    });
    let body = client
        .build_request_json(
            &[Message::user_text("hi")],
            &[],
            None,
        )
        .unwrap();
    assert!(
        body.get("reasoning_effort").is_none(),
        "reasoning_effort should be omitted when None, got: {:?}",
        body.get("reasoning_effort")
    );
}

#[test]
fn build_request_json_injects_glm_stop_boundaries_for_glm_53() {
    // Backlog 82a9480c: GLM-5.3-Flash introduced new tokenizer boundaries
    // (the user/assistant/observation role tags + the triple-newline
    // cascade) that servers don't enforce — the model runs past a closing
    // tag into thousands of empty lines. The request must carry the
    // explicit EOS stops: the string boundaries AND their token ids
    // (belt+suspenders for servers that honor only one of the two).
    // Config-driven since 2027-01-05 (backlog f322277c): the boundaries
    // come from stop_boundary_strings / stop_token_ids on the client
    // config (resolved from endpoints.toml in production), never from a
    // model-name prefix match.
    // Transport-safe expectations: build the boundary tags from the same
    // escapes as the production constants — raw angle-bracket tag text
    // has been stripped in text transports during development (round-1).
    let endoftext = "\u{3c}|endoftext|\u{3e}";
    let user = "\u{3c}|user|\u{3e}";
    let assistant = "\u{3c}|assistant|\u{3e}";
    let observation = "\u{3c}|observation|\u{3e}";
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "glm-5.3-flash".into(),
        stop_boundary_strings: vec![
            endoftext.into(),
            user.into(),
            assistant.into(),
            observation.into(),
            "\n\n\n\n".into(),
            "\n\n\n".into(),
        ],
        stop_token_ids: vec![151329, 151330, 151336],
        ..OpenAiClientConfig::test_default()
    });
    let body = client
        .build_request_json(&[Message::user_text("hi")], &[], None)
        .unwrap();
    assert_eq!(
        body["stop"],
        serde_json::json!([endoftext, user, assistant, observation, "\n\n\n\n", "\n\n\n"]),
        "stop sequences must carry the configured boundaries, got: {:?}",
        body.get("stop")
    );
    assert_eq!(
        body["stop_token_ids"],
        serde_json::json!([151329, 151330, 151336]),
        "stop_token_ids must carry the configured boundary tokens, got: {:?}",
        body.get("stop_token_ids")
    );
    // The stops must be the REAL boundary tags, never empty strings: an
    // empty stop sequence makes OpenAI-compatible servers stop on the
    // first token (or reject the request) — the round-1 defect.
    for seq in body["stop"].as_array().expect("stop array").iter() {
        let s = seq.as_str().expect("string stop");
        assert!(!s.is_empty(), "empty stop sequence leaked: {:?}", body["stop"]);
    }
}

#[test]
fn build_request_json_escapes_boundary_tokens_in_tool_results() {
    // Backlog 1db26c95: tool results carrying a configured boundary token
    // must be ESCAPED in the request body. The app-side pipeline is clean
    // (the tool result carries the raw token; the serialization is plain),
    // but the serving layer strips/maps it — the model's textual view
    // shows an empty string (live-verified 2027-01-08: a regex search
    // matched the raw token in 11 source lines while every displayed
    // line showed it stripped) — so a read-then-write round-trip silently
    // corrupts the file. The escape is a visible, deterministic marker
    // (⟦raw:\uXXXX…⟧) the model CAN see and re-emit; the file tools
    // restore it on write.
    // Transport-safe expectations: build the boundary tag from the same
    // escapes as the production constants — raw angle-bracket tag text
    // has been stripped in text transports during development (round-1).
    let endoftext = "\u{3c}|endoftext|\u{3e}";
    let marker_prefix = "\u{27e6}raw:";
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "glm-5.3-flash".into(),
        stop_boundary_strings: vec![endoftext.into()],
        ..OpenAiClientConfig::test_default()
    });
    let issuing = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "c1", "type": "function",
                "function": { "name": "read_files", "arguments": "{}" }
            }]
        })),
        ..Message::assistant_text("")
    };
    let tool_result = Message::tool_result(
        "c1",
        "read_files",
        &format!("stop = [\"{endoftext}\"]\n"),
    );
    let user = Message::user_text("Reply ok");
    let body = client
        .build_request_json(&[issuing, tool_result, user], &[], None)
        .unwrap();
    // messages[0] is the issuing assistant echo; [1] is the tool result.
    let tool_content = body["messages"][1]["content"]
        .as_str()
        .expect("tool message content");
    assert!(
        tool_content.contains(marker_prefix),
        "the boundary token in a tool result must be escaped to the visible \
         marker, got: {tool_content:?}"
    );
    assert!(
        !tool_content.contains(endoftext),
        "the raw boundary token must not pass through to the request body — \
         the serving layer strips it and the model writes back corrupted \
         content, got: {tool_content:?}"
    );
}

#[test]
fn build_responses_request_json_escapes_boundary_tokens_in_tool_results() {
    // Review L1: the Responses-API path applies the same tool-result
    // boundary-token escape as the chat path (backlog 1db26c95) — a
    // refactor of message_to_responses_input must not silently drop it.
    // Transport-safe expectations: build the boundary tag from the same
    // escapes as the production constants — raw angle-bracket tag text
    // has been stripped in text transports during development (round-1).
    let endoftext = "\u{3c}|endoftext|\u{3e}";
    let marker_prefix = "\u{27e6}raw:";
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "glm-5.3-flash".into(),
        stop_boundary_strings: vec![endoftext.into()],
        use_responses_api: true,
        ..OpenAiClientConfig::test_default()
    });
    let assistant = Message {
        response_id: Some("resp_abc".into()),
        ..Message::assistant_text("calling tool")
    };
    let tool_result = Message::tool_result(
        "c1",
        "read_files",
        &format!("stop = [\"{endoftext}\"]\n"),
    );
    let body = client
        .build_responses_request_json(&[assistant, tool_result], &[], None)
        .unwrap();
    // The anchored response_id holds the assistant turn server-side, so
    // the input carries only the tool result.
    let input = body["input"].as_array().expect("input array");
    assert_eq!(input.len(), 1);
    assert_eq!(
        input[0]["type"],
        serde_json::json!("function_call_output")
    );
    let output = input[0]["output"].as_str().expect("output string");
    assert!(
        output.contains(marker_prefix),
        "the boundary token in a tool result must be escaped to the \
         visible marker, got: {output:?}"
    );
    assert!(
        !output.contains(endoftext),
        "the raw boundary token must not pass through to the request body, \
         got: {output:?}"
    );
}

#[test]
fn build_request_json_omits_glm_stops_for_other_models() {
    // The injection is scoped to CONFIGURED boundaries — glm-5.2 shares
    // the vendor but predates the new boundaries and configures none; no
    // other model's payload may change.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "glm-5.2".into(),
        ..OpenAiClientConfig::test_default()
    });
    let body = client
        .build_request_json(&[Message::user_text("hi")], &[], None)
        .unwrap();
    assert!(
        body.get("stop").is_none(),
        "stop must be absent for models without configured boundaries, got: {:?}",
        body.get("stop")
    );
    assert!(
        body.get("stop_token_ids").is_none(),
        "stop_token_ids must be absent for models without configured boundaries, got: {:?}",
        body.get("stop_token_ids")
    );
}

#[test]
fn build_request_json_sends_configured_boundaries_for_any_model_name() {
    // Config-driven boundaries apply by configuration, not model name:
    // any casing, any vendor, any alias — a renamed or fine-tuned model
    // with stop_boundary_strings configured gets the same protection
    // (the old case-insensitive glm-5.3 prefix match is gone).
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "GLM-5.3-Flash".into(),
        stop_boundary_strings: vec![
            "\u{3c}|endoftext|\u{3e}".into(),
            "\u{3c}|user|\u{3e}".into(),
            "\u{3c}|assistant|\u{3e}".into(),
            "\u{3c}|observation|\u{3e}".into(),
            "\n\n\n\n".into(),
            "\n\n\n".into(),
        ],
        stop_token_ids: vec![151329, 151330, 151336],
        ..OpenAiClientConfig::test_default()
    });
    let body = client
        .build_request_json(&[Message::user_text("hi")], &[], None)
        .unwrap();
    assert_eq!(
        body["stop_token_ids"],
        serde_json::json!([151329, 151330, 151336])
    );
    assert_eq!(
        body["stop"],
        serde_json::json!([
            "\u{3c}|endoftext|\u{3e}",
            "\u{3c}|user|\u{3e}",
            "\u{3c}|assistant|\u{3e}",
            "\u{3c}|observation|\u{3e}",
            "\n\n\n\n",
            "\n\n\n"
        ])
    );
}

#[test]
fn build_request_json_sends_boundaries_for_aliased_model() {
    // An aliased/renamed model (no glm anywhere in the name) with
    // configured stop_boundary_strings gets them in the request stop
    // list — no name-prefix matching exists (backlog f322277c).
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "my-custom-alias".into(),
        stop_boundary_strings: vec!["\u{3c}|endoftext|\u{3e}".into()],
        ..OpenAiClientConfig::test_default()
    });
    let body = client
        .build_request_json(&[Message::user_text("hi")], &[], None)
        .unwrap();
    assert_eq!(
        body["stop"],
        serde_json::json!(["\u{3c}|endoftext|\u{3e}"]),
        "configured boundaries must ride the request stop list regardless of model name"
    );
}

#[test]
fn build_request_json_injects_configured_sampling_stop_and_extra_body() {
    let mut extra = serde_json::Map::new();
    extra.insert("repetition_penalty".into(), serde_json::json!(1.05));
    extra.insert("custom_flag".into(), serde_json::json!(true));

    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "custom-llm".into(),
        temperature: Some(0.2),
        top_p: Some(0.85),
        stop: vec!["<custom_stop>".into()],
        stop_token_ids: vec![99999],
        extra_body: Some(extra),
        ..OpenAiClientConfig::test_default()
    });

    let body = client
        .build_request_json(&[Message::user_text("hello")], &[], None)
        .unwrap();

    assert_eq!(body["temperature"], 0.2);
    assert_eq!(body["top_p"], 0.85);
    assert_eq!(body["stop"], serde_json::json!(["<custom_stop>"]));
    assert_eq!(body["stop_token_ids"], serde_json::json!([99999]));
    assert_eq!(body["repetition_penalty"], 1.05);
    assert_eq!(body["custom_flag"], true);
}

#[test]
fn http_client_uses_connect_timeout_not_total() {
    // The client must use a *connect* timeout (handshake only), NOT a
    // total request timeout. A total timeout kills long-lived SSE streams
    // mid-flight — a reasoning model can think for a long time before the
    // first token, and a long generation runs past any fixed cap. reqwest's
    // Debug impl doesn't expose these fields, so we assert on the named
    // constants that drive the builder configuration.
    assert_eq!(
        OpenAiClient::CONNECT_TIMEOUT,
        std::time::Duration::from_secs(10)
    );
    assert_eq!(
        OpenAiClient::READ_TIMEOUT,
        std::time::Duration::from_secs(90)
    );
    // Sanity: the client builds successfully with this config.
    let _client = OpenAiClient::new(OpenAiClientConfig::test_default());
}

#[test]
fn http_client_is_reused_across_instances() {
    // The reqwest::Client is stored in the struct and reused for every
    // complete() call (self.http_client.post(...)), not created per call.
    // We verify the field exists and is usable. Since reqwest::Client
    // wraps an Arc internally, clones share the same connection pool —
    // the key invariant is that complete() uses self.http_client.
    let client = OpenAiClient::new(OpenAiClientConfig::test_default());
    // The http_client is present and can produce a request builder.
    let _builder = client
        .http_client
        .post("http://localhost/v1/chat/completions");
}

#[test]
fn http_client_advertises_decompression() {
    // The client must be built with gzip/brotli/deflate enabled so a
    // gateway that sends compressed bytes is decoded transparently instead
    // of failing with "error decoding response body". reqwest's Debug
    // output includes the enabled decoders.
    let client = OpenAiClient::new(OpenAiClientConfig::test_default());
    let debug = format!("{:?}", client.http_client);
    assert!(
        debug.contains("gzip"),
        "http_client should enable gzip decompression, got debug: {debug}"
    );
}

#[test]
fn build_request_json_sends_image_blocks_when_multimodal() {
    // When the provider is multimodal, a multipart user message (text +
    // image_url) must be serialized as a content array with both blocks.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "qwen-vl".into(),
        multimodal: true,
        ..OpenAiClientConfig::test_default()
    });
    let msg = Message {
        content: MessageContent::Parts(vec![
            ContentPart::Text {
                text: "What is this?".into(),
            },
            ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "https://example.com/img.png".into(),
                },
            },
        ]),
        ..Message::user_text("")
    };
    let body = client.build_request_json(&[msg], &[], None).unwrap();
    let content = &body["messages"][0]["content"];
    // Content is an array with 2 parts (text + image_url).
    let arr = content
        .as_array()
        .expect("content should be an array when multimodal");
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["type"], "text");
    assert_eq!(arr[0]["text"], "What is this?");
    assert_eq!(arr[1]["type"], "image_url");
    assert_eq!(arr[1]["image_url"]["url"], "https://example.com/img.png");
}

#[test]
fn build_request_json_strips_image_blocks_when_not_multimodal() {
    // When the provider is NOT multimodal, image blocks must be stripped —
    // the content is sent as a plain text string (concatenated text parts
    // only). The vision client handles images separately.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "glm-5.2".into(),
        ..OpenAiClientConfig::test_default()
    });
    let msg = Message {
        content: MessageContent::Parts(vec![
            ContentPart::Text {
                text: "Describe this:".into(),
            },
            ContentPart::ImageUrl {
                image_url: ImageUrl {
                    url: "https://example.com/img.png".into(),
                },
            },
        ]),
        ..Message::user_text("")
    };
    let body = client.build_request_json(&[msg], &[], None).unwrap();
    let content = &body["messages"][0]["content"];
    // Content is a plain string — only the text, image stripped.
    assert_eq!(
        content.as_str().unwrap(),
        "Describe this:",
        "image blocks should be stripped when not multimodal"
    );
}

#[test]
fn build_request_json_echoes_assistant_reasoning_content() {
    // DeepSeek thinking mode requires every prior assistant message to
    // carry `reasoning_content`; a stored reasoning text must be echoed
    // back verbatim on the next request.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4-flash".into(),
        ..OpenAiClientConfig::test_default()
    });
    let msg = Message {
        reasoning_content: Some("thinking...".into()),
        ..Message::assistant_text("answer")
    };
    let body = client.build_request_json(&[msg], &[], None).unwrap();
    assert_eq!(
        body["messages"][0]["reasoning_content"].as_str().unwrap(),
        "thinking...",
        "stored reasoning_content must be echoed back on assistant messages"
    );
}

#[test]
fn build_request_json_assistant_without_reasoning_emits_empty_string() {
    // Regression for the provider-switch 400: an assistant message with no
    // stored reasoning (foreign model, or pre-reasoning history) must still
    // carry the key — DeepSeek rejects a *missing* field, so we emit "".
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4-flash".into(),
        ..OpenAiClientConfig::test_default()
    });
    let msg = Message::assistant_text("answer");
    let body = client.build_request_json(&[msg], &[], None).unwrap();
    let value = &body["messages"][0]["reasoning_content"];
    assert!(
        !value.is_null(),
        "reasoning_content key must be present on assistant messages"
    );
    assert_eq!(
        value.as_str().unwrap(),
        "",
        "missing reasoning must serialize as an empty string, not be omitted"
    );
}

#[test]
fn build_request_json_non_assistant_messages_omit_reasoning_content() {
    // Only assistant messages carry the key — user/tool/system messages
    // must not get a spurious `reasoning_content` field.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4-flash".into(),
        ..OpenAiClientConfig::test_default()
    });
    let messages = vec![
        Message::system("sys"),
        Message::user_text("hi"),
        Message::tool_result("call_1", "shell", "result"),
    ];
    let body = client.build_request_json(&messages, &[], None).unwrap();
    for (i, role) in ["system", "user", "tool"].iter().enumerate() {
        assert!(
            body["messages"][i].get("reasoning_content").is_none(),
            "{role} messages must not carry reasoning_content"
        );
    }
}

#[test]
fn build_request_json_echoes_assistant_provider_meta() {
    // Google stateless-mode: thought_signature must be resent verbatim.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "gemini-3".into(),
        ..OpenAiClientConfig::test_default()
    });
    let msg = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "content": "answer",
            "thought_signature": "stored_sig",
        })),
        ..Message::assistant_text("answer")
    };
    let body = client.build_request_json(&[msg], &[], None).unwrap();
    assert_eq!(
        body["messages"][0]["thought_signature"].as_str().unwrap(),
        "stored_sig",
        "assistant raw must be echoed verbatim on the request (Rule 1)"
    );
    assert_eq!(body["messages"][0]["content"].as_str().unwrap(), "answer");
}

#[test]
fn build_request_json_echoes_tool_call_provider_meta() {
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "gemini-3".into(),
        ..OpenAiClientConfig::test_default()
    });
    let msg = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": { "name": "read", "arguments": "{}" },
                "thought_signature": "call_sig"
            }]
        })),
        ..Message::assistant_text("")
    };
    let body = client.build_request_json(&[msg], &[], None).unwrap();
    assert_eq!(
        body["messages"][0]["tool_calls"][0]["thought_signature"]
            .as_str()
            .unwrap(),
        "call_sig",
        "tool-call thought_signature (in raw) must be echoed verbatim (Rule 1)"
    );
}

#[test]
fn raw_echo_injects_reasoning_content_when_required_but_absent() {
    // Live-verified DeepSeek thinking-mode contract (2026-12-23 curl
    // probes against api.deepseek.com/v1, bug plan c9b5cbe4): with
    // `reasoning_effort` present, the assistant turn that owns the
    // request tail — the last message, or the issuer of trailing tool
    // results — must carry a `reasoning_content` KEY (any value, even
    // ""); a missing key is HTTP 400 "The `reasoning_content` in the
    // thinking mode must be passed back to the API". Probe T:
    // [user, assistant(tool_calls, NO rc key), tool] → 400; T2 (same
    // with rc:"") → 200; probe R: [user, assistant] trailing, key
    // absent → 400. Text-only historical assistant turns tolerate a missing
    // key (probes A/D/G2), but the age-0 turn AND every older turn carrying
    // tool_calls are validated (third recurrence, 2026-09-12 plan c6cb69f7).
    // Foreign 429-fallback turns (GLM/Kimi) issue tool calls
    // whose raw lacks the key — the recurring session-killer bursts in
    // provider-errors.jsonl (ids 39-47, 48-56, 101). The raw echo is
    // verbatim (Rule 1), so the builder must inject the key.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4-flash".into(),
        reasoning_effort: Some("max".into()),
        ..OpenAiClientConfig::test_default()
    });
    // A foreign fallback turn issuing tool calls: raw carries NO
    // reasoning_content key (GLM-style raw may carry `reasoning`
    // instead — probe H2 shape).
    let foreign_raw = serde_json::json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [{
            "id": "c1", "type": "function",
            "function": { "name": "read", "arguments": "{}" }
        }]
    });
    let issuing = Message {
        raw: Some(foreign_raw),
        ..Message::assistant_text("")
    };
    let tool_result = Message::tool_result("c1", "read", "file contents");
    let user = Message::user_text("Reply ok");
    let body = client
        .build_request_json(&[issuing, tool_result, user], &[], None)
        .unwrap();
    // messages[0] is the issuing assistant echo; [1] is the tool result.
    let echoed = &body["messages"][0];
    assert!(
        echoed.get("tool_calls").is_some(),
        "messages[0] must be the tool-issuing assistant echo (test sanity)"
    );
    assert!(
        echoed.get("reasoning_content").is_some(),
        "age-0 raw echo of an assistant message must carry a reasoning_content key \
         when the policy requires it — a missing key is the recurring DeepSeek 400"
    );
    assert_eq!(
        echoed["reasoning_content"],
        serde_json::json!(""),
        "the injected key is empty: the structured reasoning text stays strippable \
         under pressure (plan ffe59699), and an empty key satisfies the validator"
    );
}

#[test]
fn raw_echo_not_injected_for_gemini_plain_turn() {
    // Review finding 2026-12-23 (plan c9b5cbe4): the injection gate must
    // be field-scoped. Gemini 3.x also sets `reasoning_required`, but its
    // continuity contract is `thought_signature` — a gateway echo without
    // a `reasoning_content` key (a plain turn with no thinking text) must
    // stay a byte-identical Rule-1 echo, with no fabricated key.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "gemini-3-flash-preview".into(),
        reasoning_effort: Some("max".into()),
        ..OpenAiClientConfig::test_default()
    });
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "plain answer, no thinking emitted"
    });
    let msg = Message {
        raw: Some(raw),
        ..Message::assistant_text("")
    };
    let body = client.build_request_json(&[msg], &[], None).unwrap();
    assert!(
        body["messages"][0].get("reasoning_content").is_none(),
        "Gemini echo must not get a fabricated reasoning_content key — \
         its continuity contract is thought_signature"
    );
}

#[test]
fn deepseek_tail_owner_keyed_when_system_follows_tool_results() {
    // provider-errors.jsonl id 1042 (2026-09-04 03:59:11, 18 s after commit
    // ec0d794): a 398-message auto-continue request to deepseek-v4-flash
    // (reasoning_effort=max) failed with HTTP 400 "The `reasoning_content`
    // in the thinking mode must be passed back to the API". Its tail was
    // [assistant(finish call, raw WITHOUT the key), tool(finish result),
    // system("# RECALLED MEMORIES …"), system("<context footer …>")] —
    // harness-injected system messages AFTER the tool results. Forensics
    // (.coding/analysis/deepseek-1042-shape.txt) + code-path proof: the
    // age-0 injection keys that turn unconditionally at HEAD (age counts
    // ASSISTANT turns only, openai.rs:1360-1376; the policy consult is a
    // pure (kind, model) function), and the synthetic path always keys —
    // so no post-fix build_request_json can emit an unkeyed age-0
    // assistant message. The incident body therefore came from a binary
    // built before b23463c (a long-running app process), NOT from a hole
    // in the guarantee. This test locks the incident shape so the
    // guarantee can never regress: trailing system messages (or any
    // non-assistant tail) must not affect the age-0 key contract; the
    // historical tool-call turn gets the bare key (the third recurrence,
    // 2026-09-12 plan c6cb69f7, widened the guarantee beyond the tail), while
    // text-only historical turns stay within the missing-key tolerance.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4-flash".into(),
        reasoning_effort: Some("max".into()),
        ..OpenAiClientConfig::test_default()
    });
    // Historical foreign-fallback turn (age 1): raw carries NO
    // reasoning_content key — text-only historical turns tolerate a missing
    // key (probes A/D/G2), but this one carries tool_calls, so the widened
    // guarantee (2026-09-12 plan c6cb69f7) gives it the bare "" key below.
    let historical_raw = serde_json::json!({
        "role": "assistant",
        "content": "earlier turn from a fallback provider",
        "tool_calls": [{
            "id": "call_old", "type": "function",
            "function": { "name": "read", "arguments": "{}" }
        }]
    });
    let historical = Message {
        raw: Some(historical_raw),
        ..Message::assistant_text("earlier turn from a fallback provider")
    };
    // The age-0 turn: the finish-call assistant echo (id 1042 msg[394] —
    // content + tool_calls, raw WITHOUT the key), its tool result, then
    // the two harness-injected system messages that close the request.
    let finish_raw = serde_json::json!({
        "role": "assistant",
        "content": "The commit landed. Finishing the plan with the PASS report.",
        "tool_calls": [{
            "id": "call_finish", "type": "function",
            "function": { "name": "finish", "arguments": "{}" }
        }]
    });
    let issuing = Message {
        raw: Some(finish_raw),
        ..Message::assistant_text("The commit landed. Finishing the plan with the PASS report.")
    };
    let old_result = Message::tool_result("call_old", "read", "file contents");
    let finish_result =
        Message::tool_result("call_finish", "finish", "review passed, state -> Complete");
    let recall = Message::system("# RECALLED MEMORIES\n[semantic] PLAN: ...");
    let footer = Message::system("<context footer — cache-stable sentinel, ignore>");
    let body = client
        .build_request_json(
            &[historical, old_result, issuing, finish_result, recall, footer],
            &[],
            None,
        )
        .unwrap();
    assert_eq!(
        body["reasoning_effort"], "max",
        "test sanity: thinking mode must be active (the effort activates the tail validation)"
    );
    // messages[2] is the age-0 finish-call echo (issuer of the trailing
    // tool result, with system messages after it) — it must carry the key.
    let echoed = &body["messages"][2];
    assert!(
        echoed.get("tool_calls").is_some(),
        "messages[2] must be the finish-call assistant echo (test sanity)"
    );
    assert!(
        echoed.get("reasoning_content").is_some(),
        "age-0 assistant echo must carry the reasoning_content key even when harness \
         system messages follow the tool results (provider-errors id 1042 incident shape)"
    );
    assert_eq!(
        echoed["reasoning_content"],
        serde_json::json!(""),
        "the injected key is empty: the structured reasoning text stays strippable under \
         pressure, and an empty key satisfies the validator"
    );
    // The historical turn (age 1) carries tool_calls, so the third
    // recurrence (2026-09-12, plan c6cb69f7) widens the guarantee to it: it
    // gets the BARE key — the validator is satisfied whichever turn it
    // inspects, and no stripped history text is re-sent. Text-only historical
    // turns stay unkeyed (tolerance probes A/D/G2).
    assert_eq!(
        body["messages"][0].get("reasoning_content"),
        Some(&serde_json::json!("")),
        "historical tool-call echoes get the bare key, never the stripped history text"
    );
    // The trailing system messages must stay untouched.
    for i in [4usize, 5] {
        assert!(
            body["messages"][i].get("reasoning_content").is_none(),
            "system messages must not carry reasoning_content"
        );
    }
}

#[test]
fn deepseek_keys_every_tool_call_turn_after_cross_vendor_reentry() {
    // provider-errors.jsonl ids 104 + 112 (2026-09-12 17:31:17 / 17:32:57,
    // plan c6cb69f7): the FIRST deepseek-v4-flash requests after a mid-turn
    // cross-vendor re-entry. The merge_to_main skill runs on glm-5.3-flash
    // (Ollama Cloud), so Rule 5 (strip_cross_vendor_reasoning) removed
    // `reasoning_content` from every cross-vendor assistant turn during the
    // GLM iterations; skill_end then re-resolved the provider back to the
    // deepseek pin and the request went out with a stripped, keyless open
    // tool-call chain — the tail-owner itself keyed — and DeepSeek still
    // answered HTTP 400 "The `reasoning_content` in the thinking mode must be
    // passed back to the API". Every assistant turn that carries tool_calls
    // must therefore carry the key, not just the age-0 tail-owner.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4-flash".into(),
        reasoning_effort: Some("max".into()),
        ..OpenAiClientConfig::test_default()
    });
    let tool_call_raw = |call_id: &str, name: &str, text: &str| {
        serde_json::json!({
            "role": "assistant",
            "content": text,
            "tool_calls": [{
                "id": call_id, "type": "function",
                "function": { "name": name, "arguments": "{}" }
            }]
        })
    };
    // The stripped open chain: every tool-call raw lacks the key (Rule 5
    // removed it as cross-vendor), exactly like the incident bodies.
    let first = Message {
        raw: Some(tool_call_raw("call_1", "git", "pushing")),
        ..Message::assistant_text("pushing")
    };
    let first_result = Message::tool_result("call_1", "git", "pushed");
    let second = Message {
        raw: Some(tool_call_raw("call_2", "skill_end", "spec amended")),
        ..Message::assistant_text("spec amended")
    };
    let second_result = Message::tool_result("call_2", "skill_end", "skill ended");
    let body = client
        .build_request_json(
            &[
                Message::user_text("merge and push"),
                first,
                first_result,
                second.clone(),
                second_result.clone(),
            ],
            &[],
            None,
        )
        .unwrap();
    assert_eq!(
        body["reasoning_effort"], "max",
        "test sanity: thinking mode must be active (the effort activates the validation)"
    );
    assert!(
        body["messages"][1].get("tool_calls").is_some(),
        "messages[1] must be the older tool-call echo (test sanity)"
    );
    assert_eq!(
        body["messages"][1].get("reasoning_content"),
        Some(&serde_json::json!("")),
        "every tool-call turn needs the key: the age-1 echo must carry the bare \
         reasoning_content key after a cross-vendor strip"
    );
    assert!(
        body["messages"][3].get("reasoning_content").is_some(),
        "the age-0 tail-owner keeps its key (the original tail guarantee)"
    );
    // The widened guarantee covers tool-call turns only: a text-only
    // historical raw still stays unkeyed (tolerance probes A/D/G2), so no
    // reasoning text or fabricated keys leak into plain history.
    let plain = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "content": "plain earlier turn"
        })),
        ..Message::assistant_text("plain earlier turn")
    };
    let body = client
        .build_request_json(
            &[Message::user_text("hi"), plain, second, second_result],
            &[],
            None,
        )
        .unwrap();
    assert!(
        body["messages"][1].get("reasoning_content").is_none(),
        "text-only historical echoes must stay unkeyed (probes A/D/G2)"
    );
}

#[test]
fn build_request_json_echoes_raw_byte_identical() {
    // Rule 1 acceptance test #2 (byte equality): the echoed assistant
    // message is identical to the raw the provider returned — no field
    // dropped, reordered, or fabricated.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4".into(),
        ..OpenAiClientConfig::test_default()
    });
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "result",
        "reasoning_content": "because reasons",
        "thought_signature": "sig",
        "tool_calls": [{
            "id": "c1", "type": "function",
            "function": { "name": "read", "arguments": "{\"x\":1}" }
        }]
    });
    let msg = Message {
        raw: Some(raw.clone()),
        ..Message::assistant_text("result")
    };
    let body = client.build_request_json(&[msg], &[], None).unwrap();
    assert_eq!(body["messages"][0], raw);
}

#[test]
fn echo_assistant_raw_borrows_when_the_policy_is_a_noop() {
    // Perf review L3 (2027-01-09): NEVER_STRIP providers (OpenAI Responses,
    // Claude, Qwen, Kimi, GLM, vLLM) never mutate the echo — the raw must be
    // returned BORROWED, skipping the per-message deep clone entirely, under
    // pressure or not.
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "hi",
        "reasoning_content": "thinking...",
    });
    let policy = crate::provider::ProviderPolicy::for_kind_and_model(
        crate::provider::ProviderKind::OpenAI,
        "gpt-5",
    );
    assert_eq!(
        policy.retention,
        crate::provider::ReasoningRetention::NEVER_STRIP
    );
    let echoed = request::echo_assistant_raw(&raw, &policy, 3, false, Some("thinking..."));
    assert!(matches!(echoed, std::borrow::Cow::Borrowed(_)));
    assert_eq!(echoed.as_ref(), &raw);
    // Pressure does not change the NEVER_STRIP decision.
    let echoed = request::echo_assistant_raw(&raw, &policy, 3, true, Some("thinking..."));
    assert!(matches!(echoed, std::borrow::Cow::Borrowed(_)));
}

#[test]
fn echo_assistant_raw_borrows_for_deepseek_history_without_pressure() {
    // DeepSeek strips historical reasoning only UNDER PRESSURE — without it
    // the echo is a no-op and must be borrowed.
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "hi",
        "reasoning_content": "thinking...",
    });
    let policy = crate::provider::ProviderPolicy::for_kind_and_model(
        crate::provider::ProviderKind::OpenAI,
        "deepseek-chat",
    );
    let echoed = request::echo_assistant_raw(&raw, &policy, 3, false, Some("thinking..."));
    assert!(matches!(echoed, std::borrow::Cow::Borrowed(_)));
}

#[test]
fn echo_assistant_raw_clones_once_when_stripping_under_pressure() {
    // DeepSeek + pressure + age >= keep_recent (1): the reasoning text is
    // stripped from the OUTGOING echo — an owned, mutated clone. The STORED
    // raw is untouched (the strip is echo-only).
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "hi",
        "reasoning_content": "thinking...",
    });
    let policy = crate::provider::ProviderPolicy::for_kind_and_model(
        crate::provider::ProviderKind::OpenAI,
        "deepseek-chat",
    );
    let echoed = request::echo_assistant_raw(&raw, &policy, 3, true, Some("thinking..."));
    let std::borrow::Cow::Owned(echoed) = echoed else {
        panic!("pressure strip must produce an owned clone");
    };
    assert!(echoed.get("reasoning_content").is_none());
    assert_eq!(echoed["content"], "hi");
    assert!(raw.get("reasoning_content").is_some());
}

#[test]
fn echo_assistant_raw_clones_once_for_the_age0_readd() {
    // DeepSeek thinking mode, age 0, raw lacking the key: the re-add injects
    // the structured reasoning (or "") — an owned clone.
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "hi",
    });
    let policy = crate::provider::ProviderPolicy::for_kind_and_model(
        crate::provider::ProviderKind::OpenAI,
        "deepseek-chat",
    );
    let echoed = request::echo_assistant_raw(&raw, &policy, 0, false, Some("structured rc"));
    let std::borrow::Cow::Owned(echoed) = echoed else {
        panic!("age-0 re-add must produce an owned clone");
    };
    assert_eq!(echoed["reasoning_content"], "structured rc");

    // No structured reasoning → the empty-string key (satisfies the
    // validator without leaking text).
    let echoed = request::echo_assistant_raw(&raw, &policy, 0, false, None);
    let std::borrow::Cow::Owned(echoed) = echoed else {
        panic!("age-0 re-add must produce an owned clone");
    };
    assert_eq!(echoed["reasoning_content"], "");
}

#[test]
fn echo_assistant_raw_borrows_when_the_readd_key_is_already_present() {
    // Age 0 with the key already on the raw: no mutation — borrowed.
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "hi",
        "reasoning_content": "already here",
    });
    let policy = crate::provider::ProviderPolicy::for_kind_and_model(
        crate::provider::ProviderKind::OpenAI,
        "deepseek-chat",
    );
    let echoed = request::echo_assistant_raw(&raw, &policy, 0, false, Some("already here"));
    assert!(matches!(echoed, std::borrow::Cow::Borrowed(_)));
}

#[test]
fn prefix_cache_warm_build_is_byte_identical_to_cold() {
    // The strongest cache-correctness pin (perf review L3, 2027-01-09): a
    // cache-hit build must produce a body byte-identical to a cold build for
    // the same final message list — the serialized prefix is reused, the
    // tail is serialized fresh, and the splice must be indistinguishable.
    // The equality also pins the estimate split: max_completion_tokens
    // derives from the estimate, so a drifted split would surface here (the
    // split reuses message_estimate_chars — the same per-message piece
    // estimate_prompt_tokens sums — so no drift is possible by construction).
    let client = OpenAiClient::new(OpenAiClientConfig::test_default());
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "result",
        "reasoning_content": "because reasons",
    });
    let msgs = vec![
        Message::system("sys"),
        Message::text(Role::User, "u1"),
        Message {
            raw: Some(raw),
            ..Message::assistant_text("result")
        },
        Message::text(Role::User, "u2"),
    ];
    let cold = client.build_request_body(&msgs, &[], None, false).unwrap();
    let warm = client.build_request_body(&msgs, &[], None, false).unwrap();
    assert_eq!(cold, warm, "warm build must be byte-identical to cold");
    assert!(client.prefix_cache.lock().unwrap().is_some());
    // The hit must actually occur (review round-1, Finding 2): warm==cold
    // holds trivially on a miss too — a fresh client's first build misses,
    // the second must hit.
    assert_eq!(
        client.hit_count.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the second build must be a genuine cache hit"
    );
}

#[test]
fn prefix_cache_hits_on_append_and_matches_cold() {
    // The turn-loop shape: history is append-mostly — the next request adds
    // messages after the stable prefix. The warm build must equal the cold
    // build exactly.
    let client = OpenAiClient::new(OpenAiClientConfig::test_default());
    let fresh = OpenAiClient::new(OpenAiClientConfig::test_default());
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "result",
        "tool_calls": [{
            "id": "c1", "type": "function",
            "function": { "name": "read", "arguments": "{\"x\":1}" }
        }],
    });
    let base = vec![
        Message::system("sys"),
        Message::text(Role::User, "u1"),
        Message {
            raw: Some(raw),
            ..Message::assistant_text("result")
        },
        Message::text(Role::User, "u2"),
    ];
    let _ = client.build_request_body(&base, &[], None, false).unwrap();
    let mut grown = base.clone();
    grown.push(Message::text(Role::User, "u3"));
    let warm = client.build_request_body(&grown, &[], None, false).unwrap();
    let cold = fresh.build_request_body(&grown, &[], None, false).unwrap();
    assert_eq!(warm, cold);
    assert_eq!(
        client.hit_count.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the append build must be a genuine cache hit"
    );
}

#[test]
fn prefix_cache_excludes_trailing_system_run() {
    // The volatile per-request tail + CONTEXT_FOOTER are system messages
    // pushed after the token accounting and popped after the request — they
    // must not be cached, or the next iteration's fingerprint could never
    // match (the cached region would cover messages that no longer exist).
    let client = OpenAiClient::new(OpenAiClientConfig::test_default());
    let fresh = OpenAiClient::new(OpenAiClientConfig::test_default());
    let with_tail = vec![
        Message::system("sys"),
        Message::text(Role::User, "u1"),
        Message::system("volatile per-request tail"),
        Message::system("CONTEXT_FOOTER"),
    ];
    let _ = client.build_request_body(&with_tail, &[], None, false).unwrap();
    // The trailing system run is excluded: the cache covers the list minus
    // the trailing run (2), not the whole list (4) — review round-1,
    // Finding 2 (without the exclusion the next build could never hit).
    assert_eq!(
        client.prefix_cache.lock().unwrap().as_ref().map(|c| c.len),
        Some(2)
    );
    // Next iteration: the tail is popped, a new user turn appended.
    let next = vec![
        Message::system("sys"),
        Message::text(Role::User, "u1"),
        Message::text(Role::User, "u2"),
    ];
    let warm = client.build_request_body(&next, &[], None, false).unwrap();
    let cold = fresh.build_request_body(&next, &[], None, false).unwrap();
    assert_eq!(warm, cold, "the popped tail must not poison the cache");
    assert_eq!(
        client.hit_count.load(std::sync::atomic::Ordering::Relaxed),
        1,
        "the next build must HIT the trimmed cache"
    );
}

#[test]
fn prefix_cache_rebuilds_after_history_shrink() {
    // Compaction replaces the history with a summary — the list shrinks
    // below the cached prefix length → structural miss → full rebuild.
    let client = OpenAiClient::new(OpenAiClientConfig::test_default());
    let fresh = OpenAiClient::new(OpenAiClientConfig::test_default());
    let long = vec![
        Message::system("sys"),
        Message::text(Role::User, "u1"),
        Message::assistant_text("a1"),
        Message::text(Role::User, "u2"),
        Message::assistant_text("a2"),
        Message::text(Role::User, "u3"),
    ];
    let _ = client.build_request_body(&long, &[], None, false).unwrap();
    let compacted = vec![
        Message::system("sys"),
        Message::text(Role::User, "summary of the above"),
        Message::text(Role::User, "u4"),
    ];
    let warm = client.build_request_body(&compacted, &[], None, false).unwrap();
    let cold = fresh.build_request_body(&compacted, &[], None, false).unwrap();
    assert_eq!(warm, cold);
    assert_eq!(
        client.hit_count.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "a shrunk history must genuinely miss"
    );
}

#[test]
fn prefix_cache_rebuilds_after_head_swap() {
    // The 429-fallback head swap replaces the system head's content — the
    // fingerprint (hashing the head) mismatches → rebuild.
    let client = OpenAiClient::new(OpenAiClientConfig::test_default());
    let fresh = OpenAiClient::new(OpenAiClientConfig::test_default());
    let base = vec![Message::system("sys v1"), Message::text(Role::User, "u1")];
    let _ = client.build_request_body(&base, &[], None, false).unwrap();
    let swapped = vec![
        Message::system("sys v2 (fallback provider)"),
        Message::text(Role::User, "u1"),
    ];
    let warm = client.build_request_body(&swapped, &[], None, false).unwrap();
    let cold = fresh.build_request_body(&swapped, &[], None, false).unwrap();
    assert_eq!(warm, cold);
    assert_eq!(
        client.hit_count.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "a swapped head must genuinely miss"
    );
}

#[test]
fn prefix_cache_rebuilds_on_pressure_flip_and_strips_history() {
    // DeepSeek (keep_recent=1, pressure-triggered): build 1 is small — no
    // pressure, the age-1 assistant's reasoning_content is echoed. Build 2
    // grows the conversation past the proxy cache ceiling
    // (340_000 - 32_768 = 307_232 tokens ≈ 1.23 MB of text) → pressure ON →
    // the strip decision flips → the fingerprint changes → rebuild → the
    // historical reasoning is STRIPPED from the wire (a stale hit would
    // leak it cross-vendor).
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4".into(),
        ..OpenAiClientConfig::test_default()
    });
    let fresh = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4".into(),
        ..OpenAiClientConfig::test_default()
    });
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "result",
        "reasoning_content": "historical thinking that must be stripped under pressure",
    });
    let historical = Message {
        raw: Some(raw),
        ..Message::assistant_text("result")
    };
    let small = vec![
        Message::system("sys"),
        Message::text(Role::User, "u1"),
        historical.clone(),
        Message::assistant_text("a2"),
        Message::text(Role::User, "u2"),
    ];
    let b1 = client.build_request_body(&small, &[], None, false).unwrap();
    let v1: serde_json::Value = serde_json::from_str(&b1).unwrap();
    assert_eq!(
        v1["messages"][2]["reasoning_content"],
        "historical thinking that must be stripped under pressure",
        "no pressure → the historical reasoning is echoed"
    );
    // Grow past the ceiling: ~1.3 MB of text ≈ 325K tokens > 307_232.
    let pad = "x".repeat(1_300_000);
    let grown = vec![
        Message::system("sys"),
        Message::text(Role::User, "u1"),
        historical,
        Message::assistant_text("a2"),
        Message::text(Role::User, pad),
        Message::text(Role::User, "u3"),
    ];
    let b2 = client.build_request_body(&grown, &[], None, false).unwrap();
    let cold = fresh.build_request_body(&grown, &[], None, false).unwrap();
    assert_eq!(
        b2, cold,
        "the pressure-flip rebuild must match the cold build"
    );
    let v2: serde_json::Value = serde_json::from_str(&b2).unwrap();
    assert!(
        v2["messages"][2].get("reasoning_content").is_none(),
        "pressure ON → the age-1 historical reasoning is stripped"
    );
    assert_eq!(
        client.hit_count.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the pressure flip must genuinely miss (a stale hit would leak unstripped reasoning)"
    );
}

#[test]
fn prefix_cache_rebuilds_when_the_readd_ages_out() {
    // DeepSeek thinking mode: the age-0 assistant gets the reasoning_content
    // key injected with the structured text (the tail-turn contract, SPEC
    // e010881d); when it ages to 1 a TOOL-CALL turn keeps the guarantee — the
    // bare key — so the serialized form still changes (mode 2 -> 1) and the
    // cache must rebuild: the third recurrence (2026-09-12, plan c6cb69f7)
    // proved a stripped tool-call chain must stay keyed whichever turn the
    // validator inspects.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4".into(),
        ..OpenAiClientConfig::test_default()
    });
    let fresh = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4".into(),
        ..OpenAiClientConfig::test_default()
    });
    // A raw WITHOUT the reasoning_content key (e.g. a foreign 429-fallback
    // turn — GLM/Kimi issue tool calls whose raw lacks the key).
    let raw = serde_json::json!({
        "role": "assistant",
        "content": "result",
        "tool_calls": [{
            "id": "c1", "type": "function",
            "function": { "name": "read", "arguments": "{\"x\":1}" }
        }],
    });
    let issuer = Message {
        raw: Some(raw),
        ..Message::assistant_text("result")
    };
    let base = vec![Message::system("sys"), Message::text(Role::User, "u1"), issuer.clone()];
    let b1 = client.build_request_body(&base, &[], None, false).unwrap();
    let v1: serde_json::Value = serde_json::from_str(&b1).unwrap();
    assert_eq!(
        v1["messages"][2]["reasoning_content"], "",
        "age 0 → the tail-turn contract injects the (empty) key"
    );
    // The issuer ages to 1: a tool result + a new assistant turn follow.
    let aged = vec![
        Message::system("sys"),
        Message::text(Role::User, "u1"),
        issuer,
        Message::text(Role::User, "tool result"),
        Message::assistant_text("a2"),
    ];
    let b2 = client.build_request_body(&aged, &[], None, false).unwrap();
    let cold = fresh.build_request_body(&aged, &[], None, false).unwrap();
    assert_eq!(b2, cold, "the aged-out rebuild must match the cold build");
    let v2: serde_json::Value = serde_json::from_str(&b2).unwrap();
    assert_eq!(
        v2["messages"][2]["reasoning_content"], "",
        "age 1 tool-call turn → the bare key (never a stale age-0 serialization)"
    );
    assert_eq!(
        client.hit_count.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "the re-add aging out must genuinely miss (the mode flips 2 -> 1)"
    );
}

#[test]
fn retention_gemini_drops_historical_reasoning_but_keeps_signatures() {
    // Gemini 3 via an OpenAI-compatible gateway: thinking *text* is not
    // needed for continuity (the thought_signature is), so historical
    // assistant turns drop reasoning text while signatures survive. The
    // most recent assistant turn (age 0) stays byte-verbatim (Rule 1).
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "gemini-3.0-flash".into(),
        ..OpenAiClientConfig::test_default()
    });
    let historical_raw = serde_json::json!({
        "role": "assistant",
        "content": "calling tool",
        "reasoning_content": "long thinking text",
        "thought_signature": "msg_sig",
        "tool_calls": [{
            "id": "c1", "type": "function",
            "function": { "name": "read", "arguments": "{}" },
            "thought_signature": "call_sig"
        }]
    });
    let historical = Message {
        raw: Some(historical_raw.clone()),
        ..Message::assistant_text("calling tool")
    };
    let recent_raw = serde_json::json!({
        "role": "assistant",
        "content": "final",
        "reasoning_content": "recent thinking",
    });
    let recent = Message {
        raw: Some(recent_raw.clone()),
        ..Message::assistant_text("final")
    };
    let body = client
        .build_request_json(&[historical, recent], &[], None)
        .unwrap();
    // Historical turn: reasoning text dropped, signatures preserved.
    let echoed = &body["messages"][0];
    assert!(echoed.get("reasoning_content").is_none());
    assert!(echoed.get("reasoning").is_none());
    assert_eq!(
        echoed["thought_signature"].as_str().unwrap(),
        "msg_sig",
        "message-level thought_signature must survive the strip"
    );
    assert_eq!(
        echoed["tool_calls"][0]["thought_signature"].as_str().unwrap(),
        "call_sig",
        "per-tool-call thought_signature must survive the strip"
    );
    // Most recent assistant turn (age 0): byte-verbatim echo (Rule 1).
    assert_eq!(body["messages"][1], recent_raw);
}

#[test]
fn retention_deepseek_strips_historical_reasoning_only_under_pressure() {
    // DeepSeek keeps historical reasoning_content below the pressure
    // threshold (continuity needs it) and drops it only when the request
    // is near the proxy cache ceiling. The most recent turn is never
    // stripped.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4".into(),
        ..OpenAiClientConfig::test_default()
    });
    let historical = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "content": "earlier",
            "reasoning_content": "earlier thinking",
        })),
        ..Message::assistant_text("earlier")
    };
    let recent = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "content": "latest",
            "reasoning_content": "latest thinking",
        })),
        ..Message::assistant_text("latest")
    };
    // Below pressure: nothing is dropped.
    let body = client
        .build_request_json(&[historical.clone(), recent.clone()], &[], None)
        .unwrap();
    assert_eq!(body["messages"][0]["reasoning_content"], "earlier thinking");
    assert_eq!(body["messages"][1]["reasoning_content"], "latest thinking");

    // Under pressure (prompt estimate + margin crosses the ceiling): the
    // historical turn drops reasoning_content, the recent turn keeps it.
    // Pad with a huge user message to push the estimate over the cliff.
    let filler = "x".repeat(crate::provider::PROXY_CACHE_CEILING_TOKENS * 5);
    let pressure = Message::user_text(filler);
    let body = client
        .build_request_json(&[pressure, historical, recent], &[], None)
        .unwrap();
    assert!(
        body["messages"][1].get("reasoning_content").is_none(),
        "historical reasoning must drop under cache pressure"
    );
    assert_eq!(
        body["messages"][2]["reasoning_content"], "latest thinking",
        "the most recent assistant turn is never stripped"
    );
}

#[test]
fn retention_qwen_never_strips_even_under_pressure() {
    // Providers with no droppable history keep the full payload verbatim
    // under all conditions (Rule 1 unchanged for them).
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "qwen-3.5-coder".into(),
        ..OpenAiClientConfig::test_default()
    });
    let historical = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "content": "earlier",
            "reasoning_content": "earlier thinking",
        })),
        ..Message::assistant_text("earlier")
    };
    let recent = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "content": "latest",
            "reasoning_content": "latest thinking",
        })),
        ..Message::assistant_text("latest")
    };
    let filler = "x".repeat(crate::provider::PROXY_CACHE_CEILING_TOKENS * 5);
    let pressure = Message::user_text(filler);
    let body = client
        .build_request_json(&[pressure, historical, recent], &[], None)
        .unwrap();
    assert_eq!(body["messages"][1]["reasoning_content"], "earlier thinking");
    assert_eq!(body["messages"][2]["reasoning_content"], "latest thinking");
}

#[test]
fn retention_strip_is_repeatable_across_calls() {
    // build_request_json takes &[Message] (borrowed), so the stored
    // Message cannot be mutated by construction. This pins the operational
    // consequence: repeated request builds produce identical stripped
    // payloads (no strip-once-then-gone state leak).
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4".into(),
        ..OpenAiClientConfig::test_default()
    });
    let messages = [
        Message {
            raw: Some(serde_json::json!({
                "role": "assistant",
                "content": "earlier",
                "reasoning_content": "earlier thinking",
            })),
            ..Message::assistant_text("earlier")
        },
        Message {
            raw: Some(serde_json::json!({
                "role": "assistant",
                "content": "latest",
                "reasoning_content": "latest thinking",
            })),
            ..Message::assistant_text("latest")
        },
    ];
    // Below pressure: identical, unstripped outputs on both calls.
    let b1 = client.build_request_json(&messages, &[], None).unwrap();
    let b2 = client.build_request_json(&messages, &[], None).unwrap();
    assert_eq!(b1["messages"][0], b2["messages"][0]);
    assert_eq!(b1["messages"][1], b2["messages"][1]);
    // Under pressure: both calls strip identically.
    let filler = "x".repeat(crate::provider::PROXY_CACHE_CEILING_TOKENS * 5);
    let pressure = Message::user_text(filler);
    let messages = [pressure, messages[0].clone(), messages[1].clone()];
    let b1 = client.build_request_json(&messages, &[], None).unwrap();
    let b2 = client.build_request_json(&messages, &[], None).unwrap();
    assert_eq!(b1["messages"][1], b2["messages"][1]);
    assert!(b1["messages"][1].get("reasoning_content").is_none());
}

#[test]
fn retention_deepseek_synthetic_path_strips_under_pressure() {
    // Review L3 (2026-12-22): the synthetic assistant branch (no raw) had
    // no direct test. The re-add makes reasoning_content unconditionally
    // present on synthetic assistants; a pressure-triggered DeepSeek strip
    // must still remove it on historical turns.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4".into(),
        ..OpenAiClientConfig::test_default()
    });
    let hist = Message {
        reasoning_content: Some("earlier thinking".into()),
        ..Message::assistant_text("earlier")
    };
    let recent = Message {
        reasoning_content: Some("latest thinking".into()),
        ..Message::assistant_text("latest")
    };
    // Below pressure: the re-added reasoning_content survives on both.
    let body = client
        .build_request_json(&[hist.clone(), recent.clone()], &[], None)
        .unwrap();
    assert_eq!(body["messages"][0]["reasoning_content"], "earlier thinking");
    assert_eq!(body["messages"][1]["reasoning_content"], "latest thinking");
    // Under pressure: the historical synthetic turn loses the re-added
    // key; the most recent synthetic turn keeps it.
    let filler = "x".repeat(crate::provider::PROXY_CACHE_CEILING_TOKENS * 5);
    let body = client
        .build_request_json(&[Message::user_text(filler), hist, recent], &[], None)
        .unwrap();
    assert!(body["messages"][1].get("reasoning_content").is_none());
    assert_eq!(body["messages"][2]["reasoning_content"], "latest thinking");
}

#[test]
fn retention_deepseek_synthetic_tool_call_turn_keeps_bare_key_under_pressure() {
    // Review LOW 2 (2026-09-12, plan c6cb69f7): the synthetic branch re-adds
    // reasoning_content BEFORE the retention strip, so a pressure strip could
    // leave a synthetic TOOL-CALL turn keyless — the exact shape the third
    // DeepSeek recurrence proved fatal. The widened guarantee (every wire
    // tool-call turn carries the key under a requires-rc policy) must hold on
    // both paths: the strip drops the historical TEXT, the bare key returns.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4".into(),
        ..OpenAiClientConfig::test_default()
    });
    let hist = Message {
        reasoning_content: Some("earlier thinking".into()),
        ..Message::assistant("earlier", vec![ToolCall::new("call_1", "read", "{}")])
    };
    let recent = Message {
        reasoning_content: Some("latest thinking".into()),
        ..Message::assistant_text("latest")
    };
    let filler = "x".repeat(crate::provider::PROXY_CACHE_CEILING_TOKENS * 5);
    let body = client
        .build_request_json(&[Message::user_text(filler), hist, recent], &[], None)
        .unwrap();
    assert_eq!(
        body["messages"][1]["reasoning_content"], "",
        "a stripped synthetic tool-call turn gets the bare key back, never the stripped text"
    );
    assert_eq!(body["messages"][2]["reasoning_content"], "latest thinking");
}

#[test]
fn retention_qwen_synthetic_path_never_strips() {
    // NEVER_STRIP providers keep the re-added reasoning_content on the
    // synthetic path even under pressure (the safe failure mode).
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "qwen-3.5-coder".into(),
        ..OpenAiClientConfig::test_default()
    });
    let hist = Message {
        reasoning_content: Some("earlier thinking".into()),
        ..Message::assistant_text("earlier")
    };
    let filler = "x".repeat(crate::provider::PROXY_CACHE_CEILING_TOKENS * 5);
    let body = client
        .build_request_json(&[Message::user_text(filler), hist], &[], None)
        .unwrap();
    assert_eq!(body["messages"][1]["reasoning_content"], "earlier thinking");
}

#[test]
fn retention_age0_protected_with_trailing_user_and_zero_assistant_histories() {
    // Review L3 edge cases: age-0 protection holds when the most recent
    // assistant turn is NOT the last message (trailing user turn), and a
    // zero-assistant history builds without panicking.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "gemini-3.0-flash".into(),
        ..OpenAiClientConfig::test_default()
    });
    let hist = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "content": "earlier",
            "reasoning_content": "earlier thinking",
        })),
        ..Message::assistant_text("earlier")
    };
    let recent_raw = serde_json::json!({
        "role": "assistant",
        "content": "latest",
        "reasoning_content": "latest thinking",
    });
    let recent = Message {
        raw: Some(recent_raw.clone()),
        ..Message::assistant_text("latest")
    };
    let body = client
        .build_request_json(
            &[hist, recent, Message::user_text("trailing question")],
            &[],
            None,
        )
        .unwrap();
    // Historical (age 1): stripped. Recent (age 0, despite the trailing
    // user turn): byte-verbatim.
    assert!(body["messages"][0].get("reasoning_content").is_none());
    assert_eq!(body["messages"][1], recent_raw);
    // Zero-assistant history: builds, the single user message passes
    // through untouched.
    let body = client
        .build_request_json(&[Message::user_text("hi")], &[], None)
        .unwrap();
    assert_eq!(body["messages"].as_array().unwrap().len(), 1);
    assert_eq!(body["messages"][0]["role"], "user");
}

#[test]
fn build_request_json_retains_reasoning_across_tool_calls() {
    // Rule 1 acceptance test #1 (sequential tool call): after a tool
    // call, the next request still contains the provider's reasoning
    // field with the same value the model returned.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4".into(),
        ..OpenAiClientConfig::test_default()
    });
    let assistant1 = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "content": "",
            "reasoning_content": "first thought",
            "tool_calls": [{
                "id": "c1", "type": "function",
                "function": { "name": "read", "arguments": "{\"a\":1}" }
            }]
        })),
        ..Message::assistant_text("")
    };
    let tool1 = Message::tool_result("c1", "read", "42");
    let assistant2 = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "content": "",
            "reasoning_content": "second thought",
            "tool_calls": [{
                "id": "c2", "type": "function",
                "function": { "name": "write", "arguments": "{\"b\":2}" }
            }]
        })),
        ..Message::assistant_text("")
    };
    let tool2 = Message::tool_result("c2", "write", "ok");
    let body = client
        .build_request_json(&[assistant1, tool1, assistant2, tool2], &[], None)
        .unwrap();
    // Both assistant turns retain their reasoning_content verbatim.
    assert_eq!(
        body["messages"][0]["reasoning_content"].as_str().unwrap(),
        "first thought"
    );
    assert_eq!(
        body["messages"][2]["reasoning_content"].as_str().unwrap(),
        "second thought"
    );
}

#[test]
fn build_request_json_omits_thought_signature_when_no_meta() {
    // Provenance-gating regression: messages with no captured provider_meta
    // must produce a request body with NO thought_signature key — non-signature
    // endpoints see byte-identical requests (fabrication is impossible).
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "gemini-3".into(),
        ..OpenAiClientConfig::test_default()
    });
    let msg = Message::assistant("answer", vec![ToolCall::new("call_1", "read", "{}")]);
    let body = client.build_request_json(&[msg], &[], None).unwrap();
    assert!(
        body["messages"][0].get("thought_signature").is_none(),
        "no thought_signature key when provider_meta is None"
    );
    assert!(
        body["messages"][0]["tool_calls"][0]
            .get("thought_signature")
            .is_none(),
        "no thought_signature on tool_calls when provider_meta is None"
    );
}

#[test]
fn build_request_json_plain_text_unchanged_when_not_multimodal() {
    // Plain-text content is always sent as a string, regardless of
    // multimodal capability.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "glm-5.2".into(),
        ..OpenAiClientConfig::test_default()
    });
    let msg = Message::user_text("hello");
    let body = client.build_request_json(&[msg], &[], None).unwrap();
    assert_eq!(body["messages"][0]["content"], "hello");
}


/// Build a Local-kind client with the minimal config used by the
/// sanitizer tests below.
fn local_test_client() -> OpenAiClient {
    OpenAiClient::new(OpenAiClientConfig {
        kind: ProviderKind::Local,
        ..OpenAiClientConfig::test_default()
    })
}

#[test]
fn record_tools_phase_ms_targets_the_clients_own_record() {
    // Review M1 regression: the tools phase must land on THIS client's
    // request record even when a concurrent agent created a NEWER record
    // in the shared ring between the stream end and the batch end.
    let log = Arc::new(LlmRequestLog::new());
    let client = OpenAiClient::new_with_trace(
        OpenAiClientConfig::test_default(),
        Some(log.clone()),
    );

    // Simulate the wiring complete() performs: remember the record id.
    let own = log.start("m", "http://u/v1/", "p", serde_json::json!({}));
    *client
        .last_record_id
        .lock()
        .expect("last_record_id lock poisoned") = Some(own);
    // A concurrent agent's request lands in the ring afterwards.
    let other = log.start("m", "http://u/v1/", "p", serde_json::json!({}));

    client.record_tools_phase_ms(4321);

    assert_eq!(log.get(own).expect("present").tools_ms, Some(4321));
    assert_eq!(
        log.get(other).expect("present").tools_ms,
        None,
        "the concurrent record must not receive the tools phase"
    );
}

#[test]
fn record_raw_tool_calls_targets_the_clients_own_record() {
    // Backlog e8b39d72 H1: the delivered-args tap must land on THIS
    // client's request record even when a concurrent agent created a
    // NEWER record in the shared ring between the stream end and the
    // tap call.
    let log = Arc::new(LlmRequestLog::new());
    let client = OpenAiClient::new_with_trace(
        OpenAiClientConfig::test_default(),
        Some(log.clone()),
    );

    // Simulate the wiring the stream performs: remember the record id.
    let own = log.start("m", "http://u/v1/", "p", serde_json::json!({}));
    *client
        .last_record_id
        .lock()
        .expect("last_record_id lock poisoned") = Some(own);
    // A concurrent agent's request lands in the ring afterwards.
    let other = log.start("m", "http://u/v1/", "p", serde_json::json!({}));

    client.record_raw_tool_calls(vec![(
        "call-1".into(),
        "file_edit".into(),
        "{\"command\":\"cargo test\"}".into(),
    )]);

    let calls = log
        .get(own)
        .expect("present")
        .raw_tool_calls
        .expect("delivered args");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].arguments, "{\"command\":\"cargo test\"}");
    assert!(
        log.get(other).expect("present").raw_tool_calls.is_none(),
        "the concurrent record must not receive the delivered args"
    );
}

#[tokio::test]
async fn pending_prep_compact_ms_stamp_the_next_record() {
    // The turn loop measures the prep/compact window BEFORE the request
    // (and its trace record) exists: record_prep_ms/record_compact_ms
    // park the values, and the next complete() stamps them onto the
    // record it creates — then clears them so a stale value can never
    // land on a later, unrelated request.
    let server = StubServer::start(vec![StubResponse {
        status: 200,
        content_type: "text/event-stream",
        body: SSE_OK,
    }])
    .await;
    let log = Arc::new(LlmRequestLog::new());
    let client = OpenAiClient::new_with_trace(
        OpenAiClientConfig {
            base_url: format!("http://{}/v1/", server.addr),
            ..OpenAiClientConfig::test_default()
        },
        Some(log.clone()),
    );

    client.record_prep_ms(1234);
    client.record_compact_ms(567);

    let messages = vec![Message::user_text("hello")];
    let mut stream = client
        .complete(&messages, &[], None)
        .await
        .expect("stubbed stream succeeds");
    // Drain the stream (it is `must_use` — and consuming it lets the
    // StubServer task finish cleanly).
    while futures::StreamExt::next(&mut stream).await.is_some() {}

    let id = *client
        .last_record_id
        .lock()
        .expect("last_record_id lock poisoned");
    let id = id.expect("complete() must create a record");
    let rec = log.get(id).expect("record present");
    // prep_ms = parked 1234 + the real local sliver from complete()
    // entry to record creation (body build + trace-log start — see the
    // stamping site in complete()). On a slow machine that sliver
    // crosses a millisecond boundary, so assert a bounded range, not
    // exact equality (the pre-fix exact assert was timing-flaky:
    // observed Some(1265) vs Some(1234) on 2026-01-03, pre-existing on
    // clean HEAD). compact_ms carries no sliver — exact is fine.
    let prep = rec.prep_ms.expect("parked prep_ms must be stamped");
    assert!(
        (1234..1234 + 10_000).contains(&prep),
        "prep_ms = parked 1234 + entry-to-record sliver, got {prep}"
    );
    assert_eq!(rec.compact_ms, Some(567));
    // Cleared after stamping — a later request starts clean.
    assert!(
        client
            .pending_prep_ms
            .lock()
            .expect("pending_prep_ms lock poisoned")
            .is_none(),
        "prep_ms must be cleared after stamping"
    );
    assert!(
        client
            .pending_compact_ms
            .lock()
            .expect("pending_compact_ms lock poisoned")
            .is_none(),
        "compact_ms must be cleared after stamping"
    );
}

/// A minimal reasoning stream: two reasoning deltas (the second via the
/// Ollama `reasoning` alias), then answer content, finish, usage.
const SSE_REASONING: &str = concat!(
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\",\"reasoning\":\"think\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"ing...\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"completion_tokens_details\":{\"reasoning_tokens\":3},\"prompt_tokens_details\":{\"cached_tokens\":0}},\"choices\":[]}\n\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn reasoning_stream_records_reasoning_ms() {
    // Regression (2026-08-22): a reasoning stream's thinking window must
    // be measured on the trace record as `reasoning_ms` (first reasoning
    // delta → first answer delta) instead of being folded invisibly into
    // the generate segment. The StubServer delivers the body in one read,
    // so the measured values can be 0ms — the invariant that matters is
    // that reasoning_ms is RECORDED and fits inside generation_ms.
    let server = StubServer::start(vec![StubResponse {
        status: 200,
        content_type: "text/event-stream",
        body: SSE_REASONING,
    }])
    .await;
    let log = Arc::new(LlmRequestLog::new());
    let client = OpenAiClient::new_with_trace(
        OpenAiClientConfig {
            base_url: format!("http://{}/v1/", server.addr),
            ..OpenAiClientConfig::test_default()
        },
        Some(log.clone()),
    );

    let messages = vec![Message::user_text("hello")];
    let mut stream = client
        .complete(&messages, &[], None)
        .await
        .expect("stubbed stream succeeds");
    while futures::StreamExt::next(&mut stream).await.is_some() {}

    let id = client
        .last_record_id
        .lock()
        .expect("last_record_id lock poisoned")
        .expect("complete() must create a record");
    let rec = log.get(id).expect("record present");
    let reasoning_ms = rec.reasoning_ms.expect("reasoning_ms must be recorded");
    let gen = rec.generation_ms.expect("generation_ms recorded");
    assert!(
        reasoning_ms <= gen,
        "reasoning ({reasoning_ms}ms) must fit inside generation ({gen}ms)"
    );
}

#[tokio::test]
async fn non_reasoning_stream_reports_no_reasoning_ms() {
    // The plain SSE_OK stream has no reasoning deltas — reasoning_ms must
    // stay None (the trace row renders no reason segment).
    let server = StubServer::start(vec![StubResponse {
        status: 200,
        content_type: "text/event-stream",
        body: SSE_OK,
    }])
    .await;
    let log = Arc::new(LlmRequestLog::new());
    let client = OpenAiClient::new_with_trace(
        OpenAiClientConfig {
            base_url: format!("http://{}/v1/", server.addr),
            ..OpenAiClientConfig::test_default()
        },
        Some(log.clone()),
    );

    let messages = vec![Message::user_text("hello")];
    let mut stream = client
        .complete(&messages, &[], None)
        .await
        .expect("stubbed stream succeeds");
    while futures::StreamExt::next(&mut stream).await.is_some() {}

    let id = client
        .last_record_id
        .lock()
        .expect("last_record_id lock poisoned")
        .expect("complete() must create a record");
    let rec = log.get(id).expect("record present");
    assert_eq!(rec.reasoning_ms, None, "no reasoning → no reasoning_ms");
    assert!(rec.generation_ms.is_some(), "generation still timed");
}

/// An SSE stream whose answer is wrapped in literal <think>…</think> tags
/// (local Ollama / LM Studio style).
const SSE_THINK_TAGS: &str = concat!(
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"<think>\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hidden reasoning\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"</think>\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"visible answer\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":10,\"completion_tokens_details\":{\"reasoning_tokens\":0},\"prompt_tokens_details\":{\"cached_tokens\":0}},\"choices\":[]}\n\n",
    "data: [DONE]\n\n",
);

#[tokio::test]
async fn think_tag_extraction_only_engages_for_local_providers() {
    // Regression (review 2026-08-22 Low 3): the <think> tag filter must NOT
    // run for OpenAI-kind gateways — a literal `<think>` at answer position
    // 0 from a non-thinking model must survive as answer text (otherwise it
    // is swallowed into reasoning and echoed back as reasoning_content).
    // Local providers (local Ollama / LM Studio) get the extraction.
    let server = StubServer::start(vec![
        StubResponse {
            status: 200,
            content_type: "text/event-stream",
            body: SSE_THINK_TAGS,
        },
        StubResponse {
            status: 200,
            content_type: "text/event-stream",
            body: SSE_THINK_TAGS,
        },
    ])
    .await;
    let make_client = |kind: ProviderKind| {
        OpenAiClient::new_with_trace(
            OpenAiClientConfig {
                base_url: format!("http://{}/v1/", server.addr),
                kind,
                ..OpenAiClientConfig::test_default()
            },
            None,
        )
    };
    let messages = vec![Message::user_text("hello")];
    // Drain a stream, collecting reasoning vs answer text separately.
    async fn collect(stream: futures::stream::BoxStream<'_, LlmEvent>) -> (String, String) {
        tokio::pin!(stream);
        let mut reasoning = String::new();
        let mut answer = String::new();
        while let Some(ev) = futures::StreamExt::next(&mut stream).await {
            match ev {
                LlmEvent::ReasoningDelta { text } => reasoning.push_str(&text),
                LlmEvent::TextDelta { text } => answer.push_str(&text),
                _ => {}
            }
        }
        (reasoning, answer)
    }

    // Local provider: the tags are extracted → reasoning, then the answer.
    let local_client = make_client(ProviderKind::Local);
    let stream = local_client
        .complete(&messages, &[], None)
        .await
        .expect("stubbed stream succeeds");
    let (reasoning, answer) = collect(stream).await;
    assert_eq!(reasoning, "hidden reasoning");
    assert_eq!(answer, "visible answer");

    // OpenAI-kind provider: no extraction — the literal tags stay in the
    // answer (a non-thinking model opening with a tag is not a think block).
    let openai_client = make_client(ProviderKind::OpenAI);
    let stream = openai_client
        .complete(&messages, &[], None)
        .await
        .expect("stubbed stream succeeds");
    let (reasoning, answer) = collect(stream).await;
    assert_eq!(reasoning, "");
    assert_eq!(answer, "<think>hidden reasoning</think>visible answer");
}

#[test]
fn local_request_demotes_non_leading_system_messages() {
    // A Local endpoint must never receive a `system` message anywhere but
    // index 0. Here the conversation summary lands at index 1 and a
    // suggestion lands at the end — both must be demoted to `user` with a
    // `System: ` prefix, leaving only the leading system message.
    let client = local_test_client();
    let messages = vec![
        Message::system("You are a coding agent."),
        Message::system("## Conversation summary\n\nstuff"),
        Message::user_text("hello"),
        Message::system("User suggestion: focus on tests"),
    ];
    let body = client.build_request_json(&messages, &[], None).unwrap();
    let arr = body["messages"].as_array().unwrap();
    assert_eq!(arr.len(), 4);
    assert_eq!(arr[0]["role"], "system");
    assert_eq!(arr[0]["content"], "You are a coding agent.");
    // Non-leading system messages demoted to user with "System: " prefix.
    assert_eq!(arr[1]["role"], "user");
    assert_eq!(
        arr[1]["content"],
        "System: ## Conversation summary\n\nstuff"
    );
    assert_eq!(arr[2]["role"], "user");
    assert_eq!(arr[2]["content"], "hello");
    assert_eq!(arr[3]["role"], "user");
    assert_eq!(arr[3]["content"], "System: User suggestion: focus on tests");
}

#[test]
fn openai_request_passes_system_roles_through_unchanged() {
    // OpenAI-kind providers accept system messages anywhere — the
    // sanitizer must NOT touch them (byte-for-byte role preservation).
    let client = OpenAiClient::new(OpenAiClientConfig::test_default());
    let messages = vec![
        Message::system("head"),
        Message::user_text("hi"),
        Message::system("volatile tail"),
    ];
    let body = client.build_request_json(&messages, &[], None).unwrap();
    let arr = body["messages"].as_array().unwrap();
    assert_eq!(arr[0]["role"], "system");
    assert_eq!(arr[1]["role"], "user");
    // The trailing system message is preserved verbatim for OpenAI.
    assert_eq!(arr[2]["role"], "system");
    assert_eq!(arr[2]["content"], "volatile tail");
}

#[test]
fn local_request_without_extra_system_messages_is_unchanged() {
    // The fast path: no non-leading system message → roles pass through
    // untouched (system at 0, user, assistant, tool all preserved).
    let client = local_test_client();
    let messages = vec![
        Message::system("head"),
        Message::user_text("hi"),
        Message::assistant_text("hello"),
        Message {
            tool_call_id: Some("call_1".into()),
            ..Message::text(Role::Tool, "tool output")
        },
    ];
    let body = client.build_request_json(&messages, &[], None).unwrap();
    let arr = body["messages"].as_array().unwrap();
    assert_eq!(arr[0]["role"], "system");
    assert_eq!(arr[1]["role"], "user");
    assert_eq!(arr[2]["role"], "assistant");
    assert_eq!(arr[3]["role"], "tool");
    assert_eq!(arr[3]["tool_call_id"], "call_1");
}

#[test]
fn validate_rejects_empty_messages_array() {
    let err = validate_request_messages(&[]).unwrap_err();
    assert!(
        err.to_string().contains("messages array is empty"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_rejects_empty_assistant_message() {
    // An assistant turn with no text AND no tool calls is rejected by
    // Kimi ("must not be empty") and GLM (code 1214) alike.
    let messages = vec![
        Message::system("head"),
        Message::assistant_text("   "),
        Message::user_text("hi"),
    ];
    let err = validate_request_messages(&messages).unwrap_err();
    assert!(
        err.to_string()
            .contains("messages[1] is an assistant message with no content"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_accepts_assistant_with_tool_calls_and_empty_text() {
    // The standard tool-call turn shape: assistant text can be empty when
    // tool_calls are present — must NOT be rejected.
    let messages = vec![
        Message::user_text("list files"),
        Message::assistant("", vec![ToolCall::new("call_1", "file_read", "{}")]),
        Message {
            tool_call_id: Some("call_1".into()),
            ..Message::text(Role::Tool, "contents")
        },
    ];
    assert!(validate_request_messages(&messages).is_ok());
}

#[test]
fn validate_rejects_dangling_tool_call_id() {
    // The tool message references call_9, but the assistant turn only
    // carries call_1 — the referenced call was dropped from history.
    let messages = vec![
        Message::assistant("", vec![ToolCall::new("call_1", "file_read", "{}")]),
        Message {
            tool_call_id: Some("call_9".into()),
            ..Message::text(Role::Tool, "contents")
        },
    ];
    let err = validate_request_messages(&messages).unwrap_err();
    assert!(
        err.to_string()
            .contains("tool_call_id \"call_9\" has no matching assistant tool call"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_rejects_tool_message_without_tool_call_id() {
    let messages = vec![Message::text(Role::Tool, "contents")];
    let err = validate_request_messages(&messages).unwrap_err();
    assert!(
        err.to_string()
            .contains("tool message with no tool_call_id"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn complete_with_empty_messages_fails_before_any_request() {
    // The guard must fire inside complete() BEFORE the HTTP client is
    // touched. The base_url points at an unroutable port, so if the guard
    // were missing the error would be a transport failure ("failed to
    // start stream") or an upstream 400 — asserting our local message
    // proves no request was sent.
    let client = OpenAiClient::new(OpenAiClientConfig {
        base_url: "http://127.0.0.1:9/v1/".into(),
        ..OpenAiClientConfig::test_default()
    });
    let err = client
        .complete(&[], &[], None)
        .await
        .err()
        .expect("empty messages should fail before any request");
    assert!(
        err.to_string().contains("messages array is empty"),
        "unexpected error: {err}"
    );
}

// ── Self-healing reasoning_effort fallback (regression: backlog item #37
//    failed with "Invalid 'reasoning_effort' value: 'max'" from a local
//    Qwen/LM Studio endpoint that only accepts
//    none/minimal/low/medium/high/xhigh) ─────────────────────────────────

/// The exact rejection a Qwen3-via-LM Studio endpoint returns for the app
/// default `reasoning_effort: "max"` (recorded in provider-errors.jsonl).
const EFFORT_REJECTION: &str = "{\"error\":{\"message\":\"Invalid 'reasoning_effort' value: 'max'. Supported values: none, minimal, low, medium, high, xhigh.\",\"type\":\"invalid_request_error\",\"param\":\"reasoning_effort\",\"code\":\"invalid_value\"}}";

/// A minimal successful SSE chat-completions stream (role/content/finish
/// chunks + usage + the `[DONE]` sentinel, which the parser skips).
const SSE_OK: &str = concat!(
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
    "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"completion_tokens_details\":{\"reasoning_tokens\":0},\"prompt_tokens_details\":{\"cached_tokens\":0}},\"choices\":[]}\n\n",
    "data: [DONE]\n\n",
);

/// A canned HTTP response served by [`StubServer`].
struct StubResponse {
    status: u16,
    content_type: &'static str,
    body: &'static str,
}

/// A minimal HTTP/1.1 server for `complete()` tests: serves a canned
/// response per connection (in order) and records each request's parsed
/// JSON body, so tests can assert both the retry count and the retried
/// request's body. Kept tiny on purpose — only what `complete()` sends:
/// POST /chat/completions with a JSON body.
struct StubServer {
    addr: std::net::SocketAddr,
    /// Request bodies received, in request order.
    bodies: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
}

impl StubServer {
    /// Start the server. `responses[i]` is served for the i-th request;
    /// the test must send exactly as many requests as responses (the
    /// server task ends after the last one).
    async fn start(responses: Vec<StubResponse>) -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("test server local addr");
        let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let bodies_task = Arc::clone(&bodies);
        tokio::spawn(async move {
            for response in responses {
                let (mut socket, _peer) = listener.accept().await.expect("accept");
                // Read the request head (up to the \r\n\r\n terminator).
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = socket.read(&mut chunk).await.expect("read head");
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let head_end = buf
                    .windows(4)
                    .position(|w| w == b"\r\n\r\n")
                    .map(|p| p + 4)
                    .expect("request head terminator");
                let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                let content_length: usize = head
                    .lines()
                    .find_map(|l| {
                        let lower = l.to_ascii_lowercase();
                        lower
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse().ok())
                    })
                    .unwrap_or(0);
                // The head read may have pulled body bytes in too — read
                // until the full content-length is buffered.
                while buf.len() < head_end + content_length {
                    let n = socket.read(&mut chunk).await.expect("read body");
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                let body_bytes = buf[head_end..head_end + content_length].to_vec();
                let body_json =
                    serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
                bodies_task.lock().expect("bodies lock").push(body_json);

                let reason = if response.status == 200 {
                    "OK"
                } else {
                    "Bad Request"
                };
                let head = format!(
                    "HTTP/1.1 {} {}\r\ncontent-type: {}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    response.status,
                    reason,
                    response.content_type,
                    response.body.len(),
                );
                socket.write_all(head.as_bytes()).await.expect("write head");
                socket
                    .write_all(response.body.as_bytes())
                    .await
                    .expect("write body");
                socket.shutdown().await.expect("shutdown");
            }
        });
        Self { addr, bodies }
    }

    /// The request bodies received so far, in request order.
    fn received(&self) -> Vec<serde_json::Value> {
        self.bodies.lock().expect("bodies lock").clone()
    }
}

/// A test client pointed at a [`StubServer`] with an optional
/// `reasoning_effort` (the field whose rejection the fallback handles).
fn stub_client(server: &StubServer, reasoning_effort: Option<&str>) -> OpenAiClient {
    OpenAiClient::new(OpenAiClientConfig {
        base_url: format!("http://{}/v1", server.addr),
        reasoning_effort: reasoning_effort.map(|s| s.to_string()),
        ..OpenAiClientConfig::test_default()
    })
}

#[tokio::test]
async fn complete_retries_once_without_reasoning_effort_on_400_rejection() {
    // Regression: a local Qwen3/LM Studio endpoint rejects the app default
    // `reasoning_effort: "max"` with HTTP 400. The client must retry ONCE
    // with the field omitted so the turn (and the backlog item driving it)
    // succeeds instead of failing with the provider error.
    let server = StubServer::start(vec![
        StubResponse {
            status: 400,
            content_type: "application/json",
            body: EFFORT_REJECTION,
        },
        StubResponse {
            status: 200,
            content_type: "text/event-stream",
            body: SSE_OK,
        },
    ])
    .await;
    let client = stub_client(&server, Some("max"));

    let mut stream = client
        .complete(&[Message::user_text("hi")], &[], None)
        .await
        .expect("complete must succeed via the fallback retry");

    use futures::StreamExt;
    let mut finished = false;
    let mut errored: Option<String> = None;
    while let Some(event) = stream.next().await {
        match event {
            LlmEvent::Finish { .. } => finished = true,
            LlmEvent::Error { error } => errored = Some(error),
            _ => {}
        }
    }
    assert!(finished, "stream must finish after the fallback retry");
    assert!(errored.is_none(), "no error expected, got: {errored:?}");

    // Exactly two requests: the original with `reasoning_effort`, and the
    // self-healing retry without it.
    let bodies = server.received();
    assert_eq!(bodies.len(), 2, "expected original + one retry");
    assert_eq!(bodies[0]["reasoning_effort"], serde_json::json!("max"));
    assert!(
        bodies[1].get("reasoning_effort").is_none(),
        "retry body must omit reasoning_effort, got: {}",
        bodies[1]
    );
}

#[tokio::test]
async fn complete_does_not_retry_unrelated_400() {
    // A 400 that does NOT name `reasoning_effort` (GLM's code-1214
    // "messages parameter is illegal" shape) must NOT trigger the
    // fallback — and its full body must survive into the error so the
    // GLM diagnostics stay debuggable.
    let rejection = "{\"error\":{\"message\":\"The messages parameter is illegal\",\"type\":\"invalid_request_error\",\"param\":\"messages\",\"code\":\"1214\"}}";
    let server = StubServer::start(vec![StubResponse {
        status: 400,
        content_type: "application/json",
        body: rejection,
    }])
    .await;
    let client = stub_client(&server, Some("max"));

    let err = match client.complete(&[Message::user_text("hi")], &[], None).await {
        Ok(_) => panic!("unrelated 400 must surface as an error"),
        Err(e) => e,
    };

    // The body text is preserved (no double-consume of the response).
    assert!(
        err.to_string().contains("messages parameter is illegal"),
        "error: {err}"
    );
    assert_eq!(server.received().len(), 1, "no retry for an unrelated 400");
}

#[tokio::test]
async fn complete_does_not_retry_400_when_effort_was_omitted() {
    // When the client never sent `reasoning_effort` (effort "off" or an
    // endpoint with supports_reasoning_effort=false), there is nothing to
    // drop — a 400 naming the field must surface as-is, not retry.
    let server = StubServer::start(vec![StubResponse {
        status: 400,
        content_type: "application/json",
        body: EFFORT_REJECTION,
    }])
    .await;
    let client = stub_client(&server, None);

    let err = match client.complete(&[Message::user_text("hi")], &[], None).await {
        Ok(_) => panic!("400 must surface when no effort was sent"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("400"), "error: {err}");
    assert_eq!(
        server.received().len(),
        1,
        "no retry when the field was never sent"
    );
}

#[tokio::test]
async fn http_error_stamps_connect_bucket_and_parked_backoff() {
    // Regression (trace-graph honesty): an HTTP error used to stamp the
    // whole in-flight window into ttft_ms — the "wait" bucket — while
    // the same window on success lands in connect_ms. Now the error
    // path stamps connect_ms (record creation → the error, including
    // the server's reject processing) so the same window gets the same
    // bucket on success and failure. The parked retry backoff must
    // land on the record it preceded.
    let server = StubServer::start(vec![StubResponse {
        status: 500,
        content_type: "application/json",
        body: "{\"error\":{\"message\":\"boom\"}}",
    }])
    .await;
    let log = Arc::new(LlmRequestLog::new());
    let client = OpenAiClient::new_with_trace(
        OpenAiClientConfig {
            base_url: format!("http://{}/v1", server.addr),
            ..OpenAiClientConfig::test_default()
        },
        Some(log.clone()),
    );
    // Park a retry backoff like complete_with_retry does after a sleep.
    client.record_backoff_ms(1500);

    let result = client.complete(&[Message::user_text("hi")], &[], None).await;
    assert!(result.is_err(), "the 500 must surface as an error");

    let id = client
        .last_record_id
        .lock()
        .expect("last_record_id lock poisoned")
        .expect("complete() recorded the request");
    let record = log.get(id).expect("the record exists");
    assert_eq!(record.backoff_ms, Some(1500));
    assert!(
        record.connect_ms.is_some(),
        "the HTTP error must stamp the connect bucket"
    );
    assert_eq!(
        record.ttft_ms, None,
        "no first chunk — nothing to attribute to TTFT"
    );
    assert_eq!(record.http_status, Some(500));
}

#[tokio::test]
async fn transport_failure_stamps_connect_bucket() {
    // A request that dies before any response headers arrived
    // (connection refused — nothing listens on port 1) must stamp the
    // connect bucket, not TTFT: the same window success attributes to
    // connect_ms.
    let log = Arc::new(LlmRequestLog::new());
    let client = OpenAiClient::new_with_trace(
        OpenAiClientConfig {
            base_url: "http://127.0.0.1:1/v1".into(),
            ..OpenAiClientConfig::test_default()
        },
        Some(log.clone()),
    );

    let result = client.complete(&[Message::user_text("hi")], &[], None).await;
    assert!(result.is_err(), "connection refused must fail the request");

    let id = client
        .last_record_id
        .lock()
        .expect("last_record_id lock poisoned")
        .expect("complete() recorded the request");
    let record = log.get(id).expect("the record exists");
    assert!(
        record.connect_ms.is_some(),
        "the transport failure must stamp the connect bucket"
    );
    assert_eq!(record.ttft_ms, None, "no headers ever arrived");
    assert_eq!(record.http_status, Some(0));
}

// ── Responses API (Rule 3 stateful path) ──────────────────────────────

fn responses_client() -> OpenAiClient {
    OpenAiClient::new(OpenAiClientConfig {
        model: "gpt-4o".into(),
        use_responses_api: true,
        ..OpenAiClientConfig::test_default()
    })
}

#[test]
fn build_responses_request_json_sends_previous_response_id() {
    // Rule 3: when a previous assistant turn has a response_id, the
    // request sends previous_response_id and only the new input (the
    // server holds the previous context).
    let client = responses_client();
    let assistant = Message {
        response_id: Some("resp_abc".into()),
        ..Message::assistant_text("calling tool")
    };
    let tool_result = Message::tool_result("c1", "read", "42");
    let body = client
        .build_responses_request_json(&[assistant, tool_result], &[], None)
        .unwrap();
    assert_eq!(
        body["previous_response_id"].as_str().unwrap(),
        "resp_abc"
    );
    // Input contains only the new messages (tool_result), not the
    // assistant turn (the server holds it via previous_response_id).
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 1);
    assert_eq!(input[0]["type"], serde_json::json!("function_call_output"));
    assert_eq!(input[0]["call_id"], serde_json::json!("c1"));
    // include param for reasoning encrypted_content.
    assert_eq!(
        body["include"],
        serde_json::json!(["reasoning.encrypted_content"])
    );
}

#[test]
fn build_responses_request_json_full_input_when_no_anchor() {
    // Rule 3: first request (no previous response_id) sends full input.
    let client = responses_client();
    let messages = vec![
        Message::user_text("Hello"),
        Message::assistant_text("Hi there"),
    ];
    let body = client
        .build_responses_request_json(&messages, &[], None)
        .unwrap();
    // No previous_response_id.
    assert!(body.get("previous_response_id").is_none());
    // Full input.
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 2);
    assert_eq!(input[0]["role"], serde_json::json!("user"));
    assert_eq!(input[1]["role"], serde_json::json!("assistant"));
}

#[test]
fn parse_responses_sse_chunk_captures_response_id_text_and_finish() {
    // response.created → ResponseId
    let events = parse_responses_sse_chunk(&serde_json::json!({
        "type": "response.created",
        "response": {"id": "resp_abc"}
    }));
    assert!(matches!(
        &events[0],
        LlmEvent::ResponseId { id } if id == "resp_abc"
    ));

    // response.output_text.delta → TextDelta
    let events = parse_responses_sse_chunk(&serde_json::json!({
        "type": "response.output_text.delta",
        "delta": "Hello"
    }));
    assert!(matches!(
        &events[0],
        LlmEvent::TextDelta { text } if text == "Hello"
    ));

    // response.completed → ResponseId + Usage + Finish
    let events = parse_responses_sse_chunk(&serde_json::json!({
        "type": "response.completed",
        "response": {
            "id": "resp_abc",
            "usage": {
                "input_tokens": 10,
                "output_tokens": 5,
                "output_tokens_details": {"reasoning_tokens": 3}
            }
        }
    }));
    assert!(events.iter().any(|e| matches!(
        e,
        LlmEvent::ResponseId { id } if id == "resp_abc"
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        LlmEvent::Usage { prompt_tokens: 10, completion_tokens: 5, reasoning_tokens: 3, .. }
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        LlmEvent::Finish { .. }
    )));
}

#[test]
fn parse_responses_sse_chunk_parses_cached_tokens() {
    // The Responses API reports prompt tokens served from cache under
    // usage.input_tokens_details.cached_tokens — the counterpart of the
    // chat-completions path's prompt_tokens_details.cached_tokens
    // (backlog 648051bf).
    let events = parse_responses_sse_chunk(&serde_json::json!({
        "type": "response.completed",
        "response": {
            "id": "resp_abc",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 5,
                "input_tokens_details": {"cached_tokens": 80},
                "output_tokens_details": {"reasoning_tokens": 3}
            }
        }
    }));
    assert!(events.iter().any(|e| matches!(
        e,
        LlmEvent::Usage { prompt_tokens: 100, cached_tokens: 80, .. }
    )));
}

#[test]
fn parse_responses_sse_chunk_handles_tool_calls() {
    // response.output_item.added (function_call) → ToolCallStart
    let events = parse_responses_sse_chunk(&serde_json::json!({
        "type": "response.output_item.added",
        "output_index": 0,
        "item": {
            "type": "function_call",
            "call_id": "call_1",
            "name": "read_file"
        }
    }));
    assert!(matches!(
        &events[0],
        LlmEvent::ToolCallStart { index: 0, id, name }
            if id == "call_1" && name == "read_file"
    ));

    // response.function_call_arguments.delta → ToolCallArgumentDelta
    let events = parse_responses_sse_chunk(&serde_json::json!({
        "type": "response.function_call_arguments.delta",
        "output_index": 0,
        "delta": "{\"path\":"
    }));
    assert!(matches!(
        &events[0],
        LlmEvent::ToolCallArgumentDelta { index: 0, fragment }
            if fragment == "{\"path\":"
    ));
}

#[test]
fn parse_responses_sse_chunk_handles_error() {
    let events = parse_responses_sse_chunk(&serde_json::json!({
        "type": "response.failed",
        "error": {"message": "rate limit exceeded"}
    }));
    assert!(matches!(
        &events[0],
        LlmEvent::Error { error } if error == "rate limit exceeded"
    ));
}

#[test]
fn stateful_and_stateless_both_retain_reasoning_continuity() {
    // Acceptance test #5: stateful and stateless parity. Run test 1
    // (sequential tool call) both ways. Both must maintain reasoning
    // continuity — stateless via raw echo, stateful via
    // previous_response_id (the server holds the reasoning).

    // ── Stateless path (chat.completions): the second request body
    // contains the raw assistant turn with reasoning_content, echoed
    // verbatim with the same value the model returned. ──
    let stateless = OpenAiClient::new(OpenAiClientConfig {
        model: "deepseek-v4".into(),
        ..OpenAiClientConfig::test_default()
    });
    let assistant1 = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "content": "",
            "reasoning_content": "first thought",
            "tool_calls": [{
                "id": "c1", "type": "function",
                "function": { "name": "read", "arguments": "{\"a\":1}" }
            }]
        })),
        ..Message::assistant_text("")
    };
    let tool1 = Message::tool_result("c1", "read", "42");
    let body = stateless
        .build_request_json(&[assistant1, tool1], &[], None)
        .unwrap();
    assert_eq!(
        body["messages"][0]["reasoning_content"].as_str().unwrap(),
        "first thought",
        "stateless: reasoning_content must be echoed verbatim"
    );

    // ── Stateful path (Responses API): the second request sends
    // previous_response_id — the server holds the reasoning state, so
    // full history is not resent. ──
    let stateful = OpenAiClient::new(OpenAiClientConfig {
        model: "gpt-4o".into(),
        use_responses_api: true,
        ..OpenAiClientConfig::test_default()
    });
    let assistant_with_id = Message {
        response_id: Some("resp_abc".into()),
        ..Message::assistant_text("calling tool")
    };
    let tool_result = Message::tool_result("c1", "read", "42");
    let body = stateful
        .build_responses_request_json(&[assistant_with_id, tool_result], &[], None)
        .unwrap();
    assert_eq!(
        body["previous_response_id"].as_str().unwrap(),
        "resp_abc",
        "stateful: previous_response_id must be sent (server holds reasoning)"
    );
    // The input contains only the new tool result (not the full history).
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 1, "stateful: only new input is sent");
}

// ── H1+H2+H3 regression: raw echo must not override structured fixes ─

#[test]
fn build_request_json_bad_tool_call_type_falls_through_to_fields() {
    // H3: `Message::raw` is persisted, so a conversation saved before the
    // identity/fragment split in `stream::merge_identity` carries a
    // concatenated discriminator and would 400 forever on every replay.
    // Rejecting the raw also repairs the identically-corrupted `id`, which
    // would otherwise break tool_call_id pairing.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "gpt-4o".into(),
        ..OpenAiClientConfig::test_default()
    });
    let msg = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "content": "calling",
            "tool_calls": [{
                "id": "call_1call_1",
                "type": "functionfunction",
                "function": { "name": "readread", "arguments": "{}" }
            }]
        })),
        ..Message::assistant("calling", vec![ToolCall::new("call_1", "read", "{}")])
    };
    let body = client.build_request_json(&[msg], &[], None).unwrap();
    let tc = &body["messages"][0]["tool_calls"][0];
    assert_eq!(
        tc["type"],
        serde_json::json!("function"),
        "corrupted discriminator must not reach the wire"
    );
    assert_eq!(
        tc["id"],
        serde_json::json!("call_1"),
        "id repaired from structured field"
    );
    assert_eq!(tc["function"]["name"], serde_json::json!("read"));
}

#[test]
fn build_request_json_well_formed_tool_call_raw_still_echoed() {
    // The H3 guard must not over-reject: a valid raw tool call is still
    // echoed verbatim, unknown keys included (Rule 1).
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "gpt-4o".into(),
        ..OpenAiClientConfig::test_default()
    });
    let msg = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "content": "calling",
            "custom_unknown": "keep me",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": { "name": "read", "arguments": "{}" }
            }]
        })),
        ..Message::assistant("calling", vec![ToolCall::new("call_1", "read", "{}")])
    };
    let body = client.build_request_json(&[msg], &[], None).unwrap();
    assert_eq!(
        body["messages"][0]["custom_unknown"],
        serde_json::json!("keep me"),
        "valid raw must still be echoed verbatim"
    );
}

#[test]
fn build_request_json_null_turn_uses_placeholder_not_empty_raw() {
    // H1: a null turn (Finish with no content) produces raw
    // {role:"assistant"} with no content key. The builder must fall
    // through to field construction so the "(no output)" placeholder
    // is sent — not an empty assistant message that 400s.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "gpt-4o".into(),
        ..OpenAiClientConfig::test_default()
    });
    let msg = Message {
        raw: Some(serde_json::json!({"role": "assistant"})),
        ..Message::assistant_text("(no output)")
    };
    let body = client.build_request_json(&[msg], &[], None).unwrap();
    assert_eq!(
        body["messages"][0]["content"].as_str().unwrap(),
        "(no output)",
        "null turn must use the placeholder, not the empty raw"
    );
}

#[test]
fn build_request_json_malformed_args_fall_through_to_sanitized() {
    // H2: when the turn loop sanitizes malformed tool-call arguments,
    // the builder must use the sanitized structured tool_calls — not
    // echo the raw with the malformed arguments.
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "gpt-4o".into(),
        ..OpenAiClientConfig::test_default()
    });
    let msg = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "content": "",
            "tool_calls": [{
                "id": "c1", "type": "function",
                "function": { "name": "read", "arguments": "{bad json" }
            }]
        })),
        ..Message::assistant("calling", vec![
            ToolCall::new("c1", "read", "{}"),
        ])
    };
    let body = client.build_request_json(&[msg], &[], None).unwrap();
    // The sanitized "{}" must be sent, not the malformed "{bad json".
    assert_eq!(
        body["messages"][0]["tool_calls"][0]["function"]["arguments"]
            .as_str()
            .unwrap(),
        "{}",
        "malformed args must fall through to sanitized structured tool_calls"
    );
}

#[test]
fn build_request_json_reasoning_only_turn_echoed_via_raw() {
    // LOW-1: a raw turn carrying only reasoning fields (no content, no
    // tool_calls) must be echoed via raw — not dropped to field
    // construction (which only re-adds reasoning_content, not
    // thought_signature or unknown keys).
    let client = OpenAiClient::new(OpenAiClientConfig {
        model: "gemini-3.0-flash".into(),
        ..OpenAiClientConfig::test_default()
    });
    let msg = Message {
        raw: Some(serde_json::json!({
            "role": "assistant",
            "reasoning_content": "a thought",
            "thought_signature": "sig"
        })),
        ..Message::assistant_text("")
    };
    let body = client.build_request_json(&[msg], &[], None).unwrap();
    // Raw echoed verbatim — thought_signature preserved (field construction
    // would drop it).
    assert_eq!(
        body["messages"][0]["thought_signature"].as_str().unwrap(),
        "sig",
        "reasoning-only turn must echo raw so thought_signature survives"
    );
    assert_eq!(
        body["messages"][0]["reasoning_content"].as_str().unwrap(),
        "a thought"
    );
}

#[test]
fn apply_stream_guard_reports_cut_details_mid_delta() {
    let stops = vec!["<custom_stop>".to_string()];
    let events = vec![
        LlmEvent::TextDelta {
            text: "answer here".into(),
        },
        LlmEvent::TextDelta {
            text: "before<custom_stop>after".into(),
        },
        LlmEvent::TextDelta {
            text: "never reached".into(),
        },
    ];
    let (guarded, hit) = apply_stream_guard(events, &stops, 11);
    let texts: Vec<String> = guarded
        .into_iter()
        .filter_map(|ev| match ev {
            LlmEvent::TextDelta { text } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(
        texts,
        vec!["answer here".to_string(), "before".to_string()]
    );
    let hit = hit.expect("guard fired");
    assert_eq!(hit.boundary, "<custom_stop>");
    assert_eq!(hit.byte_idx, "before".len());
    assert_eq!(hit.delta_len, "before<custom_stop>after".len());
    assert_eq!(hit.chars_before, 11);
    assert!(!hit.prefix_empty);
}

#[test]
fn apply_stream_guard_invisible_cut_at_delta_start() {
    let stops = vec!["<custom_stop>".to_string()];
    let events = vec![LlmEvent::TextDelta {
        text: "<custom_stop>tail".into(),
    }];
    let (guarded, hit) = apply_stream_guard(events, &stops, 0);
    assert!(guarded.is_empty());
    let hit = hit.expect("guard fired");
    assert!(hit.prefix_empty);
    assert_eq!(hit.byte_idx, 0);
    assert_eq!(hit.chars_before, 0);
}

#[test]
fn apply_stream_guard_passthrough_without_boundary() {
    let stops = vec!["<custom_stop>".to_string()];
    let events = vec![
        LlmEvent::TextDelta {
            text: "plain".into(),
        },
        LlmEvent::Finish {
            reason: crate::provider::FinishReason::Stop,
        },
    ];
    let (guarded, hit) = apply_stream_guard(events, &stops, 5);
    assert_eq!(guarded.len(), 2);
    assert!(hit.is_none());
}

#[test]
fn test_find_boundary_cutoff() {
    let stops = vec![
        "<|user|>".to_string(),
        "<|assistant|>".to_string(),
        "<|observation|>".to_string(),
        "<custom_stop>".to_string(),
    ];
    // Exact match at start
    assert_eq!(
        find_boundary_cutoff("<|user|>hello", &stops),
        Some((0, "<|user|>".len()))
    );
    // Match inside text
    assert_eq!(
        find_boundary_cutoff("answer here<|observation|>tail", &stops),
        Some((11, "<|observation|>".len()))
    );
    // Multiple matches returns earliest
    assert_eq!(
        find_boundary_cutoff("first<|assistant|>then<|user|>", &stops),
        Some((5, "<|assistant|>".len()))
    );
    // Custom stop
    assert_eq!(
        find_boundary_cutoff("some text<custom_stop>", &stops),
        Some((9, "<custom_stop>".len()))
    );
    // No match
    assert_eq!(find_boundary_cutoff("just ordinary text", &stops), None);
    // Empty text or empty stops list
    assert_eq!(find_boundary_cutoff("", &stops), None);
    assert_eq!(find_boundary_cutoff("text", &[]), None);
}

#[tokio::test]
async fn stream_guard_truncates_and_stops_on_boundary_token() {
    // SSE payload where assistant outputs some text, then emits a raw GLM boundary
    // token (<|user|>) followed by hallucinated prompt text and further SSE chunks.
    let sse_body = "\
data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"valid code here\"},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"2\",\"choices\":[{\"delta\":{\"content\":\"\\n<|user|>\\nhallucinated dialogue\"},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"3\",\"choices\":[{\"delta\":{\"content\":\"more garbage tokens\"},\"finish_reason\":null}]}\n\n\
data: [DONE]\n\n";

    let server = StubServer::start(vec![StubResponse {
        status: 200,
        content_type: "text/event-stream",
        body: sse_body,
    }])
    .await;

    let client = OpenAiClient::new(OpenAiClientConfig {
        base_url: format!("http://{}/v1/", server.addr),
        model: "glm-5.3-flash".into(),
        // Config-driven guard boundaries (was: hardcoded glm-5.3 prefix
        // match) — the four GLM role tags, built from \u escapes.
        stop_boundary_strings: vec![
            "\u{3c}|endoftext|\u{3e}".into(),
            "\u{3c}|user|\u{3e}".into(),
            "\u{3c}|assistant|\u{3e}".into(),
            "\u{3c}|observation|\u{3e}".into(),
        ],
        ..OpenAiClientConfig::test_default()
    });

    use futures::StreamExt;

    let mut rx = client
        .complete(&[Message::user_text("test")], &[], None)
        .await
        .expect("stream started");

    let mut received_text = String::new();
    let mut got_finish_stop = false;

    while let Some(event) = rx.next().await {
        match event {
            LlmEvent::TextDelta { text } => {
                received_text.push_str(&text);
            }
            LlmEvent::Finish { reason } => {
                if reason == crate::provider::FinishReason::Stop {
                    got_finish_stop = true;
                }
            }
            _ => {}
        }
    }

    // Must receive the prefix before the boundary token
    assert_eq!(received_text, "valid code here\n");
    // Must NOT receive the boundary token itself or any subsequent hallucinated content
    assert!(!received_text.contains("<|user|>"));
    assert!(!received_text.contains("hallucinated dialogue"));
    assert!(!received_text.contains("more garbage tokens"));
    // Must receive finish reason stop
    assert!(got_finish_stop, "stream guard must emit FinishReason::Stop");
}

#[tokio::test]
async fn stream_guard_protects_aliased_model_via_config_only() {
    // The core config-driven guarantee (backlog f322277c): a model whose
    // name matches NO vendor prefix — an alias, a fine-tune, a proxy
    // rename — gets the full stop-boundary protection purely from
    // stop_boundary_strings: the stream guard truncates on a raw
    // boundary token. Tag literals are built from \u escapes
    // (transport-safe, like every other test here).
    let sse_body = "\
data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"answer prefix\"},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"2\",\"choices\":[{\"delta\":{\"content\":\"\\n\u{3c}|assistant|\u{3e}\\nleaked turn\"},\"finish_reason\":null}]}\n\n\
data: {\"id\":\"3\",\"choices\":[{\"delta\":{\"content\":\"garbage\"},\"finish_reason\":null}]}\n\n\
data: [DONE]\n\n";

    let server = StubServer::start(vec![StubResponse {
        status: 200,
        content_type: "text/event-stream",
        body: sse_body,
    }])
    .await;

    let client = OpenAiClient::new(OpenAiClientConfig {
        base_url: format!("http://{}/v1/", server.addr),
        model: "my-custom-alias".into(),
        stop_boundary_strings: vec!["\u{3c}|assistant|\u{3e}".into()],
        ..OpenAiClientConfig::test_default()
    });

    use futures::StreamExt;

    let mut rx = client
        .complete(&[Message::user_text("test")], &[], None)
        .await
        .expect("stream started");

    let mut received_text = String::new();
    let mut got_finish_stop = false;

    while let Some(event) = rx.next().await {
        match event {
            LlmEvent::TextDelta { text } => {
                received_text.push_str(&text);
            }
            LlmEvent::Finish { reason } => {
                if reason == crate::provider::FinishReason::Stop {
                    got_finish_stop = true;
                }
            }
            _ => {}
        }
    }

    // The guard cut the stream at the configured boundary: the prefix
    // arrived, the boundary token and everything after it did not.
    assert_eq!(received_text, "answer prefix\n");
    assert!(!received_text.contains("leaked turn"));
    assert!(!received_text.contains("garbage"));
    assert!(got_finish_stop, "stream guard must emit FinishReason::Stop");
}

/// A minimal HTTP server that writes a 200 SSE head plus two content
/// chunks (with a pause between them), then holds the connection open —
/// so a test can drop the event receiver mid-flight and exercise the
/// pump's consumer-drop arm (D1).
struct SlowSseServer {
    addr: std::net::SocketAddr,
}

impl SlowSseServer {
    async fn start() -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind slow sse server");
        let addr = listener.local_addr().expect("slow sse server addr");
        tokio::spawn(async move {
            let (mut socket, _peer) = listener.accept().await.expect("accept");
            // Read the request head (up to the \r\n\r\n terminator).
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut chunk).await.expect("read head");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            let head =
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
            socket.write_all(head.as_bytes()).await.expect("write head");
            let sse_chunk = |text: &str| {
                format!(
                    "data: {{\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":null}}]}}\n\n"
                )
            };
            socket
                .write_all(sse_chunk("hel").as_bytes())
                .await
                .expect("write chunk 1");
            // Pause so the test can drop the receiver before chunk 2.
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            socket
                .write_all(sse_chunk("lo").as_bytes())
                .await
                .expect("write chunk 2");
            // Hold the connection open — the pump is parked on read until
            // the test's drop makes its next send fail.
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        });
        Self { addr }
    }
}

#[tokio::test]
async fn consumer_drop_stamps_cancelled_not_failed() {
    // D1 regression: dropping the event receiver mid-stream is a USER
    // interrupt (Interrupt/Cancel/Compact/Clear), not a provider failure.
    // The record must be stamped cancelled (terminal, healthy http_status,
    // no error text) and provider-errors.jsonl must NOT grow — previously
    // the drop arm called fail(), which set r.error and mirrored the row
    // into provider-errors.jsonl (45+ false positives).
    use futures::StreamExt;

    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(LlmRequestLog::new());
    log.set_error_log_path(dir.path().join("provider-errors.jsonl"));
    let server = SlowSseServer::start().await;
    let client = OpenAiClient::new_with_trace(
        OpenAiClientConfig {
            base_url: format!("http://{}/v1", server.addr),
            ..OpenAiClientConfig::test_default()
        },
        Some(log.clone()),
    );

    let mut stream = client
        .complete(&[Message::user_text("hi")], &[], None)
        .await
        .expect("complete must succeed");
    // Consume the first event so the pump is running, then drop the
    // receiver mid-flight (what the turn loop's hard-stop fold does).
    let _first = stream.next().await.expect("at least one event");
    drop(stream);

    // Give the pump task time to observe the failed send (the server
    // writes chunk 2 ~250ms after chunk 1; the pump's next send then
    // fails and the drop arm fires).
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;

    let summaries = log.list();
    assert_eq!(summaries.len(), 1, "exactly one request record");
    let s = &summaries[0];
    assert!(s.cancelled, "the record must be stamped cancelled");
    assert!(s.is_complete, "cancelled is terminal — the UI stops polling");
    assert_eq!(s.error, None, "no error text — not a provider failure");
    assert_eq!(s.http_status, Some(200), "the HTTP response was healthy");
    // The always-on provider-error log must not grow for a user cancel.
    assert!(
        !dir.path().join("provider-errors.jsonl").exists(),
        "a user cancel must not write a provider-errors row"
    );
}

#[tokio::test]
async fn cancelled_request_persists_to_cancels_log() {
    // F3 regression: a user cancel must leave a PERSISTENT record. D1 made
    // the in-ring record cancelled (terminal, not failed) but the ring
    // rotates — the dedicated always-on cancels log is the record that
    // survives, appended at stamping time.
    use futures::StreamExt;

    let dir = tempfile::tempdir().unwrap();
    let log = Arc::new(LlmRequestLog::new());
    log.set_cancels_log_path(dir.path().join("cancels.jsonl"));
    let server = SlowSseServer::start().await;
    let client = OpenAiClient::new_with_trace(
        OpenAiClientConfig {
            base_url: format!("http://{}/v1", server.addr),
            ..OpenAiClientConfig::test_default()
        },
        Some(log.clone()),
    );

    let mut stream = client
        .complete(&[Message::user_text("hi")], &[], None)
        .await
        .expect("complete must succeed");
    // Consume the first event so the pump is running, then drop the
    // receiver mid-flight (what the turn loop's hard-stop fold does).
    let _first = stream.next().await.expect("at least one event");
    drop(stream);

    // Give the pump task time to observe the failed send (the server
    // writes chunk 2 ~250ms after chunk 1; the pump's next send then
    // fails and the drop arm fires).
    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    // Deterministic file read: the flush ack covers the cancel line (the
    // writer's cancel/history blocks sit before the flush acks).
    log.flush_file_writes();

    let summaries = log.list();
    assert_eq!(summaries.len(), 1, "exactly one request record");
    let s = &summaries[0];
    assert!(s.cancelled, "the record must be stamped cancelled");

    // The dedicated always-on cancels log carries the persistent row.
    let cancels = std::fs::read_to_string(dir.path().join("cancels.jsonl")).unwrap();
    let lines: Vec<&str> = cancels.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 1, "exactly one cancel row");
    let row: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(row["id"], serde_json::json!(s.id));
    assert_eq!(row["model"], serde_json::json!(s.model));
    assert_eq!(row["cancelled"], serde_json::json!(true));
    assert_eq!(row["http_status"], serde_json::json!(200));
}

/// A minimal HTTP server that delays the SSE head AND the first chunk
/// ~300ms after the POST — the realistic provider pattern (LiteLLM and the
/// direct providers only begin the HTTP response when the first token is
/// ready, so headers and the first SSE chunk arrive in the same burst) —
/// then streams two chunks, a usage chunk, and [DONE], closing the
/// connection so a full-stream test can assert the POST-send→first-chunk
/// TTFT (perf review L1: the old headers-arrived anchor measured ~0).
struct DelayedFirstChunkSseServer {
    addr: std::net::SocketAddr,
}

impl DelayedFirstChunkSseServer {
    async fn start() -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind delayed sse server");
        let addr = listener.local_addr().expect("delayed sse server addr");
        tokio::spawn(async move {
            let (mut socket, _peer) = listener.accept().await.expect("accept");
            // Read the request head (up to the \r\n\r\n terminator).
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = socket.read(&mut chunk).await.expect("read head");
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            // The prefill/queue wait: headers AND the first chunk arrive
            // together, only after the delay — exactly how SSE providers
            // behave (the reason the old headers-arrived ttft anchor
            // measured ~0).
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let head =
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n";
            socket.write_all(head.as_bytes()).await.expect("write head");
            let sse_chunk = |text: &str| {
                format!(
                    "data: {{\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"{text}\"}},\"finish_reason\":null}}]}}\n\n"
                )
            };
            socket
                .write_all(sse_chunk("hel").as_bytes())
                .await
                .expect("write chunk 1");
            socket
                .write_all(sse_chunk("lo").as_bytes())
                .await
                .expect("write chunk 2");
            // Final chunk with usage (the include_usage shape) + [DONE].
            socket
                .write_all(
                    b"data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",\"model\":\"test\",\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2}}\n\n",
                )
                .await
                .expect("write usage chunk");
            socket
                .write_all(b"data: [DONE]\n\n")
                .await
                .expect("write done");
            // Drop the socket: the stream ends cleanly.
        });
        Self { addr }
    }
}

#[tokio::test]
async fn ttft_measures_post_send_to_first_chunk() {
    // L1 regression: the ttft anchor was stamped when the POST response
    // HEADERS arrived — but SSE providers only begin the response when the
    // first token is ready, so headers and the first chunk arrived in the
    // same burst and the measured window was structurally ~0 (ttft_ms was 0
    // on 99.2% of request_stats rows). The anchor is now POST-send: with
    // the server holding headers+first chunk ~300ms after the POST, ttft_ms
    // must cover that wait. connect_ms keeps its record_created→headers
    // window (unchanged semantics — the delay lands there too, since
    // headers arrive with the chunk).
    use futures::StreamExt;

    let log = Arc::new(LlmRequestLog::new());
    let server = DelayedFirstChunkSseServer::start().await;
    let client = OpenAiClient::new_with_trace(
        OpenAiClientConfig {
            base_url: format!("http://{}/v1", server.addr),
            ..OpenAiClientConfig::test_default()
        },
        Some(log.clone()),
    );

    let mut stream = client
        .complete(&[Message::user_text("hi")], &[], None)
        .await
        .expect("complete must succeed");
    let mut usage_ttft = None;
    while let Some(ev) = stream.next().await {
        if let LlmEvent::Usage { ttft_ms, .. } = ev {
            usage_ttft = ttft_ms;
        }
    }
    let ttft = usage_ttft.expect("a Usage event with ttft_ms");
    assert!(
        ttft >= 200,
        "ttft must cover the POST→first-chunk wait (got {ttft}ms; the old headers-arrived anchor measured ~0)"
    );

    // connect_ms keeps its record_created→headers window — the delay lands
    // there too (headers arrive with the first chunk), but its definition
    // is unchanged by this fix.
    let summaries = log.list();
    assert_eq!(summaries.len(), 1);
    let connect = summaries[0].connect_ms.expect("connect_ms set");
    assert!(
        connect >= 200,
        "connect keeps its record→headers window (got {connect}ms)"
    );
}
