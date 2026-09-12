+++
title = "claude-opus-5 400 \"each thinking block must contain thinking\" — empty thinking block in echoed history (ids 1022-1030, 879-888)"
created = "2026-12-24"
status = "superseded"
+++

Symptom (UNINVESTIGATED — tracked post-merge at user request): claude-opus-5 requests fail non-retryably with HTTP 400 "messages.3.content.0.thinking: each thinking block must contain thinking" in bursts of ~9 attempts — provider-errors.jsonl ids 1022-1030 (ts 1788508444631-1788508460358, 2026-09-04 03:53-03:56, same session that later produced the DeepSeek id-1042 incident) and an earlier burst ids 879-888. The failing request body was unparsable in traces.jsonl (truncated 256KiB rows), so the exact serialized shape is unknown; the error says a thinking content block arrived EMPTY (missing its thinking field) at messages[3]. Hypothesis: Claude raw-turn echo or SSE capture serializes an empty/invalid thinking block in some conversation shape (sibling of the DeepSeek reasoning_content echo family — cross-provider conversation-history echo corruption has happened before in this app). Next step: reproduce with the failing request body (capture with traces enabled and untruncated row), root-cause in src/provider/anthropic.rs build_request_json / raw echo, add fails-without-fix regression test.
