+++
title = "Reasoning-state continuity spec — full compliance Rules 1-6, Rule 7 UI waived"
created = "2026-12-20"
+++

Scoping decision (2026-12-21): migrate the harness to the multi-provider reasoning-state-continuity spec. FULL compliance for Rules 1-6: (1) raw-payload source of truth — store the provider's assistant turn verbatim, request builder echoes raw, MUST NOT rebuild from a fixed-field canonical Message; (2) one ProviderPolicy data object per provider; (3) prefer server-side stateful mode (previous_response_id / previous_interaction_id) + keep stateless replay working, both pass same tests; (4) never trim inside an open tool loop, echo assistant content/functionCall verbatim, history append-only; (5) model switching — same-vendor resend reasoning, cross-vendor strip + set reasoning_stripped:true; (6) fail loudly on serialization-bug 400s (missing thought_signature / reasoning_content must be passed back / Expected thinking…found text / modified prior content) — log offending body, surface, never retry-by-stripping.

Rule 7 (raw CoT MUST NOT reach end users) is WAIVED at the UI level by explicit user choice: the shipped thinking panel + Trace raw-response view stay as product features. The backend raw/view type separation (Rule 1) still lands — view derives from raw, the builder uses raw — but view is allowed to expose reasoning for display.

ROOT GAP verified this session: current architecture violates Rule 1 fundamentally. Message (src/provider/mod.rs:148) is a fixed-field canonical struct (role/content/tool_calls/tool_call_id/name/reasoning_content/provider_meta); both build_request_json (openai.rs:1271, anthropic.rs:221) REBUILD JSON from these fields; provider_meta is an allowlist (PROVIDER_PASSTHROUGH_KEYS=&["thought_signature"], mod.rs:378) that silently drops any reasoning field not on the list — exactly "the entire bug this spec exists to prevent." Anthropic thinking-block signatures are never captured (parser has no thinking arm). No ProviderPolicy, no stateful_key, no reasoning_stripped flag, no fail-loud classifier.

Plan: 'Reasoning-state continuity: raw-payload source of truth + ProviderPolicy' (12 steps).
