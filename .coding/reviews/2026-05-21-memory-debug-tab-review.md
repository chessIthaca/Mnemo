# Memory Debug Tab — Review

**Scope:** All uncommitted changes for the new right-panel "Memory" debug tab.
**Files reviewed:** `src-tauri/src/ipc/memory_debug.rs` (new), `src-tauri/src/ipc/mod.rs`, `src-tauri/src/main.rs`, `frontend/src/lib/tauri.ts`, `frontend/src/hooks/agentState.ts`, `frontend/src/hooks/rightPanelViews.tsx`, `frontend/src/components/views/MemoryDebugView.tsx` (new). Surrounding code read for verification: `src/memory/mod.rs`, `src/memory/types.rs`, `src/memory/embedder.rs`, `src-tauri/src/ipc/state.rs`, `src-tauri/src/ipc/trace.rs`, `src-tauri/src/ipc/error.rs`, `frontend/src/hooks/rightPanelViews.test.ts`.

## Summary

The implementation is solid and faithfully mirrors the Trace tab's architecture. **Concurrency is clean** (no lock guards held across `.await`), **security is clean** (no new secret exposure; tier is enum-validated), and **constitution compliance is clean** (no `#[allow]`, no dead code, all public items documented, parity test holds at 11 entries). The findings below are behavioral / UX / performance notes, not crashes or security holes.

## Correctness

### C1 — (Medium) Debug "recall test" mutates production memory state (access_count + last_accessed_at → future ranking)
**File:** `src-tauri/src/ipc/memory_debug.rs:229`

`memory_debug_recall` calls `store.recall(&query, &filter)`, and `MemoryStore::recall` (`src/memory/mod.rs:791-798`) unconditionally runs `batch_access` on the returned ids — bumping `access_count` and setting `last_accessed_at = now`. Because decayed strength is `strength::decay(m.strength, now - m.last_accessed_at)` (`mod.rs:768`), a debug recall test *freshens* the recalled memories, raising their decayed strength and thus their score in **future production auto-recalls**. A user debugging recall ranking therefore inadvertently changes it by testing.

This is inherent to the `recall` API (there is no read-only recall path; production auto-recall does the same), so it is not a defect in the new code per se — but it is a non-obvious side effect of a tool whose purpose is *inspection*. Worth at minimum a doc-comment note on `memory_debug_recall` (and ideally a future read-only `recall` store method). The `access_count` bump is at least *visible* in the browser (`×{m.access_count}`, `MemoryDebugView.tsx:232`), which softens the surprise.

### C2 — (Low) `memory_debug_overview` swallows `stored_model_fingerprints` errors → false "Mismatch" alarm
**File:** `src-tauri/src/ipc/memory_debug.rs:147-153`

```rust
let fingerprints = store.stored_model_fingerprints().await.unwrap_or_default();
let fingerprint_matches = fingerprints.iter().any(|(m, d)| m == &model_id && *d == dim);
```

A transient DB error → `unwrap_or_default()` → empty vec → `fingerprint_matches = false` → the UI shows a red **"Mismatch"** indicator (`MemoryDebugView.tsx:147-151`) with the tooltip "stored vectors are in a different space — recall is degraded." That is a false alarm: the vectors may be fine; the *query* failed. For a debug tool whose job is to surface real problems, a false "mismatch" is misleading. Same pattern in `memory_counts` (`.unwrap_or(0)` per tier, lines 264/270/276/280) and `memory_debug_list` (`.unwrap_or_default()`, line 197) — a DB error silently renders as zeroed counts / empty list rather than an error banner. Consider propagating these errors (the commands already return `Result<_, IpcError>`) or at least distinguishing "empty" from "errored".

## Bugs

### B1 — (Low, UX) Recall error renders "No matches." alongside the error banner
**File:** `frontend/src/components/views/MemoryDebugView.tsx:338-340, 409-414`

On a recall error the handler sets both `setError(...)` and `setRecallResults([])`:
```tsx
} catch (e) {
  setError(errMsg(e));
  setRecallResults([]);
}
```
`recallResults !== null` then renders the results box, whose empty-state text is "No matches." So the user sees the red error banner *and* "No matches." in the recall box — which is misleading (an error is not "no matches"). On error, keep `recallResults` as `null` (so the results box stays hidden) or render the error inside the results box.

## Performance

### P1 — (Low) `memory_counts` loads full rows + embeddings every 2s just to count
**File:** `src-tauri/src/ipc/memory_debug.rs:260-288` (and `memory_debug_list:197-199`)

`memory_counts` calls `list_by_tier` once per tier (4×) and takes `.len()`. `list_by_tier` → `load_all(Some(tier))` (`src/memory/mod.rs:853-855, 420-448`) has **no SQL LIMIT** and SELECTs the `embedding` BLOB column, so it loads every full row + vector per tier just to count them. This runs on the 2s overview poll (`POLL_MS = 2000`, `MemoryDebugView.tsx:31, 301`). The code documents the tradeoff ("a project store is small", lines 257-259), and the tab only mounts when active — but a `SELECT COUNT(*) FROM memories WHERE tier = ?` per tier would be dramatically cheaper and avoid decoding embedding vectors that are immediately discarded. `memory_debug_list` has the same shape: it loads the full tier then truncates in Rust (`memories.truncate(n)`, line 199) rather than pushing the limit into SQL. Low severity given the documented "small store" assumption, but the 2s polling amplifies it.

## Security

**No findings.** Confirmed:

- **No new secret exposure.** Memory `content` is truncated to 500 chars in `to_wire` (`m.content.chars().take(500)`, line 242 — char-boundary safe ✓). The `data` JSON is passed through untruncated by `to_wire`, but every string leaf in `data` was already capped to 500 chars at write time via `cap_json_strings` (`src/memory/mod.rs:47-69, 1056`), so it is bounded. Memory content/data can include tool outputs the agent saw (file contents, search results) — this is the agent's own working memory, already visible in the conversation and in the existing Trace/Output tabs. No API keys are deliberately stored as memory content. The debug tab is a local dev tool surfacing data the user already has.
- **No user-controlled path segments.** The `tier` parameter is validated against the `MemoryTier` enum via `MemoryTier::from_str` (`src/memory/types.rs:36-44`), returning `Option` → mapped to `Err` via `.ok_or_else(...)` (line 187-188). Unknown tiers are rejected. ✓
- **No injection surface.** All store queries use parameterized SQL (verified in `src/memory/mod.rs`).

## Constitution Compliance

**No findings — fully compliant.**

- **No `#[allow(...)]` suppressions** anywhere in the new Rust file. ✓
- **No dead code / `_`-prefixed unused bindings.** Every binding in `memory_debug.rs` is consumed; private helpers `to_wire` and `memory_counts` are both called. The imports (`serde::{Deserialize, Serialize}`, `tauri::State`, `MemoryFilter`/`MemoryStoreTrait`/`MemoryTier`/`ScoredMemory`, `IpcError`, `IpcState`) are all used — `MemoryStoreTrait` is required in scope for the `list_by_tier`/`recall`/`stored_model_fingerprints` trait calls on `&Arc<MemoryStore>`. ✓
- **All public items have doc comments.** `MemoryDebugWire`, `ScoredMemoryDebugWire`, `MemoryCounts`, `MemoryDebugOverview`, `MAX_CONTENT_CHARS`, and the three `#[tauri::command]` fns all have doc comments; every struct field is documented. Private helpers `to_wire`/`memory_counts` are documented too. ✓
- **Frontend parity test holds.** `ALL_RIGHT_PANEL_TABS` and `RIGHT_PANEL_VIEWS` both have 11 entries (md/plan/diff/output/files/stats/trace/backlog/browser/game/memory). `rightPanelViews.test.ts` asserts (a) every tab id has a registry entry, (b) no registry id is outside the union, (c) equal length — all pass with the new `"memory"` entry added to both sites. ✓
- **`m.data != null` is correct.** `data` is typed `unknown` (a JSON value). A truthy check (`m.data`) would wrongly hide `false`/`0`/`""` payloads; `!= null` correctly renders the JSON block for any non-null value and skips only `null`/`undefined` (the `Value::Null` case). ✓
- **Polling cleans up on unmount.** The overview `useEffect` (`MemoryDebugView.tsx:288-306`) returns a cleanup that sets `cancelled = true` and `clearInterval(timer)`; the in-flight async `tick` checks `cancelled` before `setState`. The tier-filter `useEffect` (309-326) likewise sets `cancelled = true` on cleanup. ✓

## Concurrency (verified per the review brief)

**No findings — clean.**

- **`embedder_status` RwLock guard is dropped before any `.await`.** In `memory_debug_overview` (lines 118-123), `.read().expect(...).clone()` clones the `EmbedderStatus` into an owned `status`; the `RwLockReadGuard` is a temporary dropped at the `;`. The subsequent `.await` points (`stored_model_fingerprints().await` line 149, `memory_counts(store).await` line 155) run with no guard held. ✓
- **`config` async Mutex guard is dropped before any `.await`.** The `configured_model` block (lines 126-129) acquires `state.project.config.lock().await`, reads + clones the field, and the block scope drops the guard before the `let Some(store)` borrow and any store `.await`. ✓
- **`memory_debug_recall` holds no lock across `.await`.** It borrows `&Arc<MemoryStore>` (no lock) and `.await`s `store.recall()`, which internally moves its DB work onto `spawn_blocking` (`src/memory/mod.rs:697`). ✓
- **No nested locking / deadlock risk.** The three locks touched (`embedder_status` RwLock, `config` async Mutex, the store's internal `Mutex<Connection>`) are never held simultaneously. ✓

## Correctness — verified items (no findings)

- **`memory_debug_overview` None case** (lines 131-141): returns `model_id = "<none>"`, `dim = 0`, `MemoryCounts::default()` (all zero), empty `fingerprints`, `fingerprint_matches = false`, and the real `status` + `configured_model`. No panic. ✓
- **`memory_debug_list` tier parsing** (lines 186-195): `MemoryTier::from_str` returns `Option`; `.ok_or_else(|| IpcError::msg(...))` maps `None` → `Err`. Unknown tier is rejected; `None` lists all four tiers. ✓
- **`memory_debug_recall` uses `.include_working()`** (line 225): `MemoryFilter::new().include_working()` — confirmed it sets `exclude_working = false` (`src/memory/types.rs:188-191`), so the debug view surfaces raw working-tier tool events unlike production auto-recall. ✓
- **Content truncation is char-boundary safe** (line 242): `.chars().take(MAX_CONTENT_CHARS).collect()` iterates Unicode scalar values, never splitting a multi-byte char. ✓
- **`EmbedderStatus` serialization matches the TS type + `StatusBadge`.** Externally-tagged + `rename_all = "lowercase"` (`src/memory/embedder.rs:89-99`): unit variants → bare strings (`"ready"` etc.), `Downloading` → `{"downloading": {model, progress}}`. TS `status: string | Record<string, unknown>` and `StatusBadge`'s `"downloading" in status` check (line 66) handle both shapes. ✓
- **`ScoredMemoryDebugWire` `#[serde(flatten)]`** (line 57) inlines `MemoryDebugWire` fields alongside `score`; the TS interface (flat fields + `score`) matches. ✓
- **`fingerprints: Vec<(String, usize)>`** serializes as `[[string, number], ...]`; TS `[string, number][]` and the `[m, d]` destructure (line 155) match. ✓

## Recommendation

The diff is safe to commit after addressing **C2** (false "Mismatch" on DB error — the most concrete user-facing bug) and optionally **B1** (recall-error UX). **C1** and **P1** are acceptable to ship as-is with a doc-comment note (C1 is inherent to the `recall` API; P1 is a documented tradeoff for small stores). No blockers.
