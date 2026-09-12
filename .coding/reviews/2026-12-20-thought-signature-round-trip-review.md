## Verdict: FINDINGS (0 high, 2 low)

The thought-signature round-trip implementation is **correct, well-tested, and warning-free**. The capture→accumulate→package→echo pipeline round-trips `thought_signature` verbatim, all 5 packaging paths in `turn.rs` are covered with sound borrow ordering, the `PROVIDER_PASSTHROUGH_KEYS` allowlist is the sole provenance gate, and docs (PLAN.md + README.md) are synced. The two findings below are low-severity polish items (one doc caveat, one test-coverage gap) — no correctness, security, or build defects.


## Findings

### L1 (low, docs) — PLAN.md invariant (3) overstates survival across model switches

**Location:** `PLAN.md` item 7, invariant (3).

**Issue:** Invariant (3) states signatures "persist across model switches mid-conversation because they ride on stored messages, not on provider config." This is true for model switches that do **not** trigger summarization, but signatures are **lost** when a model switch triggers context summarization — the deferred-swap path (`turn.rs` `summarize_with_interrupt`, ~line 87) and auto-compaction (`context.rs` `summarize_with_interrupt`) both replace the signed messages with fresh summary `Message`s that carry `provider_meta: None` (verified: `src/agent/context.rs` lines 206, 224, 290, 347, 467 all set `provider_meta: None` on summary/summary-prompt messages). The signed assistant turns are discarded, so their `thought_signature` bags are gone.

**Why it's low, not a bug:** This is inherent to compaction — you cannot preserve signatures on messages that no longer exist, and the implementation correctly avoids fabrication (the summary message has `provider_meta: None`, never a fabricated signature). Google's "resend previous model's signed blocks after switching models" rule assumes you resend the actual prior turns; once summarized, those turns are gone. The implementation fails safe. But the doc's unqualified "persist across model switches" claim could mislead a reader into expecting survival through a summarizing switch.

**Fix:** Add a caveat to invariant (3) in PLAN.md noting the compaction/summarization exception, e.g. "…persist across model switches mid-conversation (when the context fits the new model; a summarizing switch — smaller context window — replaces signed turns with an unsigned summary, dropping their signatures by necessity)."

### L2 (low, tests) — No end-to-end no-fabrication regression test at the request-builder level

**Location:** `src/provider/openai.rs` test module.

**Issue:** The provenance-gating guarantee ("non-signature endpoints produce byte-identical requests") is covered at the helper level (`apply_provider_meta_is_noop_for_none`, `capture_provider_meta_returns_none_when_no_passthrough_keys` in `mod.rs`), and the echo-when-present path is covered (`build_request_json_echoes_assistant_provider_meta`, `build_request_json_echoes_tool_call_provider_meta`). But there is **no** test that builds a full request body from messages with `provider_meta: None` and asserts the serialized body contains **no** `thought_signature` key. Such a test would harden the guarantee against a future regression (e.g. someone adding a default value to the field, or a refactor that accidentally emits the key).

**Fix:** Add a test like `build_request_json_omits_thought_signature_when_no_meta` that builds a request from plain assistant + tool messages (all `provider_meta: None`) and asserts `body["messages"][0].get("thought_signature").is_none()` (and likewise for a tool_calls entry).

---

## Correctness analysis (verified)

### Capture → accumulate → package → echo pipeline

**Capture (`openai.rs` `parse_sse_chunk`, lines 1756-1778):** Two scopes captured verbatim via `capture_provider_meta`:
- Per-tool-call: `capture_provider_meta(tc)` inside the `tool_calls` loop → `ProviderMeta { tool_call_index: Some(index), meta }`. ✓
- Message-level: `capture_provider_meta(delta)` after the tool_calls loop → `ProviderMeta { tool_call_index: None, meta }`. ✓

`capture_provider_meta` (mod.rs:287) walks `PROVIDER_PASSTHROUGH_KEYS` (`["thought_signature"]`) and clones values untouched — no parsing/normalization. Returns `None` when no passthrough key present (cheap skip). ✓

**Accumulate (`stream.rs` `DeltaAccumulator`):**
- `feed()` (line 91-106): `ProviderMeta` events merge per-scope — `Some(index)` → `call_metas[index]`, `None` → `message_meta`. ✓
- `merge_provider_metadata` (line 158): string+string concatenates in arrival order (mirrors argument-fragment aggregation); any other combination **replaces** (never crashes on non-string values). ✓ Matches the spec's "streaming delivers signature as its own delta near stream end → must aggregate."
- `finalize(mut self)` (line 124): sorts calls by index, attaches `call_metas.remove(&index)` onto each packaged `ToolCall.provider_meta`. ✓
- `take_message_provider_meta(&mut self)` (line 142): yields the message-level bag. ✓

**Package (`turn.rs`) — all 5 sites verified:**
1. **Stop-path** (line 1271): `take_message_provider_meta()` called **before** `finalize()` (line 1291) — correct ordering (take borrows `&mut`, finalize consumes). Message pushed with `provider_meta: msg_meta`. ✓
2. **Pre-finalize capture** (line 1353): `take_message_provider_meta()` then `finalize()` (line 1354). ✓
3. **Sanitized-calls propagation** (bad-JSON retry, line 1443): `tc.provider_meta.clone()` preserved on sanitized calls; `msg_meta.clone()` on the message (clone is safe — this branch `continue`s). ✓
4. **Null-turn push** (line 1530): `provider_meta: msg_meta` (moved — last use on this path). ✓
5. **Normal push** (line 1556): `provider_meta: msg_meta` (moved). ✓

**Borrow flow:** `msg_meta` is moved on the null-turn and normal paths (last use), cloned on the bad-JSON path (which loops). No use-after-move. `take_message_provider_meta` (`&mut self`) always precedes `finalize` (`self`). Sound. ✓

**Echo (`openai.rs` `build_request_json`, lines 1348, 1368):**
- Per-tool-call: `apply_provider_meta(&mut entry, &tc.provider_meta)` on each tool_call entry. ✓
- Message-level: `apply_provider_meta(&mut obj, &m.provider_meta)` on assistant messages (inside `if m.role == Role::Assistant`). ✓

`apply_provider_meta` (mod.rs:311) is a no-op when `meta` is `None` or `target` is not an object — so non-signature endpoints see byte-identical requests. ✓

### Streaming loop feeds ProviderMeta to the accumulator

The no-op `LlmEvent::ProviderMeta` match arm (turn.rs:1208) is correct: `acc.feed(&event)` is called unconditionally at **line 1067** for every stream event **before** the match (verified — the comment "Feed the event to the accumulator first (before the match moves any fields out of it)" at line 1065 confirms the design). The accumulator stores the metadata; the match arm only forwards UI-relevant events. No double-feed, no missed feed. ✓

### No double-capture / missed-capture

Message-level capture is on `delta`; per-tool-call capture is on each `tc`. These are distinct JSON objects (different scopes), so a signature at one scope never duplicates into the other's bag. A signature could theoretically appear at both scopes in one chunk — they'd accumulate into separate bags (message vs per-call), which is correct (echoed at both scopes, matching what the provider sent). ✓

### merge_provider_metadata non-string handling

`match (dst.get_mut(key), value.as_str())`: `(Some(String), Some(str))` → concatenate; everything else → `insert` (replace). Non-string values replace, never panic. The doc comment acknowledges Google emits strings, so the replace branch only guards pathological inputs. ✓

### Serialization / persistence

`provider_meta` carries `#[serde(default, skip_serializing_if = "Option::is_none")]` on both `Message` and `ToolCall`. When `Some`, it serializes as a JSON object and round-trips through storage unchanged (serde_json::Map ↔ JSON object is lossless). When `None`, omitted — old history files deserialize fine (`default`). The request body is built manually by `build_request_json` (not via Message's Serialize), so `apply_provider_meta` is the sole echo gate. ✓

## Security analysis

- **Sensitive data leak:** `provider_meta` only ever holds keys in `PROVIDER_PASSTHROUGH_KEYS` (`["thought_signature"]`) — provider-generated opaque attestations, not user data. Not a leak vector. ✓
- **Trace logging:** Signatures appear in `response_raw` (the raw SSE, already logged) and in `request_json` (the echoed request body). This is consistent with existing behavior — the response is already logged raw; the signature is not a user secret. No new exposure. ✓
- **Malicious endpoint injection:** A malicious endpoint could inject `thought_signature` with arbitrary content, which would be echoed verbatim. But the endpoint can already inject arbitrary content into response text/reasoning, so this is not a new attack surface. The `PROVIDER_PASSTHROUGH_KEYS` allowlist **is** the sole gate (verified — `capture_provider_meta` only copies allowlisted keys; `apply_provider_meta` only inserts from the captured map). ✓

## Constitution checks

- **Documentation sync:** PLAN.md item 7 added (accurate description of the round-trip + 3 invariants, modulo the L1 caveat). README.md feature bullet added (accurate). ✓
- **Multi-platform neutrality:** Feature code is pure Rust (serde_json manipulation) — no Windows-only APIs, no platform-conditional paths. ✓
- **Warning-free build:** `cargo test` passes with 1799 tests, 0 failures. `#![deny(warnings)]` at both crate roots means a green build proves zero warnings. The mechanical `provider_meta: None` additions across ~90 Message + ~20 ToolCall struct literals are complete (any missing field would be E0063, a compile error). ✓

## Test coverage assessment

8 new tests exercise the right paths:
- `mod.rs` (6): capture extracts / returns-none / non-object; apply merges / noop-for-none; full round-trip (capture→apply→byte-identical). ✓
- `openai.rs` (2 parse + 2 builder): message-level + per-tool-call ProviderMeta emission; assistant + tool-call echo. ✓
- `stream.rs` (4): message-level capture, per-call attach on finalize, repeated-fragment merge, no-meta-returns-none. ✓

The round-trip test (`capture_and_apply_round_trip_preserves_signature_verbatim`) directly verifies verbatim fidelity. The no-fabrication property is covered at the helper level but lacks an end-to-end request-builder assertion (L2). The `tests/integration/provider_integration.rs` catch-all `_ => acc.feed(&event)` (lines 111, 160) ensures real provider streams exercise the accumulator path. ✓

## Scope note

The diff spans 127 files, but per the plan brief the vast majority is `cargo fmt` reformatting + mechanical `provider_meta: None` additions to Message/ToolCall struct literals (step 1). This review focused on the 4 core source files (`mod.rs`, `openai.rs`, `stream.rs`, `turn.rs`) plus the docs. The `cargo fmt` changes are cosmetic and the `provider_meta: None` additions are provably complete (green build under `deny(warnings)`). The Anthropic provider (`anthropic.rs`) is intentionally untouched (it uses the native Messages API, not the OpenAI-compatible path) — correct per the plan. ✓
