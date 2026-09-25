// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

//! Unified error type for the Mnemo library.
//!
//! Library code uses `thiserror` for typed errors; the binary boundary (`main.rs`)
//! converts these into `anyhow::Error` for reporting.

use std::path::PathBuf;

/// The unified error type used throughout the library.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),

    #[error("project error: {0}")]
    Project(String),

    #[error("provider error: {0}")]
    Provider(String),

    #[error("tool error: {0}")]
    Tool(String),

    #[error("workflow error: {0}")]
    Workflow(String),

    /// No plan exists on the stack — an operation that requires an active plan
    /// (`update_plan`, `complete_step`, `abandon_plan`) was called with an
    /// empty stack. Discriminates the "no plan exists" case from other
    /// [`Workflow`](Error::Workflow) failures so callers can react distinctly.
    #[error("workflow error: no plan exists")]
    WorkflowNoPlan,

    /// An operation was attempted in the wrong workflow state (e.g.
    /// `update_plan` outside `Executing`/`Reviewing`, `finish` outside
    /// `Reviewing`).
    /// `current` and `expected` are the [`WorkflowState`](crate::workflow::WorkflowState)
    /// display strings. Discriminates the wrong-state case from other
    /// [`Workflow`](Error::Workflow) failures.
    #[error("workflow error: wrong state (current: {current}, expected: {expected})")]
    WorkflowWrongState { current: String, expected: String },

    #[error("memory error: {0}")]
    Memory(String),

    #[error("safety rules error: {0}")]
    Safety(String),

    #[error("runtime error: {0}")]
    Runtime(String),

    #[error("browser error: {0}")]
    Browser(String),

    #[error("mcp error: {0}")]
    Mcp(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("toml parse error: {0}")]
    Toml(#[from] toml::de::Error),

    #[error("toml serialize error: {0}")]
    TomlSer(#[from] toml::ser::Error),

    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("path outside project root: {0}")]
    PathOutsideRoot(PathBuf),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid input: {0}")]
    InvalidInput(String),
}

impl Error {
    /// Whether this error is non-retryable — retrying would never succeed
    /// because the underlying condition is permanent (the prompt won't
    /// shrink, the API key won't change, the model won't appear).
    ///
    /// Used by `complete_with_retry` and `run_turn_attempt` to skip the
    /// 3×3 retry stack for errors like context-overflow (502 with
    /// "maximum context length"), auth failures (401/403), and model
    /// not-found (404). Transient errors (500/503/504/connect failures)
    /// remain retryable; a 429 is handled separately by
    /// [`is_rate_limited`](Self::is_rate_limited) — it skips same-provider
    /// retry and triggers the cross-provider fallback instead.
    ///
    /// The check is string-based because provider errors arrive as
    /// `Error::Provider(String)` — the HTTP status and body are folded
    /// into the message text by the provider client.
    pub fn is_non_retryable(&self) -> bool {
        // Serialization bugs are non-retryable (Rule 6): the request builder
        // dropped or mutated a reasoning field the provider requires. Retrying
        // the same request fails identically; stripping reasoning to "fix" it
        // sometimes succeeds but permanently degrades quality.
        if self.is_serialization_bug() {
            return true;
        }
        // A confident failure-triage classification marked the error as a
        // SKIP (plan 02deea7c): the inner retry layer appended
        // `classified needs_user` / `classified permanent` plus a hint, and
        // both retry layers must skip through THIS shared predicate — without
        // it the inner ladder alone would still burn three provider calls on
        // a condition only the user can clear.
        if self.is_classified_skip() {
            return true;
        }
        // Context overflow: the prompt + requested output exceed the
        // model's context window — permanent. The shared classification
        // lives in is_context_overflow (compaction's mechanical fallback
        // relies on it, so the size needles must stay in one place).
        if self.is_context_overflow() {
            return true;
        }
        let s = match self {
            Error::Provider(msg) => msg.to_lowercase(),
            _ => return false,
        };
        // Auth: the API key is wrong or missing. Retrying never helps.
        s.contains("unauthorized")
            || s.contains("auth_error")
            || s.contains("virtual key expected")
            // Model not found: the model name is wrong or unavailable.
            // Retrying never helps.
            || s.contains("notfounderror")
            || (s.contains("model") && s.contains("not found"))
    }

    /// Whether a confident failure-triage classification marked this error as
    /// a SKIP — the inner retry layer (`complete_with_retry`) appends
    /// `classified needs_user` / `classified permanent` (the classifier's
    /// canonical labels) plus a hint to the error text when the classifier
    /// says retrying cannot help (plan 02deea7c), and
    /// [`is_non_retryable`](Self::is_non_retryable) then makes BOTH retry
    /// layers skip their stacks without re-classifying.
    ///
    /// The check is string-based for the same reason as the others: the
    /// classification rides `Error::Provider(String)`.
    pub fn is_classified_skip(&self) -> bool {
        let s = match self {
            Error::Provider(msg) => msg.to_lowercase(),
            _ => return false,
        };
        s.contains("classified needs_user") || s.contains("classified permanent")
    }

    /// Whether this error is a context-window overflow — the prompt + output
    /// exceed the model's context window.
    ///
    /// The shared classifier for size errors: [`is_non_retryable`](Self::is_non_retryable)
    /// delegates its size needles here (retrying the same oversized prompt
    /// never succeeds), and compaction's mechanical fallback uses it to
    /// recognize when the summarizer cannot accept the history — the budget
    /// guard makes the summarization prompt fit by construction, and a
    /// provider whose reported window is smaller than its `Capabilities` (or
    /// wording the budget missed) falls back to dropping the old region
    /// instead of wedging the session.
    ///
    /// The check is string-based because provider errors arrive as
    /// `Error::Provider(String)` — the HTTP status and body are folded into
    /// the message text by the provider client. Covers native endpoints,
    /// LiteLLM proxy wrappers, cloud gateway 500/502 errors that embed
    /// provider context-overflow messages, native Anthropic Messages-API
    /// wording ("prompt is too long: N tokens > M maximum"), and
    /// DeepSeek/GLM-style "longer than the limit" 400s.
    pub fn is_context_overflow(&self) -> bool {
        let s = match self {
            Error::Provider(msg) => msg.to_lowercase(),
            _ => return false,
        };
        s.contains("maximum context length")
            || s.contains("context length is")
            || s.contains("max_completion_tokens")
            || s.contains("max_model_len")
            || s.contains("contextwindowexceedederror")
            || s.contains("context_window_exceeded")
            || s.contains("context_length_exceeded")
            || s.contains("reduce the length of the input prompt")
            || s.contains("reduce the length of the messages")
            || s.contains("exceeds the context window")
            || s.contains("exceeded the context window")
            || s.contains("longer than the limit")
            || (s.contains("input_tokens")
                && (s.contains("maximum")
                    || s.contains("exceeds")
                    || s.contains("exceeded")
                    || s.contains("too long")
                    || s.contains("greater than")))
            // Native Anthropic Messages-API context-overflow wording (the
            // LiteLLM gateway wraps it in OpenAI format, but native endpoints
            // return "prompt is too long: N tokens > M maximum").
            || s.contains("prompt is too long")
    }

    /// Whether this error is a rate-limit (HTTP 429) — the provider rejected
    /// the request because the account/key is out of quota or hit a rate
    /// ceiling.
    ///
    /// Distinct from [`is_non_retryable`](Self::is_non_retryable): a 429 is
    /// NOT permanent (the quota may refill, or a different provider serving the
    /// same model may have its own quota). So it is not classified as
    /// non-retryable. Instead, `complete_with_retry` returns immediately on a
    /// 429 (no same-provider backoff — "stop after a single 429"), and
    /// `run_turn_attempt` tries to switch to the same model on a different
    /// endpoint before giving up.
    ///
    /// The check is string-based because provider errors arrive as
    /// `Error::Provider(String)` — the HTTP status and body are folded into
    /// the message text by the provider client. Matches the wording used by
    /// OpenAI-compatible gateways (LiteLLM, z.ai, GLM) and native Anthropic
    /// endpoints: the status line ("HTTP 429"), the standard reason phrase
    /// ("too many requests"), and the quota-exhausted bodies some gateways
    /// return ("rate limit", "limit exhausted" — e.g. GLM's "Weekly/Monthly
    /// Limit Exhausted").
    pub fn is_rate_limited(&self) -> bool {
        let s = match self {
            Error::Provider(msg) => msg.to_lowercase(),
            _ => return false,
        };
        s.contains("http 429")
            || s.contains("too many requests")
            || s.contains("rate limit")
            || s.contains("limit exhausted")
    }

    /// Whether this error is a reasoning-state serialization bug (Rule 6).
    ///
    /// These 400s mean the request builder dropped or mutated a reasoning
    /// field the provider requires. They are NOT transient — retrying the
    /// same request fails identically, and stripping reasoning to "fix" the
    /// retry sometimes succeeds but permanently degrades agent quality in a
    /// way no test catches. The error must be surfaced (not retried) with the
    /// offending request body logged for debugging.
    ///
    /// Detected strings (case-insensitive, matched against the provider error
    /// message):
    /// - `"thought_signature"` + `"missing"` (Gemini 3.x — hard 400)
    /// - `"reasoning_content"` + `"must be passed back"` (DeepSeek thinking)
    /// - `"expected thinking"` + `"found text"` (Claude — content mutated)
    /// - `"modified prior content"` (Claude — assistant content array modified)
    pub fn is_serialization_bug(&self) -> bool {
        let s = match self {
            Error::Provider(msg) => msg.to_lowercase(),
            _ => return false,
        };
        (s.contains("thought_signature") && s.contains("missing"))
            || (s.contains("reasoning_content") && s.contains("must be passed back"))
            || (s.contains("expected thinking") && s.contains("found text"))
            || s.contains("modified prior content")
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Jittered exponential backoff delay for retry attempt `attempt` (1-based),
/// in milliseconds — "equal jitter": half fixed + half random.
///
/// Attempt 1 → 500–1000 ms, 2 → 1000–2000 ms, 3 → 2000–4000 ms (the shift
/// is capped so absurd attempt numbers can't overflow). The fixed half keeps
/// a floor — a retry is never instant, the server gets a moment to recover;
/// the random half desynchronizes concurrent agents (subagents, run-all)
/// retrying against the same recovering endpoint so they don't burst in
/// lockstep (the AWS "Exponential Backoff and Jitter" pattern).
///
/// Randomness is `SystemTime` nanoseconds — cheap, non-crypto, sufficient
/// for desynchronization (this is not a security boundary). Shared by both
/// retry layers: `complete_with_retry` (request attempts) and
/// `run_turn_attempt` (whole-turn attempts).
pub fn retry_backoff_ms(attempt: u32) -> u64 {
    let shift = attempt.saturating_sub(1).min(4); // cap the nominal at 16 s
    let nominal = 1000u64 << shift;
    let half = nominal / 2;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    half + nanos % (half + 1)
}

#[cfg(test)]
mod tests {
    use super::Error;

    #[test]
    fn retry_backoff_ms_stays_in_the_equal_jitter_band() {
        // Attempt 1 → [500, 1000], 2 → [1000, 2000], 3 → [2000, 4000]:
        // the fixed half floors the delay (a retry is never instant), the
        // random half desynchronizes concurrent retries.
        for _ in 0..64 {
            let a1 = super::retry_backoff_ms(1);
            assert!((500..=1000).contains(&a1), "attempt 1 out of band: {a1}");
            let a2 = super::retry_backoff_ms(2);
            assert!((1000..=2000).contains(&a2), "attempt 2 out of band: {a2}");
            let a3 = super::retry_backoff_ms(3);
            assert!((2000..=4000).contains(&a3), "attempt 3 out of band: {a3}");
        }
    }

    #[test]
    fn retry_backoff_ms_actually_jitters() {
        // The random half must vary across calls — otherwise concurrent
        // agents would retry in lockstep (the whole point of the jitter).
        // Needs a working wall clock (SystemTime nanos); 32 samples over
        // microseconds of real time always span multiple clock ticks.
        let samples: Vec<u64> = (0..32).map(|_| super::retry_backoff_ms(1)).collect();
        assert!(
            samples.iter().max() > samples.iter().min(),
            "32 samples of attempt 1 produced identical delays: {samples:?}"
        );
    }

    #[test]
    fn context_overflow_is_non_retryable() {
        let cases = [
            "This model's maximum context length is 262144 tokens. However, you requested \
             131072 output tokens and your prompt contains at least 131073 input tokens",
            "stream request: HTTP 502 from https://proxy.net/v1 — \
             {\"error\":{\"message\":\"litellm.BadGatewayError: BadGatewayError: OpenAIException - \
             Error code: 502 - {'detail': 'vertex: please reduce the length of the input prompt'}\"}}",
            "litellm.ContextWindowExceededError: ContextWindowExceededError: OpenAIException - \
             exceeds the context window",
            "context_window_exceeded: input_tokens 150000 exceeds maximum allowable tokens",
            // 2027-01-23 user report: DeepSeek-style 400 on a 7.2M-token tool
            // history — the wording that wedged compaction.
            "stream request: HTTP 400 from https://api.deepseek.com/v1 — \
             {\"error\":{\"message\":\"The number of input tokens is longer than the limit: \
             7211000 > 1000000\"}}",
        ];
        for msg in cases {
            let e = Error::Provider(msg.into());
            assert!(
                e.is_non_retryable(),
                "context overflow must be non-retryable: {msg}"
            );
        }
    }

    #[test]
    fn context_overflow_is_classified_directly() {
        // is_context_overflow is the shared classifier: is_non_retryable
        // delegates the size needles to it, and compaction's mechanical
        // fallback uses it to recognize when the summarizer cannot accept
        // the history.
        let cases = [
            "stream request: HTTP 400 from https://api.deepseek.com/v1 — \
             {\"error\":{\"message\":\"input is longer than the limit\"}}",
            "This model's maximum context length is 262144 tokens",
            "prompt is too long: 150001 tokens > 131072 maximum",
            "litellm.ContextWindowExceededError: exceeds the context window",
        ];
        for msg in cases {
            assert!(
                Error::Provider(msg.into()).is_context_overflow(),
                "context overflow must be classified: {msg}"
            );
        }
        let negatives = [
            "stream request: HTTP 401 — {\"error\":{\"message\":\"Unauthorized\"}}",
            "failed to start stream: error sending request for url",
        ];
        for msg in negatives {
            assert!(
                !Error::Provider(msg.into()).is_context_overflow(),
                "non-overflow error must not be classified: {msg}"
            );
        }
    }

    #[test]
    fn max_completion_tokens_is_non_retryable() {
        let e = Error::Provider(
            "max_completion_tokens=131072 cannot be greater than max_model_len=65536".into(),
        );
        assert!(
            e.is_non_retryable(),
            "max_completion_tokens error must be non-retryable"
        );
    }

    #[test]
    fn auth_error_is_non_retryable() {
        let e = Error::Provider("LiteLLM Virtual Key expected".into());
        assert!(e.is_non_retryable(), "auth error must be non-retryable");
    }

    #[test]
    fn model_not_found_is_non_retryable() {
        let e = Error::Provider(
            "litellm.NotFoundError: Error code: 404. Received Model Group=glm-5.2".into(),
        );
        assert!(
            e.is_non_retryable(),
            "model not found must be non-retryable"
        );
    }

    #[test]
    fn transient_errors_remain_retryable() {
        let cases = [
            "failed to start stream: error sending request for url",
            "stream stalled — no data for 90s",
            "HTTP 502 from gateway",
            "HTTP 500: internal server error, usage: {\"input_tokens\": 42, \"output_tokens\": 10}",
            "HTTP 429 Too Many Requests",
            "flaky failure",
        ];
        for msg in cases {
            let e = Error::Provider(msg.into());
            assert!(
                !e.is_non_retryable(),
                "transient error must stay retryable: {msg}"
            );
        }
    }

    #[test]
    fn non_provider_errors_are_retryable() {
        // Only Provider errors are classified; all other variants are
        // retryable (they're not network errors).
        assert!(!Error::Tool("oops".into()).is_non_retryable());
        assert!(!Error::Config("bad".into()).is_non_retryable());
    }

    // --- is_rate_limited -------------------------------------------------

    #[test]
    fn http_429_status_is_rate_limited() {
        // The standard provider_error format: "stream request: HTTP 429 from
        // {url} — {truncated_body}".
        let e = Error::Provider(
            "stream request: HTTP 429 from https://api.example.com/v1/chat/completions — \
             {\"error\":{\"message\":\"Too Many Requests\"}}"
                .into(),
        );
        assert!(e.is_rate_limited(), "HTTP 429 must be rate-limited");
    }

    #[test]
    fn too_many_requests_is_rate_limited() {
        let e = Error::Provider("Rate limit exceeded: too many requests".into());
        assert!(
            e.is_rate_limited(),
            "'too many requests' must be rate-limited"
        );
    }

    #[test]
    fn quota_exhausted_body_is_rate_limited() {
        // GLM's "Weekly/Monthly Limit Exhausted" body (observed in production
        // with HTTP 429).
        let e = Error::Provider("HTTP 429 — Weekly/Monthly Limit Exhausted".into());
        assert!(
            e.is_rate_limited(),
            "quota-exhausted body must be rate-limited"
        );
    }

    #[test]
    fn rate_limit_phrase_is_rate_limited() {
        let e = Error::Provider("Error code: 429 — rate limit reached".into());
        assert!(
            e.is_rate_limited(),
            "'rate limit' phrase must be rate-limited"
        );
    }

    #[test]
    fn non_429_errors_are_not_rate_limited() {
        // Context overflow, auth, model-not-found, 500/502, connect failures
        // are NOT rate-limited (they're either non-retryable or transient-
        // retryable, but not 429s).
        let cases = [
            "This model's maximum context length is 262144 tokens",
            "LiteLLM Virtual Key expected",
            "litellm.NotFoundError: Error code: 404",
            "HTTP 500 from gateway — internal server error",
            "HTTP 502 from gateway — bad gateway",
            "failed to start stream: error sending request for url",
            "stream stalled — no data for 90s",
        ];
        for msg in cases {
            let e = Error::Provider(msg.into());
            assert!(
                !e.is_rate_limited(),
                "non-429 error must not be rate-limited: {msg}"
            );
        }
    }

    #[test]
    fn non_provider_errors_are_not_rate_limited() {
        assert!(!Error::Tool("oops".into()).is_rate_limited());
        assert!(!Error::Config("bad".into()).is_rate_limited());
    }

    #[test]
    fn rate_limited_is_distinct_from_non_retryable() {
        // A 429 is rate-limited but NOT non-retryable — it gets the fallback
        // path, not the immediate-fail path. This is the key distinction:
        // non-retryable = fail immediately; rate-limited = try another provider.
        let e = Error::Provider("HTTP 429 Too Many Requests".into());
        assert!(e.is_rate_limited(), "429 is rate-limited");
        assert!(
            !e.is_non_retryable(),
            "429 must NOT be non-retryable (it gets the fallback path)"
        );
    }

    // ── Serialization-bug classification (Rule 6) ─────────────────────────

    #[test]
    fn missing_thought_signature_is_serialization_bug() {
        let e = Error::Provider(
            "Function call read_file in the 1. content block is missing a \
             thought_signature"
                .into(),
        );
        assert!(e.is_serialization_bug(), "missing thought_signature is a serialization bug");
        assert!(e.is_non_retryable(), "serialization bugs must be non-retryable");
    }

    #[test]
    fn reasoning_content_must_be_passed_back_is_serialization_bug() {
        let e = Error::Provider(
            "The reasoning_content in the thinking mode must be passed back to the API"
                .into(),
        );
        assert!(e.is_serialization_bug());
        assert!(e.is_non_retryable());
    }

    #[test]
    fn expected_thinking_found_text_is_serialization_bug() {
        let e = Error::Provider(
            "Expected thinking or redacted_thinking, but found text"
                .into(),
        );
        assert!(e.is_serialization_bug());
        assert!(e.is_non_retryable());
    }

    #[test]
    fn modified_prior_content_is_serialization_bug() {
        let e = Error::Provider(
            "messages: roles: assistant: modified prior content"
                .into(),
        );
        assert!(e.is_serialization_bug());
        assert!(e.is_non_retryable());
    }

    #[test]
    fn transient_error_is_not_serialization_bug() {
        let e = Error::Provider("HTTP 503 Service Unavailable".into());
        assert!(!e.is_serialization_bug(), "503 is transient, not a serialization bug");
        assert!(!e.is_non_retryable(), "503 is retryable");
    }

    #[test]
    fn non_provider_error_is_not_serialization_bug() {
        assert!(!Error::Tool("oops".into()).is_serialization_bug());
        assert!(!Error::Config("bad".into()).is_serialization_bug());
    }
}
