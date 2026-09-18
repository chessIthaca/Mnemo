// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! The LLM provider abstraction.
//!
//! Clients: `openai.rs` (the OpenAI-compatible client — config, the
//! complete() pipeline stages, and the `openai::` submodule tree: request
//! building, SSE parsing, the stream task, the stream guard) and
//! `anthropic.rs`. Shared plumbing: `sse_util.rs` (SSE + HTTP-response
//! error helpers), `stream.rs` (repetition guard + delta accumulators).
//! `models.rs` fetches the live `/models` lists for the Settings pickers;
//! `policy.rs` holds the per-provider behavior policies; `trace.rs` the
//! request trace log; `vision.rs` the vision fallback client;
//! `client_factory.rs` builds clients from endpoint config.

pub mod anthropic;
pub mod boundary;
pub mod client_factory;
pub mod models;
pub mod openai;
pub mod policy;
pub mod sse_util;
pub mod strict;
pub mod stream;
pub mod trace;
pub mod vision;

pub use stream::{DeltaAccumulator, ToolCallAccumulator};
pub use policy::{ProviderPolicy, ReasoningRetention, StatefulKey, Vendor};

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

/// A provider's capability set. The agent loop queries this before each request
/// to decide whether to send `tool_choice`, `strict` schemas, etc.
#[derive(Debug, Clone)]
pub struct Capabilities {
    /// OpenAI: true, Ollama: false.
    pub supports_tool_choice: bool,
    /// OpenAI: true (enforced), Ollama: false (accepted but not enforced).
    pub supports_strict_schema: bool,
    /// Whether parallel tool calls are supported.
    pub supports_parallel_tools: bool,
    /// Whether `finish_reason` can be trusted to indicate tool calls.
    pub reliable_finish_reason: bool,
    /// Maximum context window in tokens (input + output).
    pub max_context: usize,
    /// Maximum output tokens per request. Defaults to max_context / 2.
    pub max_output_tokens: usize,
    /// Whether the model accepts image inputs (multimodal). When `false`,
    /// image content blocks are stripped from requests, and a separate vision
    /// model (if configured) is used for image-to-text instead.
    pub multimodal: bool,
}

impl Capabilities {
    /// Full OpenAI capabilities.
    pub fn openai() -> Self {
        Self {
            supports_tool_choice: true,
            supports_strict_schema: true,
            supports_parallel_tools: true,
            reliable_finish_reason: true,
            max_context: 128_000,
            max_output_tokens: 32_000,
            multimodal: false,
        }
    }

    /// Degraded capabilities for a local (Ollama/vLLM/LM Studio) endpoint.
    pub fn local() -> Self {
        Self {
            supports_tool_choice: false,
            supports_strict_schema: false,
            supports_parallel_tools: false,
            reliable_finish_reason: false,
            max_context: 32_768,
            max_output_tokens: 16_384,
            multimodal: false,
        }
    }

    /// Capabilities for a native Anthropic Messages API endpoint.
    ///
    /// Tool use is fully supported (the Messages API has a real
    /// `tool_choice` parameter and a reliable `stop_reason`), but there is no
    /// `strict` schema enforcement — input schemas are advisory. Anthropic
    /// models are multimodal (image blocks), but the flag defaults to `false`
    /// so an endpoint only advertises it when the configured model accepts
    /// images (same convention as the OpenAI kind).
    pub fn anthropic() -> Self {
        Self {
            supports_tool_choice: true,
            supports_strict_schema: false,
            supports_parallel_tools: true,
            reliable_finish_reason: true,
            max_context: 200_000,
            max_output_tokens: 32_000,
            multimodal: false,
        }
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::openai()
    }
}

/// What kind of provider an endpoint is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    OpenAI,
    Local,
    Anthropic,
}

impl ProviderKind {
    /// Lowercase wire name (for `Message::origin_provider` — Rule 5).
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::OpenAI => "openai",
            ProviderKind::Local => "local",
            ProviderKind::Anthropic => "anthropic",
        }
    }

    /// Parse a lowercase wire name back to a [`ProviderKind`] (inverse of
    /// [`as_str`]); used to reconstruct the origin policy for Rule 5.
    pub fn from_str(s: &str) -> Option<ProviderKind> {
        match s {
            "openai" => Some(ProviderKind::OpenAI),
            "local" => Some(ProviderKind::Local),
            "anthropic" => Some(ProviderKind::Anthropic),
            _ => None,
        }
    }

    /// The capability set for this provider kind.
    pub fn capabilities(&self) -> Capabilities {
        match self {
            ProviderKind::OpenAI => Capabilities::openai(),
            ProviderKind::Local => Capabilities::local(),
            ProviderKind::Anthropic => Capabilities::anthropic(),
        }
    }

    /// The capability set, with optional overrides from the endpoint config.
    /// `max_context` overrides the context window; `max_output_tokens` overrides
    /// the per-request completion budget; `multimodal` overrides whether the
    /// model accepts image inputs; `strict_schema` overrides whether the
    /// endpoint enforces strict tool schemas (`None` = the kind's default).
    /// Use these when the endpoint proxies a model with different limits or
    /// capabilities than the kind implies — e.g. an OpenAI-kind endpoint
    /// behind a litellm/vertex proxy that rejects the `strict` field sets
    /// `strict_schema = Some(false)`.
    pub fn capabilities_with_overrides(
        &self,
        max_context: Option<usize>,
        max_output_tokens: Option<usize>,
        multimodal: bool,
        strict_schema: Option<bool>,
    ) -> Capabilities {
        let mut caps = self.capabilities();
        if let Some(mc) = max_context {
            caps.max_context = mc;
        }
        if let Some(mo) = max_output_tokens {
            caps.max_output_tokens = mo;
        }
        caps.multimodal = multimodal;
        if let Some(ss) = strict_schema {
            caps.supports_strict_schema = ss;
        }
        caps
    }
}

/// A chat message in the conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
    /// Tool calls made by the assistant in this message.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    /// The tool-call id this message responds to (for `tool` role messages).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// A name attached to tool-role messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The model's reasoning/thinking text for this assistant turn. DeepSeek
    /// thinking mode validates the request TAIL: the assistant turn that owns
    /// it — the last message, or the issuer of trailing tool results — must
    /// carry a `reasoning_content` KEY (any value, even ""); a missing key is
    /// an HTTP 400 "The `reasoning_content` in the thinking mode must be
    /// passed back to the API" (live-verified 2026-12-23, bug plan c9b5cbe4).
    /// Text-only historical assistant turns tolerate a missing key; every
    /// historical turn that carries `tool_calls` is validated too (third
    /// recurrence, 2026-09-12, bug plan c6cb69f7). Captured during the turn
    /// and stored here so the request builder can round-trip it; the builder
    /// guarantees the key on the tail turn (age 0, carrying the structured
    /// text) and on every older tool-call turn (the bare "" when the raw echo
    /// lacks it) — foreign 429-fallback turns included.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    /// Opaque provider metadata captured verbatim from a streamed response and
    /// **Legacy bridge (M2):** superseded by [`Message::raw`] (Rule 1 raw echo).
    /// No builder reads this field — it is set at packaging sites from
    /// `acc.take_message_provider_meta()` (which returns `None` since
    /// `capture_provider_meta` was removed) and on `ToolCall` from
    /// `tc.provider_meta.clone()` (also always `None`). Retained for
    /// serialized-conversation backward compatibility (old saves have the
    /// field); removal is a follow-up that requires a migration on load.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_meta: Option<serde_json::Map<String, serde_json::Value>>,
    /// The verbatim provider assistant turn (Rule 1 raw) — the exact JSON the
    /// provider returned, reassembled from streamed deltas. The request
    /// builder echoes this unchanged instead of reconstructing from the
    /// structured fields, so every key the provider sent (reasoning fields,
    /// signatures, unknown keys) round-trips byte-identical. `None` for
    /// user/tool/system/synthetic messages (constructed by us, not received).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw: Option<serde_json::Value>,
    /// The provider that produced this assistant turn (Rule 5 cross-vendor
    /// detection). `None` for non-assistant / synthetic messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_provider: Option<String>,
    /// The model that produced this assistant turn (Rule 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin_model: Option<String>,
    /// `true` when reasoning was stripped from this turn on a cross-vendor
    /// model switch (Rule 5). Downstream code must not assume continuity on a
    /// stripped turn.
    #[serde(default, skip_serializing_if = "is_false")]
    pub reasoning_stripped: bool,
    /// The server-assigned response id (Rule 3 stateful path). When the
    /// provider supports server-side state (OpenAI Responses API), this id is
    /// captured from the streamed response and sent as `previous_response_id`
    /// on the next request — the server holds the reasoning state, so full
    /// history need not be resent. `None` for stateless providers / synthetic
    /// messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
}

/// Serde helper: skip `reasoning_stripped` when false (the common case).
fn is_false(b: &bool) -> bool {
    !*b
}

impl Message {
    /// Create a text message with an explicit role (for dynamic-role sites).
    pub fn text(role: Role, content: impl Into<String>) -> Self {
        Self {
            role,
            content: MessageContent::text(content),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            provider_meta: None,
            raw: None,
            origin_provider: None,
            origin_model: None,
            reasoning_stripped: false,
            response_id: None,
        }
    }

    /// Create a user-role text message with no tool calls.
    pub fn user_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: MessageContent::text(text),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            provider_meta: None,
            raw: None,
            origin_provider: None,
            origin_model: None,
            reasoning_stripped: false,
            response_id: None,
        }
    }

    /// Create a system-role text message.
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: MessageContent::text(text),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            provider_meta: None,
            raw: None,
            origin_provider: None,
            origin_model: None,
            reasoning_stripped: false,
            response_id: None,
        }
    }

    /// Create an assistant text message with no tool calls.
    pub fn assistant_text(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::text(text),
            tool_calls: Vec::new(),
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            provider_meta: None,
            raw: None,
            origin_provider: None,
            origin_model: None,
            reasoning_stripped: false,
            response_id: None,
        }
    }

    /// Create an assistant message with tool calls.
    pub fn assistant(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::text(text),
            tool_calls,
            tool_call_id: None,
            name: None,
            reasoning_content: None,
            provider_meta: None,
            raw: None,
            origin_provider: None,
            origin_model: None,
            reasoning_stripped: false,
            response_id: None,
        }
    }

    /// Create a tool-result message responding to a tool call.
    pub fn tool_result(
        tool_call_id: impl Into<String>,
        name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: MessageContent::text(content),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
            name: Some(name.into()),
            reasoning_content: None,
            provider_meta: None,
            raw: None,
            origin_provider: None,
            origin_model: None,
            reasoning_stripped: false,
            response_id: None,
        }
    }

    /// Derive a read-only view of this message for planning, logging, and UI
    /// (Rule 1: the normalized view is never the thing sent back — the request
    /// builder echoes [`Message::raw`]).
    ///
    /// For an assistant turn captured from a provider response (`raw` is set),
    /// the view is derived from the verbatim raw JSON. For synthetic messages
    /// (no raw), it is derived from the structured fields.
    pub fn view(&self) -> TurnView {
        if let Some(raw) = &self.raw {
            view_from_raw(raw)
        } else {
            TurnView {
                text: self.content.as_text(),
                tool_calls: self.tool_calls.clone(),
                reasoning: self.reasoning_content.clone(),
            }
        }
    }
}

/// A read-only derived view of an assistant turn for planning, logging, and UI
/// (Rule 1). The request builder never reads this — it echoes [`Message::raw`].
#[derive(Debug, Clone)]
pub struct TurnView {
    /// The assistant's text content (concatenated, for display/planning).
    pub text: String,
    /// The tool calls the assistant made.
    pub tool_calls: Vec<ToolCall>,
    /// The reasoning/thinking text (derived from raw or `reasoning_content`).
    pub reasoning: Option<String>,
}

/// Derive a [`TurnView`] from a verbatim raw assistant turn (Rule 1).
///
/// Handles both the OpenAI-compatible flat shape (`content` string,
/// `reasoning_content`/`reasoning`, `tool_calls` array) and the Anthropic
/// content-array shape (text / thinking / tool_use blocks).
fn view_from_raw(raw: &serde_json::Value) -> TurnView {
    let obj = raw.as_object();
    let content = obj.and_then(|o| o.get("content"));
    let text = match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|b| {
                if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                    b.get("text").and_then(|t| t.as_str()).map(String::from)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    };
    let reasoning = obj
        .and_then(|o| o.get("reasoning_content").or_else(|| o.get("reasoning")))
        .and_then(|r| r.as_str())
        .map(String::from)
        .or_else(|| {
            content.and_then(|c| c.as_array()).and_then(|arr| {
                let t: String = arr
                    .iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(|t| t.as_str()) == Some("thinking") {
                            b.get("thinking").and_then(|t| t.as_str()).map(String::from)
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("");
                if t.is_empty() {
                    None
                } else {
                    Some(t)
                }
            })
        });
    let tool_calls = obj
        .and_then(|o| o.get("tool_calls"))
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let id = tc.get("id").and_then(|v| v.as_str()).unwrap_or("");
                    let func = tc.get("function")?;
                    let name = func.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let arguments = func.get("arguments").and_then(|v| v.as_str()).unwrap_or("");
                    Some(ToolCall::new(id, name, arguments))
                })
                .collect()
        })
        .unwrap_or_else(|| {
            content
                .and_then(|c| c.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|b| {
                            if b.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                                let id = b.get("id").and_then(|v| v.as_str()).unwrap_or("");
                                let name = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                let arguments = b
                                    .get("input")
                                    .map(|i| i.to_string())
                                    .unwrap_or_else(|| "{}".to_string());
                                Some(ToolCall::new(id, name, arguments))
                            } else {
                                None
                            }
                        })
                        .collect()
                })
                .unwrap_or_default()
        });
    TurnView {
        text,
        tool_calls,
        reasoning,
    }
}

/// Strip reasoning from cross-vendor assistant turns (Rule 5).
///
/// When a turn's origin provider is a different vendor than the current
/// provider, reasoning fields are stripped from the raw and
/// [`Message::reasoning_stripped`] is set to `true`. Same-vendor
/// different-model turns are left unchanged (the backend handles
/// compatibility). Already-stripped turns are skipped (idempotent).
///
/// Signatures are encrypted per-provider and will either 400 or be silently
/// ignored across vendors, so they must not be sent. Downstream code must not
/// assume continuity on a stripped turn.
///
/// **One-way (L1):** stripping is irreversible — once `reasoning_stripped` is
/// set, the reasoning fields are gone from `raw` and cannot be restored, even
/// if the user switches back to the original vendor. This is acceptable because
/// the reasoning state was vendor-specific and cannot be replayed across the
/// intervening turn anyway. The idempotent guard prevents double-processing but
/// does not undo a prior strip.
///
/// Call this as a pre-build normalization pass, before the request builder
/// echoes raw — the builder then simply echoes the (already-stripped) raw.
pub fn strip_cross_vendor_reasoning(
    messages: &mut [Message],
    current_kind: ProviderKind,
    current_model: &str,
) {
    let current_policy = ProviderPolicy::for_kind_and_model(current_kind, current_model);
    for msg in messages.iter_mut() {
        if msg.role != Role::Assistant || msg.reasoning_stripped {
            continue;
        }
        let Some(origin_kind) = msg
            .origin_provider
            .as_deref()
            .and_then(ProviderKind::from_str)
        else {
            continue;
        };
        let Some(origin_model) = msg.origin_model.as_deref() else {
            continue;
        };
        let origin_policy = ProviderPolicy::for_kind_and_model(origin_kind, origin_model);
        if origin_policy.is_cross_vendor(&current_policy) {
            strip_reasoning_from_raw(&mut msg.raw);
            msg.reasoning_stripped = true;
        }
    }
}

/// Remove reasoning fields from a raw assistant turn (Rule 5 cross-vendor strip).
///
/// Removes:
/// - Flat OpenAI-compatible keys: `reasoning_content`, `reasoning`,
///   `thought_signature` (message-level and per-tool-call).
/// - Anthropic content-array blocks: `thinking` and `redacted_thinking`
///   (their per-block `signature` goes with them).
///
/// Text content and tool-call structure are preserved so the message remains
/// a valid history entry.
fn strip_reasoning_from_raw(raw: &mut Option<serde_json::Value>) {
    let Some(val) = raw.as_mut() else {
        return;
    };
    let Some(obj) = val.as_object_mut() else {
        return;
    };
    // Flat OpenAI-compatible reasoning keys.
    obj.remove("reasoning_content");
    obj.remove("reasoning");
    obj.remove("thought_signature");
    // Per-tool-call signatures.
    if let Some(tcs) = obj.get_mut("tool_calls").and_then(|t| t.as_array_mut()) {
        for tc in tcs {
            if let Some(tc_obj) = tc.as_object_mut() {
                tc_obj.remove("thought_signature");
            }
        }
    }
    // Anthropic content array: drop thinking / redacted_thinking blocks.
    if let Some(content) = obj.get_mut("content").and_then(|c| c.as_array_mut()) {
        content.retain(|b| {
            let bt = b.get("type").and_then(|t| t.as_str());
            bt != Some("thinking") && bt != Some("redacted_thinking")
        });
    }
}

/// A message role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// The content of a message — either plain text or a list of content blocks.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    /// Plain text content.
    Text(String),
    /// A list of content parts (e.g. text + image blocks).
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    /// Create plain-text content.
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    /// Get the text if this is plain text; otherwise concatenate text parts.
    pub fn as_text(&self) -> String {
        match self {
            MessageContent::Text(s) => s.clone(),
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(text.clone()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(""),
        }
    }
}

impl From<String> for MessageContent {
    fn from(s: String) -> Self {
        Self::text(s)
    }
}

impl From<&str> for MessageContent {
    fn from(s: &str) -> Self {
        Self::text(s)
    }
}

/// A content part within a multipart message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

/// An image URL block.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

/// A tool call made by the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// The id assigned by the model.
    pub id: String,
    /// The tool/function name.
    pub name: String,
    /// The arguments as a JSON string (per OpenAI spec).
    pub arguments: String,
    /// Opaque provider metadata captured verbatim from a streamed response's
    /// tool-call entry and echoed back byte-identical on subsequent requests.
    /// Provenance-gated: a key is only ever sent if it was first *received*
    /// from this conversation's responses, so non-signature endpoints see
    /// byte-identical requests. Gemini attaches `thought_signature`
    /// attestations to `functionCall` parts; this field carries them through
    /// storage and back into the request untouched. See the wire-contract
    /// spec at
    /// `.coding/knowledge/spec/2026-12-20-gemini-thought-signature-wire-contract-round-tri.md`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_meta: Option<serde_json::Map<String, serde_json::Value>>,
}

impl ToolCall {
    /// Create a tool call with no provider metadata.
    pub fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        arguments: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
            provider_meta: None,
        }
    }
}

/// A tool-call id + index, used during streaming accumulation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallRef {
    pub index: u32,
    pub id: Option<String>,
    pub name: Option<String>,
}

/// The JSON schema for a tool, in OpenAI function format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// The JSON Schema for the parameters object.
    pub parameters: Value,
    /// Whether to request strict schema enforcement on this tool.
    ///
    /// Set (with the parameters normalized to strict-legal form — see
    /// [`strict`](crate::provider::strict)) only for the mutation/plan
    /// tools on endpoints whose [`Capabilities`] report
    /// `supports_strict_schema`; `None` otherwise. The per-endpoint
    /// `supports_strict_schema` key in `endpoints.toml` overrides the
    /// provider-kind default (an OpenAI-kind endpoint behind a
    /// litellm/vertex proxy that rejects the field sets it `false`).
    /// The request builders omit the field entirely when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strict: Option<bool>,
}

/// Estimate the prompt token count for a request, using a conservative
/// byte-based heuristic (bytes / 4 + 4 per message + tool-schema bytes / 4).
///
/// This mirrors the fallback `ContextManager` uses when the BPE encoder is
/// unavailable. It **overestimates** (English averages ~4 bytes/token, but
/// multi-byte UTF-8 like CJK/emoji is denser, so bytes/4 overestimates more
/// for non-ASCII text), which is safe for capping `max_completion_tokens`:
/// an overestimate yields a tighter cap, never a looser one.
///
/// Used by [`OpenAiClient::build_request_json`](crate::provider::openai::OpenAiClient::build_request_json)
/// and [`AnthropicClient::build_request_json`](crate::provider::anthropic::AnthropicClient::build_request_json)
/// to cap `max_completion_tokens` / `max_tokens` so the request never exceeds
/// the model's context window (input + output).
pub(crate) fn estimate_prompt_tokens(messages: &[Message], tools: &[ToolSchema]) -> usize {
    let chars: usize = messages.iter().map(message_estimate_chars).sum();
    // 4 tokens per message overhead (same as ContextManager) + chars/4.
    messages.len() * 4 + (chars + tools_chars(tools)) / 4
}

/// The [`estimate_prompt_tokens`] char sum of ONE message — content text +
/// tool-call arguments + reasoning_content (tool-call arguments are sent as
/// JSON strings in the request). Split out (perf review L3, 2027-01-09) so
/// the request builder's serialized-prefix cache can sum message REGIONS
/// without re-walking the whole history: the cached prefix char sum + the
/// fresh tail chars reproduce [`estimate_prompt_tokens`] exactly — same
/// formula, same basis.
pub(crate) fn message_estimate_chars(m: &Message) -> usize {
    let mut chars = m.content.as_text().len();
    for tc in &m.tool_calls {
        chars += tc.arguments.len();
    }
    if let Some(rc) = &m.reasoning_content {
        chars += rc.len();
    }
    chars
}

/// The serialized char length of the tools-schema array: name + description
/// + parameters JSON per tool (the parameters schema is serialized into the
/// request body). Shared by [`estimate_prompt_tokens`],
/// [`estimate_tools_tokens`], and the request builder's split estimate
/// (perf review L3) so the bases cannot drift.
pub(crate) fn tools_chars(tools: &[ToolSchema]) -> usize {
    tools
        .iter()
        .map(|t| t.name.len() + t.description.len() + t.parameters.to_string().len())
        .sum()
}

/// The tools-schema overhead the provider counts in `usage.prompt_tokens`:
/// [`tools_chars`] at ~4 chars per token. The turn loop feeds this to
/// [`crate::agent::context::TokenAccounting::set_tools_tokens`] so the ctx
/// readout includes the schema block a message-only accounting misses.
pub(crate) fn estimate_tools_tokens(tools: &[ToolSchema]) -> usize {
    tools_chars(tools) / 4
}

/// Ceiling on total input tokens above which LiteLLM-class proxies drop
/// whole-conversation prefix caching (observed around ~340K tokens; hit rate
/// collapses from ~99% to ~4%). Two consumers keep requests below it:
/// `ContextManager` summarizes before the cliff (agent-side compaction
/// ceiling), and the request builders treat
/// `ceiling - PROXY_CACHE_PRESSURE_MARGIN_TOKENS` as the pressure threshold
/// for vendor-specific reasoning retention (wire-side).
pub const PROXY_CACHE_CEILING_TOKENS: usize = 340_000;

/// Safety margin kept below [`PROXY_CACHE_CEILING_TOKENS`]: the prompt
/// estimate is a conservative byte heuristic, so consumers act on pressure
/// this many tokens before the estimated cliff. Also the gap `ContextManager`
/// keeps between its summarize trigger and the ceiling.
pub const PROXY_CACHE_PRESSURE_MARGIN_TOKENS: usize = 32_768;

/// Remove reasoning-text keys from an outgoing assistant message object.
///
/// Policy-driven Rule-1 deviation (vendor-specific reasoning retention): some
/// providers do not need historical thinking *text* for continuity — Gemini 3
/// needs only `thought_signature`, DeepSeek tolerates dropping historical
/// `reasoning_content` under context pressure. The request builder calls this
/// on historical assistant turns when the provider's
/// [`ReasoningRetention`](crate::provider::ReasoningRetention) allows it; the
/// stored `Message` is never mutated (the builder works on a clone of the raw
/// echo or a freshly built object).
///
/// Always removes the top-level `reasoning_content` and `reasoning` keys (the
/// two spellings reasoning text travels under across gateways). When
/// `keep_signatures` is `false` it also removes `thought_signature` at
/// message level and inside every `tool_calls[]` entry. `content`, the
/// `tool_calls` structure, and every other key are untouched; a non-object
/// value is left unchanged.
pub fn strip_reasoning_text_fields(obj: &mut Value, keep_signatures: bool) {
    let Some(map) = obj.as_object_mut() else {
        return;
    };
    map.remove("reasoning_content");
    map.remove("reasoning");
    if !keep_signatures {
        map.remove("thought_signature");
        if let Some(Value::Array(calls)) = map.get_mut("tool_calls") {
            for call in calls.iter_mut() {
                if let Some(entry) = call.as_object_mut() {
                    entry.remove("thought_signature");
                }
            }
        }
    }
}

impl ToolSchema {
    /// Build a new tool schema.
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
            strict: None,
        }
    }
}

/// How the model should choose tools.
#[derive(Debug, Clone)]
pub enum ToolChoice {
    /// Let the model decide.
    Auto,
    /// Force a specific tool.
    Function(String),
    /// Force any tool call.
    Required,
}

/// A single event from the LLM stream.
#[derive(Debug, Clone)]
pub enum LlmEvent {
    /// A fragment of assistant text.
    TextDelta { text: String },
    /// A fragment of reasoning text (reasoning models).
    ReasoningDelta { text: String },
    /// The start of a tool call (id + name arrive here).
    ToolCallStart { index: u32, id: String, name: String },
    /// A fragment of a tool-call's arguments.
    ToolCallArgumentDelta { index: u32, fragment: String },
    /// The stream finished.
    Finish { reason: FinishReason },
    /// Token usage for this request.
    Usage {
        prompt_tokens: u32,
        completion_tokens: u32,
        /// Tokens spent on reasoning (reasoning models like glm-5.2 report
        /// these under completion_tokens_details.reasoning_tokens).
        reasoning_tokens: u32,
        /// Prompt tokens served from the provider's cache (OpenAI
        /// `prompt_tokens_details`/`input_tokens_details` `cached_tokens`;
        /// Anthropic `cache_read_input_tokens`). 0 when the provider doesn't
        /// report cache details.
        cached_tokens: u32,
        /// Time-to-first-token: milliseconds from the HTTP POST to the first
        /// streamed chunk. `None` when timing wasn't captured (e.g. tests).
        /// Used for input tok/sec = prompt_tokens / ttft_ms.
        ttft_ms: Option<u32>,
        /// Generation time: milliseconds from the first token to the usage
        /// event (the final chunk). `None` when timing wasn't captured.
        /// Used for output tok/sec = completion_tokens / generation_ms.
        generation_ms: Option<u32>,
    },
    /// An error during streaming.
    Error { error: String },
    /// Opaque provider metadata captured verbatim from a streamed response.
    /// Carries passthrough keys (e.g. Gemini `thought_signature`) outside the
    /// standard text/reasoning/tool-call event kinds so the accumulator can
    /// store them without polluting the text stream. `tool_call_index` is
    /// `None` for message-level metadata, `Some(i)` for per-tool-call metadata
    /// scoped to the tool call at that stream index.
    ProviderMeta {
        tool_call_index: Option<u32>,
        meta: serde_json::Map<String, serde_json::Value>,
    },
    /// The verbatim `choices[0].delta` object from a streamed OpenAI-compatible
    /// chunk — the raw source of truth for replay (Rule 1).
    ///
    /// The accumulator merges these into the final assistant message object,
    /// preserving every key the provider sent: exact field names (`reasoning`
    /// vs `reasoning_content`), `thought_signature`, and any unknown keys —
    /// untouched, never parsed or regenerated. The typed events (`TextDelta`,
    /// `ReasoningDelta`, `ToolCallStart`, …) are a derived view for live UI
    /// streaming; this carries the bytes the request builder echoes back.
    /// Turn-loop match arms ignore it (the accumulator consumes it via `feed`).
    RawAssistantDelta {
        delta: serde_json::Value,
    },
    /// The server-assigned response id (Rule 3 stateful path). Emitted by the
    /// Responses API parser from `response.created` / `response.completed`
    /// events. The accumulator captures it; the turn-loop packaging sites store
    /// it on the assistant `Message` so the next request can send
    /// `previous_response_id` instead of resending full history.
    ResponseId {
        id: String,
    },
}

/// Why the model stopped generating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
    Other(String),
}

/// Byte-level stall tracking for the trace graph's `stall_ms` field.
///
/// A "stall" is a gap between consecutive byte arrivals on the response
/// stream longer than [`StallTracker::THRESHOLD`] — the connection is open
/// but the server is sending nothing. This is the waiting that used to be
/// lumped into `generation_ms` ("generate"); it is now split out so the
/// graph shows where time actually goes. Slow local inference (a CPU-bound
/// model with multi-second inter-token gaps) legitimately shows up as
/// stall — from the client's perspective it IS waiting.
///
/// Gaps are measured between raw byte arrivals, so SSE keepalive/ping
/// chunks reset the timer — a server that keeps the connection warm is not
/// stalled. The trailing silence before a read timeout is added by
/// [`StallTracker::finalize`].
#[derive(Default)]
pub struct StallTracker {
    /// When the previous byte arrival happened (`None` before the first chunk).
    last_chunk_at: Option<std::time::Instant>,
    /// Total stalled time in ms (gaps between chunks > [`StallTracker::THRESHOLD`]).
    accumulated_ms: u32,
}

impl StallTracker {
    /// A gap between consecutive byte arrivals longer than this counts as a
    /// stall. Normal token gaps are 20–200 ms; 2 s is well above that.
    pub const THRESHOLD: std::time::Duration = std::time::Duration::from_secs(2);

    /// Record a byte arrival. Call once per `Ok(bytes)` chunk with the same
    /// `now` used for the first-chunk timestamp.
    pub fn on_chunk(&mut self, now: std::time::Instant) {
        if let Some(last) = self.last_chunk_at {
            let gap = now.duration_since(last);
            if gap > Self::THRESHOLD {
                self.accumulated_ms += gap.as_millis() as u32;
            }
        }
        self.last_chunk_at = Some(now);
    }

    /// Add the trailing gap (last chunk → now) when the stream dies on a
    /// read timeout — the final silence is a stall too. A no-op when no
    /// chunk ever arrived (that window is TTFT, not generation).
    pub fn finalize(&mut self, now: std::time::Instant) {
        if let Some(last) = self.last_chunk_at {
            let gap = now.duration_since(last);
            if gap > Self::THRESHOLD {
                self.accumulated_ms += gap.as_millis() as u32;
            }
        }
    }

    /// Total stalled time in ms accumulated so far (a subset of
    /// `generation_ms`).
    pub fn ms(&self) -> u32 {
        self.accumulated_ms
    }
}

/// The trait every LLM client implements.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// The capability set for this client.
    fn capabilities(&self) -> &Capabilities;

    /// The provider kind.
    fn kind(&self) -> ProviderKind;

    /// The model id being used.
    fn model(&self) -> &str;

    /// The endpoint/provider name from `endpoints.toml` this client was
    /// built from (e.g. "openai", "zai"). Used by the agent loop to report
    /// WHICH endpoint serves the current model on the wire (`AgentInfo` /
    /// `ModelChanged`) — the frontend needs it to label the provider
    /// correctly when the same model id is listed under two endpoints
    /// (first-match resolution cannot disambiguate those). Real clients
    /// override this with their config's `provider` field; the default
    /// `""` keeps test mocks warning-free — an empty name makes the
    /// frontend fall back to its endpoint-list resolution, exactly the
    /// pre-wire behavior.
    fn provider_name(&self) -> &str {
        ""
    }

    /// Start a streaming completion.
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        tool_choice: Option<ToolChoice>,
    ) -> Result<BoxStream<'_, LlmEvent>>;

    /// Record the tools phase (stream end → tool batch finished) for the
    /// request whose response produced the tool calls, in milliseconds.
    ///
    /// Called by the turn loop once per tool batch. The default is a no-op —
    /// clients without a trace log (test mocks) simply don't record. Real
    /// clients override it to attribute the duration to the newest trace
    /// record via `LlmRequestLog::set_last_tools_ms`.
    fn record_tools_phase_ms(&self, _ms: u32) {}

    /// Record the local prompt-prep time (turn-loop iteration start → the
    /// POST), in milliseconds.
    ///
    /// Called by the turn loop right before the provider request. The target
    /// trace record does not exist yet, so real clients park the value and
    /// stamp it onto the record they next create. The default is a no-op —
    /// clients without a trace log (test mocks) simply don't record.
    fn record_prep_ms(&self, _ms: u32) {}

    /// Record the auto-compaction time (the summarization LLM call) when it
    /// fired before this request, in milliseconds. Parked + stamped exactly
    /// like [`record_prep_ms`](Self::record_prep_ms). Default no-op.
    fn record_compact_ms(&self, _ms: u32) {}

    /// Record the retry-backoff sleep (the `complete_with_retry` sleeps
    /// between failed attempts) that just elapsed, in milliseconds.
    ///
    /// Called by the retry loop right after each backoff sleep. The target
    /// trace record does not exist yet (it belongs to the NEXT attempt), so
    /// real clients park the value and stamp it onto the record they next
    /// create — exactly like [`record_prep_ms`](Self::record_prep_ms). The
    /// default is a no-op — clients without a trace log (test mocks)
    /// simply don't record.
    fn record_backoff_ms(&self, _ms: u32) {}

    /// Record the tool calls AS DELIVERED by the model for the trace record
    /// of this client's most recent request (backlog e8b39d72 H1: the
    /// generation-vs-harness discrimination tap). `(id, name, arguments)` per
    /// call — the arguments string verbatim, post-JSON-parse,
    /// pre-normalization. The turn loop calls this immediately after
    /// finalizing the stream's accumulator, before any validation.
    fn record_raw_tool_calls(&self, _calls: Vec<(String, String, String)>) {}
}

/// A preview shown in an approval prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalPreview {
    /// A unified diff for a file edit.
    Diff { path: PathBuf, diff: String },
    /// The full content of a new file.
    NewFile { path: PathBuf, content: String },
}

/// A runtime-swappable provider slot — a shared handle to the main LLM client
/// that sees runtime model swaps (the status-bar picker, Settings saves).
///
/// Mirrors [`SwappableVision`](crate::provider::vision::SwappableVision): the
/// factory holds the canonical `Arc<SwappableProvider>`, and tools that need
/// an LLM at call time (currently `memory_consolidate`, which uses it for
/// semantic/procedural extraction) snapshot the current client via [`get`].
/// Holding the slot (not a snapshot) means a Settings model swap takes effect
/// on the tool's next call without rebuilding the registry.
pub struct SwappableProvider {
    inner: std::sync::RwLock<Option<Arc<dyn LlmClient>>>,
}

impl SwappableProvider {
    /// Create a slot with an optional initial client.
    pub fn new(initial: Option<Arc<dyn LlmClient>>) -> Arc<Self> {
        Arc::new(Self {
            inner: std::sync::RwLock::new(initial),
        })
    }

    /// Replace the active client (`None` disables LLM-backed tool paths).
    pub fn set(&self, client: Option<Arc<dyn LlmClient>>) {
        *self.inner.write().expect("swappable provider lock poisoned") = client;
    }

    /// Snapshot the current client (if any).
    pub fn get(&self) -> Option<Arc<dyn LlmClient>> {
        self.inner
            .read()
            .expect("swappable provider lock poisoned")
            .clone()
    }

    /// Whether a client is currently configured.
    pub fn is_configured(&self) -> bool {
        self.inner
            .read()
            .expect("swappable provider lock poisoned")
            .is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stall_tracker_accumulates_gaps_over_threshold_only() {
        // Regression (trace-graph honesty): byte-silence gaps inside the
        // generation window used to be lumped into `generation_ms`
        // ("generate"). StallTracker splits them out: only gaps longer
        // than THRESHOLD count, and multiple gaps sum.
        let t0 = std::time::Instant::now();
        let mut tracker = StallTracker::default();
        // First chunk: no gap yet.
        tracker.on_chunk(t0);
        assert_eq!(tracker.ms(), 0);
        // 500 ms gap (under the 2 s threshold): not a stall.
        tracker.on_chunk(t0 + std::time::Duration::from_millis(500));
        assert_eq!(tracker.ms(), 0);
        // 3 s gap: a stall — the full gap counts.
        tracker.on_chunk(t0 + std::time::Duration::from_millis(3500));
        assert_eq!(tracker.ms(), 3000);
        // Another 2.5 s gap: accumulates.
        tracker.on_chunk(t0 + std::time::Duration::from_millis(6000));
        assert_eq!(tracker.ms(), 5500);
    }

    #[test]
    fn stall_tracker_finalize_adds_trailing_gap_before_timeout() {
        // The trailing silence (last chunk → read timeout) is a stall too.
        let t0 = std::time::Instant::now();
        let mut tracker = StallTracker::default();
        tracker.on_chunk(t0);
        tracker.on_chunk(t0 + std::time::Duration::from_millis(100));
        tracker.finalize(t0 + std::time::Duration::from_secs(90));
        assert_eq!(tracker.ms(), 89_900);
    }

    #[test]
    fn stall_tracker_no_chunks_measures_nothing() {
        // No chunk ever arrived → nothing to measure (that window is
        // TTFT, not generation).
        let mut tracker = StallTracker::default();
        tracker.finalize(std::time::Instant::now() + std::time::Duration::from_secs(90));
        assert_eq!(tracker.ms(), 0);
    }

    #[test]
    fn strip_reasoning_text_fields_removes_reasoning_but_keeps_signatures() {
        let mut obj = serde_json::json!({
            "role": "assistant",
            "content": "text",
            "reasoning_content": "think",
            "reasoning": "think2",
            "thought_signature": "sig",
            "tool_calls": [{
                "id": "c1", "type": "function",
                "function": {"name": "t", "arguments": "{}"},
                "thought_signature": "call_sig"
            }]
        });
        // keep_signatures = true: reasoning text removed, signatures intact.
        strip_reasoning_text_fields(&mut obj, true);
        assert!(obj.get("reasoning_content").is_none());
        assert!(obj.get("reasoning").is_none());
        assert_eq!(obj["thought_signature"], "sig");
        assert_eq!(obj["tool_calls"][0]["thought_signature"], "call_sig");
        // Content and tool-call structure untouched.
        assert_eq!(obj["content"], "text");
        assert_eq!(obj["tool_calls"][0]["function"]["name"], "t");
    }

    #[test]
    fn strip_reasoning_text_fields_also_drops_signatures_when_not_required() {
        let mut obj = serde_json::json!({
            "role": "assistant",
            "content": "text",
            "reasoning_content": "think",
            "thought_signature": "sig",
            "tool_calls": [{
                "id": "c1", "type": "function",
                "function": {"name": "t", "arguments": "{}"},
                "thought_signature": "call_sig"
            }]
        });
        strip_reasoning_text_fields(&mut obj, false);
        assert!(obj.get("reasoning_content").is_none());
        assert!(obj.get("thought_signature").is_none());
        assert!(obj["tool_calls"][0].get("thought_signature").is_none());
        // Structure untouched.
        assert_eq!(obj["tool_calls"][0]["function"]["name"], "t");
    }

    #[test]
    fn strip_reasoning_text_fields_noops_on_non_object_and_bare_payloads() {
        // Non-object payload: untouched, no panic.
        let mut scalar = serde_json::json!("just a string");
        strip_reasoning_text_fields(&mut scalar, false);
        assert_eq!(scalar, serde_json::json!("just a string"));
        // Object without tool_calls / signature keys: reasoning keys removed,
        // nothing else disturbed.
        let mut obj =
            serde_json::json!({"role": "assistant", "content": "x", "reasoning_content": "t"});
        strip_reasoning_text_fields(&mut obj, false);
        assert!(obj.get("reasoning_content").is_none());
        assert_eq!(obj["content"], "x");
    }

    #[test]
    fn capabilities_with_overrides() {
        let caps = ProviderKind::OpenAI.capabilities_with_overrides(
            Some(64_000),
            Some(8_192),
            true,
            None,
        );
        assert_eq!(caps.max_context, 64_000);
        assert_eq!(caps.max_output_tokens, 8_192);
        assert!(caps.multimodal);
        assert!(caps.supports_tool_choice); // other caps unchanged
    }

    #[test]
    fn capabilities_without_overrides_uses_defaults() {
        let caps = ProviderKind::OpenAI.capabilities_with_overrides(None, None, false, None);
        assert_eq!(caps.max_context, 128_000);
        assert_eq!(caps.max_output_tokens, 32_000);
        assert!(!caps.multimodal);
    }

    #[test]
    fn local_capabilities_with_overrides() {
        let caps = ProviderKind::Local.capabilities_with_overrides(
            Some(128_000),
            Some(32_000),
            true,
            None,
        );
        assert_eq!(caps.max_context, 128_000);
        assert_eq!(caps.max_output_tokens, 32_000);
        assert!(caps.multimodal);
        assert!(!caps.supports_tool_choice); // still degraded
    }

    #[test]
    fn strict_schema_override_flips_the_kind_default() {
        // The per-endpoint escape hatch: an OpenAI-kind endpoint behind a
        // proxy that rejects `strict` (litellm/vertex) opts out, and a
        // Local-kind endpoint that does enforce schemas opts in. `None`
        // keeps the kind default.
        let off =
            ProviderKind::OpenAI.capabilities_with_overrides(None, None, false, Some(false));
        assert!(!off.supports_strict_schema);
        let on = ProviderKind::Local.capabilities_with_overrides(None, None, false, Some(true));
        assert!(on.supports_strict_schema);
        let dflt = ProviderKind::OpenAI.capabilities_with_overrides(None, None, false, None);
        assert!(dflt.supports_strict_schema);
    }


    #[test]
    fn message_user_text_sets_defaults() {
        let m = Message::user_text("hello");
        assert_eq!(m.role, Role::User);
        assert!(matches!(&m.content, MessageContent::Text(t) if t == "hello"));
        assert!(m.tool_calls.is_empty());
        assert!(m.tool_call_id.is_none());
        assert!(m.name.is_none());
        assert!(m.reasoning_content.is_none());
        assert!(m.provider_meta.is_none());
    }

    #[test]
    fn message_system_sets_defaults() {
        let m = Message::system("be helpful");
        assert_eq!(m.role, Role::System);
        assert!(matches!(&m.content, MessageContent::Text(t) if t == "be helpful"));
        assert!(m.tool_calls.is_empty());
        assert!(m.provider_meta.is_none());
    }

    #[test]
    fn message_assistant_text_sets_defaults() {
        let m = Message::assistant_text("done");
        assert_eq!(m.role, Role::Assistant);
        assert!(matches!(&m.content, MessageContent::Text(t) if t == "done"));
        assert!(m.tool_calls.is_empty());
        assert!(m.provider_meta.is_none());
    }

    #[test]
    fn message_assistant_carries_tool_calls() {
        let tc = ToolCall::new("c1", "read", "{}");
        let m = Message::assistant("working", vec![tc]);
        assert_eq!(m.role, Role::Assistant);
        assert!(matches!(&m.content, MessageContent::Text(t) if t == "working"));
        assert_eq!(m.tool_calls.len(), 1);
        assert_eq!(m.tool_calls[0].id, "c1");
        assert!(m.provider_meta.is_none());
    }

    #[test]
    fn message_tool_result_sets_id_and_name() {
        let m = Message::tool_result("call_42", "file_read", "contents here");
        assert_eq!(m.role, Role::Tool);
        assert!(matches!(&m.content, MessageContent::Text(t) if t == "contents here"));
        assert_eq!(m.tool_call_id.as_deref(), Some("call_42"));
        assert_eq!(m.name.as_deref(), Some("file_read"));
        assert!(m.tool_calls.is_empty());
        assert!(m.provider_meta.is_none());
    }

    #[test]
    fn toolcall_new_sets_defaults() {
        let tc = ToolCall::new("id_1", "search", r#"{"q":"rust"}"#);
        assert_eq!(tc.id, "id_1");
        assert_eq!(tc.name, "search");
        assert_eq!(tc.arguments, r#"{"q":"rust"}"#);
        assert!(tc.provider_meta.is_none());
    }

    #[test]
    fn view_derives_from_openai_raw() {
        let m = Message {
            raw: Some(serde_json::json!({
                "role": "assistant",
                "content": "Hello",
                "reasoning_content": "thinking",
                "tool_calls": [{
                    "id": "c1", "type": "function",
                    "function": { "name": "read", "arguments": "{\"x\":1}" }
                }]
            })),
            ..Message::assistant_text("")
        };
        let v = m.view();
        assert_eq!(v.text, "Hello");
        assert_eq!(v.reasoning.as_deref(), Some("thinking"));
        assert_eq!(v.tool_calls.len(), 1);
        assert_eq!(v.tool_calls[0].id, "c1");
        assert_eq!(v.tool_calls[0].name, "read");
    }

    #[test]
    fn view_derives_from_anthropic_raw() {
        let m = Message {
            raw: Some(serde_json::json!({
                "role": "assistant",
                "content": [
                    { "type": "thinking", "thinking": "reasoning", "signature": "sig" },
                    { "type": "text", "text": "Answer" },
                    { "type": "tool_use", "id": "t1", "name": "write", "input": {"p": 2} }
                ]
            })),
            ..Message::assistant_text("")
        };
        let v = m.view();
        assert_eq!(v.text, "Answer");
        assert_eq!(v.reasoning.as_deref(), Some("reasoning"));
        assert_eq!(v.tool_calls.len(), 1);
        assert_eq!(v.tool_calls[0].id, "t1");
        assert_eq!(v.tool_calls[0].name, "write");
    }

    #[test]
    fn view_derives_from_synthetic_message() {
        let tc = ToolCall::new("c1", "read", "{}");
        let m = Message {
            reasoning_content: Some("synthetic reasoning".into()),
            ..Message::assistant("hi", vec![tc])
        };
        let v = m.view();
        assert_eq!(v.text, "hi");
        assert_eq!(v.reasoning.as_deref(), Some("synthetic reasoning"));
        assert_eq!(v.tool_calls.len(), 1);
    }

    #[test]
    fn serde_round_trip_preserves_raw_and_origin() {
        let m = Message {
            raw: Some(serde_json::json!({"role":"assistant","content":"x","reasoning_content":"r"})),
            origin_provider: Some("openai".into()),
            origin_model: Some("gpt-4o".into()),
            reasoning_stripped: false,
            response_id: None,
            ..Message::assistant_text("x")
        };
        let json = serde_json::to_string(&m).unwrap();
        let back: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(back.raw, m.raw);
        assert_eq!(back.origin_provider.as_deref(), Some("openai"));
        assert_eq!(back.origin_model.as_deref(), Some("gpt-4o"));
        assert!(!back.reasoning_stripped);
    }

    #[test]
    fn reasoning_stripped_serializes_only_when_true() {
        // Default false → omitted from JSON (skip_serializing_if = "is_false").
        let m = Message::assistant_text("hi");
        let json = serde_json::to_string(&m).unwrap();
        assert!(!json.contains("reasoning_stripped"));
        // True → present.
        let m = Message {
            reasoning_stripped: true,
            ..Message::assistant_text("hi")
        };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("reasoning_stripped"));
        let back: Message = serde_json::from_str(&json).unwrap();
        assert!(back.reasoning_stripped);
    }

    #[test]
    fn old_saved_conversation_without_raw_deserializes() {
        // A pre-raw saved assistant message (no raw/origin/reasoning_stripped
        // fields) must deserialize cleanly (Rule 1 migration: old saves).
        let json = r#"{"role":"assistant","content":"legacy","tool_calls":[]}"#;
        let m: Message = serde_json::from_str(json).unwrap();
        assert_eq!(m.role, Role::Assistant);
        assert!(m.raw.is_none());
        assert!(m.origin_provider.is_none());
        assert!(!m.reasoning_stripped);
    }

    #[test]
    fn cross_vendor_strip_removes_reasoning_and_sets_flag() {
        // Rule 5: switching from OpenAI (gpt-4o) to Anthropic (claude) is
        // cross-vendor — reasoning fields stripped, reasoning_stripped set.
        let mut messages = vec![Message {
            raw: Some(serde_json::json!({
                "role": "assistant",
                "content": "answer",
                "reasoning_content": "secret reasoning",
                "thought_signature": "sig",
                "tool_calls": [{
                    "id": "c1", "type": "function",
                    "function": { "name": "read", "arguments": "{}" },
                    "thought_signature": "call_sig"
                }]
            })),
            origin_provider: Some("openai".into()),
            origin_model: Some("gpt-4o".into()),
            ..Message::assistant_text("answer")
        }];
        strip_cross_vendor_reasoning(&mut messages, ProviderKind::Anthropic, "claude-sonnet-4-5");
        let m = &messages[0];
        assert!(m.reasoning_stripped);
        let raw = m.raw.as_ref().unwrap();
        assert!(raw.get("reasoning_content").is_none());
        assert!(raw.get("thought_signature").is_none());
        assert!(raw["tool_calls"][0].get("thought_signature").is_none());
        // Text + tool-call structure preserved.
        assert_eq!(raw["content"], serde_json::json!("answer"));
        assert_eq!(raw["tool_calls"][0]["function"]["name"], serde_json::json!("read"));
    }

    #[test]
    fn same_vendor_different_model_retains_reasoning() {
        // Rule 5: same vendor (OpenAI → OpenAI), different model — reasoning
        // retained unchanged (the backend handles compatibility).
        let mut messages = vec![Message {
            raw: Some(serde_json::json!({
                "role": "assistant",
                "content": "answer",
                "reasoning_content": "reasoning"
            })),
            origin_provider: Some("openai".into()),
            origin_model: Some("gpt-4o".into()),
            ..Message::assistant_text("answer")
        }];
        strip_cross_vendor_reasoning(&mut messages, ProviderKind::OpenAI, "gpt-4o-mini");
        let m = &messages[0];
        assert!(!m.reasoning_stripped);
        assert_eq!(
            m.raw.as_ref().unwrap()["reasoning_content"],
            serde_json::json!("reasoning")
        );
    }

    #[test]
    fn cross_vendor_strip_anthropic_thinking_blocks() {
        // Rule 5: Anthropic raw (content array) → thinking + redacted_thinking
        // blocks removed, text + tool_use preserved.
        let mut messages = vec![Message {
            raw: Some(serde_json::json!({
                "role": "assistant",
                "content": [
                    { "type": "thinking", "thinking": "reasoning", "signature": "sig" },
                    { "type": "redacted_thinking", "data": "<enc>" },
                    { "type": "text", "text": "answer" },
                    { "type": "tool_use", "id": "t1", "name": "read", "input": {} }
                ]
            })),
            origin_provider: Some("anthropic".into()),
            origin_model: Some("claude-sonnet-4-5".into()),
            ..Message::assistant_text("answer")
        }];
        strip_cross_vendor_reasoning(&mut messages, ProviderKind::OpenAI, "gpt-4o");
        let m = &messages[0];
        assert!(m.reasoning_stripped);
        let content = m.raw.as_ref().unwrap()["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], serde_json::json!("text"));
        assert_eq!(content[1]["type"], serde_json::json!("tool_use"));
        // No thinking / redacted_thinking blocks remain.
        assert!(content.iter().all(|b| {
            let t = b.get("type").and_then(|t| t.as_str());
            t != Some("thinking") && t != Some("redacted_thinking")
        }));
    }

    #[test]
    fn cross_vendor_strip_is_idempotent() {
        // An already-stripped turn is skipped (idempotent).
        let mut messages = vec![Message {
            raw: Some(serde_json::json!({
                "role": "assistant",
                "content": "answer",
                "reasoning_content": "reasoning"
            })),
            origin_provider: Some("openai".into()),
            origin_model: Some("gpt-4o".into()),
            reasoning_stripped: true,
            ..Message::assistant_text("answer")
        }];
        strip_cross_vendor_reasoning(&mut messages, ProviderKind::Anthropic, "claude");
        // Already stripped → reasoning_content NOT removed (it was already
        // stripped on the first pass; the flag prevents re-processing).
        let m = &messages[0];
        assert!(m.reasoning_stripped);
        // The raw still has reasoning_content because the idempotent guard
        // skipped this message (in practice, the first strip would have removed
        // it; this test verifies the guard prevents double-processing).
        assert!(m.raw.as_ref().unwrap().get("reasoning_content").is_some());
    }

    #[test]
    fn cross_vendor_strip_skips_non_assistant_messages() {
        // User/tool/system messages are never stripped.
        let mut messages = vec![
            Message::user_text("question"),
            Message::tool_result("c1", "read", "result"),
        ];
        strip_cross_vendor_reasoning(&mut messages, ProviderKind::Anthropic, "claude");
        // No panic, no changes (no raw to strip, no flag set).
        assert!(!messages[0].reasoning_stripped);
        assert!(!messages[1].reasoning_stripped);
    }
}
