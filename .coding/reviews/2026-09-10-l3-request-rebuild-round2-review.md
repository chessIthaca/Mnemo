## Verdict: PASS

Round-2 verification of the two round-1 fixes (commit c3841f7, HEAD on wt/agenticcoding — clean tree confirmed: empty `git diff HEAD` and `git status --short`) for plan 7c8d90cd "Cut per-iteration O(history) request-rebuild costs (L3)". Both LOW findings are fixed exactly as prescribed and correctly: the length-prefixed `hash_str` makes the fold's byte-stream encoding genuinely injective (every string write verified — no bare write remains), and the hit_count assertions genuinely pin hit/miss behavior (they fail on an always-mismatch regression, on removal of the trailing-system exclusion, and on an incorrect hit). No regression from the fixes; all round-1 verified-sound conclusions hold at this tree; the note-4 knowledge-record amendment is in place and accurate.

### 1. Fix 1 (round-1 Finding 1) — the encoding is now injective

**Every string write goes through hash_str.** `hash_str` (request.rs:94-97) writes `write_usize(s.len())` then the bytes. Enumerated exhaustively from a full read of the fold (request.rs:90-257) plus a crate-wide search (`as_bytes`, `write_u8`, `write_usize`, `DefaultHasher` over src/**/*.rs):

- `hash_value`: Number → `hash_str(&n.to_string())` (:111), String → `hash_str(s)` (:115), Object keys → `hash_str(k)` (:128).
- `hash_message`: content Text (:151), Part::Text (:160), Part::ImageUrl url (:164), tool-call id/name/arguments (:172-174), provider_meta keys (:180), tool_call_id (:190), name (:197), reasoning_content (:204).

The only `h.write(...)` in request.rs is hash_str's own `h.write(s.as_bytes())` (:96); every other hasher write in the fold is a tag byte or a count. The only other DefaultHasher sites in the crate (codegraph/mod.rs, memory/indexer.rs) are unrelated subsystems. No bare string write remains.

**Injectivity argument.** The stream grammar is self-delimiting at every boundary: the fold opens with the message count (`write_usize(len)`) and the pressure byte; every variant choice is a distinct tag byte (role 0-3; content 1=Text/2=Parts; part 1/2; Option Some=1/None=0 for provider_meta, tool_call_id, name, reasoning_content, raw; hash_value variants 0-5); every string is length-prefixed; every sequence (parts, tool_calls, provider_meta entries, arrays, objects) is count-prefixed. A deterministic parser can therefore read the stream back and recover exactly the hashed input (role, content, tool_calls incl. provider_meta, tool_call_id, name, reasoning_content, reasoning_stripped, raw, per-assistant strip/re-add bools, pressure, len) — parse∘encode is the identity, so two different serialization-relevant inputs cannot produce the same byte stream. `Number::to_string` is injective (Rust Display for u64/i64/f64 is round-trip canonical, so distinct numbers give distinct strings). All three round-1 collision examples are now distinguished: `{id:"ab",name:"c"}` vs `{id:"a",name:"bc"}` → `[2]"ab"[1]"c"` vs `[1]"a"[2]"bc"`; `Text("a")+tool_calls.len()==1` vs `Text("a\x01")+len==0` → different length prefixes; `{"a":"xy","b":null}` vs `{"a":"x","yb":null}` → key lengths 1,1 vs 1,2.

**Consistency + docs.** Both the lookup fold (request.rs:400) and the refresh fold (:786) call the same `fingerprint_prefix`, so store and lookup can never drift. The `fingerprint_prefix` doc (:227-229) now states the exact guarantee — "The encoding is injective (length-prefixed strings — review round-1, Finding 1): two different prefixes share a fingerprint only via a negligible 2⁻⁶⁴ SipHash collision" — replacing the round-1 "sound by construction" overstatement. Coverage is unchanged from round-1's verification (same field set; only the string encoding within it changed).

### 2. Fix 2 (round-1 Finding 2) — the hit_count assertions genuinely pin hit/miss

**The counter.** `#[cfg(test)] hit_count: AtomicUsize` on `OpenAiClient` (openai.rs:217-218), initialized in `new_with_trace` (:295-296) — the single struct-literal construction site (`new` delegates at :240-242; search-verified no other `OpenAiClient {` literals anywhere). Incremented only inside the `cache_hit` branch of `build_request_body` (request.rs:431-433), `#[cfg(test)]`-gated. Read only by the `#[cfg(test)] mod tests` (openai.rs:36-37).

**The assertions** (7 sites, one per cache test — search-verified):

- warm (tests.rs:1976-1980) and append (:2013-2017): `hit_count == 1` — a fresh client's first build misses, the second must hit. **An always-mismatch regression (broken fold, missing hash field, broken len filter) makes these FAIL** — the exact silent-disable gap round-1 flagged.
- trailing-system (:2051-2055): `hit_count == 1` on the post-pop build, plus the cached-len pin (:2038-2041): `Some(2)` after building a 4-message list with a 2-message trailing system run. **Removing the exclusion fails this directly** (cacheable_len would be 4 → `Some(4)`), and would additionally fail the hit assertion (cached len 4 > the next list's cacheable 3 → the len filter forces a miss → 0 ≠ 1). Double-pinned.
- shrink (:2081-2085), head-swap (:2103-2107), pressure-flip (:2171-2175), re-add-aging-out (:2230-2234): `hit_count == 0` — **an incorrect hit on any of these paths makes them FAIL** (the counter would read 1). The pressure-flip and re-add tests additionally assert the wire effect (historical reasoning actually stripped / the fabricated key actually absent), so a stale hit fails them twice over.

The `== 1` form is strictly stronger than round-1's prescribed `>= 1` — each test client performs exactly two builds, so 1 is the exact expected count (the `fresh` comparison clients are separate instances with their own counters).

### 3. No regression from the fixes

- **cfg(test) gating is airtight**: field, init, increment, and the only readers (tests.rs) are all test-gated; in non-test builds none of them exist. `cargo test --workspace` builds the lib both with and without cfg(test) (unit harness + integration-test linkage), and the parent reports exit=0 warning-free under `deny(warnings)` at both crate roots — consistent with the diff; nothing in the code suggests otherwise.
- **No behavior change outside tests**: `hash_str` changes fingerprint *values* only (store and lookup use the same fold — a value change is invisible to hit/miss semantics); `hit_count` is test-only; `PrefixCache` field visibility widened to `pub(super)` (visible to openai and its descendants — exactly the sibling tests module); doc comments only.
- The commit's non-code content is bookkeeping: the plan file, the backlog in_flight flip, the round-1 report itself, the L3 knowledge amendment, and two L5 sidecar artifacts (the L5 round-2 review + its SPEC record — written after ccb568d landed, so they rode this commit; harmless `.coding/` files).

### 4. Round-1 verified-sound conclusions — spot-checked at this tree

The fixes touched only the hash helpers, the hit counter, PrefixCache visibility, the tests, and docs; every round-1-verified mechanism is byte-for-byte as reviewed (the commit contains the reviewed L3 change plus the fixes). Re-confirmed at this tree: the Cow echo path (`echo_assistant_raw`, request.rs:1070-1098 — Borrowed iff !needs_strip && !needs_readd, re-add checked against pre-strip raw with the mutual-exclusion argument documented); the pre-serialized RawValue body (RequestBody splice, reqwest `.body(String)`); the fingerprint coverage incl. the age-dependent strip/re-add bools and the pressure flag (fingerprint_prefix :230-257); the trailing-system exclusion (:319-324); the retry rebuild via `omit_effort` (openai.rs effort_rejected branch); the trace parse on the blocking pool; the extra_body override-on-collision merge draining the pre-merge `messages_raw` for the cache refresh (:758-797); the estimate split equivalence (:346-425); and the documented TokenAccounting deviation (:332-336).

### 5. Note-4 follow-up — knowledge record amended

`.coding/knowledge/spec/2027-01-05-provider-module-layout-after-the-openai-rs-split-2.md` (:9) now carries the amendment: `build_request_body` as the pre-serialized String path, `build_request_json` as the `#[cfg(test)]` lens, the `omit_effort` retry rebuild, and the PrefixCache — accurate against the code, and landed in the live (superseding) record, which is the right target. Informational only: the amendment opens "Amended 2027-01-07: 2027-01-10 amendment" — the leading date doesn't match the amendment's own date; cosmetic.

## Informational notes (no action required)

1. `hash_message`'s doc says "No allocation" — strictly, `hash_value`'s Number branch allocates via `n.to_string()` when a raw payload contains numbers (tiny, per-number). The claim is accurate for the message-content walk it describes.
2. The `PrefixCache` struct doc still says "a stale hit is impossible by construction" — informal; the referenced `fingerprint_prefix` doc states the precise 2⁻⁶⁴ residual. Fine as-is.
3. `write_usize` width is platform-dependent (8 bytes on 64-bit) — irrelevant here: fingerprints are in-memory only, never persisted or compared cross-platform.

## Constitution checks

- **Documentation sync:** the one stale artifact round-1 flagged (note 4) is amended; module doc comments updated and accurate. PASS.
- **Multi-platform neutrality:** pure Rust (std hash/atomics, serde_json) — no platform APIs, paths, or shell syntax. PASS.
- **File-tools-first policy:** no shell-based file mutation in the change. PASS.
- **Doc comments:** the new `hash_str` helper and the amended docs carry accurate doc comments. PASS.
- **Tests:** 12 new tests (5 echo + 7 cache) all carry hit/miss pins; parent reports `cargo test --workspace` green (warning-free under deny(warnings)); this reviewer is read-only and did not re-run.

## Methodology

Read the round-1 report in full; `git show c3841f7` (complete commit diff); confirmed clean tree (`git diff HEAD`, `git status --short`) and HEAD identity (`git log`). Read request.rs in full (the four hash helpers, `build_request_body`, `echo_assistant_raw`), openai.rs 165-314 (struct, constructors, module gating), and tests.rs 1900-2259 (all 7 cache tests + the echo tests). Search-verified: every `as_bytes`/`write_u8`/`write_usize`/`DefaultHasher` site in the crate, every `hit_count` site, every `OpenAiClient` construction site, and the `#[cfg(test)] mod tests` gating. Read the amended knowledge record.
