+++
title = "GLM/z.ai 400 code 1214 \"content[0].type type error\" — Anthropic raw echoed into OpenAI-compat body after provider switch"
created = "2027-01-11"
+++

Symptom (2027-01 session): after switching mid-conversation from Anthropic to z.ai (GLM 5.3), EVERY request failed with HTTP 400 code 1214 `{"error":{"code":"1214","message":"messages[8].content[0].type type error"}}`, retried identically until the session died.

Root cause: src/provider/openai/request.rs echoed `Message.raw` verbatim into the chat-completions body with no origin check. Anthropic-born raw carries the Messages-API content array (`{"type":"thinking"...}` / `{"type":"tool_use",...}`), which z.ai rejects outright. The payload never changed between attempts, so the 400 was permanent.

Fix: `raw_origin_allows_echo(origin_provider, kind)` — the Rule-1 raw echo is allowed only when `origin_provider` is absent (legacy saves) or parses to the same `ProviderKind` as the client; foreign raws fall through to field construction (content + tool_calls rebuilt from structured fields; the existing raw_is_usable trade). Commit ad9c76f (wt/mnemo). Tests: provider::openai::tests::foreign_anthropic_raw_is_not_echoed_into_an_openai_compat_body (red before: the thinking/tool_use array shipped verbatim) and same_family_origin_raw_still_echoes_verbatim (precision guard: same-kind raws still echo byte-identical).

Related: plan 428a9f1e finding B.
