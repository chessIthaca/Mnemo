# Review — memory recall self-reinforcement fix + expandable auto-recall hits

**Branch:** feat/memory-recall-fixes · **Date:** 2026-08-20 · **Reviewer:** reviewer subagent (read-only)

**Scope reviewed:** all uncommitted changes (`git diff HEAD` + `git status --short`): src/memory/mod.rs, src/agent/turn.rs, src/agent/tests.rs, src/runtime/channels.rs, src-tauri/src/ipc/memory_debug.rs, tests/contract_fixtures.rs, frontend/src/lib/types.ts, frontend/src/lib/ipc-fixtures/event-memory-recalled.json, frontend/src/lib/ipc-contract.test.ts, frontend/src/hooks/agentEventReducer.ts, frontend/src/hooks/useAgentStore.test.ts, frontend/src/components/chat/Message.tsx. Bookkeeping noted, not deep-reviewed: .coding/backlog.json (user backlog items + screenshots), .coding/plans/stack.json (active plan id), .coding/plans/420ed603-….md (the plan doc).

## Verdict: NO FINDINGS — the diff is clean.

Every review axis from the task was verified against the actual code; details below.

### (a) recall_peek used by ALL passive callers and ONLY those — VERIFIED

Grep of every `.recall(` / `.recall_peek(` call site in the repo:

**recall_peek (no bump) — 5 sites, all correct:**
- src/agent/turn.rs:385 and :396 — the two FRESH auto-recall branches (passive prompt injection). ✓
- src-tauri/src/ipc/memory_debug.rs:255 — `memory_debug_recall` debug preview (passive; resolves the 2026-05-21 review note about debug recalls perturbing production rankings). ✓
- src/memory/mod.rs:837 — `recall()` delegates to `recall_peek` then applies `batch_access` (internal composition). ✓
- src/memory/mod.rs:1790 — the new regression test. ✓

**recall (bumping) — production sites, all correct:**
- src/tool/memory/mod.rs:171 — the explicit `memory_recall` tool (deliberate, agent-initiated — the one caller that SHOULD reinforce). ✓
- src/memory/maintenance.rs:333 — inside a `#[tokio::test]` (`rebuild_search` test sanity check), not production code. ✓
- All other `.recall(` matches are in src/memory/mod.rs's `mod tests` or .coding/reviews docs. ✓

**Adjacent passive read path checked too:** `strongest` (session primer, mod.rs:1066–1119) reads, sorts by decayed strength, truncates, logs — no `batch_access`. ✓

### (b) explicit tool path still bumps — VERIFIED

`MemoryStore::recall` (mod.rs:836–845) = `recall_peek` + `batch_access(ids)`; `batch_access` SQL (mod.rs:1007–1035: `access_count + 1, last_accessed_at = now`) unchanged. `recall_bumps_access_via_batch` test untouched and still pins the bump. `log_access("read", …)` now lives in `recall_peek` (mod.rs:978) so it journals on BOTH paths — intentional (UI visibility), and `log_access` (mod.rs:356) only appends to the in-memory ring buffer; it does not mutate access state, so it cannot reintroduce reinforcement. ✓

### (c) event serde shape parity — VERIFIED

- `RecallHit { pub tier: String, pub title: String }` (channels.rs:88) derives Serialize/Deserialize; field names serialize as `tier`/`title` verbatim. ✓
- `SerializableAgentEvent` is `#[serde(tag = "kind", rename_all = "snake_case")]` (channels.rs:292) → `"memory_recalled"`; both `AgentEvent::` and `SerializableAgentEvent::MemoryRecalled` are `{ hits: Vec<RecallHit> }`; the into_serializable arm passes the vec through (channels.rs:507). ✓
- `tests/contract_fixtures.rs:213` sample (3 hits: semantic/"merge instructions", procedural/"release checklist", episodic/"session summary 2026-04-20") ≡ `frontend/src/lib/ipc-fixtures/event-memory-recalled.json` — same pairs, same array order. ✓
- `frontend/src/lib/types.ts` union member `{ kind: "memory_recalled"; hits: { tier: string; title: string }[] }` and the optional `hits` on the memory TranscriptEntry variant. ✓
- `MemoryTier::as_str()` (types.rs:26) returns the lowercase strings ("semantic"/"procedural"/"episodic") used by tests/fixtures. ✓
- No stray consumers of the old shape: repo-wide grep for `top_title` hits only .coding plan/review docs. ✓

### (d) fixture drift detector — VERIFIED

`assert_fixture` (tests/contract_fixtures.rs:45–68) compares parsed `serde_json::Value` (whitespace/key-order-insensitive; arrays order-sensitive). Rust sample ≡ JSON fixture content and hit order; `event_fixtures_match_serde` will pass. The channels.rs roundtrip test was updated to the new shape and asserts no oneshot senders. ✓

### (e) turn.rs emission gating — VERIFIED

`fresh_recall` is set true only on `Ok` in the two peek branches (turn.rs:387, :398); error arm → `Vec::new()` (fresh stays false); cache-reuse branch (turn.rs:382–384) leaves fresh false. Emission is guarded by `if !results.is_empty() { if fresh_recall { … } }` (turn.rs:406–428) — fires only on FRESH recalls with hits>0, never on cache-reuse or empty/error. Hit construction uses `sm.memory.tier.as_str().to_string()` + `title.clone()`; the vec is bounded by the recall `limit(5)` filter. The doc comment on the channels.rs variant ("not on cache reuse or empty results") matches the implementation. ✓

### (f) MemoryEntryCard (Message.tsx) — VERIFIED

- Hooks-in-switch fixed correctly: `useState` is at the top of the extracted `MemoryEntryCard` component; the `case "memory":` arm just returns `<MemoryEntryCard entry={entry} />`. ✓
- Keyboard access: `role="button"`, `tabIndex={0}`, onKeyDown handles Enter + Space with `preventDefault()` — byte-identical interaction pattern to ToolCard (Message.tsx:464–473), including the span-instead-of-button rationale. ✓
- Non-auto-recall entries (`entry.name !== "auto-recall"` or no hits) render the same visual line in a plain non-interactive div — unchanged behavior for memory_recall/memory_write entries. ✓
- `arePropsEqual` compares `hits` by reference with a comment justifying it; sound because the reducer stamps a fresh array per event and entries are never mutated after push. ✓
- Imports (`useState`, `ChevronDown`, `ChevronRight`) pre-existing at Message.tsx:1–2; the diff needed no import change. Index keys on the static hits list are fine. ToolCard likewise has no `aria-expanded`, so no consistency regression. ✓
- Reducer (agentEventReducer.ts:702–720): `showMemoryActivity` gating preserved, snippet format `"N hit(s) — top: …"` preserved via `event.hits.length` / `event.hits[0]?.title ?? "—"`, entry pushed finalized (`running: false`), `capTranscript` applied. ✓

### (g) constitution — VERIFIED

- Doc comments on all new public items: `RecallHit` struct + both fields, `recall_peek` (trait doc explaining the self-reinforcement rationale), both `MemoryRecalled` variants, plus JSDoc on `MemoryEntryCard` and the `hits` TranscriptEntry field. ✓
- No `#[allow(...)]` added anywhere in the diff. ✓
- Regression test per defect: `recall_peek_does_not_bump_access` (mod.rs:1766+) pins both halves — peek leaves `access_count`/`last_accessed_at` untouched AND explicit recall still bumps — and fails without the fix (old behavior would bump on the peek call). The agent-level test `auto_recall_emits_memory_recalled_event` was updated to the vec shape (len ≥ 1, first title, tier string). ✓
- Trait surgery was required after all (the plan md speculated it might not be): the method was added to `MemoryStoreTrait` with a default-free signature — single impl `MemoryStore` (mod.rs:779), no mocks exist (confirmed by grep + the 2026-08-18 review), so nothing else needed updating. ✓

### Non-findings (checked, deliberately not raised)

- `log_access`'s private doc ("Called by `write`, `recall`, and `strongest`", mod.rs:354) remains transitively accurate — `recall` still triggers it via delegation to `recall_peek`.
- Staging state: `.coding/backlog.json` and `.coding/plans/stack.json` are staged (first-column `M`), source files unstaged — commit will need `git add` of the rest; procedural, not a code issue.
- Green test matrix (root 1076 / src-tauri 75 / vitest 255 / build clean) accepted as reported; consistent with the diff (all shape consumers updated in lockstep).
