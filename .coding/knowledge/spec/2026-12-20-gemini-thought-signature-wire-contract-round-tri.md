+++
title = "Gemini thought_signature wire contract + round-trip invariants"
created = "2026-12-20"
+++

SPEC verified via ai.google.dev/gemini-api/docs/thinking (+ /docs/openai) on 2026 session:

- Thought signatures = encrypted attestations of model internal reasoning state; REQUIRED for reasoning continuity across multi-turn turns; server validates them.
- generateContent path has NO dedicated thought blocks -> signatures attach as METADATA ON ARBITRARY PARTS ("such as living inside functionCall parts or the final part of a response"). OpenAI-compat mapping therefore has exactly two scopes we must support: per-tool-call entry fields and assistant-message-level fields.
- Stateless-mode rules stated by Google: MUST resend all signed blocks EXACTLY as received; must NOT remove or modify them from history; still resend previous model's signed blocks after switching models within a session ("backend manages compatibility"). Built-in tools may carry their own distinct signatures on call/result blocks.
- Streaming delivers signature as its own delta type arriving near stream end -> must aggregate into final stored item.
- Google serves this through ordinary chat.completions compat APIs ("Gemini 3 supports OpenAI compatibility for thought signatures") -> implement INSIDE existing openai.rs generic path; no new ProviderKind::Gemini needed.
- Design invariants adopted for Mnemo implementation: (1) verbatim fidelity raw-string store/echo only; (2) provenance-gated echo -- key echoed only if captured from this conversation's responses so non-signature endpoints see byte-identical requests; (3) echo survives mid-conversation endpoint/model switch since rule is conversation-provenance not endpoint-config-gated.

