// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Per-provider reasoning-state policy — Rule 2 of the reasoning-state-
//! continuity spec.
//!
//! Reasoning models pause their internal reasoning when they call a tool, and
//! every provider resumes it differently: which wire field carries the
//! reasoning, whether the server holds the state, whether a missing field is a
//! hard 400 or silent degradation. Rather than branching on provider kind
//! inside the request builder (so that adding a provider is a code change),
//! each provider's reasoning contract is captured here as a [`ProviderPolicy`]
//! value. Adding a provider is then a **data change** — a new entry in the
//! [`ProviderPolicy::for_kind_and_model`] registry — not a new branch in the
//! builder. The one name-keyed behavior users may need to override per
//! endpoint/model — the off encoding ([`reasoning_effort_off_wire_value`]) —
//! has a config escape hatch (`reasoning_effort_off_wire` in
//! `endpoints.toml`); this policy is its zero-config fallback.
//!
//! The values are opaque by design: the request builder echoes them verbatim
//! and never parses, inspects, truncates, or regenerates them (Rule 1). This
//! module only describes *which* field/param each provider uses; it does not
//! touch the bytes.
//!
//! # Known providers (per the spec)
//!
//! | Provider | `reasoning_required` | `stateful_key` | `reasoning_field` | historical retention | extras |
//! |---|---|---|---|---|---|
//! | Gemini 3.x | `true` | `PreviousInteractionId` | — | signatures-only history | portable within Google |
//! | Claude | `true` (tool loops) | `None` | — | never strip | thinking blocks carry `signature` |
//! | OpenAI Responses | `false` | `PreviousResponseId` | — | never strip | `include: [reasoning.encrypted_content]` |
//! | DeepSeek | `true` | `None` | `reasoning_content` | strip history under pressure | — |
//! | Qwen / Kimi | `false` | `None` | `reasoning_content` | never strip | — |
//! | GLM 4.7+ | `false` | `None` | `reasoning_content` | never strip | — |
//! | vLLM (self-hosted) | `false` | `None` | version-detected | never strip | — |
//!
//! The *historical retention* column records how far back the raw reasoning
//! payload must be echoed verbatim (Rule 1). Providers whose continuity does
//! not need historical thinking *text* may drop it from older assistant turns
//! to slow context ballooning (Gemini needs only `thought_signature`; DeepSeek
//! tolerates dropping the oldest turns under context pressure) — see
//! [`ReasoningRetention`].
//!
//! `stateful_key` describes the provider's *capability*; whether the harness
//! actually uses the stateful path is decided by the client (the stateless
//! raw-echo path is always available and must pass the same tests — Rule 3).

use std::collections::HashMap;

use serde_json::Value;

use super::ProviderKind;

/// The server-side state handle a provider accepts in lieu of resending full
/// history.
///
/// When a policy sets this, the server holds the reasoning state itself: the
/// request builder sends this key with the previous turn's server-assigned id
/// and the server replays the reasoning. `None` means stateless replay — the
/// builder echoes the raw assistant turn (Rule 1) and the provider
/// reconstructs continuity from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatefulKey {
    /// OpenAI Responses API — `previous_response_id`.
    PreviousResponseId,
    /// Gemini Interactions API — `previous_interaction_id`.
    PreviousInteractionId,
}

/// How much of a provider's historical reasoning payload must be echoed
/// verbatim across turns (Rule 1 scope).
///
/// Thinking tokens routinely outnumber completion tokens 3:1 to 10:1, so on
/// long sessions the echoed reasoning balloons the context and pushes requests
/// past proxy cache ceilings. Some vendors do not need historical thinking
/// text for continuity: Gemini 3.x requires only `thought_signature` on tool
/// calls (the thinking *text* is droppable), while DeepSeek requires a
/// `reasoning_content` KEY (any value, even `""`) on the assistant turn that
/// owns the request tail — the last message, or the issuer of trailing tool
/// results — AND on every older assistant turn that carries `tool_calls`
/// (live-verified 2026-12-23 against api.deepseek.com, bug plan c9b5cbe4;
/// widened by the third recurrence 2026-09-12, bug plan c6cb69f7: a
/// cross-vendor re-entry left the open tool-call chain keyless while the
/// tail-owner was keyed, and the API still returned 400). Historical
/// TEXT-ONLY turns tolerate a missing key entirely. The retention value
/// records, per provider, how many of the most recent assistant turns are
/// protected and whether the remainder may be stripped — `keep_recent >= 1`
/// keeps the tail turn's key intact even under pressure, and the builder
/// re-adds the bare key on stripped tool-call turns, so stripping historical
/// text never violates the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReasoningRetention {
    /// The number of most-recent assistant turns whose reasoning payload is
    /// always echoed verbatim. Ages are counted from the end of history
    /// (`0` = the most recent assistant turn, `1` = the one before it, …).
    /// [`Self::NEVER_STRIP`] (`usize::MAX`) protects the entire history.
    pub keep_recent: usize,
    /// `true` when reasoning text older than [`keep_recent`](Self::keep_recent)
    /// may be stripped unconditionally (continuity does not need it). When
    /// `false`, historical reasoning is stripped only under context pressure
    /// (`under_pressure` in [`strips_reasoning`](Self::strips_reasoning)).
    pub historical_droppable: bool,
    /// `true` when continuity depends on signature-bearing keys that must
    /// survive a strip (Gemini 3.x per-tool-call `thought_signature`). The
    /// request builder passes this flag to the strip helper so signatures are
    /// preserved even when reasoning text is dropped.
    pub signatures_required: bool,
}

impl ReasoningRetention {
    /// Retain the reasoning payload on every assistant turn — the Rule 1
    /// default. Used by every provider whose continuity requires the full
    /// historical payload under all conditions (Claude, OpenAI Responses,
    /// Qwen, Kimi, GLM, vLLM). Gemini uses a signatures-only policy and
    /// DeepSeek a pressure-triggered one instead.
    pub const NEVER_STRIP: ReasoningRetention = ReasoningRetention {
        keep_recent: usize::MAX,
        historical_droppable: false,
        signatures_required: false,
    };

    /// Whether the reasoning payload may be stripped from the assistant turn
    /// `assistant_age` turns back from the end of history.
    ///
    /// Age `0` — the most recent assistant turn — is always protected: the
    /// provider resumes reasoning from it on the next request, so stripping it
    /// breaks continuity regardless of `keep_recent`. Turns newer than
    /// [`keep_recent`](Self::keep_recent) are likewise protected. Older turns
    /// strip when [`historical_droppable`](Self::historical_droppable) is set
    /// (continuity never needs the text), or when `under_pressure` is set
    /// (the request is near the proxy cache ceiling) for providers whose
    /// historical payload is only conditionally droppable (DeepSeek).
    pub fn strips_reasoning(&self, assistant_age: usize, under_pressure: bool) -> bool {
        if assistant_age == 0 {
            return false;
        }
        if self.keep_recent == usize::MAX {
            return false;
        }
        if assistant_age < self.keep_recent {
            return false;
        }
        self.historical_droppable || under_pressure
    }
}

/// The vendor family a provider belongs to.
///
/// Used by Rule 5 (model switching): a switch to a different model of the
/// *same* vendor resends the prior reasoning blocks (the backend handles
/// compatibility), while a *cross-vendor* switch strips reasoning entirely
/// and sets `reasoning_stripped: true` (signatures are encrypted per-provider
/// and 400 or are silently ignored across vendors).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Vendor {
    /// Google — Gemini.
    Google,
    /// Anthropic — Claude.
    Anthropic,
    /// OpenAI — GPT / Responses.
    OpenAI,
    /// DeepSeek.
    DeepSeek,
    /// Alibaba — Qwen.
    Alibaba,
    /// Moonshot — Kimi.
    Moonshot,
    /// Zhipu — GLM.
    Zhipu,
    /// Self-hosted (vLLM / Ollama / LM Studio).
    SelfHosted,
    /// Any other / unrecognized provider.
    Other,
}

/// One provider's reasoning-state-continuity contract.
///
/// The request builder consults this value instead of branching on provider
/// kind, so adding a provider is a data change (a new registry entry in
/// [`ProviderPolicy::for_kind_and_model`]), not a code change. See the spec's
/// Rule 2.
#[derive(Debug, Clone)]
pub struct ProviderPolicy {
    /// Human-readable provider name (logging / trace attribution).
    pub name: String,
    /// `true` when the provider hard-400s if its reasoning field is missing
    /// from a prior assistant turn (Gemini 3.x, DeepSeek thinking mode). For
    /// Claude this is `true` *during tool loops only* — the enforcement layer
    /// applies that condition.
    pub reasoning_required: bool,
    /// When set, the server holds the reasoning state; the builder sends this
    /// key with the previous turn's id instead of resending history. `None`
    /// means stateless replay (the builder echoes the raw assistant turn).
    pub stateful_key: Option<StatefulKey>,
    /// The wire field that carries reasoning on an assistant message
    /// (`"reasoning"` or `"reasoning_content"`). `None` when the provider
    /// carries reasoning in a different shape (Anthropic thinking blocks, OpenAI
    /// Responses reasoning items, Gemini `thoughtSignature` parts).
    pub reasoning_field: Option<&'static str>,
    /// Provider-specific request params to merge into the request body when
    /// reasoning state is replayed statelessly (e.g. OpenAI Responses
    /// `{"include": ["reasoning.encrypted_content"]}`).
    pub include_params: HashMap<String, Value>,
    /// How much historical reasoning this provider requires verbatim (Rule 1
    /// scope). [`ReasoningRetention::NEVER_STRIP`] for providers whose
    /// continuity needs the full payload; droppable-history policies for
    /// Gemini (signatures only) and DeepSeek (pressure-triggered).
    pub retention: ReasoningRetention,
    /// Whether reasoning signatures survive a switch to a different model of
    /// the *same* vendor (true for Google models). Cross-vendor switches always
    /// strip reasoning (Rule 5) regardless of this flag.
    pub signatures_portable_across_models: bool,
    /// The vendor family (Rule 5 cross-vendor detection).
    pub vendor: Vendor,
    /// `true` when this vendor cannot receive trailing SYSTEM messages: its
    /// models echo them instead of answering the user (DeepSeek:
    /// sentinel-mirror 5cf5469c + the 2027-01-11 exit-note loop). Local-kind
    /// (Ollama) gets the same placement for an analogous reason — its API
    /// allows a system message only in first position. For such vendors
    /// `turn.rs` sends the volatile tail + `prompt::CONTEXT_FOOTER` as trailing
    /// USER messages: the request still ends the way a normal client turn does
    /// (nothing to mirror), while `messages[0]` stays byte-stable — the prefix a
    /// prompt cache keys on. Folding the tail into `messages[0]` instead (the
    /// previous strategy) made every `complete_step` progress bump a full cache
    /// miss: measured 2026-09-14, 5.1-9.0% hit with 115-120K tokens re-billed
    /// per check-off (.coding/analysis/cache-hit-6-aggregates.txt).
    pub tail_as_user_messages: bool,
}

impl ProviderPolicy {
    /// Resolve the reasoning-state policy for a provider kind + model id.
    ///
    /// The model id is matched case-insensitively against the known
    /// sub-providers (Gemini, DeepSeek, Qwen, Kimi, GLM) for OpenAI-compatible
    /// endpoints; an Anthropic-kind endpoint always resolves to the Claude
    /// policy. A self-hosted (`Local`) endpoint whose model id matches no known
    /// sub-provider resolves to the vLLM default (its `reasoning_field` is
    /// refined by startup version detection — see [`vllm_reasoning_field`]).
    ///
    /// This is a pure function of `(kind, model)`; the request builder calls it
    /// at build time, so no policy needs to be cached on the client config.
    pub fn for_kind_and_model(kind: ProviderKind, model: &str) -> ProviderPolicy {
        let m = model.to_ascii_lowercase();
        match kind {
            ProviderKind::Anthropic => claude(),
            ProviderKind::OpenAI | ProviderKind::Local => {
                if m.contains("gemini") {
                    gemini()
                } else if m.contains("deepseek") {
                    deepseek()
                } else if m.contains("qwen") {
                    qwen()
                } else if m.contains("kimi") {
                    kimi()
                } else if m.contains("glm") {
                    glm()
                } else if kind == ProviderKind::Local {
                    // Self-hosted (vLLM/Ollama/LM Studio) with an unrecognized
                    // model id. vLLM's reasoning field depends on the server
                    // version; startup detection refines it later. Default to
                    // "reasoning_content" (the older, more widely-deployed key)
                    // until then.
                    vllm_default()
                } else {
                    openai_responses()
                }
            }
        }
    }

    /// Whether two policies are from different vendors (Rule 5: a cross-vendor
    /// switch strips reasoning and sets `reasoning_stripped`).
    pub fn is_cross_vendor(&self, other: &ProviderPolicy) -> bool {
        self.vendor != other.vendor
    }
}

/// The wire encoding of the harness's `"off"` reasoning effort for this
/// provider kind + model.
///
/// `"off"` (omit the field) is the harness's universal off, but it is not a
/// valid `reasoning_effort` variant anywhere: DeepSeek's enum is
/// `none|minimal|low|medium|high|xhigh|max` and any other string — including
/// `"off"` — is rejected with an instant, non-retryable HTTP 400. DeepSeek
/// also has no absent-field way to *disable* thinking on its thinking models:
/// its `"none"` variant is the explicit off. DeepSeek-family models therefore
/// encode `"off"` as `"none"`; every other provider omits the field (the
/// historical behavior). Pure, so it is unit-testable in isolation.
///
/// This is the **zero-config fallback**: the endpoint/model
/// `reasoning_effort_off_wire` config (resolved by
/// [`crate::config::Endpoint::reasoning_effort_off_wire_for`]) takes
/// precedence when set — the escape hatch for renamed, aliased, or
/// fine-tuned DeepSeek models whose names this name-based policy can't see.
pub fn reasoning_effort_off_wire_value(kind: ProviderKind, model: &str) -> Option<&'static str> {
    if ProviderPolicy::for_kind_and_model(kind, model).vendor == Vendor::DeepSeek {
        Some("none")
    } else {
        None
    }
}

/// Resolve the reasoning field name for a self-hosted vLLM server from its
/// reported version string.
///
/// vLLM 0.20 and later use `"reasoning"`; earlier versions use
/// `"reasoning_content"`. The wrong key fails silently — vLLM drops
/// `reasoning_content` on incoming assistant messages — so the version must be
/// detected at startup (the stateful-path step wires the probe). This helper
/// is exposed so the detection logic is unit-testable in isolation.
pub fn vllm_reasoning_field(server_version: &str) -> &'static str {
    let v = server_version.trim_start_matches('v');
    let mut parts = v.split('.');
    let major = parts.next().and_then(leading_u32);
    let minor = parts.next().and_then(leading_u32);
    match (major, minor) {
        (Some(maj), _) if maj >= 1 => "reasoning",
        (Some(0), Some(min)) if min >= 20 => "reasoning",
        _ => "reasoning_content",
    }
}

/// Parse the leading run of ASCII digits in `s` as a `u32` (`"20rc1"` → `20`).
fn leading_u32(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

// --- Known-provider policies (the spec's registry values) -------------------

/// Claude (Anthropic Messages API). Reasoning lives in `thinking` blocks that
/// carry a per-block `signature`; the builder echoes the assistant content
/// array verbatim. `reasoning_required` is `true` during tool loops only.
fn claude() -> ProviderPolicy {
    ProviderPolicy {
        name: "claude".into(),
        reasoning_required: true,
        stateful_key: None,
        reasoning_field: None,
        include_params: HashMap::new(),
        retention: ReasoningRetention::NEVER_STRIP,
        signatures_portable_across_models: true,
        vendor: Vendor::Anthropic,
        tail_as_user_messages: false,
    }
}

/// Gemini 3.x. Reasoning lives in `thoughtSignature` (inside `functionCall`
/// parts, or the last part of a `generateContent` response). Hard 400 if
/// missing. Signatures are portable within Google models.
///
/// **Stateful path:** `None` — this app accesses Gemini exclusively via
/// OpenAI-compatible proxies (LiteLLM, OpenRouter, Google's OpenAI-compat
/// endpoint), not the native Interactions API. The stateless `thought_signature`
/// path (captured in `raw` and echoed verbatim by the request builder) IS
/// Gemini's reasoning-continuity mechanism. The native Interactions API
/// (`previous_interaction_id`) would require a separate client path that is
/// out of scope until a direct Gemini endpoint is configured.
///
/// **Historical retention:** thinking *text* is not required for continuity —
/// only `thought_signature` is — so history older than the most recent
/// assistant turn may have its thinking text stripped (signatures always
/// survive) to slow context ballooning.
fn gemini() -> ProviderPolicy {
    ProviderPolicy {
        name: "gemini".into(),
        reasoning_required: true,
        stateful_key: None,
        reasoning_field: None,
        include_params: HashMap::new(),
        retention: ReasoningRetention {
            keep_recent: 1,
            historical_droppable: true,
            signatures_required: true,
        },
        signatures_portable_across_models: true,
        vendor: Vendor::Google,
        tail_as_user_messages: false,
    }
}

/// OpenAI Responses API. Reasoning lives in `encrypted_content` on reasoning
/// items. Soft enforcement (degrades quality). When stateless, the request
/// carries `include: ["reasoning.encrypted_content"]`.
fn openai_responses() -> ProviderPolicy {
    let mut include_params = HashMap::new();
    include_params.insert(
        "include".into(),
        serde_json::json!(["reasoning.encrypted_content"]),
    );
    ProviderPolicy {
        name: "openai".into(),
        reasoning_required: false,
        stateful_key: Some(StatefulKey::PreviousResponseId),
        reasoning_field: None,
        include_params,
        retention: ReasoningRetention::NEVER_STRIP,
        signatures_portable_across_models: true,
        vendor: Vendor::OpenAI,
        tail_as_user_messages: false,
    }
}

/// DeepSeek (thinking mode). Reasoning lives in `reasoning_content` on the
/// assistant message. Hard 400 in thinking mode if missing.
///
/// **Tail as user messages:** DeepSeek models echo trailing system blocks
/// instead of answering the user (sentinel-mirror 5cf5469c + the 2027-01-11
/// exit-note loop), so `tail_as_user_messages` is `true`: the volatile tail +
/// CONTEXT_FOOTER are sent as trailing USER messages, which keeps the request
/// shaped like a normal client turn and leaves messages[0] (the cached prefix)
/// byte-stable across `complete_step` progress bumps.
///
/// **Historical retention:** the most recent assistant turn must always carry
/// `reasoning_content`, and every older turn carrying `tool_calls` must carry
/// the KEY too (the builder re-adds the bare `""` key when the strip or a
/// cross-vendor re-entry removed it — plan c6cb69f7); the historical reasoning
/// TEXT is what is droppable, and only under context pressure (near the proxy
/// cache ceiling), never unconditionally.
fn deepseek() -> ProviderPolicy {
    ProviderPolicy {
        name: "deepseek".into(),
        reasoning_required: true,
        stateful_key: None,
        reasoning_field: Some("reasoning_content"),
        include_params: HashMap::new(),
        retention: ReasoningRetention {
            keep_recent: 1,
            historical_droppable: false,
            signatures_required: false,
        },
        signatures_portable_across_models: false,
        vendor: Vendor::DeepSeek,
        tail_as_user_messages: true,
    }
}

/// Qwen 3.5/3.6. Reasoning in `reasoning_content`; silent degradation.
fn qwen() -> ProviderPolicy {
    ProviderPolicy {
        name: "qwen".into(),
        reasoning_required: false,
        stateful_key: None,
        reasoning_field: Some("reasoning_content"),
        include_params: HashMap::new(),
        retention: ReasoningRetention::NEVER_STRIP,
        signatures_portable_across_models: false,
        vendor: Vendor::Alibaba,
        tail_as_user_messages: false,
    }
}

/// Kimi K2.5/K2.6. Reasoning in `reasoning_content`; silent degradation.
fn kimi() -> ProviderPolicy {
    ProviderPolicy {
        name: "kimi".into(),
        reasoning_required: false,
        stateful_key: None,
        reasoning_field: Some("reasoning_content"),
        include_params: HashMap::new(),
        retention: ReasoningRetention::NEVER_STRIP,
        signatures_portable_across_models: false,
        vendor: Vendor::Moonshot,
        tail_as_user_messages: false,
    }
}

/// GLM 4.7+. Reasoning in `reasoning_content`; silent degradation.
fn glm() -> ProviderPolicy {
    ProviderPolicy {
        name: "glm".into(),
        reasoning_required: false,
        stateful_key: None,
        reasoning_field: Some("reasoning_content"),
        include_params: HashMap::new(),
        retention: ReasoningRetention::NEVER_STRIP,
        signatures_portable_across_models: false,
        vendor: Vendor::Zhipu,
        tail_as_user_messages: false,
    }
}

/// Self-hosted vLLM default (model id matched no known sub-provider). The
/// `reasoning_field` is refined by startup version detection
/// ([`vllm_reasoning_field`]); until then it defaults to
/// `"reasoning_content"`.
fn vllm_default() -> ProviderPolicy {
    ProviderPolicy {
        name: "vllm".into(),
        reasoning_required: false,
        stateful_key: None,
        reasoning_field: Some("reasoning_content"),
        include_params: HashMap::new(),
        retention: ReasoningRetention::NEVER_STRIP,
        signatures_portable_across_models: false,
        vendor: Vendor::SelfHosted,
        tail_as_user_messages: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_policy_for_anthropic_kind() {
        let p = ProviderPolicy::for_kind_and_model(ProviderKind::Anthropic, "claude-sonnet-4-5");
        assert_eq!(p.vendor, Vendor::Anthropic);
        assert!(p.reasoning_required);
        assert_eq!(p.stateful_key, None);
        assert_eq!(p.reasoning_field, None);
        assert!(p.signatures_portable_across_models);
        assert!(p.include_params.is_empty());
        assert!(!p.tail_as_user_messages);
    }

    #[test]
    fn gemini_policy_detected_by_model_id() {
        let p = ProviderPolicy::for_kind_and_model(ProviderKind::OpenAI, "gemini-3.0-flash");
        assert_eq!(p.vendor, Vendor::Google);
        assert!(p.reasoning_required);
        // stateful_key is None: Gemini is accessed via OpenAI-compatible proxies
        // (not the native Interactions API), so the stateless thought_signature
        // path (raw echo) is the continuity mechanism.
        assert_eq!(p.stateful_key, None);
        assert!(p.signatures_portable_across_models);
    }

    #[test]
    fn deepseek_policy_detected_by_model_id() {
        let p = ProviderPolicy::for_kind_and_model(ProviderKind::OpenAI, "deepseek-v4-flash");
        assert_eq!(p.vendor, Vendor::DeepSeek);
        assert!(p.reasoning_required);
        assert_eq!(p.stateful_key, None);
        assert_eq!(p.reasoning_field, Some("reasoning_content"));
        // DeepSeek echoes trailing system blocks, so its requests carry the
        // volatile tail + footer as trailing USER messages (2027-01-11
        // exit-note loop) and keep messages[0] byte-stable.
        assert!(p.tail_as_user_messages);
    }

    #[test]
    fn reasoning_effort_off_wire_value_none_for_deepseek() {
        // DeepSeek's enum is none|minimal|low|medium|high|xhigh|max — the
        // harness's "off" encodes as the explicit thinking-off "none".
        assert_eq!(
            reasoning_effort_off_wire_value(ProviderKind::OpenAI, "deepseek-v4-flash"),
            Some("none")
        );
        assert_eq!(
            reasoning_effort_off_wire_value(ProviderKind::Local, "deepseek-chat"),
            Some("none")
        );
    }

    #[test]
    fn reasoning_effort_off_wire_value_omits_for_other_providers() {
        // No other provider has a "none" variant: "off" omits the field.
        assert_eq!(reasoning_effort_off_wire_value(ProviderKind::OpenAI, "gpt-4o"), None);
        assert_eq!(
            reasoning_effort_off_wire_value(ProviderKind::Local, "qwen-3.5-coder"),
            None
        );
        // Kind gate: the Messages API has no reasoning_effort field, so an
        // Anthropic endpoint never encodes "off" — even for a deepseek-named
        // model id.
        assert_eq!(
            reasoning_effort_off_wire_value(ProviderKind::Anthropic, "deepseek-v4-flash"),
            None
        );
    }

    #[test]
    fn qwen_policy_detected_by_model_id() {
        let p = ProviderPolicy::for_kind_and_model(ProviderKind::OpenAI, "qwen-3.5-coder");
        assert_eq!(p.vendor, Vendor::Alibaba);
        assert!(!p.reasoning_required);
        assert_eq!(p.reasoning_field, Some("reasoning_content"));
    }

    #[test]
    fn kimi_policy_detected_by_model_id() {
        let p = ProviderPolicy::for_kind_and_model(ProviderKind::OpenAI, "kimi-k2.6");
        assert_eq!(p.vendor, Vendor::Moonshot);
        assert_eq!(p.reasoning_field, Some("reasoning_content"));
    }

    #[test]
    fn glm_policy_detected_by_model_id() {
        let p = ProviderPolicy::for_kind_and_model(ProviderKind::OpenAI, "glm-4.7");
        assert_eq!(p.vendor, Vendor::Zhipu);
        assert_eq!(p.reasoning_field, Some("reasoning_content"));
        // GLM keeps the trailing tail + footer placement (cache law).
        assert!(!p.tail_as_user_messages);
    }

    #[test]
    fn openai_policy_for_unrecognized_openai_model() {
        let p = ProviderPolicy::for_kind_and_model(ProviderKind::OpenAI, "gpt-4o");
        assert_eq!(p.vendor, Vendor::OpenAI);
        assert!(!p.reasoning_required);
        assert_eq!(p.stateful_key, Some(StatefulKey::PreviousResponseId));
        assert_eq!(
            p.include_params.get("include"),
            Some(&serde_json::json!(["reasoning.encrypted_content"]))
        );
    }

    #[test]
    fn vllm_default_for_unrecognized_local_model() {
        let p = ProviderPolicy::for_kind_and_model(ProviderKind::Local, "llama3.1");
        assert_eq!(p.vendor, Vendor::SelfHosted);
        assert_eq!(p.reasoning_field, Some("reasoning_content"));
        assert_eq!(p.stateful_key, None);
    }

    #[test]
    fn model_id_matching_is_case_insensitive() {
        let p = ProviderPolicy::for_kind_and_model(ProviderKind::OpenAI, "DeepSeek-V4");
        assert_eq!(p.vendor, Vendor::DeepSeek);
    }

    #[test]
    fn cross_vendor_detection() {
        let claude = ProviderPolicy::for_kind_and_model(ProviderKind::Anthropic, "claude");
        let openai = ProviderPolicy::for_kind_and_model(ProviderKind::OpenAI, "gpt-4o");
        assert!(claude.is_cross_vendor(&openai));
        assert!(openai.is_cross_vendor(&claude));
        // Same vendor, different model → NOT cross-vendor (Rule 5: resend).
        let g1 = ProviderPolicy::for_kind_and_model(ProviderKind::OpenAI, "gemini-3.0");
        let g2 = ProviderPolicy::for_kind_and_model(ProviderKind::OpenAI, "gemini-3.5");
        assert!(!g1.is_cross_vendor(&g2));
    }

    #[test]
    fn retention_values_per_provider() {
        // Gemini: thinking text droppable, signatures required for continuity.
        let g = ProviderPolicy::for_kind_and_model(ProviderKind::OpenAI, "gemini-3.0-flash");
        assert_eq!(g.retention.keep_recent, 1);
        assert!(g.retention.historical_droppable);
        assert!(g.retention.signatures_required);
        // DeepSeek: history retained unless under pressure; no signatures.
        let d = ProviderPolicy::for_kind_and_model(ProviderKind::OpenAI, "deepseek-v4-flash");
        assert_eq!(d.retention.keep_recent, 1);
        assert!(!d.retention.historical_droppable);
        assert!(!d.retention.signatures_required);
        // Every other provider: never strip (Rule 1 verbatim echo).
        for (kind, model) in [
            (ProviderKind::Anthropic, "claude-sonnet-4-5"),
            (ProviderKind::OpenAI, "gpt-4o"),
            (ProviderKind::OpenAI, "qwen-3.5-coder"),
            (ProviderKind::OpenAI, "kimi-k2.6"),
            (ProviderKind::OpenAI, "glm-4.7"),
            (ProviderKind::Local, "llama3.1"),
        ] {
            let p = ProviderPolicy::for_kind_and_model(kind, model);
            assert_eq!(p.retention, ReasoningRetention::NEVER_STRIP, "{model}");
        }
    }

    #[test]
    fn strips_reasoning_matrix() {
        let gemini = ReasoningRetention {
            keep_recent: 1,
            historical_droppable: true,
            signatures_required: true,
        };
        // Age 0 (the most recent assistant turn) is never stripped.
        assert!(!gemini.strips_reasoning(0, false));
        assert!(!gemini.strips_reasoning(0, true));
        // Historical turns strip unconditionally (thinking text is droppable).
        assert!(gemini.strips_reasoning(1, false));
        assert!(gemini.strips_reasoning(5, true));

        let deepseek = ReasoningRetention {
            keep_recent: 1,
            historical_droppable: false,
            signatures_required: false,
        };
        assert!(!deepseek.strips_reasoning(0, true));
        // Below pressure: history retained (continuity needs it).
        assert!(!deepseek.strips_reasoning(1, false));
        assert!(!deepseek.strips_reasoning(5, false));
        // Under pressure: history strips to stay below the proxy cliff.
        assert!(deepseek.strips_reasoning(1, true));
        assert!(deepseek.strips_reasoning(5, true));

        // NEVER_STRIP never strips, even under pressure at any age.
        assert!(!ReasoningRetention::NEVER_STRIP.strips_reasoning(0, true));
        assert!(!ReasoningRetention::NEVER_STRIP.strips_reasoning(10, true));
    }

    #[test]
    fn vllm_reasoning_field_by_version() {
        // 0.20+ → "reasoning".
        assert_eq!(vllm_reasoning_field("0.20.1"), "reasoning");
        assert_eq!(vllm_reasoning_field("v0.20.0"), "reasoning");
        assert_eq!(vllm_reasoning_field("0.21"), "reasoning");
        // 1.x+ → "reasoning".
        assert_eq!(vllm_reasoning_field("1.0.0"), "reasoning");
        // Pre-0.20 → "reasoning_content".
        assert_eq!(vllm_reasoning_field("0.19.0"), "reasoning_content");
        assert_eq!(vllm_reasoning_field("0.10"), "reasoning_content");
        // Unparseable → conservative "reasoning_content".
        assert_eq!(vllm_reasoning_field("unknown"), "reasoning_content");
    }

    /// BUG 10 (2027-01-09 provider-comms review, LOW): `template_kwargs` —
    /// GLM `clear_thinking: false`, Qwen/Kimi `preserve_thinking: true` —
    /// carried doc comments claiming vendor must-send requirements, but no
    /// request builder ever read the field: the params never left the
    /// process, and neither name appears in current public vendor docs. The
    /// field, its registry entries, and the claims were deleted (2027-01-11);
    /// wiring them back requires the rolled-back vendor-policy direction to
    /// be re-decided and the param names verified against the proxy's
    /// accepted params first. This guard scans the module's non-test source
    /// and fails if the never-sent registry is reintroduced.
    #[test]
    fn policy_source_carries_no_never_sent_template_kwargs() {
        let src = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/src/provider/policy.rs"
        ))
        .expect("policy.rs readable next to the crate manifest");
        let (non_test, _) = src
            .split_once("mod tests")
            .expect("tests module marker present");
        for needle in ["template_kwargs", "clear_thinking", "preserve_thinking"] {
            assert!(
                !non_test.contains(needle),
                "`{needle}` found in policy.rs non-test source: the never-sent \
                 template_kwargs registry was deleted (BUG 10) — re-adding it \
                 needs the vendor-policy direction re-decided and the param \
                 names verified against the proxy first"
            );
        }
    }
}
