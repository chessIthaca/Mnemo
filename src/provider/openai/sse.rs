// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! SSE parsing for the OpenAI-compatible client.
//!
//! Pure byte-buffer to event parsers for both wire formats — the
//! chat.completions `choices[]` chunk protocol and the Responses API event
//! protocol — plus the inline think-tag filter that reroutes local
//! reasoning-tag blocks into reasoning events. Tool-call argument deltas are
//! emitted as `ToolCallArgumentDelta` events for the `DeltaAccumulator` to
//! reassemble.

use crate::provider::LlmEvent;
use crate::provider::sse_util::{truncate_raw_stream, SseOutcome};
/// Parse one Responses API SSE `data:` payload into [`LlmEvent`]s (Rule 3).
///
/// The Responses API streams events with a `type` field in each `data:`
/// payload. This maps them to the same `LlmEvent` types the chat.completions
/// parser produces, so the accumulator and turn loop work unchanged. The
/// response `id` (from `response.created` / `response.completed`) is captured
/// as [`LlmEvent::ResponseId`] for the next request's `previous_response_id`.
pub(super) fn parse_responses_sse_chunk(json: &serde_json::Value) -> Vec<LlmEvent> {
    let mut events = Vec::new();
    let event_type = json.get("type").and_then(|t| t.as_str()).unwrap_or("");
    match event_type {
        "response.created" => {
            if let Some(id) = json
                .get("response")
                .and_then(|r| r.get("id"))
                .and_then(|i| i.as_str())
            {
                events.push(LlmEvent::ResponseId { id: id.to_string() });
            }
        }
        "response.output_text.delta" => {
            if let Some(text) = json.get("delta").and_then(|d| d.as_str()) {
                events.push(LlmEvent::TextDelta { text: text.to_string() });
            }
        }
        "response.output_item.added" => {
            if let Some(item) = json.get("item") {
                if item.get("type").and_then(|t| t.as_str()) == Some("function_call") {
                    let index = json
                        .get("output_index")
                        .and_then(|i| i.as_u64())
                        .unwrap_or(0) as u32;
                    let id = item
                        .get("call_id")
                        .and_then(|c| c.as_str())
                        .unwrap_or("");
                    let name = item
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    if !id.is_empty() && !name.is_empty() {
                        events.push(LlmEvent::ToolCallStart {
                            index,
                            id: id.to_string(),
                            name: name.to_string(),
                        });
                    }
                }
            }
        }
        "response.function_call_arguments.delta" => {
            let index = json
                .get("output_index")
                .and_then(|i| i.as_u64())
                .unwrap_or(0) as u32;
            if let Some(fragment) = json.get("delta").and_then(|d| d.as_str()) {
                events.push(LlmEvent::ToolCallArgumentDelta {
                    index,
                    fragment: fragment.to_string(),
                });
            }
        }
        "response.completed" => {
            if let Some(id) = json
                .get("response")
                .and_then(|r| r.get("id"))
                .and_then(|i| i.as_str())
            {
                events.push(LlmEvent::ResponseId { id: id.to_string() });
            }
            if let Some(usage) = json.get("response").and_then(|r| r.get("usage")) {
                let prompt = usage
                    .get("input_tokens")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0) as u32;
                let completion = usage
                    .get("output_tokens")
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0) as u32;
                let reasoning = usage
                    .get("output_tokens_details")
                    .and_then(|d| d.get("reasoning_tokens"))
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0) as u32;
                // The Responses API reports prompt tokens served from cache
                // under input_tokens_details.cached_tokens (the counterpart
                // of the chat-completions path's
                // prompt_tokens_details.cached_tokens). Absent → 0.
                let cached = usage
                    .get("input_tokens_details")
                    .and_then(|d| d.get("cached_tokens"))
                    .and_then(|t| t.as_u64())
                    .unwrap_or(0) as u32;
                events.push(LlmEvent::Usage {
                    prompt_tokens: prompt,
                    completion_tokens: completion,
                    reasoning_tokens: reasoning,
                    cached_tokens: cached,
                    ttft_ms: None,
                    generation_ms: None,
                });
            }
            events.push(LlmEvent::Finish {
                reason: crate::provider::FinishReason::Stop,
            });
        }
        "response.failed" => {
            let msg = json
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("Responses API request failed");
            events.push(LlmEvent::Error {
                error: msg.to_string(),
            });
        }
        _ => {}
    }
    events
}

/// Process all complete SSE lines from the Responses API stream (Rule 3).
///
/// Same line-extraction logic as [`parse_sse_buffer`] (extracts `data:` lines,
/// skips `event:` lines and empty lines), but dispatches each JSON payload to
/// [`parse_responses_sse_chunk`] instead of [`parse_sse_chunk`].
pub(super) fn parse_responses_sse_buffer(buffer: &mut String) -> Vec<SseOutcome> {
    let mut outcomes = Vec::new();
    while let Some(pos) = buffer.find('\n') {
        let line = buffer[..pos].trim().to_string();
        buffer.drain(..=pos);
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(data) = line.strip_prefix("data: ") {
            if data.trim() == "[DONE]" {
                continue;
            }
            match serde_json::from_str::<serde_json::Value>(data) {
                Ok(json) => {
                    for event in parse_responses_sse_chunk(&json) {
                        outcomes.push(SseOutcome::Event(event));
                    }
                }
                Err(e) => {
                    outcomes.push(SseOutcome::ParseError(format!(
                        "Failed to parse Responses API SSE chunk: {e}"
                    )));
                }
            }
        }
    }
    outcomes
}

/// Parse a raw SSE JSON chunk into zero or more `LlmEvent`s.
///
/// This handles the full OpenAI-compatible streaming format, including the
/// non-standard `reasoning_content` field emitted by reasoning models like
/// glm-5.2 / DeepSeek-R1.
///
/// Consumes the chunk: each choice's `delta` object is MOVED into the
/// [`LlmEvent::RawAssistantDelta`] event (mem-perf review LOW 6 — no
/// per-chunk clone of the whole delta map). The typed extraction borrows
/// the taken value first; nothing reads the chunk afterward.
pub(super) fn parse_sse_chunk(mut json: serde_json::Value) -> Vec<LlmEvent> {
    let mut events = Vec::new();

    // Usage (arrives in the final chunk when include_usage is true).
    if let Some(usage) = json.get("usage") {
        if !usage.is_null() {
            let prompt = usage
                .get("prompt_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            let completion = usage
                .get("completion_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            // Reasoning models (glm-5.2) report reasoning tokens under
            // completion_tokens_details.reasoning_tokens. Absent → 0.
            let reasoning = usage
                .get("completion_tokens_details")
                .and_then(|d| d.get("reasoning_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            // OpenAI reports prompt tokens served from its cache under
            // prompt_tokens_details.cached_tokens. Absent → 0 (the provider
            // either doesn't cache or didn't report it).
            let cached = usage
                .get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as u32;
            events.push(LlmEvent::Usage {
                prompt_tokens: prompt,
                completion_tokens: completion,
                reasoning_tokens: reasoning,
                cached_tokens: cached,
                // Timing is enriched by the stream loop (it owns the
                // timestamps); parse_sse_chunk has no clock context.
                ttft_ms: None,
                generation_ms: None,
            });
        }
    }

    // Choices.
    if let Some(choices) = json.get_mut("choices").and_then(|c| c.as_array_mut()) {
        for choice in choices {
            // A choice may carry a delta (content/tool-call/reasoning
            // fragments), a finish_reason, or both. Process the delta if
            // present, then the finish_reason — don't skip the choice just
            // because it has no delta (a finish-only chunk has no delta).
            // Move the delta object out of the chunk (mem-perf review
            // LOW 6): the raw-echo event below takes ownership, so the
            // per-chunk clone of the whole delta map is gone. The typed
            // extraction borrows the taken value; the chunk's `delta` slot
            // is left `null` — nothing reads it afterward.
            let delta = choice.get_mut("delta").map(serde_json::Value::take);

            if let Some(delta) = delta.as_ref() {
                // Text content.
                if let Some(text) = delta.get("content").and_then(|c| c.as_str()) {
                    if !text.is_empty() {
                        events.push(LlmEvent::TextDelta {
                            text: text.to_string(),
                        });
                    }
                }

                // Reasoning content (non-standard — reasoning models). The
                // field name varies by provider: DeepSeek-direct / GLM use
                // `reasoning_content`; Ollama's OpenAI-compatible endpoint
                // uses `reasoning` (traces 2026-08-22: deepseek-v4-flash
                // streams `{"content":"","reasoning":"…"}` — before this
                // fallback the whole thinking phase produced zero events).
                // `reasoning_content` wins when both are present.
                let reasoning = delta
                    .get("reasoning_content")
                    .and_then(|c| c.as_str())
                    .filter(|s| !s.is_empty())
                    .or_else(|| {
                        delta
                            .get("reasoning")
                            .and_then(|c| c.as_str())
                            .filter(|s| !s.is_empty())
                    });
                if let Some(reasoning) = reasoning {
                    events.push(LlmEvent::ReasoningDelta {
                        text: reasoning.to_string(),
                    });
                }

                // Tool calls.
                if let Some(tool_calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
                    for tc in tool_calls {
                        let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
                        let id = tc
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let function = tc.get("function");
                        let name = function
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("")
                            .to_string();
                        let args_fragment = function
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                            .unwrap_or("")
                            .to_string();

                        // The first delta for a tool call carries id + name.
                        if !id.is_empty() && !name.is_empty() {
                            events.push(LlmEvent::ToolCallStart {
                                index,
                                id: id.clone(),
                                name: name.clone(),
                            });
                        }
                        // Argument fragments arrive in every delta (including the first).
                        if !args_fragment.is_empty() {
                            events.push(LlmEvent::ToolCallArgumentDelta {
                                index,
                                fragment: args_fragment,
                            });
                        }
                        // Per-tool-call provider metadata (e.g. Gemini
                        // thought_signature) is now captured verbatim via
                        // RawAssistantDelta (the delta object is emitted
                        // unchanged) — no separate ProviderMeta extraction
                        // needed (Rule 1 raw echo supersedes the allowlist).
                    }
                }

                // Message-level provider metadata (e.g. Gemini
                // thought_signature) is now captured verbatim via
                // RawAssistantDelta below — no separate ProviderMeta
                // extraction needed (Rule 1 raw echo supersedes the
                // allowlist).
            }

            // Finish reason (may appear with or without a delta).
            if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
                let fr = match reason {
                    "stop" => crate::provider::FinishReason::Stop,
                    "tool_calls" => crate::provider::FinishReason::ToolCalls,
                    "length" => crate::provider::FinishReason::Length,
                    "content_filter" => crate::provider::FinishReason::ContentFilter,
                    other => crate::provider::FinishReason::Other(other.to_string()),
                };
                events.push(LlmEvent::Finish { reason: fr });
            }

            // The verbatim delta object — raw source of truth for replay
            // (Rule 1). Emitted LAST so the typed events above (the derived UI
            // view) keep stable indices; the accumulator merges this into the
            // final assistant message, preserving every key the provider sent
            // (field names, `reasoning` vs `reasoning_content`,
            // `thought_signature`, unknown keys) untouched. Never parsed,
            // inspected, truncated, or regenerated.
            if let Some(d) = delta {
                if d.is_object() {
                    events.push(LlmEvent::RawAssistantDelta { delta: d });
                }
            }
        }
    }

    events
}

/// The open/close tags of an inline reasoning block.
const THINK_OPEN_TAG: &str = "<think>";
const THINK_CLOSE_TAG: &str = "</think>";

/// State of the [`ThinkTagFilter`] state machine.
#[derive(Debug, PartialEq, Eq)]
enum ThinkState {
    /// No content seen yet — deciding whether the stream opens with `<think>`.
    Start,
    /// Inside a think block — routing text to `ReasoningDelta`.
    InThink,
    /// Past any think block — everything passes through as answer text.
    Passthrough,
}

/// Streaming filter for inline `<think>…</think>` reasoning blocks.
///
/// Some providers (local Ollama builds, LM Studio) stream a thinking model's
/// reasoning inside `delta.content` wrapped in literal `<think>` tags instead
/// of a separate reasoning field. The tags can split across chunk boundaries.
/// This filter reroutes that text to [`LlmEvent::ReasoningDelta`] so the
/// thinking is displayed (and echoed back on the next request) instead of
/// leaking raw tags into the visible answer.
///
/// Position-0 rule: the filter only engages when the stream's FIRST content
/// begins with optional whitespace + `<think>` — exactly how R1-style models
/// emit it. A literal `<think>` later in the answer passes through as text,
/// so a non-thinking model that writes the word never loses content.
pub(super) struct ThinkTagFilter {
    state: ThinkState,
    /// Undecided prefix (Start) or a possible partial `</think>` suffix
    /// (InThink). Never more than a tag's length plus one delta.
    buf: String,
}

/// Push a `ReasoningDelta` onto `out`, skipping empty text (mirrors the
/// parser's no-empty-deltas invariant).
fn push_reasoning_event(out: &mut Vec<LlmEvent>, text: String) {
    if !text.is_empty() {
        out.push(LlmEvent::ReasoningDelta { text });
    }
}

/// Push a `TextDelta` onto `out`, skipping empty text.
fn push_text_event(out: &mut Vec<LlmEvent>, text: String) {
    if !text.is_empty() {
        out.push(LlmEvent::TextDelta { text });
    }
}

/// The longest suffix of `s` (in bytes) that is a strict prefix of `tag` —
/// i.e. a tag fragment that might complete in the next chunk and must be
/// held back. `tag` is ASCII, so byte slicing is boundary-safe.
fn partial_tag_suffix(s: &str, tag: &str) -> usize {
    let max = s.len().min(tag.len() - 1);
    (1..=max)
        .rev()
        .find(|&k| s.ends_with(&tag[..k]))
        .unwrap_or(0)
}

impl ThinkTagFilter {
    /// Create a filter in the start state (no content seen yet).
    pub(super) fn new() -> Self {
        Self {
            state: ThinkState::Start,
            buf: String::new(),
        }
    }

    /// Feed one content fragment; returns the events to emit (0–2: reasoning,
    /// text, or one of each when a close tag lands mid-fragment).
    pub(super) fn feed(&mut self, text: &str) -> Vec<LlmEvent> {
        let mut out = Vec::new();
        self.buf.push_str(text);
        loop {
            match self.state {
                ThinkState::Start => {
                    let trimmed = self.buf.trim_start().to_string();
                    if let Some(rest) = trimmed.strip_prefix(THINK_OPEN_TAG) {
                        // The stream opens with a think block.
                        self.state = ThinkState::InThink;
                        self.buf = rest.to_string();
                        continue; // process the remainder as reasoning
                    }
                    if THINK_OPEN_TAG.starts_with(&trimmed) {
                        // Undecided: whitespace + a strict prefix of the open
                        // tag (or nothing yet). Wait for more content.
                        return out;
                    }
                    // Not a think block — pass everything through.
                    self.state = ThinkState::Passthrough;
                    let text = std::mem::take(&mut self.buf);
                    push_text_event(&mut out, text);
                    return out;
                }
                ThinkState::InThink => {
                    if let Some(pos) = self.buf.find(THINK_CLOSE_TAG) {
                        let reasoning = self.buf[..pos].to_string();
                        let rest = self.buf[pos + THINK_CLOSE_TAG.len()..].to_string();
                        self.buf.clear();
                        self.state = ThinkState::Passthrough;
                        push_reasoning_event(&mut out, reasoning);
                        push_text_event(&mut out, rest);
                        return out;
                    }
                    // Hold back the longest suffix that could be a partial
                    // close tag; emit the rest as reasoning now.
                    let hold = partial_tag_suffix(&self.buf, THINK_CLOSE_TAG);
                    if hold == self.buf.len() {
                        return out; // nothing safe to emit yet
                    }
                    let reasoning = self.buf[..self.buf.len() - hold].to_string();
                    self.buf = self.buf[self.buf.len() - hold..].to_string();
                    push_reasoning_event(&mut out, reasoning);
                    return out;
                }
                ThinkState::Passthrough => {
                    let text = std::mem::take(&mut self.buf);
                    push_text_event(&mut out, text);
                    return out;
                }
            }
        }
    }

    /// Flush at stream end. An unclosed think block flushes as reasoning (the
    /// model thought and never answered); an undecided prefix flushes as
    /// answer text (it was never a think block).
    pub(super) fn finish(&mut self) -> Vec<LlmEvent> {
        let mut out = Vec::new();
        let rest = std::mem::take(&mut self.buf);
        match self.state {
            ThinkState::InThink => push_reasoning_event(&mut out, rest),
            _ => push_text_event(&mut out, rest),
        }
        out
    }
}

/// Process all complete SSE lines currently in `buffer`, draining each as it's
/// consumed and returning the parsed outcomes. Incomplete trailing data (no
/// trailing newline) is left in the buffer for the next chunk.
///
/// Uses `String::drain()` to drop processed bytes in-place — O(1) amortized
/// per line instead of the O(n²) re-copy of `buffer = buffer[pos+1..].to_string()`.
pub(super) fn parse_sse_buffer(buffer: &mut String) -> Vec<SseOutcome> {
    let mut outcomes = Vec::new();
    while let Some(pos) = buffer.find('\n') {
        // Extract the line up to (but not including) the newline, then drop
        // the processed bytes (line + newline) in-place via drain().
        let line = buffer[..pos].trim().to_string();
        buffer.drain(..=pos);

        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(data) = line.strip_prefix("data: ") {
            if data.trim() == "[DONE]" {
                continue;
            }
            match serde_json::from_str::<serde_json::Value>(data) {
                Ok(json) => {
                    for event in parse_sse_chunk(json) {
                        outcomes.push(SseOutcome::Event(event));
                    }
                }
                Err(e) => {
                    // Include the raw data so the failure is debuggable.
                    outcomes.push(SseOutcome::ParseError(format!(
                        "failed to parse SSE chunk: {e}\n\
                         raw stream data:\n{}",
                        truncate_raw_stream(data, 2000)
                    )));
                }
            }
        }
    }
    outcomes
}
