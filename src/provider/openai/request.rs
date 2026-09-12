// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Request-body building for the OpenAI-compatible client.
//!
//! The chat.completions and Responses API JSON builders (params + provider
//! special-casing — GLM/DeepSeek reasoning fields, tool schemas, sampling
//! caps), the Local-provider system-message sanitization, and the pre-flight
//! request-shape validation that fails fast on malformed conversation shapes
//! instead of burning a round-trip on an opaque upstream 400.

use std::borrow::Cow;

use crate::error::{Error, Result};
use crate::provider::{Message, MessageContent, ProviderKind, Role, ToolChoice, ToolSchema};

use super::OpenAiClient;

/// `stream_options` — always `{ "include_usage": true }` (usage arrives on
/// the final SSE chunk).
#[derive(serde::Serialize)]
struct StreamOptions {
    include_usage: bool,
}

/// The chat-completions request body, pre-serialized (perf review L3,
/// 2027-01-09): `messages` carries each message's JSON VERBATIM
/// ([`serde_json::value::RawValue`]) so the ~MiB history is serialized
/// exactly once per request — never cloned into an intermediate
/// `serde_json::Value` tree and never re-serialized by the HTTP layer
/// (reqwest `.body(str)` ships the string as-is). Field set and
/// conditional-inclusion rules are equivalent to the former
/// `serde_json::json!` assembly (pinned by the request-body test suite);
/// `extra_body` is merged after serialization to preserve its
/// override-on-collision semantics.
#[derive(serde::Serialize)]
struct RequestBody<'a> {
    model: &'a str,
    messages: &'a [Box<serde_json::value::RawValue>],
    stream: bool,
    stream_options: StreamOptions,
    max_completion_tokens: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [serde_json::Value]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'a serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    top_p: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_token_ids: Option<&'a [u64]>,
}

/// The serialized message prefix memo (perf review L3, 2027-01-09): history
/// is append-mostly across a turn loop's iterations, so the serialized JSON
/// of the stable prefix is byte-identical from request to request. The cache
/// stores it with a sound fingerprint (see [`fingerprint_prefix`]) — a
/// mismatch rebuilds from scratch, so a stale hit is impossible by
/// construction. Single-slot per client: any other conversation, compaction,
/// head swap, pressure flip, or strip crossing simply misses and rebuilds.
pub(super) struct PrefixCache {
    /// Number of leading messages covered by `prefix` — always
    /// `messages.len()` minus the trailing system-message run (the volatile
    /// per-request tail + CONTEXT_FOOTER are pushed after the token
    /// accounting and popped after the request; they disappear between
    /// iterations, so caching them would make the cache never hit).
    pub(super) len: usize,
    /// The serialized JSON of `messages[0..len]`, one entry per message. The
    /// boxes circulate: taken out on a hit, stored back on refresh — the
    /// stable history is never re-serialized or cloned.
    pub(super) prefix: Vec<Box<serde_json::value::RawValue>>,
    /// The fingerprint of `messages[0..len]` at fill time.
    pub(super) fingerprint: u64,
    /// The [`estimate_prompt_tokens`] char sum over `messages[0..len]` at
    /// fill time (the walk is skipped on hits — the fresh tail chars plus
    /// this sum reproduce the full walk exactly).
    pub(super) prefix_chars: usize,
    /// The pressure signal at fill time — part of the key: a pressure flip
    /// changes the strip decisions (and thus the serialized forms) of
    /// strippable history.
    pub(super) under_pressure: bool,
}

/// Length-prefixed string write — makes the fold's byte-stream encoding
/// injective across adjacent fields (review round-1, Finding 1): without the
/// length, `{id:"ab", name:"c"}` and `{id:"a", name:"bc"}` would hash
/// identically despite serializing differently.
fn hash_str(h: &mut impl std::hash::Hasher, s: &str) {
    h.write_usize(s.len());
    h.write(s.as_bytes());
}

/// Structural hash of a JSON value — deterministic given the value (Map
/// iteration order is stable), no allocation. Strings are length-prefixed
/// (see [`hash_str`]) so the stream is self-delimiting.
fn hash_value(h: &mut impl std::hash::Hasher, v: &serde_json::Value) {
    match v {
        serde_json::Value::Null => h.write_u8(0),
        serde_json::Value::Bool(b) => {
            h.write_u8(1);
            h.write_u8(u8::from(*b));
        }
        serde_json::Value::Number(n) => {
            h.write_u8(2);
            hash_str(h, &n.to_string());
        }
        serde_json::Value::String(s) => {
            h.write_u8(3);
            hash_str(h, s);
        }
        serde_json::Value::Array(a) => {
            h.write_u8(4);
            h.write_usize(a.len());
            for x in a {
                hash_value(h, x);
            }
        }
        serde_json::Value::Object(o) => {
            h.write_u8(5);
            h.write_usize(o.len());
            for (k, x) in o {
                hash_str(h, k);
                hash_value(h, x);
            }
        }
    }
}

/// Hash one message's serialization-relevant bytes into `h` — the exact
/// inputs the request builder's output depends on (perf review L3,
/// 2027-01-09). No allocation: content parts and tool-call arguments are
/// hashed in place, and the raw payload is hashed structurally. Strings are
/// length-prefixed (see [`hash_str`]) so the stream is self-delimiting —
/// different field splits can never produce the same byte stream.
fn hash_message(h: &mut impl std::hash::Hasher, m: &Message) {
    match m.role {
        Role::System => h.write_u8(0),
        Role::User => h.write_u8(1),
        Role::Assistant => h.write_u8(2),
        Role::Tool => h.write_u8(3),
    }
    match &m.content {
        crate::provider::MessageContent::Text(s) => {
            h.write_u8(1);
            hash_str(h, s);
        }
        crate::provider::MessageContent::Parts(parts) => {
            h.write_u8(2);
            h.write_usize(parts.len());
            for p in parts {
                match p {
                    crate::provider::ContentPart::Text { text } => {
                        h.write_u8(1);
                        hash_str(h, text);
                    }
                    crate::provider::ContentPart::ImageUrl { image_url } => {
                        h.write_u8(2);
                        hash_str(h, &image_url.url);
                    }
                }
            }
        }
    }
    h.write_usize(m.tool_calls.len());
    for tc in &m.tool_calls {
        hash_str(h, &tc.id);
        hash_str(h, &tc.name);
        hash_str(h, &tc.arguments);
        match &tc.provider_meta {
            Some(meta) => {
                h.write_u8(1);
                h.write_usize(meta.len());
                for (k, v) in meta {
                    hash_str(h, k);
                    hash_value(h, v);
                }
            }
            None => h.write_u8(0),
        }
    }
    match &m.tool_call_id {
        Some(id) => {
            h.write_u8(1);
            hash_str(h, id);
        }
        None => h.write_u8(0),
    }
    match &m.name {
        Some(n) => {
            h.write_u8(1);
            hash_str(h, n);
        }
        None => h.write_u8(0),
    }
    match &m.reasoning_content {
        Some(rc) => {
            h.write_u8(1);
            hash_str(h, rc);
        }
        None => h.write_u8(0),
    }
    h.write_u8(u8::from(m.reasoning_stripped));
    match &m.raw {
        Some(raw) => {
            h.write_u8(1);
            hash_value(h, raw);
        }
        None => h.write_u8(0),
    }
}

/// The prefix fingerprint (perf review L3, 2027-01-09): a fresh O(n) fold
/// over `messages[0..len]` (see [`hash_message`]) PLUS — per assistant
/// message — the two age-dependent mutation decisions the builder applies at
/// echo time: [`ReasoningRetention::strips_reasoning`] and the age-0
/// `reasoning_content` re-add. Ages are counted from the END of history (the
/// builder's convention) and grow as the conversation does, so each decision
/// flips at most once per message (the re-add at age 0→1, the strip when age
/// crosses keep_recent under pressure) — a flip changes the fingerprint and
/// forces a rebuild, which is exactly when the serialized form changes. The
/// encoding is injective (length-prefixed strings — review round-1,
/// Finding 1): two different prefixes share a fingerprint only via a
/// negligible 2⁻⁶⁴ SipHash collision.
fn fingerprint_prefix(
    messages: &[Message],
    len: usize,
    policy: &crate::provider::ProviderPolicy,
    assistant_ages: &[usize],
    under_pressure: bool,
) -> u64 {
    use std::hash::Hasher as _;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write_usize(len);
    h.write_u8(u8::from(under_pressure));
    for (m, age) in messages[..len].iter().zip(assistant_ages) {
        hash_message(&mut h, m);
        if m.role == Role::Assistant {
            let needs_strip = policy.retention.strips_reasoning(*age, under_pressure);
            let needs_readd = *age == 0
                && policy.reasoning_required
                && policy.reasoning_field == Some("reasoning_content")
                && m.raw
                    .as_ref()
                    .and_then(|r| r.get("reasoning_content"))
                    .is_none();
            h.write_u8(u8::from(needs_strip));
            h.write_u8(u8::from(needs_readd));
        }
    }
    h.finish()
}

impl OpenAiClient {
    /// Build the request body as a JSON value — the TEST lens over
    /// [`OpenAiClient::build_request_body`] (perf review L3, 2027-01-09): the
    /// wire body is built ONCE as a String (messages spliced verbatim via
    /// RawValue, never cloned into a Value tree) and this lens parses it back
    /// for the request-body test suite, which indexes the parsed Value. The
    /// send path uses [`OpenAiClient::build_request_body`] directly; the
    /// trace parses the wire string on the blocking pool.
    #[cfg(test)]
    pub(super) fn build_request_json(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        tool_choice: Option<ToolChoice>,
    ) -> Result<serde_json::Value> {
        let body = self.build_request_body(messages, tools, tool_choice, false)?;
        Ok(serde_json::from_str(&body).map_err(|e| {
            Error::Provider(format!("built request body failed to re-parse: {e}"))
        })?)
    }

    /// Build the chat-completions request body as its wire JSON string (for
    /// the raw reqwest SSE stream) — a plain JSON object for the
    /// OpenAI-compatible `/chat/completions` endpoint. `omit_effort` drops
    /// the `reasoning_effort` field (the 400-rejection fallback retry).
    pub(super) fn build_request_body(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        tool_choice: Option<ToolChoice>,
        omit_effort: bool,
    ) -> Result<String> {
        // Local endpoints (Ollama/vLLM/LM Studio) render prompts with a Jinja
        // chat template that hard-fails when any `system` message is not the
        // FIRST message ("System message must be at the beginning"). Upstream
        // paths can produce non-leading system messages (the conversation
        // summary lands at index 1, suggestion re-injection appends one at the
        // end), so sanitize here — the single serialization choke point — to
        // guarantee no Local request ever carries one. OpenAI-kind providers
        // accept system messages anywhere, so they pass through untouched.
        let messages = if self.config.kind == ProviderKind::Local {
            sanitize_local_messages(messages)
        } else {
            Cow::Borrowed(messages)
        };
        // Vendor-specific reasoning retention (policy-driven Rule-1 scope):
        // resolve the provider policy once and derive the pressure signal.
        // When the request is near the proxy cache ceiling (LiteLLM-class
        // proxies drop whole-conversation prefix caching above ~340K input
        // tokens), providers whose continuity tolerates it drop historical
        // reasoning text from the OUTGOING payload only — the stored
        // `Message` is never mutated.
        let policy = crate::provider::ProviderPolicy::for_kind_and_model(
            self.config.kind,
            &self.config.model,
        );
        // Trailing system run (the volatile per-request tail + CONTEXT_FOOTER,
        // pushed after the token accounting and popped after the request):
        // never byte-stable across iterations — excluded from the prefix
        // cache and always serialized fresh.
        let trailing_systems = messages
            .iter()
            .rev()
            .take_while(|m| m.role == Role::System)
            .count();
        let cacheable_len = messages.len() - trailing_systems;

        // Prefix-cache lookup (perf review L3, 2027-01-09): on a fingerprint
        // match the serialized prefix is reused and only the tail is
        // serialized; the estimate walk is skipped (the cached prefix char
        // sum + the fresh tail chars reproduce estimate_prompt_tokens
        // exactly — same formula, same basis). The fingerprint is a fresh
        // O(n) fold (see `fingerprint_prefix`) — a stale hit is impossible
        // by construction. TokenAccounting's incremental total is
        // deliberately NOT fed into the cap: it is computed before the
        // per-request head/tail scaffolding is installed (turn.rs), so it
        // understates the actual request input, and its BPE basis differs
        // from this bytes/4 basis (feeding it would loosen the cap).
        let mut cache_guard = self
            .prefix_cache
            .lock()
            .expect("OpenAiClient prefix cache lock poisoned");
        let cached_snapshot = cache_guard
            .as_ref()
            .filter(|c| c.len > 0 && c.len <= cacheable_len)
            .map(|c| (c.len, c.prefix_chars, c.fingerprint, c.under_pressure));
        let tools_sum = crate::provider::tools_chars(tools);
        let (newly_stable_chars, tail_region_chars) = match cached_snapshot {
            Some((clen, _, _, _)) => (
                messages[clen..cacheable_len]
                    .iter()
                    .map(crate::provider::message_estimate_chars)
                    .sum::<usize>(),
                messages[cacheable_len..]
                    .iter()
                    .map(crate::provider::message_estimate_chars)
                    .sum::<usize>(),
            ),
            None => (0, 0),
        };
        // Provisional estimate: the split (cached prefix chars + fresh tail
        // chars) when a structurally-valid cache entry exists, else the full
        // walk. A fingerprint match below PROVES the split equaled the full
        // walk (the chars are additive and the prefix was just validated); a
        // mismatch falls back to the full walk.
        let provisional_est = match cached_snapshot {
            Some((_, cchars, _, _)) => {
                (cchars + newly_stable_chars + tail_region_chars + tools_sum) / 4
                    + messages.len() * 4
            }
            None => crate::provider::estimate_prompt_tokens(&messages, tools),
        };
        let provisional_pressure = provisional_est
            + crate::provider::PROXY_CACHE_PRESSURE_MARGIN_TOKENS
            > crate::provider::PROXY_CACHE_CEILING_TOKENS;
        // Age of each assistant message, counted from the END of history
        // (0 = the most recent assistant turn, 1 = the one before it, …).
        // Non-assistant entries carry a dummy 0 that is never consulted.
        let assistant_count = messages
            .iter()
            .filter(|m| m.role == Role::Assistant)
            .count();
        let mut seen_assistants = 0usize;
        let assistant_ages: Vec<usize> = messages
            .iter()
            .map(|m| {
                if m.role == Role::Assistant {
                    let age = assistant_count - 1 - seen_assistants;
                    seen_assistants += 1;
                    age
                } else {
                    0
                }
            })
            .collect();
        // Fingerprint validation (fresh fold over the CACHED length —
        // includes the pressure flag and the per-assistant age-dependent
        // decision bools).
        let (cache_hit, hit_prefix_chars) = match cached_snapshot {
            Some((clen, cchars, cfp, cpressure)) if cpressure == provisional_pressure => {
                let fold =
                    fingerprint_prefix(&messages, clen, &policy, &assistant_ages, provisional_pressure);
                (
                    fold == cfp,
                    Some(cchars + newly_stable_chars),
                )
            }
            _ => (false, None),
        };
        let (prompt_est, under_pressure, new_prefix_chars) = if cache_hit {
            (
                provisional_est,
                provisional_pressure,
                hit_prefix_chars.unwrap_or_default(),
            )
        } else {
            // Full walk (the cache was absent or stale — the split estimate
            // may have been wrong).
            let chars: Vec<usize> = messages
                .iter()
                .map(crate::provider::message_estimate_chars)
                .collect();
            let est = (chars.iter().sum::<usize>() + tools_sum) / 4 + messages.len() * 4;
            let pressure = est + crate::provider::PROXY_CACHE_PRESSURE_MARGIN_TOKENS
                > crate::provider::PROXY_CACHE_CEILING_TOKENS;
            (est, pressure, chars[..cacheable_len].iter().sum())
        };
        // Take the cached prefix on a hit — the boxes circulate (stored back
        // on refresh below), so the stable history is never re-serialized or
        // cloned. The lock is dropped during serialization (a std Mutex held
        // only across synchronous work).
        let prefix_boxes: Vec<Box<serde_json::value::RawValue>> = if cache_hit {
            #[cfg(test)]
            self.hit_count
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            cache_guard.take().map(|c| c.prefix).unwrap_or_default()
        } else {
            Vec::new()
        };
        let prefix_len = if cache_hit {
            cached_snapshot.map(|(l, ..)| l).unwrap_or(0)
        } else {
            0
        };
        drop(cache_guard);

        let messages_json: Vec<Cow<'_, serde_json::Value>> = messages
            .iter()
            .zip(assistant_ages.iter().copied())
            .skip(prefix_len)
            .map(|(m, assistant_age)| {
                let role = match m.role {
                    Role::System => "system",
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "tool",
                };
                // Rule 1: echo the verbatim raw assistant turn unchanged — the
                // exact JSON the provider returned, reassembled from streamed
                // deltas. Reconstructing from view fields drops unknown keys
                // (the bug this exists to prevent). Synthetic messages (no raw)
                // fall through to field-based construction below. The one
                // sanctioned deviation is policy-driven reasoning retention:
                // providers whose continuity does not need historical thinking
                // text drop it from the outgoing echo only.
                //
                // Defense-in-depth (H1+H2): only echo raw when it carries
                // usable content or valid tool calls. Fall through to field
                // construction when raw is empty (null-turn placeholder) or
                // has malformed tool-call arguments (sanitized by the turn
                // loop) — the structured fields carry the corrected values.
                if m.role == Role::Assistant {
                    if let Some(raw) = &m.raw {
                        if raw_is_usable(raw) {
                            return echo_assistant_raw(
                                raw,
                                &policy,
                                assistant_age,
                                under_pressure,
                                m.reasoning_content.as_deref(),
                            );
                        }
                    }
                }
                // Serialize the content. When the provider is multimodal and
                // the message has multipart content (text + image blocks), send
                // the parts array as-is. When the provider is NOT multimodal,
                // strip image blocks (the vision client handles them separately)
                // and send only the text. Plain-text content is always sent as
                // a string.
                let content_json = match &m.content {
                    crate::provider::MessageContent::Text(s) => {
                        // (backlog 1db26c95) Tool results carrying a
                        // configured boundary token are ESCAPED to the
                        // visible marker — the serving layer strips/maps
                        // the raw token from request input (live-verified
                        // 2027-01-08: the model perceived the token
                        // position as "a blank line"), so the model's
                        // textual view would show an empty string and a
                        // read-then-write round-trip would corrupt the
                        // file. The file tools restore the marker on
                        // write. Other roles pass through unchanged.
                        if m.role == Role::Tool && !self.config.stop_boundary_strings.is_empty()
                        {
                            serde_json::json!(
                                crate::provider::boundary::escape_boundary_tokens(
                                    s,
                                    &self.config.stop_boundary_strings,
                                )
                            )
                        } else {
                            serde_json::json!(s)
                        }
                    }
                    crate::provider::MessageContent::Parts(parts) => {
                        if self.caps.multimodal {
                            // Send all parts (text + image blocks) as an array.
                            serde_json::json!(parts)
                        } else {
                            // Strip image blocks — send only the concatenated
                            // text. A non-multimodal model can't process images;
                            // the vision client handles them separately.
                            let text: String = parts
                                .iter()
                                .filter_map(|p| match p {
                                    crate::provider::ContentPart::Text { text } => {
                                        Some(text.as_str())
                                    }
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("");
                            serde_json::json!(text)
                        }
                    }
                };
                let mut obj = serde_json::json!({
                    "role": role,
                    "content": content_json,
                });
                if !m.tool_calls.is_empty() {
                    obj["tool_calls"] = serde_json::json!(m
                        .tool_calls
                        .iter()
                        .map(|tc| {
                            let entry = serde_json::json!({
                                "id": tc.id,
                                "type": "function",
                                "function": {
                                    "name": tc.name,
                                    "arguments": tc.arguments,
                                }
                            });
                            entry
                        })
                        .collect::<Vec<_>>());
                }
                if let Some(id) = &m.tool_call_id {
                    obj["tool_call_id"] = serde_json::json!(id);
                }
                if let Some(name) = &m.name {
                    obj["name"] = serde_json::json!(name);
                }
                // DeepSeek thinking mode validates the request TAIL: the
                // assistant turn that owns it — the last message, or the
                // issuer of trailing tool results — must carry a
                // `reasoning_content` KEY (any value, even ""), or the API
                // rejects the request with HTTP 400 "The `reasoning_content`
                // in the thinking mode must be passed back to the API"
                // (live-verified 2026-12-23, bug plan c9b5cbe4). This branch
                // is only reached for SYNTHETIC assistant messages (no raw):
                // real provider turns echo their raw above, where the age-0
                // injection guarantees the key for reasoning_required
                // providers. Fall back to an empty string so the key is
                // always present on synthetic assistant messages too — a
                // synthetic turn can own the tail (e.g. the request ends
                // with an assistant summary).
                if m.role == Role::Assistant {
                    obj["reasoning_content"] =
                        serde_json::json!(m.reasoning_content.clone().unwrap_or_default());
                }
                // Vendor-specific reasoning retention on the synthetic path
                // too: applied AFTER the reasoning_content re-add above so a
                // pressure-triggered DeepSeek strip also removes the re-added
                // key. Age >= 1 only — the most recent assistant turn is never
                // stripped (the provider resumes reasoning from it).
                if m.role == Role::Assistant
                    && policy
                        .retention
                        .strips_reasoning(assistant_age, under_pressure)
                {
                    crate::provider::strip_reasoning_text_fields(
                        &mut obj,
                        policy.retention.signatures_required,
                    );
                }
                Cow::Owned(obj)
            })
            .collect();

        let tools_json: Vec<serde_json::Value> = tools
            .iter()
            .map(|t| {
                // Only include `strict` when it's Some — the json! macro
                // serializes None as null, which litellm/vertex rejects
                // ("Input should be a valid boolean"). Omitting the field
                // entirely is the correct behavior for providers that don't
                // support strict schemas.
                let mut function = serde_json::json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                });
                if let Some(strict) = t.strict {
                    function["strict"] = serde_json::json!(strict);
                }
                serde_json::json!({
                    "type": "function",
                    "function": function,
                })
            })
            .collect();

        /// Absolute ceiling on output tokens regardless of endpoint config.
        /// Most coding tasks need 2-8K; 32K is generous. Prevents wasteful
        /// 128K output budgets on large-context endpoints where the R9
        /// context-window cap never triggers.
        const SANE_MAX_OUTPUT_TOKENS: usize = 32_000;
        // Cap max_completion_tokens so input + output never exceeds the
        // model's context window. The prompt estimate is conservative
        // (overestimates), so the cap is safe — it only tightens, never
        // loosens. Floor at 1024 so we never send a useless 0. The sane
        // ceiling above caps the budget even when the context-window cap
        // doesn't (large-context endpoints). prompt_est was already computed
        // above the message map (it feeds the cache-pressure check).
        // Quantize the remaining context budget to 2048-token buckets to avoid
        // per-turn parameter jitter (e.g. 7472 -> 7464 -> 7446). Continuous
        // jitter invalidates proxy-level cache keys (LiteLLM) on every turn,
        // causing full prompt-cache misses and 15-45s prefill delays.
        const TOKEN_QUANTUM: usize = 2048;
        let context_budget = self
            .caps
            .max_context
            .saturating_sub(prompt_est)
            .saturating_sub(1024);
        let quantized_context_budget = (context_budget / TOKEN_QUANTUM) * TOKEN_QUANTUM;
        let max_completion = self
            .caps
            .max_output_tokens
            .max(4096)
            .min(quantized_context_budget)
            .min(SANE_MAX_OUTPUT_TOKENS)
            .max(1024);

        // Pre-serialized body (perf review L3, 2027-01-09): each message's
        // JSON is produced exactly once — a borrowed raw serializes straight
        // into its RawValue (no deep clone), a mutated/synthetic one
        // serializes from its owned Value — and the final body string splices
        // them verbatim. The former json! assembly deep-copied the whole
        // history into a Value tree and reqwest .json() serialized it again
        // per request.
        let mut messages_raw: Vec<Box<serde_json::value::RawValue>> = prefix_boxes;
        messages_raw.extend(
            messages_json
                .into_iter()
                .map(|m| {
                    serde_json::value::to_raw_value(m.as_ref())
                        .map_err(|e| Error::Provider(format!("message serialization failed: {e}")))
                })
                .collect::<Result<Vec<_>>>()?,
        );

        let tools_slice: Option<&[serde_json::Value]> =
            if tools.is_empty() { None } else { Some(&tools_json) };
        let tool_choice_value = if self.caps.supports_tool_choice {
            tool_choice.map(|tc| match tc {
                ToolChoice::Auto => serde_json::json!("auto"),
                ToolChoice::Required => serde_json::json!("required"),
                ToolChoice::Function(name) => serde_json::json!({
                    "type": "function",
                    "function": { "name": name }
                }),
            })
        } else {
            None
        };

        // Reasoning effort — only when set (endpoints that don't accept the
        // field must not receive it). A literal "off" must never reach the
        // wire: it is not a valid `reasoning_effort` variant anywhere
        // (DeepSeek's enum is none|minimal|low|medium|high|xhigh|max and
        // rejects "off" with an instant non-retryable 400). The config
        // resolvers normalize it (omit, or "none" for DeepSeek-family); this
        // guard keeps that guarantee for any path that skips them. The
        // endpoint/model `reasoning_effort_off_wire` config wins when set
        // (the escape hatch for aliases/renames/fine-tunes); the built-in
        // policy is the zero-config fallback.
        let effort_value: Option<&str> = if omit_effort {
            None
        } else {
            match &self.config.reasoning_effort {
                Some(effort) if effort == "off" => self
                    .config
                    .reasoning_effort_off_wire
                    .as_deref()
                    .or_else(|| {
                        crate::provider::policy::reasoning_effort_off_wire_value(
                            self.config.kind,
                            &self.config.model,
                        )
                    }),
                Some(effort) => Some(effort.as_str()),
                None => None,
            }
        };

        // Stop boundaries (backlog 82a9480c & spec 2026-12-21; config-driven
        // since 2027-01-05): models whose tokenizer emits its stop
        // boundaries as raw text (GLM-5.3's role tags + newline cascades)
        // must have them sent explicitly to OpenAI-compatible inference
        // servers. The boundary strings come from the endpoint/model
        // config (`stop_boundary_strings`, resolved per model — never
        // matched by name prefix, so aliases and fine-tunes are covered)
        // and ride ahead of the user's `stop` sequences (deduped).
        // `stop_token_ids` stays a plain config pass-through.
        let mut stops: Vec<String> = self.config.stop_boundary_strings.clone();
        for s in &self.config.stop {
            if !stops.contains(s) {
                stops.push(s.clone());
            }
        }
        let stops_slice: Option<&[String]> = if stops.is_empty() { None } else { Some(&stops) };
        let stop_ids_slice: Option<&[u64]> = if self.config.stop_token_ids.is_empty() {
            None
        } else {
            Some(&self.config.stop_token_ids)
        };

        let wire = RequestBody {
            model: &self.config.model,
            messages: &messages_raw,
            stream: true,
            stream_options: StreamOptions { include_usage: true },
            max_completion_tokens: max_completion,
            tools: tools_slice,
            tool_choice: tool_choice_value.as_ref(),
            reasoning_effort: effort_value,
            temperature: self.config.temperature,
            top_p: self.config.top_p,
            stop: stops_slice,
            stop_token_ids: stop_ids_slice,
        };
        let mut body_string = serde_json::to_string(&wire)
            .map_err(|e| Error::Provider(format!("request body serialization failed: {e}")))?;

        // Arbitrary extra body parameters merged into the request payload —
        // keys here override any standard field on collision (the former
        // in-place Map::insert semantics). The merge pays a parse+serialize
        // round trip; the hot path (no extra_body) never does.
        if let Some(extra) = &self.config.extra_body {
            if !extra.is_empty() {
                let mut merged: serde_json::Value = serde_json::from_str(&body_string)
                    .map_err(|e| Error::Provider(format!("built body failed to re-parse: {e}")))?;
                if let Some(obj) = merged.as_object_mut() {
                    for (k, v) in extra {
                        obj.insert(k.clone(), v.clone());
                    }
                }
                body_string = serde_json::to_string(&merged).map_err(|e| {
                    Error::Provider(format!("request body serialization failed: {e}"))
                })?;
            }
        }

        // Refresh the prefix cache: the stable region (everything before the
        // trailing system run) is byte-stable for the next iteration — the
        // boxes circulate (taken on a hit above, stored back here), so the
        // stable history is never re-serialized or cloned.
        {
            let mut guard = self
                .prefix_cache
                .lock()
                .expect("OpenAiClient prefix cache lock poisoned");
            let prefix: Vec<Box<serde_json::value::RawValue>> =
                messages_raw.drain(..cacheable_len).collect();
            *guard = Some(PrefixCache {
                len: cacheable_len,
                fingerprint: fingerprint_prefix(
                    &messages,
                    cacheable_len,
                    &policy,
                    &assistant_ages,
                    under_pressure,
                ),
                prefix_chars: new_prefix_chars,
                prefix,
                under_pressure,
            });
        }

        Ok(body_string)
    }

    /// Build a request body for the OpenAI Responses API (Rule 3 stateful path).
    ///
    /// When a previous assistant turn has a [`Message::response_id`], sends
    /// `previous_response_id` and only the new input (the server holds the
    /// previous context — no full history resend). Otherwise, sends the full
    /// conversation as input (first request). Includes
    /// `["reasoning.encrypted_content"]` per the provider policy so the
    /// server returns the opaque reasoning state for stateless fallback.
    pub(super) fn build_responses_request_json(
        &self,
        messages: &[Message],
        tools: &[ToolSchema],
        tool_choice: Option<ToolChoice>,
    ) -> Result<serde_json::Value> {
        // Find the last assistant turn with a response_id (stateful anchor).
        let anchor_idx: Option<usize> = messages
            .iter()
            .enumerate()
            .rev()
            .find(|(_, m)| m.role == Role::Assistant && m.response_id.is_some())
            .map(|(i, _)| i);

        let input_start = anchor_idx.map(|i| i + 1).unwrap_or(0);
        let input: Vec<serde_json::Value> = messages[input_start..]
            .iter()
            .flat_map(|m| message_to_responses_input(m, &self.config.stop_boundary_strings))
            .collect();

        let mut body = serde_json::json!({
            "model": self.config.model,
            "input": input,
            "stream": true,
        });

        if let Some(anchor) = anchor_idx {
            if let Some(id) = &messages[anchor].response_id {
                body["previous_response_id"] = serde_json::json!(id);
            }
        }

        // Include reasoning encrypted_content (per policy, for stateless
        // fallback — the server returns the opaque reasoning state).
        body["include"] = serde_json::json!(["reasoning.encrypted_content"]);

        // Tools (Responses API format: flat, no `function` nesting).
        if !tools.is_empty() {
            body["tools"] = serde_json::json!(
                tools
                    .iter()
                    .map(|t| {
                        let mut tool = serde_json::json!({
                            "type": "function",
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        });
                        if let Some(strict) = t.strict {
                            tool["strict"] = serde_json::json!(strict);
                        }
                        tool
                    })
                    .collect::<Vec<_>>()
            );
        }

        // Tool choice.
        if let Some(tc) = tool_choice {
            body["tool_choice"] = match tc {
                ToolChoice::Auto => serde_json::json!("auto"),
                ToolChoice::Required => serde_json::json!("required"),
                ToolChoice::Function(name) => serde_json::json!({
                    "type": "function",
                    "name": name,
                }),
            };
        }

        Ok(body)
    }
}

/// Convert a [`Message`] to Responses API input items (Rule 3).
///
/// The Responses API uses a flat input array (not the chat.completions
/// `messages` array). One assistant message with multiple tool calls becomes
/// multiple `function_call` items; tool results become
/// `function_call_output` items. System messages use the `developer` role
/// (the Responses API convention).
///
/// **Assumption (L2):** the stateful path assumes every assistant turn that
/// precedes the current request received a `response_id` from the server
/// (captured via `LlmEvent::ResponseId` from `response.created` /
/// `response.completed`). If a turn lacks a `response_id` (e.g. a synthetic
/// assistant message, or a turn whose stream ended before `response.completed`),
/// `build_responses_request_json` cannot anchor on it and falls back to
/// sending full input from that point — which is correct but loses the
/// server-side state benefit for that turn.
fn message_to_responses_input(m: &Message, boundary_tokens: &[String]) -> Vec<serde_json::Value> {
    match m.role {
        Role::System => vec![serde_json::json!({
            "role": "developer",
            "content": m.content.as_text()
        })],
        Role::User => vec![serde_json::json!({
            "role": "user",
            "content": m.content.as_text()
        })],
        Role::Assistant => {
            if m.tool_calls.is_empty() {
                vec![serde_json::json!({
                    "role": "assistant",
                    "content": m.content.as_text()
                })]
            } else {
                m.tool_calls
                    .iter()
                    .map(|tc| {
                        serde_json::json!({
                            "type": "function_call",
                            "call_id": tc.id,
                            "name": tc.name,
                            "arguments": tc.arguments,
                        })
                    })
                    .collect()
            }
        }
        Role::Tool => {
            // (backlog 1db26c95) Tool results escape configured boundary
            // tokens to the visible marker — the same contract as the
            // chat path (see build_request_json): the serving layer
            // strips/maps the raw token from request input, so the
            // model's textual view would show an empty string and a
            // read-then-write round-trip would corrupt the file. The
            // file tools restore the marker on write.
            vec![serde_json::json!({
                "type": "function_call_output",
                "call_id": m.tool_call_id,
                "output": crate::provider::boundary::escape_boundary_tokens(
                    &m.content.as_text(),
                    boundary_tokens,
                ),
            })]
        }
    }
}

/// Check whether a raw assistant turn carries usable content or valid tool
/// calls (H1+H2 defense-in-depth).
///
/// Returns `false` (→ fall through to field construction) when:
/// - **H1:** raw has no `content` key and no `tool_calls` (a null turn whose
///   raw is `{role:"assistant"}` — the "(no output)" placeholder is in the
///   structured `content` field).
/// - **H2:** raw has `tool_calls` but any argument string fails to parse as
///   JSON (the turn loop sanitized the structured `tool_calls`; echoing raw
///   would resend the malformed arguments).
/// - **H3:** raw has a `tool_calls` entry whose `type` is present but is not
///   exactly `"function"`. Echoing a corrupted discriminator is a hard remote
///   400 (`Input should be 'function'`).
///
/// Tool-call arguments are structured data the model produces (not opaque
/// reasoning fields), so validating them is sanctioned. The same applies to
/// the `type` discriminator, which has exactly one legal value.
///
/// **Accepted cost of falling through:** field construction rebuilds the
/// message from the typed fields, which drops `ToolCall::provider_meta` (e.g.
/// a Gemini `thought_signature`) and any unknown keys on that one message.
/// That is a deliberate trade against a request that would otherwise fail
/// outright.
fn raw_is_usable(raw: &serde_json::Value) -> bool {
    let Some(obj) = raw.as_object() else {
        return false;
    };
    let has_content = obj.contains_key("content");
    let tool_calls = obj.get("tool_calls").and_then(|t| t.as_array());
    // LOW-1: reasoning fields also count as "usable content" — a turn carrying
    // only `reasoning_content` / `reasoning` / `thought_signature` (no content,
    // no tool_calls) must be echoed via raw so unknown reasoning keys (e.g.
    // `thought_signature`) round-trip byte-identical (field construction only
    // re-adds `reasoning_content`, not signatures or unknown keys).
    let has_reasoning = obj.contains_key("reasoning_content")
        || obj.contains_key("reasoning")
        || obj.contains_key("thought_signature");
    // H1: no content, no reasoning fields, and no tool_calls → empty raw
    // (null turn).
    if !has_content && !has_reasoning && tool_calls.map(|t| t.is_empty()).unwrap_or(true) {
        return false;
    }
    if let Some(tcs) = tool_calls {
        // H2: all tool-call arguments must be valid JSON.
        if !tcs.iter().all(|tc| {
            tc.get("function")
                .and_then(|f| f.get("arguments"))
                .and_then(|a| a.as_str())
                .map(|s| serde_json::from_str::<serde_json::Value>(s).is_ok())
                .unwrap_or(false)
        }) {
            return false;
        }
        // H3: the `type` discriminator, when present, must be exactly
        // "function". A raw payload assembled before the identity/fragment
        // split in `stream::merge_identity` can carry a concatenated value
        // ("functionfunction...") from a provider that repeats the whole
        // tool-call object on every chunk; `Message::raw` is persisted, so a
        // conversation saved back then would otherwise 400 forever.
        //
        // Rejecting the whole raw (rather than repairing `type` in place) is
        // deliberate: the same corruption also mangles `id`
        // ("call_xcall_x..."), which no longer matches the `tool_call_id` on
        // the following tool message and trips the dangling-id check in
        // `validate_request_messages`. Field construction rebuilds `id`,
        // `name` and `arguments` from the structured `ToolCall`, all of which
        // are correct, so one check heals both defects.
        //
        // A missing `type` is tolerated: the builder adds it (see the
        // field-construction path), and non-OpenAI shapes may legitimately
        // omit it.
        if !tcs.iter().all(|tc| {
            tc.get("type")
                .map(|t| t.as_str() == Some("function"))
                .unwrap_or(true)
        }) {
            return false;
        }
    }
    true
}

/// The Rule-1 echo decision for one assistant message with a usable raw
/// payload (perf review L3, 2027-01-09): the raw is echoed BORROWED when the
/// reasoning-retention policy makes no mutation, and cloned exactly once only
/// when a mutation is required. The old path cloned unconditionally — a deep
/// Value clone per assistant message per request (~25-60ms across a
/// 503-message conversation) even for the NEVER_STRIP providers that never
/// mutate.
///
/// The two sanctioned mutations (both policy-driven, never applied to the
/// stored `Message`):
///
/// 1. Reasoning-text strip ([`ReasoningRetention::strips_reasoning`]) —
///    historical thinking text dropped from the OUTGOING echo only.
///
/// 2. The age-0 `reasoning_content` re-add — live-verified DeepSeek
///    thinking-mode contract (2026-12-23 curl probes against
///    api.deepseek.com/v1, bug plan c9b5cbe4): when `reasoning_effort` is
///    present, the assistant turn that owns the request tail — the last
///    message, or the issuer of trailing tool results — must carry a
///    `reasoning_content` KEY (any value, even ""); a missing key is an HTTP
///    400 "The `reasoning_content` in the thinking mode must be passed back
///    to the API" (probe T: [user, assistant(tool_calls, no rc), tool] →
///    400; T2: same with rc:"" → 200). Historical assistant turns tolerate a
///    missing key (probes A/D/G2), so only the most recent assistant turn
///    (age 0) gets the guarantee — and only for providers whose continuity
///    field IS `reasoning_content`: Gemini also sets `reasoning_required`,
///    but its contract is `thought_signature` and fabricating the key would
///    break its Rule-1 byte-identical echo (review finding 2026-12-23).
///    Foreign 429-fallback turns (GLM/Kimi) issue tool calls whose raw lacks
///    the key — the recurring session-killer bursts in
///    provider-errors.jsonl. Value: the structured field when present, else
///    "" — an empty key satisfies the validator, and HISTORICAL reasoning
///    text stays strippable under pressure (plan ffe59699; age-0 text is
///    never stripped).
///
/// The re-add condition is checked against `raw` (not a post-strip clone):
/// the strip never runs at age 0 ([`ReasoningRetention::strips_reasoning`]
/// protects age 0 unconditionally) and the re-add only applies at age 0, so
/// the two mutations are mutually exclusive and the check is equivalent.
pub(super) fn echo_assistant_raw<'a>(
    raw: &'a serde_json::Value,
    policy: &crate::provider::ProviderPolicy,
    assistant_age: usize,
    under_pressure: bool,
    reasoning_content: Option<&str>,
) -> Cow<'a, serde_json::Value> {
    let needs_strip = policy
        .retention
        .strips_reasoning(assistant_age, under_pressure);
    let needs_readd = assistant_age == 0
        && policy.reasoning_required
        && policy.reasoning_field == Some("reasoning_content")
        && raw.get("reasoning_content").is_none();
    if !needs_strip && !needs_readd {
        return Cow::Borrowed(raw);
    }
    let mut echoed = raw.clone();
    if needs_strip {
        crate::provider::strip_reasoning_text_fields(
            &mut echoed,
            policy.retention.signatures_required,
        );
    }
    if needs_readd {
        echoed["reasoning_content"] = serde_json::json!(reasoning_content.unwrap_or_default());
    }
    Cow::Owned(echoed)
}

/// Sanitize a message list for a Local provider so that no `system` message
/// appears anywhere but the first position.
///
/// Local endpoints (Ollama/vLLM/LM Studio) render prompts through a Jinja
/// chat template that raises "System message must be at the beginning" when a
/// `system` role shows up mid-conversation. Several upstream paths produce
/// exactly that (the conversation summary is placed at index 1, suggestion
/// re-injection appends a `system` message at the end, the volatile tail and
/// CONTEXT_FOOTER are pushed as trailing `system` messages on the OpenAI
/// path). Rather than chase each producer, this function rewrites the list at
/// the single serialization choke point:
///
/// - The first message keeps its role (it may be `system`).
/// - Any later `system` message is demoted to a `user` message whose text is
///   prefixed with `System: ` so the model still sees the content.
/// - All non-system messages pass through unchanged.
///
/// Demotion can yield consecutive `user` messages (e.g. a demoted summary at
/// index 1 followed by the real user turn). That is accepted as the lesser
/// evil versus a hard 500: the chatml/llama.cpp-family templates these
/// endpoints serve do not require strict user/assistant alternation.
///
/// Returns `Cow::Borrowed` when no rewrite is needed (the common case) so the
/// happy path costs nothing.
fn sanitize_local_messages(messages: &[Message]) -> Cow<'_, [Message]> {
    // Fast path: if no non-leading system message exists, borrow as-is.
    let needs_fix = messages.iter().skip(1).any(|m| m.role == Role::System);
    if !needs_fix {
        return Cow::Borrowed(messages);
    }

    let sanitized: Vec<Message> = messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if i > 0 && m.role == Role::System {
                let mut demoted = m.clone();
                demoted.role = Role::User;
                demoted.content = crate::provider::MessageContent::text(format!(
                    "System: {}",
                    m.content.as_text()
                ));
                demoted
            } else {
                m.clone()
            }
        })
        .collect();
    Cow::Owned(sanitized)
}

/// Validate the request messages shape before dispatching an HTTP request.
///
/// Some gateways (GLM's code-1214 "The messages parameter is illegal", Kimi's
/// more specific variants) reject malformed conversation shapes with an opaque
/// 400 instead of ignoring them, which the turn driver then retries in a burst
/// (9 retries, ~2s apart) — wasting quota and never succeeding. The shapes we
/// can produce upstream, all observed in provider-errors.jsonl:
///
/// - an empty messages array (nothing to send at all);
/// - an `assistant` message with no text, no parts, and no tool calls
///   ("the message at position N with role 'assistant' must not be empty");
/// - a `tool` message whose `tool_call_id` references an assistant tool call
///   that was dropped from the history ("tool_call_id is not found").
///
/// Each case fails fast HERE with a descriptive local error naming the index,
/// so the caller sees the actual defect instead of an upstream rejection.
/// Returns `Ok(())` when the shape is sendable.
pub(super) fn validate_request_messages(messages: &[Message]) -> Result<()> {
    if messages.is_empty() {
        return Err(Error::Provider(
            "cannot send request: messages array is empty".into(),
        ));
    }
    // Assistant messages must carry something: text, parts, or tool calls.
    // An all-empty assistant turn is rejected by several providers.
    for (i, m) in messages.iter().enumerate() {
        if m.role == Role::Assistant
            && m.tool_calls.is_empty()
            && match &m.content {
                MessageContent::Text(s) => s.trim().is_empty(),
                MessageContent::Parts(parts) => parts.is_empty(),
            }
        {
            return Err(Error::Provider(format!(
                "cannot send request: messages[{i}] is an assistant message with no content and no tool calls"
            )));
        }
    }
    // Every tool message's tool_call_id must reference a tool call present in
    // some assistant message — a dangling id means the referenced assistant
    // turn was dropped from the history.
    let mut known_ids: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for m in messages {
        for tc in &m.tool_calls {
            known_ids.insert(tc.id.as_str());
        }
    }
    for (i, m) in messages.iter().enumerate() {
        if m.role == Role::Tool {
            match &m.tool_call_id {
                Some(id) if known_ids.contains(id.as_str()) => {}
                Some(id) => {
                    return Err(Error::Provider(format!(
                        "cannot send request: messages[{i}] is a tool message whose tool_call_id {id:?} has no matching assistant tool call"
                    )));
                }
                None => {
                    return Err(Error::Provider(format!(
                        "cannot send request: messages[{i}] is a tool message with no tool_call_id"
                    )));
                }
            }
        }
    }
    Ok(())
}
