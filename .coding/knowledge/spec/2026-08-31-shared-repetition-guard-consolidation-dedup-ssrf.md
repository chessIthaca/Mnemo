+++
title = "shared repetition guard + consolidation dedup + SSRF protection"
created = "2026-08-31"
+++

Three review findings from the 2026-12-17 full arch/perf/security review addressed in commit 89aea41 on wt/agenticcoding:

1. REPETITION GUARD SHARED (stream.rs): detect_repetition, bound_repetition_buffer, REPETITION_WINDOW/THRESHOLD/BUFFER_CAP lifted from openai.rs (private) into src/provider/stream.rs (pub). Applied in Anthropic provider streaming loop (anthropic.rs:1019-1052) with log.fail() trace marker matching OpenAI pattern. Both providers now share the same R10 stuck-loop guard.

2. CONSOLIDATION DEDUP (agent.rs): spawn_consolidation now has an AtomicBool dedup guard (consolidation_in_flight field on AgentTask). swap-check returns early if a consolidation is already in flight. ConsolidationFlagGuard (Drop impl) clears the flag on ALL exit paths including panic unwinding — prevents the flag from getting stuck true.

3. SSRF PROTECTION (web_fetch.rs): web_fetch now rejects internal/private/loopback IP ranges before sending HTTP requests. extract_host uses reqwest::Url::parse (not hand-rolled — fixes userinfo + IPv6-bracket bypass). is_internal_ip covers IPv4 (loopback/link-local/private/unspecified) + IPv6 (loopback/link-local/unique-local/IPv4-mapped). spawn_blocking fail-closed (.unwrap_or(true)). TOCTOU (DNS rebinding) documented as known limitation for desktop-only threat model.

Tests: 1769 pass, zero warnings. Review: .coding/reviews/2026-12-17-review-findings-verify.md (PASS).

Amended 2027-01-11: 2027-01-12 amendment: the SSRF residual noted here — "TOCTOU (DNS rebinding) documented as known limitation for desktop-only threat model" — is CLOSED. Plan aa3c8dae (security review 2026-09-09 LOW-1) made web_fetch's gate resolve-validate-PIN: the gate's validated address is pinned to the connection per hop (reqwest ClientBuilder::resolve — SNI/TLS/Host stay on the hostname), and DNS-resolution failure is now fail-closed (src/tool/agent/web_fetch.rs: resolve_and_gate_host + build_fetch_client). A rebinding DNS server can no longer swap the address between the check and the connect; the in-code "Known limitation" comment was replaced by the fix description.
