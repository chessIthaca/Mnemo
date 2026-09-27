+++
title = "Anthropic empty/truncated tool args + dropped text — SSE event name lost at network chunk boundaries"
supersedes = "2027-01-11-tool-calls-arrive-with-empty-arguments-dropped-t"
created = "2027-01-11"
+++

Root cause CONFIRMED (2027-01, plan 428a9f1e) — supersedes the "model emitted empty {}" theory: the harness dropped the frames.

Symptom: tool calls arriving with empty/truncated JSON arguments, the turn loop reporting "malformed JSON arguments", plus vanishing text fragments; clustered on the 2nd call of a parallel batch.

Root cause: src/provider/anthropic.rs::parse_sse_buffer held the pending SSE `event:` name in a function-local reset on every call. A network chunk boundary between a frame's `event:` line and its `data:` line left the data frame with an empty event name → parse_sse_event's forward-compatible catch-all dropped it silently. Dropped `input_json_delta` fragments → accumulated arguments failed to parse → handle_bad_json sanitized the raw to "{}" (anthropic.rs also rewrites raw input to {} at message_stop), and THAT "{}" is what the model saw in history and misread as its own emission. Dropped `text_delta` = the text fragments. More parallel calls → more frames → higher collision rate.

Fix: StreamState::pending_event persists the event name across parse_sse_buffer calls. Commit 5b7f99b (wt/mnemo). Regression test: provider::anthropic::tests::event_name_persists_across_chunk_boundary — red before the fix (first fragment `{"path":` dropped), green after.

Follow-up queued on the backlog: handle_bad_json batch isolation (run well-formed siblings instead of failing the whole batch).
