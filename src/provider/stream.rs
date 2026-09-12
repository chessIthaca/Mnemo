// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Delta accumulation — reassembles fragmented tool-call argument deltas and
//! opaque provider-metadata scopes.
//!
//! Tool-call argument deltas arrive in fragments, correlated by `index`. We
//! accumulate them in a `HashMap<u32, ToolCallAccumulator>` and produce complete
//! `ToolCall`s when the stream finishes.
//!
//! Provider metadata (`LlmEvent::ProviderMeta`) arrives split across chunks on
//! two scopes — per-tool-call (`tool_call_index` of `Some(i)`) or message-level
//! (`None`) — and accumulates like arguments do: repeated keys concatenate
//! their string values in arrival order via [`merge_provider_metadata`].
//! [`DeltaAccumulator::finalize`] attaches each per-index bag onto its packaged
//! [`ToolCall`] (`provider_meta`), and [`DeltaAccumulator::take_message_provider_meta`]
//! yields the message-level bag for attachment onto the final assistant Message.

use std::collections::HashMap;

use crate::provider::{LlmEvent, ToolCall};

/// Accumulates the fragments of a single tool call across stream deltas.
#[derive(Debug, Clone, Default)]
pub struct ToolCallAccumulator {
    /// The tool-call id (arrives in the first delta for this index).
    pub id: Option<String>,
    /// The function name (arrives in the first delta for this index).
    pub name: Option<String>,
    /// The accumulated argument fragments, in order.
    pub arguments: String,
}

impl ToolCallAccumulator {
    /// Whether this accumulator has seen any content.
    pub fn is_started(&self) -> bool {
        self.id.is_some() || self.name.is_some() || !self.arguments.is_empty()
    }

    /// Finalize into a `ToolCall`. Returns `None` if the accumulator is empty.
    pub fn finalize(self) -> Option<ToolCall> {
        if !self.is_started() {
            return None;
        }
        Some(ToolCall::new(self.id.unwrap_or_default(), self.name.unwrap_or_default(), self.arguments))
    }
}

/// Accumulates tool-call deltas across a stream, keyed by `index`.
#[derive(Debug, Clone, Default)]
pub struct DeltaAccumulator {
    calls: HashMap<u32, ToolCallAccumulator>,
    /// Metadata bags captured from scoped `ProviderMeta` events, keyed by
    /// tool-call stream index. Keys repeated across chunks merge by
    /// concatenating string values (same arrival-order rule as arguments).
    call_metas: HashMap<u32, serde_json::Map<String, serde_json::Value>>,
    /// The message-level metadata bag captured from scope-less `ProviderMeta`
    /// events (e.g. Gemini's assistant-level thought_signature), merged
    /// identically.
    message_meta: Option<serde_json::Map<String, serde_json::Value>>,
    /// Whether any tool-call delta was seen this turn. Tool-call detection is
    /// by *presence of tool-call deltas*, not solely by `finish_reason`.
    saw_tool_calls: bool,
    /// The verbatim assistant message object, reassembled from
    /// [`LlmEvent::RawAssistantDelta`] chunks (Rule 1 raw). The request builder
    /// echoes this unchanged instead of reconstructing from view fields, so
    /// every key the provider sent round-trips byte-identical.
    raw: Option<serde_json::Map<String, serde_json::Value>>,
    /// The server-assigned response id (Rule 3 stateful path). Captured from
    /// [`LlmEvent::ResponseId`] (emitted by the Responses API parser). The
    /// turn-loop packaging sites store it on the assistant `Message` so the
    /// next request can send `previous_response_id`.
    response_id: Option<String>,
}

impl DeltaAccumulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed an `LlmEvent` into the accumulator.
    pub fn feed(&mut self, event: &LlmEvent) {
        match event {
            LlmEvent::ToolCallStart { index, id, name } => {
                self.saw_tool_calls = true;
                let acc = self.calls.entry(*index).or_default();
                acc.id = Some(id.clone());
                acc.name = Some(name.clone());
            }
            LlmEvent::ToolCallArgumentDelta { index, fragment } => {
                self.saw_tool_calls = true;
                let acc = self.calls.entry(*index).or_default();
                acc.arguments.push_str(fragment);
            }
            LlmEvent::ProviderMeta { tool_call_index, meta } => {
                // Metadata accumulates per scope like argument fragments: a key
                // repeated across chunks concatenates its string values in
                // arrival order (Google streams thought signatures as their own
                // delta near stream end).
                match tool_call_index {
                    Some(index) => {
                        let bag = self.call_metas.entry(*index).or_default();
                        merge_provider_metadata(bag, meta);
                    }
                    None => {
                        let bag = self.message_meta.get_or_insert_with(serde_json::Map::new);
                        merge_provider_metadata(bag, meta);
                    }
                }
            }
            LlmEvent::RawAssistantDelta { delta } => {
                // The verbatim delta object — raw source of truth for replay
                // (Rule 1). Merged into the final assistant message object,
                // preserving every key the provider sent untouched.
                if let Some(obj) = delta.as_object() {
                    let raw = self.raw.get_or_insert_with(serde_json::Map::new);
                    merge_delta_into_raw(raw, obj);
                }
            }
            LlmEvent::ResponseId { id } => {
                // The server-assigned response id (Rule 3 stateful path).
                // Captured for the next request's `previous_response_id`.
                self.response_id = Some(id.clone());
            }
            _ => {}
        }
    }

    /// Whether any tool-call delta was seen this turn.
    pub fn saw_tool_calls(&self) -> bool {
        self.saw_tool_calls
    }

    /// Finalize all accumulated tool calls into a vector sorted by index.
    ///
    /// Each packaged call carries the metadata bag captured for its stream
    /// index on `provider_meta` (verbatim provider metadata such as Gemini
    /// thought signatures); calls with no captured metadata keep
    /// `provider_meta: None`. The message-level bag is consumed separately via
    /// [`take_message_provider_meta`](Self::take_message_provider_meta) and
    /// attached to the final assistant Message.
    pub fn finalize(mut self) -> Vec<ToolCall> {
        let mut entries: Vec<(u32, ToolCallAccumulator)> = self.calls.into_iter().collect();
        entries.sort_by_key(|(index, _)| *index);
        let mut calls = Vec::with_capacity(entries.len());
        for (index, acc) in entries {
            if let Some(mut call) = acc.finalize() {
                if let Some(meta) = self.call_metas.remove(&index) {
                    call.provider_meta = Some(meta);
                }
                calls.push(call);
            }
        }
        calls
    }

    /// Take the accumulated message-level metadata bag for attachment onto the
    /// final assistant Message (`Message::provider_meta`). Returns `None` when
    /// no scope-less `ProviderMeta` event was seen this turn.
    pub fn take_message_provider_meta(
        &mut self,
    ) -> Option<serde_json::Map<String, serde_json::Value>> {
        self.message_meta.take()
    }

    /// Take the assembled verbatim assistant message object (Rule 1 raw).
    ///
    /// The request builder echoes this unchanged instead of reconstructing from
    /// view fields, so every key the provider sent (reasoning fields,
    /// signatures, unknown keys) round-trips byte-identical. Returns `None`
    /// when no [`LlmEvent::RawAssistantDelta`] was seen this turn (a synthetic
    /// message, or a provider whose parser does not yet emit raw deltas).
    pub fn take_raw(&mut self) -> Option<serde_json::Value> {
        self.raw.take().map(serde_json::Value::Object)
    }

    /// Take the server-assigned response id (Rule 3 stateful path). Captured
    /// from [`LlmEvent::ResponseId`] (emitted by the Responses API parser).
    /// The turn-loop packaging sites store it on the assistant `Message` so
    /// the next request can send `previous_response_id`.
    pub fn take_response_id(&mut self) -> Option<String> {
        self.response_id.take()
    }
}

/// Merge one metadata chunk into an accumulating bag.
///
/// Provider metadata can arrive split across multiple chunks (Google streams
/// thought signatures as their own delta near stream end), so when a key is
/// already present and both old and new values are strings they concatenate in
/// arrival order — identical to passthrough delivery of an unsplit value. Any
/// other combination replaces the stored value with the later one; Google emits
/// signatures as strings, so this only guards pathological inputs. Values are
/// otherwise copied untouched — no parsing, no normalization.
fn merge_provider_metadata(
    dst: &mut serde_json::Map<String, serde_json::Value>,
    src: &serde_json::Map<String, serde_json::Value>,
) {
    for (key, value) in src {
        match (dst.get_mut(key), value.as_str()) {
            (Some(serde_json::Value::String(existing)), Some(fragment)) => {
                existing.push_str(fragment);
            }
            _ => {
                dst.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Merge a streamed `choices[0].delta` object into the accumulating raw
/// assistant message (Rule 1).
///
/// Streaming deltas are fragments of the final non-streaming message object.
/// This reassembles them faithfully. Every key is either an *identity* key
/// (arrives whole; see [`merge_identity`]) or a *fragment* key (streamed in
/// pieces; see [`merge_scalar`]):
/// - `tool_calls` (array): entries merge by `index`; within an entry,
///   `function.arguments` concatenates while `id`/`type`/`function.name` are
///   identity keys set once, and the `index` key itself is dropped (it is
///   array-position only, absent from the non-streaming shape). Any other
///   per-call key merges as a fragment, so unknown keys still round-trip.
/// - `role` is an identity key.
/// - every other key (`content`, `reasoning_content`, `reasoning`,
///   `thought_signature`, unknown keys): string values concatenate in arrival
///   order (a key repeated across chunks merges like `thought_signature`);
///   null and empty-string placeholders are skipped so the first delta's
///   `content: null`/`""` does not pollute the message; any other type replaces
///   with the later value.
///
/// This preserves unknown keys and exact field names verbatim — the entire
/// point of Rule 1.
///
/// The identity/fragment split matters because some OpenAI-compatible
/// providers (GLM via litellm, for instance) repeat the *whole* tool-call
/// object on every chunk rather than sending it once. Concatenating those
/// repeats corrupted `type` into `functionfunction...` and `id` into
/// `call_xcall_x...`, which the raw-echo path then sent verbatim — a hard
/// HTTP 400 (`Input should be 'function'`). The Anthropic accumulator has
/// always used this allowlist shape.
fn merge_delta_into_raw(
    raw: &mut serde_json::Map<String, serde_json::Value>,
    delta: &serde_json::Map<String, serde_json::Value>,
) {
    for (key, value) in delta {
        if key == "tool_calls" {
            if let Some(arr) = value.as_array() {
                let raw_arr = raw
                    .entry("tool_calls".to_string())
                    .or_insert_with(|| serde_json::Value::Array(Vec::new()));
                if let Some(raw_arr) = raw_arr.as_array_mut() {
                    for tc in arr {
                        if let Some(tc_obj) = tc.as_object() {
                            let index = tc_obj
                                .get("index")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(0) as usize;
                            while raw_arr.len() <= index {
                                raw_arr.push(serde_json::Value::Object(
                                    serde_json::Map::new(),
                                ));
                            }
                            if let Some(entry) = raw_arr[index].as_object_mut() {
                                merge_tool_call_delta(entry, tc_obj);
                            }
                        }
                    }
                }
            }
            continue;
        }
        if MESSAGE_IDENTITY_KEYS.contains(&key.as_str()) {
            merge_identity(raw, key, value);
        } else {
            merge_scalar(raw, key, value);
        }
    }
}

/// Merge one tool-call delta entry into its accumulating raw entry.
///
/// `index` is dropped (array-position only, absent from the non-streaming
/// shape). `id` and `type` are identity keys ([`TOOL_CALL_IDENTITY_KEYS`]);
/// inside `function`, `name` is an identity key and `arguments` is the one
/// field streamed as fragments. Any other key merges as a fragment so unknown
/// per-call keys still round-trip (Rule 1).
fn merge_tool_call_delta(
    entry: &mut serde_json::Map<String, serde_json::Value>,
    tc: &serde_json::Map<String, serde_json::Value>,
) {
    for (key, value) in tc {
        if key == "index" {
            continue;
        }
        if key == "function" {
            if let Some(func) = value.as_object() {
                let raw_func = entry
                    .entry("function".to_string())
                    .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                if let Some(raw_func) = raw_func.as_object_mut() {
                    for (fk, fv) in func {
                        if FUNCTION_IDENTITY_KEYS.contains(&fk.as_str()) {
                            merge_identity(raw_func, fk, fv);
                        } else {
                            merge_scalar(raw_func, fk, fv);
                        }
                    }
                }
            }
            continue;
        }
        if TOOL_CALL_IDENTITY_KEYS.contains(&key.as_str()) {
            merge_identity(entry, key, value);
        } else {
            merge_scalar(entry, key, value);
        }
    }
}

/// Per-tool-call keys that identify the call rather than carry generated
/// content. They arrive whole, so a repeat across chunks is the same value
/// again and must not concatenate.
const TOOL_CALL_IDENTITY_KEYS: [&str; 2] = ["id", "type"];

/// Identity keys inside a tool call's `function` object. `arguments` is
/// deliberately absent — it is the one field genuinely streamed as fragments.
const FUNCTION_IDENTITY_KEYS: [&str; 1] = ["name"];

/// Top-level identity keys on the assistant message. `content`,
/// `reasoning_content` and `reasoning` are deliberately absent — they are
/// streamed as fragments and must concatenate.
const MESSAGE_IDENTITY_KEYS: [&str; 1] = ["role"];

/// Set an identity key that must never concatenate (`role`, a tool call's
/// `id`/`type`, `function.name`).
///
/// The first non-empty value wins; later repeats are ignored. First-wins
/// rather than last-wins is deliberate: it is immune to a truncated or partial
/// repeat late in the stream, and it matches the structured accumulator path,
/// where a tool-call start is only seen once per index. `null` and
/// empty-string placeholders are skipped, exactly as in [`merge_scalar`].
fn merge_identity(
    dst: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: &serde_json::Value,
) {
    if value.is_null() {
        return;
    }
    if matches!(value, serde_json::Value::String(s) if s.is_empty()) {
        return;
    }
    dst.entry(key.to_string()).or_insert_with(|| value.clone());
}

/// Merge a *fragment* key (one genuinely streamed in pieces: `content`,
/// `reasoning_content`, `function.arguments`, unknown keys) into a raw object.
///
/// Identity keys must go through [`merge_identity`] instead — concatenating
/// those corrupts them.
///
/// Repeated string fragments concatenate in arrival order; null and
/// empty-string values are skipped (first-delta placeholders like
/// `content: null`); anything else replaces the stored value with the later
/// one.
fn merge_scalar(
    dst: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: &serde_json::Value,
) {
    if value.is_null() {
        return;
    }
    if let serde_json::Value::String(s) = value {
        if s.is_empty() {
            return;
        }
        if let Some(serde_json::Value::String(existing)) = dst.get_mut(key) {
            existing.push_str(s);
            return;
        }
    }
    dst.insert(key.to_string(), value.clone());
}

/// The maximum loop period (bytes) used by the R10 repetition guard. The
/// guard fires when the accumulated response text ends with a repeating
/// unit of ANY byte length ≤ this window, spanning at least
/// [`REPETITION_WINDOW`] × [`REPETITION_THRESHOLD`] bytes (i.e. the old
/// exact-window check, plus shorter units with proportionally more
/// repetitions).
pub const REPETITION_WINDOW: usize = 200;

/// How many consecutive window-spans of repetition trigger an abort. 3 =
/// the tail is periodic over [`REPETITION_WINDOW`] × 3 bytes — the same
/// ~200-byte block 3× in a row, or a shorter unit proportionally more
/// times. High enough to avoid false positives on legitimate repeated
/// content (code patterns, JSON keys), low enough to catch stuck loops
/// within ~600 bytes of degenerate output.
///
/// **Known limitation:** legitimate content with a 600+ byte run of ANY
/// repeating unit ≤ 200 bytes (e.g. a long markdown horizontal rule, wide
/// table separators, ASCII-art dividers, or ~20+ identical consecutive short
/// lines / data records) would be aborted mid-generation. This is an accepted
/// tradeoff — the alternative (allowing unbounded repetition) wastes far more
/// tokens. A future refinement could allowlist high-repetition patterns.
pub const REPETITION_THRESHOLD: usize = 3;

/// Soft cap on the accumulated `response_text` buffer used by the R10
/// repetition guard. [`detect_repetition`] only inspects the last
/// `REPETITION_WINDOW × REPETITION_THRESHOLD` bytes (the tail), so once the
/// buffer exceeds this cap everything before that tail is dropped
/// (char-boundary aligned). This keeps the buffer O(1) (~2 KB) instead of
/// growing with the full response length — semantically safe because the
/// guard is purely suffix-based.
pub const REPETITION_BUFFER_CAP: usize = 2048;

/// Bound the accumulated `response_text` buffer to [`REPETITION_BUFFER_CAP`]
/// bytes, keeping only the suffix that [`detect_repetition`] inspects.
///
/// [`detect_repetition`] only inspects the last `needed` bytes (the suffix), so
/// once `text` exceeds `cap`, everything before the last `needed` bytes is
/// dropped (backing up to a UTF-8 char boundary). This keeps the buffer O(1)
/// (~`cap` bytes) instead of growing with the full response length, with no
/// effect on detection — the truncated tail is exactly what the next
/// `detect_repetition` call examines. When `text` is at or below `cap`, it is
/// returned unchanged (no allocation).
pub fn bound_repetition_buffer(text: String, needed: usize, cap: usize) -> String {
    if text.len() <= cap || needed == 0 {
        return text;
    }
    let mut start = text.len() - needed.min(text.len());
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    text[start..].to_string()
}

/// Detect whether the accumulated response text ends with a repeating loop
/// of ANY byte period ≤ `window`, spanning at least `window × threshold`
/// bytes — i.e. the same `window`-byte segment `threshold` times in a row,
/// or a shorter unit proportionally more times.
///
/// This is the core of the R10 repetition guard: when a model gets stuck in a
/// loop (e.g. repeating "Let me commit the uncommitted work…" dozens of times),
/// the stream is aborted to prevent token waste. The check is suffix-based — it
/// only looks at the tail of `text`, so it is O(window × threshold) per call,
/// not O(text length).
///
/// The scan is byte-wise over every candidate period 1..=window: the original
/// implementation only tested a period of exactly `window`, which is
/// structurally blind to loops whose unit length does not divide the window —
/// the 2027-01-11 deepseek exit-note loop (74-byte unit, 200-byte window)
/// repeated 366× without ever firing. Byte indexing does not slice, so no
/// char-boundary checks are needed and multi-byte UTF-8 content is safe.
///
/// Returns `false` when `text` is shorter than `window × threshold` (not enough
/// material to repeat).
pub fn detect_repetition(text: &str, window: usize, threshold: usize) -> bool {
    let needed = window * threshold;
    if needed == 0 || text.len() < needed {
        return false;
    }
    // The tail is a degenerate loop iff the last `needed` bytes are p-periodic
    // for some period p ≤ window (the loop's unit length): tail[i] == tail[i-p]
    // for every i ≥ p. Each candidate period early-exits on the first
    // mismatch, so prose stays cheap.
    let tail = &text.as_bytes()[text.len() - needed..];
    (1..=window).any(|p| tail[p..].iter().zip(tail).all(|(a, b)| a == b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accumulates_single_tool_call() {
        let mut acc = DeltaAccumulator::new();
        acc.feed(&LlmEvent::ToolCallStart {
            index: 0,
            id: "call_1".into(),
            name: "multiply".into(),
        });
        // Arguments arrive in fragments.
        for frag in ["{", "\"a\": 7", ", \"b\": ", "13", "}"] {
            acc.feed(&LlmEvent::ToolCallArgumentDelta {
                index: 0,
                fragment: frag.into(),
            });
        }
        assert!(acc.saw_tool_calls());

        let calls = acc.finalize();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "multiply");
        assert_eq!(calls[0].arguments, "{\"a\": 7, \"b\": 13}");
        let parsed: serde_json::Value = serde_json::from_str(&calls[0].arguments).unwrap();
        assert_eq!(parsed["a"], 7);
        assert_eq!(parsed["b"], 13);
    }

    #[test]
    fn accumulates_multiple_tool_calls_by_index() {
        let mut acc = DeltaAccumulator::new();
        acc.feed(&LlmEvent::ToolCallStart {
            index: 0,
            id: "call_a".into(),
            name: "read".into(),
        });
        acc.feed(&LlmEvent::ToolCallStart {
            index: 1,
            id: "call_b".into(),
            name: "write".into(),
        });
        // Interleaved argument fragments.
        acc.feed(&LlmEvent::ToolCallArgumentDelta {
            index: 0,
            fragment: "{\"path\":".into(),
        });
        acc.feed(&LlmEvent::ToolCallArgumentDelta {
            index: 1,
            fragment: "{\"path\":".into(),
        });
        acc.feed(&LlmEvent::ToolCallArgumentDelta {
            index: 0,
            fragment: "\"a.rs\"}".into(),
        });
        acc.feed(&LlmEvent::ToolCallArgumentDelta {
            index: 1,
            fragment: "\"b.rs\"}".into(),
        });

        let calls = acc.finalize();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "read");
        assert_eq!(calls[0].arguments, "{\"path\":\"a.rs\"}");
        assert_eq!(calls[1].name, "write");
        assert_eq!(calls[1].arguments, "{\"path\":\"b.rs\"}");
    }

    #[test]
    fn detects_tool_calls_by_delta_presence_not_finish_reason() {
        let mut acc = DeltaAccumulator::new();
        // Only an argument delta, no start, no finish_reason.
        acc.feed(&LlmEvent::ToolCallArgumentDelta {
            index: 0,
            fragment: "{}".into(),
        });
        assert!(acc.saw_tool_calls());
    }

    #[test]
    fn no_tool_calls_when_only_text() {
        let mut acc = DeltaAccumulator::new();
        acc.feed(&LlmEvent::TextDelta {
            text: "hello".into(),
        });
        acc.feed(&LlmEvent::Finish {
            reason: crate::provider::FinishReason::Stop,
        });
        assert!(!acc.saw_tool_calls());
        assert!(acc.finalize().is_empty());
    }

    #[test]
    fn empty_accumulator_finalizes_empty() {
        let acc = DeltaAccumulator::new();
        assert!(!acc.saw_tool_calls());
        assert!(acc.finalize().is_empty());
    }

    #[test]
    fn arguments_without_start_still_accumulate() {
        // Some local models emit argument deltas without a preceding start.
        let mut acc = DeltaAccumulator::new();
        acc.feed(&LlmEvent::ToolCallArgumentDelta {
            index: 0,
            fragment: "{\"x\":1}".into(),
        });
        let calls = acc.finalize();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].arguments, "{\"x\":1}");
        // id and name default to empty.
        assert_eq!(calls[0].id, "");
        assert_eq!(calls[0].name, "");
    }

    #[test]
    fn accumulator_captures_message_level_provider_meta() {
        let mut acc = DeltaAccumulator::new();
        let mut meta = serde_json::Map::new();
        meta.insert(
            "thought_signature".to_string(),
            serde_json::json!("sig_abc"),
        );
        acc.feed(&LlmEvent::ProviderMeta {
            tool_call_index: None,
            meta,
        });
        let bag = acc.take_message_provider_meta();
        assert!(bag.is_some(), "message-level meta should be captured");
        assert_eq!(
            bag.unwrap().get("thought_signature"),
            Some(&serde_json::json!("sig_abc")),
        );
    }

    #[test]
    fn accumulator_attaches_per_call_provider_meta_on_finalize() {
        let mut acc = DeltaAccumulator::new();
        acc.feed(&LlmEvent::ToolCallStart {
            index: 0,
            id: "call_1".into(),
            name: "read".into(),
        });
        acc.feed(&LlmEvent::ToolCallArgumentDelta {
            index: 0,
            fragment: "{}".into(),
        });
        let mut meta = serde_json::Map::new();
        meta.insert(
            "thought_signature".to_string(),
            serde_json::json!("sig_xyz"),
        );
        acc.feed(&LlmEvent::ProviderMeta {
            tool_call_index: Some(0),
            meta,
        });
        let calls = acc.finalize();
        assert_eq!(calls.len(), 1);
        let pm = calls[0].provider_meta.as_ref().expect("meta on tool call");
        assert_eq!(
            pm.get("thought_signature"),
            Some(&serde_json::json!("sig_xyz")),
        );
    }

    #[test]
    fn accumulator_merges_repeated_signature_fragments() {
        let mut acc = DeltaAccumulator::new();
        let mut m1 = serde_json::Map::new();
        m1.insert(
            "thought_signature".to_string(),
            serde_json::json!("part1"),
        );
        acc.feed(&LlmEvent::ProviderMeta {
            tool_call_index: None,
            meta: m1,
        });
        let mut m2 = serde_json::Map::new();
        m2.insert(
            "thought_signature".to_string(),
            serde_json::json!("part2"),
        );
        acc.feed(&LlmEvent::ProviderMeta {
            tool_call_index: None,
            meta: m2,
        });
        let bag = acc.take_message_provider_meta().unwrap();
        assert_eq!(
            bag.get("thought_signature"),
            Some(&serde_json::json!("part1part2")),
        );
    }

    #[test]
    fn accumulator_no_meta_returns_none() {
        let mut acc = DeltaAccumulator::new();
        acc.feed(&LlmEvent::TextDelta { text: "hi".into() });
        assert!(acc.take_message_provider_meta().is_none());
    }

    #[test]
    fn raw_assembles_text_reasoning_tool_call_and_unknown_keys() {
        let mut acc = DeltaAccumulator::new();
        acc.feed(&LlmEvent::RawAssistantDelta {
            delta: serde_json::json!({
                "role": "assistant",
                "content": "Hello",
                "reasoning_content": "thin",
                "thought_signature": "sig",
                "custom_unknown": "xyz",
            }),
        });
        acc.feed(&LlmEvent::RawAssistantDelta {
            delta: serde_json::json!({
                "content": ", world",
                "reasoning_content": "king",
                "tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "read", "arguments": "{\"a\":" }
                }]
            }),
        });
        acc.feed(&LlmEvent::RawAssistantDelta {
            delta: serde_json::json!({
                "tool_calls": [{ "index": 0, "function": { "arguments": "1}" } }]
            }),
        });
        let raw = acc.take_raw().expect("raw should be assembled");
        assert_eq!(
            raw,
            serde_json::json!({
                "role": "assistant",
                "content": "Hello, world",
                "reasoning_content": "thinking",
                "thought_signature": "sig",
                "custom_unknown": "xyz",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "read", "arguments": "{\"a\":1}" }
                }]
            })
        );
    }

    #[test]
    fn raw_preserves_reasoning_field_name() {
        // The provider sent `reasoning` (Ollama / vLLM 0.20+), not
        // `reasoning_content` — the field name must round-trip verbatim (Rule 1).
        let mut acc = DeltaAccumulator::new();
        acc.feed(&LlmEvent::RawAssistantDelta {
            delta: serde_json::json!({ "reasoning": "a thought" }),
        });
        acc.feed(&LlmEvent::RawAssistantDelta {
            delta: serde_json::json!({ "reasoning": " continued" }),
        });
        let raw = acc.take_raw().unwrap();
        assert_eq!(raw["reasoning"], serde_json::json!("a thought continued"));
        assert!(raw.get("reasoning_content").is_none());
    }

    #[test]
    fn raw_skips_null_and_empty_placeholders() {
        let mut acc = DeltaAccumulator::new();
        acc.feed(&LlmEvent::RawAssistantDelta {
            delta: serde_json::json!({ "role": "assistant", "content": null }),
        });
        acc.feed(&LlmEvent::RawAssistantDelta {
            delta: serde_json::json!({ "content": "" }),
        });
        acc.feed(&LlmEvent::RawAssistantDelta {
            delta: serde_json::json!({ "content": "hi" }),
        });
        let raw = acc.take_raw().unwrap();
        assert_eq!(raw["content"], serde_json::json!("hi"));
        assert_eq!(raw["role"], serde_json::json!("assistant"));
    }

    #[test]
    fn raw_assembles_multiple_tool_calls_by_index_and_drops_index() {
        let mut acc = DeltaAccumulator::new();
        acc.feed(&LlmEvent::RawAssistantDelta {
            delta: serde_json::json!({
                "tool_calls": [
                    { "index": 0, "id": "a", "type": "function", "function": { "name": "read", "arguments": "{" } },
                    { "index": 1, "id": "b", "type": "function", "function": { "name": "write", "arguments": "{" } }
                ]
            }),
        });
        acc.feed(&LlmEvent::RawAssistantDelta {
            delta: serde_json::json!({
                "tool_calls": [
                    { "index": 0, "function": { "arguments": "}" } },
                    { "index": 1, "function": { "arguments": "}" } }
                ]
            }),
        });
        let raw = acc.take_raw().unwrap();
        let tcs = raw["tool_calls"].as_array().unwrap();
        assert_eq!(tcs.len(), 2);
        assert_eq!(tcs[0]["function"]["arguments"], "{}");
        assert_eq!(tcs[1]["function"]["arguments"], "{}");
        assert_eq!(tcs[0]["function"]["name"], "read");
        assert_eq!(tcs[1]["function"]["name"], "write");
        // `index` is array-position only — absent from the non-streaming shape.
        assert!(tcs[0].get("index").is_none());
        assert!(tcs[1].get("index").is_none());
    }

    #[test]
    fn raw_repeated_tool_call_scalars_do_not_concatenate() {
        // Some OpenAI-compatible providers (GLM via litellm) repeat the WHOLE
        // tool-call object on every chunk instead of sending it once. Only
        // `function.arguments` may accumulate: appending the identity fields
        // corrupted `type` into "functionfunction..." and `id` into
        // "call_1call_1...", which the raw-echo path then sent verbatim — a
        // hard HTTP 400 (`Input should be 'function'`).
        let mut acc = DeltaAccumulator::new();
        for fragment in ["{\"path\":", "\"a.txt\"", "}"] {
            acc.feed(&LlmEvent::RawAssistantDelta {
                delta: serde_json::json!({
                    "role": "assistant",
                    "tool_calls": [{
                        "index": 0,
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "read", "arguments": fragment }
                    }]
                }),
            });
        }
        let raw = acc.take_raw().expect("raw should be assembled");
        let tc = &raw["tool_calls"][0];
        assert_eq!(
            tc["type"],
            serde_json::json!("function"),
            "type is identity"
        );
        assert_eq!(tc["id"], serde_json::json!("call_1"), "id is identity");
        assert_eq!(
            tc["function"]["name"],
            serde_json::json!("read"),
            "function.name is identity"
        );
        // ...while arguments, the one genuine fragment field, still joins up.
        assert_eq!(
            tc["function"]["arguments"],
            serde_json::json!("{\"path\":\"a.txt\"}")
        );
        assert_eq!(raw["role"], serde_json::json!("assistant"));
    }

    #[test]
    fn raw_repeated_role_does_not_concatenate() {
        // `role` has the same defect as the tool-call identity fields: a
        // provider that repeats it every chunk would yield
        // "assistantassistant" → `Input should be 'assistant'`.
        let mut acc = DeltaAccumulator::new();
        for text in ["Hel", "lo"] {
            acc.feed(&LlmEvent::RawAssistantDelta {
                delta: serde_json::json!({ "role": "assistant", "content": text }),
            });
        }
        let raw = acc.take_raw().unwrap();
        assert_eq!(raw["role"], serde_json::json!("assistant"));
        assert_eq!(
            raw["content"],
            serde_json::json!("Hello"),
            "content still concatenates"
        );
    }

    #[test]
    fn raw_unknown_tool_call_keys_still_accumulate() {
        // Guard against over-correcting: only `id`/`type`/`function.name` are
        // identity keys. An unknown per-call key keeps fragment semantics so
        // Rule 1 still round-trips it (Gemini splits `thought_signature`).
        let mut acc = DeltaAccumulator::new();
        for fragment in ["ab", "cd"] {
            acc.feed(&LlmEvent::RawAssistantDelta {
                delta: serde_json::json!({
                    "tool_calls": [{
                        "index": 0,
                        "thought_signature": fragment,
                        "function": { "arguments": "" }
                    }]
                }),
            });
        }
        let raw = acc.take_raw().unwrap();
        assert_eq!(
            raw["tool_calls"][0]["thought_signature"],
            serde_json::json!("abcd")
        );
    }

    #[test]
    fn raw_none_when_no_raw_deltas() {
        let mut acc = DeltaAccumulator::new();
        acc.feed(&LlmEvent::TextDelta { text: "hi".into() });
        assert!(acc.take_raw().is_none());
    }
}
