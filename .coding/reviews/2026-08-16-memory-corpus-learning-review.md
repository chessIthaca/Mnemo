# Review — Memory corpus learning + convergent distillation (plan 1b9beedc)

Reviewed ALL uncommitted changes vs HEAD e9ba75c (git status / git diff HEAD): 19 files, ~1391 insertions. Plan steps 1–8 are implemented; step 9 (test/commit) is the active step. Reviewed: consolidation merge correctness, corpus_digest, "hash" tri-state across every consumer, background download + install_embedder, validate_request_messages, trace.rs force-mirror, diagnostics, constitution compliance.

## Findings

### Medium

**M1 — main.rs reembed guard is case-SENSITIVE while the sentinel contract is case-insensitive everywhere else (correctness inconsistency, destructive-ish consequence).**
`src-tauri/src/main.rs:719-721`: `let is_hash_opt_out = configured_bundled == Some(EMBEDDING_MODEL_SENTINEL_HASH);` compares `== "hash"`, but every other sentinel check uses `eq_ignore_ascii_case` (`build_embedder` client_factory.rs:181, `embedder_startup_plan` client_factory.rs:198). With a hand-edited config value `"HASH"` (which the backend otherwise honors as the opt-out), the guard does NOT skip: `embedder_startup_plan("HASH")` → `HashOptOut` → status Ready (set by build_embedder) → the reembed task fires with the hash embedder and rewrites every stored MiniLM row to hash/768 — exactly the "re-embedding with hash would destroy stored semantic vectors" case the guard's own comment (main.rs:714-717) claims to skip. Recoverable (re-selecting a model re-embeds), but the two hash paths are not both skipped for "HASH", and the guard's case-sensitivity contradicts the documented sentinel contract.
Fix: `configured_bundled.is_some_and(|v| v.eq_ignore_ascii_case(EMBEDDING_MODEL_SENTINEL_HASH))` (or better, derive the flag from the `EmbedderStartupPlan` variant instead of re-parsing the string).

**M2 — rewire.rs (Settings save path) lacks the sentinel guard the plan added only to main.rs (correctness inconsistency).**
`src-tauri/src/ipc/rewire.rs:35-84`: the documented user flow "Settings → None (keyword-only)" → `save_settings` persists `"hash"` → `rewire_vision_and_embedder` → `build_embedder` returns HashEmbedder with status **Ready** → the `status_snapshot == Ready` check at rewire.rs:49 passes → `reembed_all(hash embedder)` runs, destroying the stored MiniLM vectors + rewriting every row (full-store CPU burn) at the exact moment the user opted out. main.rs skips both hash paths; rewire.rs skips neither. The plan (step 4) only guarded main.rs; its runtime twin was missed. Consequence is recoverable (fingerprint mismatch re-embeds when a real model is re-selected), but it contradicts the plan's stated rationale ("re-embedding with hash would destroy stored semantic vectors") and the two code paths now disagree on identical config.
Fix: in `rewire_vision_and_embedder`, skip the reembed task when `cfg.general.general.bundled_embedding_model` is the sentinel (case-insensitive), mirroring main.rs.

### Low

**L1 — trace.rs `maybe_log_error` reorder creates a (currently unreachable) late-set-path regression.**
`src/provider/trace.rs:432-441`: the `error_logged_ids` dedupe insert now happens BEFORE the `error_log_path` None check (it was after, pre-change). A request that reaches its terminal error state while `error_log_path` is still None is now permanently deduped: a later `set_error_log_path` + a subsequent record mutation no longer captures its provider-errors.jsonl line (previously it would). Unreachable in the app (the path is set at startup, main.rs:598-602, before any request) and in tests; the forced traces mirror is unaffected (its own path gate is independent). Low.
Fix: keep the dedupe insert where it is but move the DirtyForced/traces handling to also run on the first error regardless of error_log_path, or document the loss; simplest: leave as-is with a comment, or move the `error_log_path` None early-return back above the dedupe insert.

**L2 — procedural frequency math can overflow/truncate (u32).**
`src/memory/consolidation.rs:478-483`: `prior.data.get("frequency").and_then(|v| v.as_u64()).unwrap_or(0) as u32 + 1` — a JSON frequency > u32::MAX truncates via `as u32`, and `+1` at u32::MAX panics in debug builds. Practically unreachable (frequency is written by this same code, min 1), but the merge path is new.
Fix: `u32::try_from(...).unwrap_or(u32::MAX).saturating_add(1)`.

**L3 — "hash" sentinel leaks as a model id in the Memory debug overview.**
`src-tauri/src/ipc/memory_debug.rs:128`: `configured_model` is the raw config value; a keyword-only user sees `configured: "hash"` in the debug tab UI (the one display path that doesn't map the sentinel — settings GET returns it raw by design and the frontend maps it, but this command doesn't).
Fix: map the sentinel to `None`/`"<none>"` in the overview DTO.

**L4 — frontend sentinel mapping is case-sensitive while the backend is case-insensitive.**
`frontend/src/components/settings/sections/EmbeddingSection.tsx:54`: `raw === "hash"` — a hand-edited `"HASH"` config renders as a selected id matching no catalog radio (no radio checked, save would re-persist "HASH"). Harmless (backend still honors it) but inconsistent with the backend contract.
Fix: `raw?.toLowerCase() === "hash"`.

**L5 — newly extracted (and never-recalled merged) facts carry `last_accessed_at = 0` → decayed strength ≈ 0 in recall ranking.**
`src/memory/consolidation.rs:387-392` / `492-497` (new facts: `Memory::new(..., 0)` — pre-existing) and the new merge paths at 381/486 which deliberately preserve the prior's `last_accessed_at`. With `strength::decay` (strength.rs:44-47, 7-day tau), `elapsed = now - 0 ≈ 1.7e9s` makes decayed strength ~0, so a fresh extracted fact ranks below every episodic memory until first recalled — and a never-recalled fact can be permanently buried (it never surfaces, so never gets an access bump). Pre-existing for new facts; the new merge path now extends it to reinforced facts (strength 0.5→0.6 compounding is moot in ranking when the timestamp stays 0). Informational relative to this plan's scope, but it undercuts the "reinforcement compounds" goal.
Fix: set `last_accessed_at` (and ideally `created_at`) to the session's `now` on both new and merged writes.

**L6 — status sequence Ready → Downloading → Ready on the deferred first-run path (informational).**
`src-tauri/src/main.rs:229-231` emits `embedder://status` = Ready (hash) from the setup hook, then `spawn_startup_download` (main.rs:239-248) immediately flips to Downloading → progress → Ready. Deliberate (the hash embedder IS ready), but the banner briefly shows "ready" before flipping to a progress bar; on download failure it ends at Failed. Consider emitting `Checking` instead of `Ready` for the interim hash on the deferred path, or accept as-is.

**L7 — doc nit.** `src/memory/consolidation.rs:55-56`: "Returns an empty string when either dir is missing or empty" — actually only when BOTH yield nothing (one missing dir still contributes the other). Fix the comment to "when both dirs are missing/empty".

## Intended-but-notable behavior (plan focus point 3)

- **Absent config key now silently triggers a ~90 MB background download + full re-embed on next startup** for every existing user with no `bundled_embedding_model` key. This is the documented intent of step 4 (out-of-box semantic recall), but it is a privacy/bandwidth behavior change worth a changelog note: the user never chose the model, and the download + re-embed happen without any UI prompt. Confirmed no path downloads for the explicit `"hash"` sentinel, and `NeedsProject` startup never spawns it.
- The `"hash"` sentinel is honored consistently in build_embedder, embedder_startup_plan, settings save (clear → "hash"), main.rs reembed guard (lowercase), frontend radio mapping — except the M1/M2/L3/L4 case-sensitivity and rewire gaps above.

## Areas verified clean

- **Merge correctness**: `store.write` is `INSERT OR REPLACE` keyed by id; `embedding.clear()` provably forces re-embed (memory/mod.rs:629-638); cloned `prior` carries access_count/created_at/last_accessed_at; `source_session_ids` append is deduped (consolidation.rs:395, 500); strength math `(x + 0.1).min(1.0)` is safe; the stale `existing` list is acceptable (only the LLM-call window, same-id double-bump compounds harmlessly).
- **corpus_digest**: char-boundary caps via `chars()`; `[...truncated...]` marker; mtime-newest sort; `.md` extension filter; missing-dir → empty; per-file + total caps; test coverage (missing dirs, non-md, section headers) is good. Both extraction prompts carry the corpus; synthesis deliberately doesn't (test asserts indices 1,2).
- **Background download**: `spawn_download` preserves the old observable behavior (Downloading mark + emit, 500ms progress poller with `clamp(0.0, 0.99)`, Ready/Failed + emit, abort on build end); `install_embedder` fingerprint logic matches rewire.rs/main.rs (re-embeds on mixed/empty/hash fingerprints, skips on exact match, always `set_embedder`, degrades gracefully on errors); no lock-ordering hazard (status lock released before install; writer thread never takes the `writer` mutex).
- **validate_request_messages**: no legitimate shape rejected — assistant-with-tool-calls-and-empty-text passes (tested), tool ids are matched order-independently (known-ids set built across all messages), system/user shapes untouched; guard sits before `build_request_json` + trace start, so no request is sent (test proves it via unroutable base_url); all 6 new tests are meaningful.
- **trace.rs**: writer gating is correct (Dirty only when toggle on at drain; DirtyForced regardless; disable path flushes while still enabled, so a Dirty queued while on always lands); dedupe between Dirty/DirtyForced in one batch; redaction holds on the forced path (test asserts `[REDACTED]` + no secret leak); updated toggle test now uses a non-error update and still proves what it claims; forced test covers "success under disabled logging writes nothing".
- **Diagnostics**: spawn_consolidation one-line eprintlns with session id + error (agent.rs:461-477); `write_review_report` added to is_durable_tool with a rationale comment (turn.rs:1378-1380).
- **Constitution**: no `#[allow(...)]` anywhere in the diff; no dead code observed (all new fns are used; `EMBEDDING_MODEL_SENTINEL_HASH` exported + used in 3 crates); every new public item has a doc comment; Windows-safe (PathBuf/tempfile, no Linux-only paths in new code; `#[cfg(unix)]` permission asserts are pre-existing and Windows-guarded); all `consolidate_session` callers updated (agent.rs, tool/memory/mod.rs, 8 test sites); scope matches plan steps 1–8.
- **Config tri-state**: serde `default = "default_bundled_embedding_model"` + manual Default both → Some(MiniLM); `"hash"` round-trips through parse/serialize (tested); settings clear persists "hash" not None (settings.rs:1129-1134); the frontend null-radio maps save → clear flag → "hash" and load → null selection with a stable snapshot (no infinite dirty loop).
