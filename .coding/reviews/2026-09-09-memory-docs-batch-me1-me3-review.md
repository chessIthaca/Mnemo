## Verdict: PASS

Review of plan cb74a78b (backlog 679d8aa2, "Memory docs batch: three stale-doc fixes (Me1-Me3)") — doc-only remediation of LOW 1-3 from .coding/reviews/2026-09-08-full-review-memory.md. All uncommitted changes on wt/agenticcoding reviewed: PLAN.md, README.md, src-tauri/src/ipc/memory_debug.rs, src-tauri/src/main.rs, plus the bookkeeping delta (.coding/backlog.jsonl status pending→in_flight for this very item; untracked .coding/plans/cb74a78b.md is the plan file — both expected). 0 high, 0 low findings. No code behavior changes; every rewritten doc claim was verified against the code it documents.

## Me1 — memory_debug_recall doc comment (memory_debug.rs:225-235) ✅

New comment verified claim-by-claim:
- "goes through `MemoryStore::recall_peek`" — code at :255 calls `store.recall_peek(&query, &filter)`, with the pre-existing "Peek, not bump" inline note (:252-254). The old comment's bump claim and "no read-only recall path today" are gone.
- "the same non-bumping path production auto-recall takes" — `auto_recall` calls `store.recall_peek` at src/agent/turn.rs:1846 and :1857 (both the cache-miss and fresh-query branches).
- "no `access_count` / `last_accessed_at` update, no decayed-strength boost" — consistent with the peek path's contract (batch bump exists only on the bumping `recall` path, per the prior round's store verification).
- The unchanged first paragraph still matches: the debug filter is `MemoryFilter::new().include_working()` (:248) while auto-recall uses `MemoryFilter::new().limit(5)` without it (turn.rs:1834) — "includes working-tier memories (unlike the production auto-recall)" holds.

## Me2 — startup re-embed comment (main.rs:1366-1385) ✅

- "gitignored per-machine cache (.gitignore)" — .gitignore:46-47 ignore `.coding/memory.db` (+ `-wal`/`-shm`); the false "committed to git / travels via git" rationale is gone.
- "a Settings model swap (a config save changes the model) re-wires the embedder under an existing DB" — matches `reembed_if_needed`'s own doc (src/memory/mod.rs:49-52: extracted from the duplicated startup + post-config-save rewire.rs paths).
- "a legacy/empty fingerprint set … has nothing to compare against" and "fingerprint (model_id + dim) is absent … re-embed all rows" — matches mod.rs:63-68: re-embed unless the stored set is exactly one fingerprint equal to (expected_model, expected_dim); an empty/legacy set has len 0 → fires.
- "Fire-and-forget: never blocks startup" — `tauri::async_runtime::spawn` at main.rs:1399-1401.
- "Only fires when a bundled model is configured AND loaded successfully (status Ready)" + both hash-path skips + pending-load skip — exactly `should_reembed_at_startup` (src-tauri/src/startup.rs:78-91: `configured.is_some()` && non-hash (case-insensitive) && `!pending_download` && `!pending_load` && `Ready`).
- Tests: all five `reembed_*` tests present and aligned (startup.rs:263-348 — runs-when-ready, hash opt-out case-insensitive, download/load pending, non-Ready statuses, unconfigured).
- Non-blocking observation (pre-existing text this change deliberately preserved, not introduced): "fingerprint … absent from the stored fingerprints" doesn't spell out the mixed-fingerprint case (interrupted re-embed → set contains the expected fingerprint *plus* another, which also fires per mod.rs:63-68). The sentence states sufficient, not necessary, conditions; the rationale it was rewritten for is now correct. No action required for this plan.

## Me3 — PLAN.md + README.md tool surface ✅

- PLAN.md memory-tools bullet: now lists exactly the registered six (factory.rs:1155-1196: write, search, consolidate, update, supersede, delete) + git bridge; "one tool replaced the six former per-purpose read tools (they differed only by a record-type constant, a tier, or the presence of a query)" matches retrieval.rs:5-12 verbatim in substance; "query-less browse mode (newest-first, narrowed by record_type/tier/prefix) replaces the former list-only tool" matches memory_search's browse contract. The obsolete "Phase-1"/"Phase-3" qualifiers are correctly dropped.
- PLAN.md reviewer line: "QUERIES (`memory_search`, `backlog_list`)" — matches spawn.rs REVIEWER_BASE_TOOLS (`memory_search` :463, `backlog_list` :457; the comment there carries the same "replaced memory_recall / memory_list / …" note).
- README.md:42 tail — hygiene trio + `memory_consolidate` for distillation + single retrieval tool `memory_search` (ranked query or query-less browse narrowed by record_type/tier/prefix) + read-only `git_log`/`git_show`: matches the registered surface.
- README.md:53 — mandatory trigger now reads "`memory_search` before non-trivial work on existing code" (was `memory_recall`).

## Acceptance criteria

1. **Retired-name grep**: `memory_recall|memory_list|plans_search|reviews_search|past_fixes|context_pack` over README.md + PLAN.md → **zero matches** (verified by search over both files).
2. **Doc claims match code**: verified above for all four files; no new staleness introduced.
3. **cargo test**: reported green on this working tree — 297 passed, exit=0, warning-free under `#![deny(warnings)]` (the run also proves both new doc comments compile). Reviewer has no shell; relied on the reported run, consistent with doc-only deltas.

## Standing checks

- **Documentation sync**: swept the full README Memory section (lines 38-47) and the steering bullet (:53) plus PLAN.md's memory/reviewer text — no other stale sites: :40 (FTS5+ONNX, cosine 0.2/0.35), :43 (derived index + startup reconciliation), :44 (DBs-are-caches — now fully consistent with the corrected main.rs comment), :46-47 all match the implementation per the prior round's verification and are untouched. The backlog.jsonl delta is this plan's own bookkeeping.
- **Multi-platform neutrality**: PASS. Doc-only change — two Rust doc comments (no code), two markdown files. No platform-specific APIs, paths, or shell syntax introduced; the "wholesale project-directory copy" phrasing in main.rs is platform-neutral.
- **Security**: no behavior change; the corrected comment documents a genuinely read-only path (no new attack surface, no secrets).
- **Constitution**: no violations — tests reported green, existing style preserved, no `#[allow]` additions, nothing committed.
