# Review — Full-Review Findings Implementation (2026-08-11)

**Scope:** All uncommitted changes in the working tree (`git diff HEAD`).
**Context:** Implements the action items from `fullreview.md` (P1, P2, A1, A2, A3, P3, P4, Q1, Q2, Q4, S9).
**Verdict:** ✅ **No correctness, bug, or security findings.** Two minor code-quality nits (stale test name/comment; untested fallback path). All 567 Rust lib tests + 86 vitest tests pass.

---

## Summary by finding

| Finding | Status | Notes |
|---|---|---|
| P1 + A1: provider cache in `ConfigModelResolver` | ✅ Correct | Cache key `(endpoint, model)` is right; invalidation on `set_config` fires on every config-change path (`save_settings` + `save_endpoints` both route through `sync_model_resolver`). No TOCTOU hazard (benign double-build on concurrent miss). |
| P2: `FtsResult` enum + `contains_ascii_ci` + capped fallback | ✅ Correct | The three-way distinction is sound. `contains_ascii_ci` is UTF-8-safe (no multi-byte sequence byte falls in the ASCII-letter range, so no false positives across char boundaries). |
| A3 + P4: `list_agents` snapshot-then-drop | ✅ Correct | Manager lock dropped before acquiring `agent_loops`; invariant documented on `IpcState`. |
| A2: `SessionState` extraction | ✅ Correct | All `AgentLoop` accesses migrated to `self.session.*`; no stale flat-field references remain. |
| P3: documented `canonicalize` trade-off | ✅ Doc-only | Comment is accurate. |
| Q4: `Box::leak` → stored `caps` field | ✅ Correct | All three mock providers (`PausingMockProvider`, `RecordingProvider`, `PausingProvider`) fixed consistently. |
| Q2: `cap_tool_output` moved to `mod.rs` | ✅ Correct | `git.rs` import (`use crate::tool::agent::cap_tool_output;`) still resolves; `shell.rs` uses `super::cap_tool_output`. |
| Q1: truncate renames | ✅ Correct | All call sites + tests updated. |
| S9: Run-All `git reset --hard` warning in UI | ✅ Correct | Warning string is clear and accurate. |

---

## Findings by severity

### Correctness — no findings

- **Provider cache invalidation (P1):** Verified that `set_config` is the *only* mutation path for the resolver's config, and it's called from `sync_model_resolver` (settings.rs:486-489), which is shared by both `save_settings` and `save_endpoints`. The `Arc<RwLock<Config>>` is *not* shared with any other writer (main.rs:408-410 constructs it fresh and hands it only to the resolver), so there's no path that mutates config in-place without clearing the cache. ✅
- **`FtsResult` distinction (P2):** The `NoMatches` → empty (no scan) vs `Unavailable` → capped scan distinction is correctly wired in `recall` (mod.rs:544-559). The `fts_query.is_empty()` (whitespace-only query) case now returns `NoMatches` instead of the old `None` (which triggered a full scan) — this is the intended behavior change and is tested. ✅
- **`SessionState` extraction (A2):** Searched all `src/agent/*.rs` — zero remaining `self.session_id` / `self.last_prompt_tokens` flat-field accesses on `AgentLoop`. The `self.session_id` hits in `src/runtime/agent.rs` are on `AgentTask` (a separate struct with its own field), not `AgentLoop`. ✅
- **`contains_ascii_ci` UTF-8 safety:** The byte-level sliding window is safe because UTF-8 multi-byte sequences use leading bytes (0xC0–0xFD) and continuation bytes (0x80–0xBF), neither of which overlaps the ASCII-letter ranges (0x41–0x5A, 0x61–0x7A). A continuation/leading byte can never be mistaken for an ASCII letter, so no false-positive match can occur across a character boundary. The `to_ascii_lowercase()` on bytes is a no-op for non-ASCII bytes. ✅
- **`load_recent_locked` column order:** The SELECT column list (`id, tier, title, content, data, strength, access_count, created_at, last_accessed_at, source_session_ids, embedding`) matches `parse_memory_row`'s `row.get(N)` indices exactly. ✅

### Bugs — no findings

- **Lock-ordering (A3/P4):** `list_agents` now snapshots `(id, name, running, parent_id)` under the manager lock, drops it, then reads models under `agent_loops`. No nested acquisition. The `running` flag is snapshotted under the manager lock (correct — `is_running()` reads manager state), and the model read under `agent_loops` tolerates a loop removed between snapshot and read (yields `None`). ✅
- **Cache key:** `(model.endpoint.clone(), model.model.clone())` — `ModelRef` fields are `String`, so `clone` is correct. Two different endpoints with the same model name get distinct entries. ✅
- **Concurrent `build_turn_provider`:** Multiple agents calling concurrently on a cache miss each build a provider; one wins the `insert`, the other's `Arc` is dropped. Benign — no deadlock (read lock released before write lock acquired), no corruption. ✅

### Security — no findings

- No new secret-logging surfaces introduced. The provider cache stores `Arc<dyn LlmClient>` (which holds the `reqwest::Client` with its configured auth), but the cache is process-local and never serialized/logged. `set_config` clears it on key changes. ✅
- The Run-All warning (S9) accurately describes the `git reset --hard` data-loss surface; no behavior change, just clearer UX. ✅

### Constitution compliance — no findings

- All new public items have doc comments (`SessionState`, `FtsResult` variants, `contains_ascii_ci`, `load_recent_locked`, `cap_tool_output` in its new location, the cache field). ✅
- Tests added for P1 (cache reuse + invalidation), P2 (`contains_ascii_ci` unit test + `recall_no_fts_match_returns_empty_no_full_scan`). ✅
- `cargo test --lib` → 567 passed, 0 failed. `vitest` → 86 passed. ✅
- Line-ending style preserved (file tools normalize). ✅

---

## Minor nits (not blocking)

### N1 (low) — Stale test name + doc comment describe the OLD behavior
**File:** `src/memory/mod.rs:1296-1333`
The test `recall_falls_back_to_full_scan_when_no_fts_match` was updated to assert `results.len() == 0` (the new empty-result behavior), but its **name** and **doc comment** still describe the old full-scan fallback:
```
// When the query shares no keyword with any memory, FTS returns no
// candidates and recall falls back to a full scan (cosine-only). ...
```
The name `recall_falls_back_to_full_scan_when_no_fts_match` is now misleading — it no longer falls back to a full scan. Recommend renaming to `recall_returns_empty_when_no_fts_match` and updating the comment to match the assertion. (The inline comment at line 1327-1330 *was* updated correctly; only the function-level name + header comment are stale.)

### N2 (low) — `load_recent_locked` (the `Unavailable` fallback) is untested
**File:** `src/memory/mod.rs:347-373`
The new `load_recent_locked` function (the capped 200-most-recent full-scan fallback for when FTS5 is unavailable) has no test. In the test environment FTS5 is always available (bundled SQLite, per `fts5_is_available_with_bundled_sqlite`), so the `FtsResult::Unavailable` arm of `recall` is never exercised. The function is a trivial variant of `load_all_locked` (same columns + `parse_memory_row`, plus `LIMIT`/`ORDER BY`), so the risk is low, but a unit test that calls `load_recent_locked` directly (or a test with FTS disabled) would close the gap. Not blocking.

### N3 (informational) — `recall_fts_special_characters_do_not_error` comment mentions "fall back to a full scan"
**File:** `src/memory/mod.rs:1339`
The test's inline comment says "It should either match via the literal token or fall back to a full scan." With the new behavior, a no-match special-character query returns empty (no full scan) rather than falling back. The test only asserts no-panic (no count check), so it still passes, but the comment is slightly stale. Cosmetic only.

---

## Tests run

- `cargo test --lib` → **567 passed, 0 failed** (3.10s)
- `cargo test --lib model_resolver` → **9 passed** (incl. 2 new cache tests)
- `cargo test --lib memory::` → **53 passed** (incl. 2 new P2 tests)
- `npx vitest run` → **86 passed** (6 files)
