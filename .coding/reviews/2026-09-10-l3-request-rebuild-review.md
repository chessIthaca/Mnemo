## Verdict: FINDINGS (0 high, 2 low)

Review of ALL uncommitted changes on wt/agenticcoding for plan 7c8d90cd "Cut per-iteration O(history) request-rebuild costs (L3)" — Cargo.toml (raw_value feature), src/provider/mod.rs (estimate split), src/provider/openai.rs (String body pipeline), src/provider/openai/request.rs (RequestBody + PrefixCache + echo_assistant_raw), src/provider/openai/tests.rs (12 new tests). The implementation is correct on every axis checked (prefix-cache soundness, warm==cold equivalence, retry rebuilds, trace rec_id semantics, extra_body merge, estimate split); the deliberate deviation from fix-instruction 3 is sound and properly documented. Two LOW findings: a theoretical fingerprint-encoding ambiguity, and a test gap on the cache-hit path.


## Verification detail (per the requested checks)

### (a) Prefix-cache soundness — can any stale hit occur?

**Fingerprint coverage.** The chat builder's serialized form of each message depends on exactly: (raw-echo path) `raw` bytes, the strip bool, the re-add bool, `m.reasoning_content` (the re-add value); (synthetic path) role, content (Text/Parts incl. image URLs), tool_calls id/name/arguments, tool_call_id, name, reasoning_content, the strip bool. `hash_message` (request.rs:129-204) hashes every one of these plus `reasoning_stripped` and tool-call `provider_meta` (over-hashed — the synthetic path doesn't serialize provider_meta; over-hashing is safe, it only costs a missed cache opportunity). Config-derived inputs (`stop_boundary_strings` for tool-result escaping, `caps.multimodal` for Parts, `caps.supports_tool_choice`, model/temperature/top_p/stops) and the policy (derived from `config.kind` + `config.model`) are all per-client immutable — `config`/`caps` are plain fields with zero mutation sites (verified by search: every `config.model` match is a read/clone; model switches rebuild the client via client_factory). The age affects serialization ONLY through the two hashed bools (`echo_assistant_raw` consults `assistant_age` solely via `needs_strip`/`needs_readd`; the synthetic strip check uses the same `strips_reasoning` bool). Coverage is complete.

**Age-dependent bools.** Ages are counted from the end among assistants (request.rs:364-380), recomputed fresh every build, and hashed per assistant message. A flip (re-add at age 0→1; strip when age crosses keep_recent under pressure) changes the fold → rebuild. `strips_reasoning(0, _)` is unconditionally false (policy.rs:130-132), confirming the doc's claim that the strip and the age-0 re-add are mutually exclusive and that checking the re-add against pre-strip raw is equivalent. The pressure-flip and re-add-aging-out tests verify both flips end-to-end with the historical reasoning actually stripped / the key actually absent — a stale hit would fail them.

**Pressure feedback loop.** `provisional_est` (stale-prefix split) is only *used* on a fingerprint match, and a match proves the prefix bytes are unchanged, hence `prefix_chars` is correct, hence the split equals the full walk. The `cpressure == provisional_pressure` gate is belt-and-suspenders: pressure is hashed into the fold, so even a gate pass with a wrong provisional pressure cannot match the stored fold. On a miss the full walk recomputes est/pressure from scratch. Sound.

**Trailing-system exclusion.** Verified the premise: the volatile tail + CONTEXT_FOOTER are pushed as trailing `system` messages (turn.rs:2128-2130) and popped after the request; the head is byte-stable (turn.rs:2078-2101). `cacheable_len` is recomputed fresh per build on the current list, so the exclusion self-adjusts (a system message that is trailing this iteration and not the next simply migrates between the cached and fresh regions — both correct). The turn-loop pattern (tail pushed/popped, new turn appended) hits the cache.

**Compaction / head swap / 429-fallback / cross-conversation.** Compaction shrinks or rewrites the list → len filter or fold mismatch → rebuild (tested). Head swap changes message[0] bytes → fold mismatch (tested). Foreign fallback turns change history bytes → mismatch. A different conversation on the same single-slot client → fold mismatch → miss and refill. All verified.

**Concurrency/locking.** std Mutex, no await while held; the lock is dropped before message serialization; the fold (~1-2ms) is synchronous. Two concurrent builds on one client: the loser sees `None` → cold build → both correct. A build error after `take()` just loses the cache (perf only).

**One theoretical gap** — see Finding 1 (encoding ambiguity). No current code path can trigger it (history is append-only; the in-place mutators — Rule-5 strip, D2 truncation — change byte lengths/keys, which cannot produce a stream-colliding reshuffle).

### (b) Warm==cold equivalence

Structurally sound: on a hit the prefix boxes were fingerprint-validated against the current bytes/ages/pressure, the tail is serialized by the same code with the same decisions, and `max_completion_tokens` derives from an estimate that is provably identical (additive split of the same formula — `(chars + tools)/4 + len*4`, request.rs:351-357 vs mod.rs:710-725). Both warm and cold use the same `RequestBody` struct → identical field order. The 7 cache tests pin it across append, trailing-system exclusion, shrink, head swap, pressure flip, and re-add aging out; the pre-existing `retention_strip_is_repeatable_across_calls` (tests.rs:2332-2372, client reused across builds, including under pressure) also passes through the warm path. The ~130 existing body tests go through the `#[cfg(test)]` build+parse lens — parsed Values are order-independent, so the field-order change (BTreeMap alphabetical → declaration order) is invisible to them and semantically irrelevant on the wire.

### (c) Retry rebuild paths

Chat: `build_request_body(messages, tools, tool_choice, true)` re-runs the same builder with `omit_effort` — same messages → cache hit → same prefix, same estimate → same `max_completion_tokens`; the retry body is exactly the original minus `reasoning_effort`. Responses: build Value → `remove("reasoning_effort")` → `to_string`, matching the former in-place removal (the Responses body never carries the field, so the removal is a no-op — same as before the change). The retry passes the ORIGINAL `messages` slice, which the builder re-sanitizes identically. Correct.

### (d) Trace parse on the blocking pool

`log.start` returns `u64` (trace.rs:502-508). The closure returns `Option<u64>`; `.await.ok().flatten()` yields `Option<u64>` for `rec_id` — types check. A parse failure maps to `None` (recording stops, no timing stamps, no `last_record_id`), the same handling class as the former join-error path; the body was just serialized by this process, so a parse failure is unreachable in practice. The async worker now pays a String memcpy instead of a deep Value-tree clone; the parse runs on the blocking pool alongside the cap logic that already serialized there. rec_id semantics preserved.

### (e) extra_body override-on-collision

The merge parses the serialized body, `Map::insert`s each extra key (override on collision — identical to the former in-place insert into the json!-built Map), re-serializes. It runs after the message boxes are built and before the cache refresh, and the refresh drains from the pre-merge `messages_raw` — correct, since extra_body only merges top-level fields and can never reach inside `messages` (a top-level `"messages"` key in extra_body replaces the whole array, exactly as before). The parse+serialize round trip only fires when extra_body is non-empty — documented trade-off.

### (f) Estimate split equivalence

`estimate_prompt_tokens` = `len*4 + (chars + tools_chars)/4`; the split = `(prefix_chars + newly_stable + tail + tools_sum)/4 + len*4` — the same sum partitioned, same division, same basis. `message_estimate_chars` is the identical per-message formula; `as_text()` for Parts concatenates text parts, all of which are hashed with lengths, so the char sum is determined by the hashed bytes. On a miss the full walk recomputes inline with the same formula. Exact equivalence confirmed.

## Findings

### Finding 1 (LOW) — Fingerprint hash encoding is ambiguous across adjacent untagged string writes

**Where:** `hash_message` (src/provider/openai/request.rs:129-204) and `hash_value` (request.rs:92-123).

**Issue:** The fold's byte-stream encoding is not injective. Adjacent string fields are written back-to-back with no length prefix or tag, so different field splits produce identical hash streams:

- Tool call `{id:"ab", name:"c", arguments:""}` vs `{id:"a", name:"bc", arguments:""}` — identical streams, different serialized forms.
- `content Text("a")` + `tool_calls.len()==1` vs `Text("a\x01")` + `tool_calls.len()==0` — `write_usize` emits 8 LE bytes that string bytes can mimic.
- In `hash_value` objects: `{"a":"xy","b":null}` vs `{"a":"x","yb":null}` — identical streams.

A stale hit therefore requires an in-place history mutation that reshuffles bytes across adjacent field boundaries while preserving the concatenated stream. **No current code path can do this** — history is append-only; the in-place mutators (Rule-5 cross-vendor strip at mod.rs:488-528 removes keys; D2 `compact_old_tool_results` truncates content) change byte lengths/keys, which cannot produce a stream-colliding reshuffle; compaction/head-swap replace bytes wholesale. So this is theoretical today — but the doc comment's claim "Sound by construction: any byte that could change the serialized prefix changes the fingerprint" (request.rs:206-210) overstates: two *different* prefixes can share a fingerprint, not just via the negligible 2⁻⁶⁴ SipHash collision but via the encoding itself.

**Fix (cheap, makes the encoding injective up to hash collisions):** length-prefix every string write in `hash_message`/`hash_value` — `h.write_usize(s.len()); h.write(s.as_bytes());` — and tag the `write_usize` boundaries (e.g., a distinct tag byte before each length). Alternatively hash one canonical `serde_json::to_vec` of a small struct per message (costs an alloc — the length-prefix is better). Adjust the doc comment to "computationally impossible" wording either way.

### Finding 2 (LOW) — No test asserts a prefix-cache HIT actually occurs

**Where:** the 7 cache tests (src/provider/openai/tests.rs, `prefix_cache_*`).

**Issue:** Every cache test asserts warm==cold byte equality — which holds trivially on a miss too. A regression that makes the fingerprint always mismatch (a bug in the fold, a missing field in `hash_message`, a broken len filter) would leave all 12 new tests green while silently disabling the entire optimization — the perf win this plan exists to deliver would vanish undetected. The correctness-*negative* paths are well covered (pressure-flip and re-add-aging-out would catch a stale hit), but the positive hit path is untested. Relatedly, `prefix_cache_excludes_trailing_system_run` passes even if the exclusion were removed (warm==cold holds either way), so the exclusion's actual effect is also unpinned.

**Fix:** add a test-visible hit signal — e.g. `#[cfg(test)] hit_count: std::sync::atomic::AtomicUsize` on `OpenAiClient`, incremented when `cache_hit` is true — and assert `hit_count >= 1` after the second build in `prefix_cache_warm_build_is_byte_identical_to_cold` and `prefix_cache_hits_on_append_and_matches_cold`; assert `hit_count == 0` in the shrink/head-swap/pressure-flip/re-add tests (pinning that those genuinely miss). In the trailing-system test, additionally assert the cached `len` equals the list minus the trailing run (e.g., 2, not 4) via the existing `client.prefix_cache` lock.

## Informational notes (no action required)

1. **`tools_chars` runs every request** (computed unconditionally before the hit/miss branch, request.rs:332) and **twice on the no-cache path** (once as `tools_sum`, once inside `estimate_prompt_tokens` at request.rs:356). Semantically required on hits (tools can change per request — schema_filter), so this is correct; the double-compute is first-build-only and negligible. The message-content walk — the O(history) cost L3 targeted — is skipped on hits as designed.
2. **Top-level field order changes** from alphabetical (serde_json BTreeMap `json!`) to declaration order (`RequestBody`). Semantically equivalent JSON; the messages array order is unchanged. One-time provider-side prefix-cache miss on the first request after upgrade; irrelevant thereafter.
3. **Local providers never hit the cache**: the volatile tail is folded into the leading system message (turn.rs:2087-2090), which changes per iteration → message[0] fingerprint mismatch → always cold. Correct (fingerprint does its job), benefit limited to OpenAI-kind — consistent with the plan's focus on the chat hot path.
4. **Knowledge sidecar staleness:** `.coding/knowledge/spec/2027-01-05-provider-module-layout-after-the-openai-rs-split.md` describes `build_request_json` as a pub(super) method of request.rs; it is now a `#[cfg(test)]` lens. Amend via `memory_amend` or let the plan's own SPEC record supersede it at finish.

## Constitution checks

- **Documentation sync:** No README.md/PLAN.md updates needed — the change is internal (request-build mechanics); no config surface, endpoint keys, or user-visible behavior changed; module doc comments in request.rs/openai.rs are updated thoroughly and accurately (including the deviation note). The one stale artifact is the knowledge record in note 4.
- **Multi-platform neutrality:** Pure Rust logic (serde_json, std Mutex, Cow) — no Windows APIs, paths, or shell syntax. PASS.
- **File-tools-first policy:** No shell-based file mutation anywhere in the diff. PASS.
- **Doc comments on public functions:** All new/changed items (`message_estimate_chars`, `tools_chars`, `estimate_prompt_tokens`, `echo_assistant_raw`, `build_request_body`, `PrefixCache`, `fingerprint_prefix`, `hash_message`, `hash_value`) carry doc comments; no new crate-public APIs. PASS.
- **Tests:** 5 echo tests (Borrowed/Owned split incl. both mutation triggers and both no-op guards) + 7 cache tests; the ~130 existing body tests unchanged via the lens; `cargo test --workspace` reported green (2266+16, warning-free under deny(warnings)). The two LOW findings above are hardening items, not blockers.

## Deliberate deviation (instruction 3) — judged SOUND

Not feeding TokenAccounting's incremental estimate into the max-token cap is the right call, and the in-code documentation (request.rs:319-323) is accurate: the accounting total is computed at turn.rs:335, before the per-request head/tail scaffolding is installed (turn.rs:2094-2130), so it understates the actual request input; and its BPE basis differs from the builder's chars/4 basis — feeding it would loosen the R9 cap (a correctness regression). Riding the estimate on the prefix cache instead (cached prefix chars + fresh tail chars, same formula, same result, walk skipped on hits) achieves the instruction's stated goal ("instead of re-walking all content per request") with zero staleness and zero basis change. The equivalence is proven by the additive split (check f) and pinned by the warm==cold tests.
