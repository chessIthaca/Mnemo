# Review: Bundled embedding model (fastembed) — residual uncommitted changes

Reviewed ALL uncommitted changes (`git diff HEAD`) across 5 files, plus the
broader feature concerns the task listed, by reading the surrounding code
(`src/memory/mod.rs`, `src/memory/embedder.rs`, `src/memory/schema.rs`,
`src/config/general.rs`, `src/provider/client_factory.rs`, `src-tauri/src/ipc/error.rs`,
`src-tauri/src/ipc/state.rs`, `src-tauri/src/main.rs`, `src-tauri/src/ipc/rewire.rs`,
`src-tauri/src/ipc/embeddings.rs`, + the frontend model picker).

## Scope note

The uncommitted diff is small — residual cleanup after the previous session
crashed mid-closing-sequence. The bulk of the feature was already committed and
reviewed (`.coding/reviews/2026-11-23-bundled-embedder-review.md`). This review
covers (a) the uncommitted diff itself, and (b) re-verification that the prior
review's critical findings stayed fixed in the current code.

### The uncommitted diff itself — CLEAN

- `src-tauri/src/ipc/embeddings.rs`: removed unused `use std::sync::Arc;` (the
  status handle is obtained via `state.runtime.embedder_status.clone()`, which
  auto-derefs the `Arc` — no explicit `Arc` name needed). Changed
  `IpcError::Other(...)` → `IpcError::msg(...)`. Verified `IpcError` has no `Other`
  variant/method — `msg` (`src-tauri/src/ipc/error.rs:35-40`) is the correct
  free-form constructor. ✓
- `src-tauri/src/ipc/rewire.rs`: added `use myharness::memory::MemoryStoreTrait;`
  — required because `stored_model_fingerprints` + `reembed_all` are trait
  methods called on the `store: Arc<MemoryStore>`. ✓
- `src-tauri/src/main.rs`: added `MemoryStoreTrait` to the import list (same
  reason). Removed the dead `let _status_for_reembed = embedder_status.clone();`
  line — confirmed gone; the spawned task does not use the status handle. ✓
- `.coding/plans/*`: bookkeeping only (step 8 checkbox + stack pointer). ✓

The uncommitted source changes are correct and warning-safe.

### Prior critical findings — CONFIRMED RESOLVED

- **C1 (probe overwrites `Failed`)**: `embedder_probe` is gone from
  `src/memory/mod.rs` (search returned no matches); `build_brain` no longer
  spawns a probe. `build_embedder` (`client_factory.rs:187-188`) sets `Failed`
  on load failure and it is no longer clobbered. ✓
- **C2 (startup re-embed used config model_id → infinite loop)**: `main.rs:647`
  uses `embedder_for_reembed.model_id()` (the embedder's actual id), and the
  whole task is gated on `status_snapshot == Ready` (`main.rs:641`) — a
  hash/`Failed` fallback never triggers re-embed. ✓
- **C3 (rewire overwrites `Failed`→`Ready`)**: `rewire.rs:49` gates on
  `status_snapshot == Ready`; the re-embed task writes no status
  (`rewire.rs:73-76` only logs). ✓
- **B1 (premature catalog refresh)**: `handleDownload`
  (`EmbeddingSection.tsx:102-119`) no longer clears the bar in a `finally`;
  the `onEmbedderStatus` listener (`:72-94`) owns the download lifecycle. ✓
- **B2 (`is_model_installed` false-positive on partial files)**: now checks for
  the actual ONNX model file (`embeddings.rs:38-42`: `model.onnx` /
  `model_quantized.onnx` at top level or under `onnx/`), not "any file." ✓
- **CC2 (dead `get_bundled_model_status` command)**: removed; `main.rs:383-384`
  registers only `list_bundled_embedding_models` + `download_bundled_model`. ✓

---

## CORRECTNESS

### CR1 (low) — Re-embed check tests for PRESENCE of the expected fingerprint, not ABSENCE of others; an interrupted re-embed leaves stale rows undetected

**Files:** `src-tauri/src/main.rs:657-660`, `src-tauri/src/ipc/rewire.rs:62-65`

Both the startup and rewire checks compute:

```rust
let needs_reembed = fps.is_empty()
    || !fps.iter().any(|(m, d)| m == &expected_model && *d == expected_dim);
```

This is true when the expected `(model_id, dim)` is **absent** from the stored
set. But `reembed_all` updates rows one at a time (`src/memory/mod.rs:983-998`,
per-row `spawn_blocking` UPDATE). If a prior `reembed_all` was interrupted
mid-way (process crash, machine power-off), the DB can contain **mixed**
fingerprints — some rows already updated to the new model, others still on the
old. On the next startup the check finds the expected fingerprint **present**
(the partially-updated rows) → `needs_reembed = false` → skips re-embed → the
stale rows from the old model remain in a different vector space, silently
degrading recall quality.

The simple cross-machine git case (all rows from model A, machine configured
for model B) IS correctly detected — the expected fingerprint is absent. This
finding is specifically the interrupted-reembed edge case.

**Impact:** degraded recall (some rows in a mismatched vector space), not data
corruption or a crash. Requires a crash mid-`reembed_all`, which is rare.

**Fix (optional):** re-embed when the stored set is not *exactly* the expected
fingerprint, i.e. `fps.len() != 1 || !(fps[0].0 == expected_model && fps[0].1 == expected_dim)`.
This re-embeds whenever more than one distinct fingerprint is present (mixed =
interrupted), at the cost of a redundant re-embed when the DB legitimately has
only the expected model.

---

## CONSTITUTION COMPLIANCE

### CC1 (low-medium) — `let _config = state.project.config.lock().await;` is dead code silencing an unused-variable warning via `_` prefix

**File:** `src-tauri/src/ipc/embeddings.rs:68`

```rust
pub async fn list_bundled_embedding_models(
    state: State<'_, IpcState>,
) -> Result<Vec<BundledModelWire>, IpcError> {
    let cache_dir = embedder_cache_dir();
    let _config = state.project.config.lock().await;   // ← acquired, never read
    Ok(BundledEmbedder::available_models()
        .into_iter()
        .map(|m| to_wire(m, &cache_dir))
        .collect())
}
```

The config lock is acquired but `_config` (the `MutexGuard`) is never read — the
function builds its result solely from the static catalog
(`BundledEmbedder::available_models()`) + the filesystem cache dir. This is a
leftover from the prior review's CC1 fix (which removed the unused `_selected`
param + the `selected` fetch from config that the lock was protecting). With
`selected` no longer read, the lock serves no purpose. The `_` prefix silences
the `unused variable` warning that would otherwise fail the build under
`#![deny(warnings)]` — exactly the pattern the constitution forbids ("fix the
root cause — remove the dead code").

**Fix:** delete line 68 entirely. The function needs no config lock.

### CC2 (low) — `if let Some(_model_id) = ...` binds an unused value; should be `.is_some()`

**File:** `src-tauri/src/main.rs:636`

```rust
if let Some(_model_id) = &config.general.general.bundled_embedding_model {
    ...
    let expected_model = embedder_for_reembed.model_id().to_string();  // uses the embedder, not _model_id
```

`_model_id` is bound but never read (the actual model id comes from
`embedder_for_reembed.model_id()` at line 647, deliberately — see the comment at
644-646). The `_` prefix silences the unused-binding warning. The intent is
clearly "is a bundled model configured?", which is better expressed as
`if config.general.general.bundled_embedding_model.is_some() {`.

**Fix:** `if config.general.general.bundled_embedding_model.is_some() {`.

### CC3 (note, low) — `RemoteEmbedder` remains as dead-in-production code

**Files:** `src/memory/embedder.rs:299-471`, `src/memory/mod.rs:22`

`RemoteEmbedder` is still fully defined + re-exported, but is no longer
constructed anywhere (`build_embedder` uses only `BundledEmbedder` or
`HashEmbedder`). It survives `#![deny(warnings)]` only because it is `pub` +
exercised by its own `#[cfg(test)]` tests. The plan (step 1: "DROP OLLAMA:
remove the RemoteEmbedder path") intended it removed. This is the prior review's
B3, still open. Not introduced by this diff; flagging for awareness. Either
remove it or leave a doc comment noting it is retained intentionally (e.g. for a
future remote-embedder revival).

### CC4 — No `#[allow(...)]` suppressions — compliant ✓

Searched all changed files: zero `#[allow(` attributes. The only
`#[allow(...)]` in the tree is the pre-existing
`#[allow(clippy::too_many_arguments)]` in `src/agent/loop_impl.rs` (untouched,
a clippy lint not a rustc warning). The two `_`-prefix findings above (CC1, CC2)
are the constitution-relevant items.

### CC5 — Doc comments on all public functions — compliant ✓

Every public item in the changed files has a doc comment:
`embedder_cache_dir`, `is_model_installed`, `list_bundled_embedding_models`,
`download_bundled_model`, `cache_dir_progress`, `dir_size`, `to_wire`,
`BundledModelWire`, `rewire_vision_and_embedder`, `sync_model_resolver`. ✓

### CC6 — `IpcError::msg` change correct ✓

`IpcError` (`src-tauri/src/ipc/error.rs`) has no `Other` variant or method —
`msg` (lines 35-40) is the correct free-form constructor (`kind = "error"`).
The diff's `IpcError::Other(...)` → `IpcError::msg(...)` is the right fix. ✓

---

## SECURITY — no findings ✓

- **Model id validated against the catalog before use:** `download_bundled_model`
  (`embeddings.rs:87-90`) finds the `model` in
  `BundledEmbedder::available_models()` before use; `hf_cache_subdir`
  (`embedder.rs:269-279`) also matches against a fixed set (returns `None` for
  unknown). The cache path is `cache_dir.join(<fixed-subdir>)` — no
  user-controlled path segments, no path traversal. ✓
- **Cache dir under the global config dir, not `.coding/`:** all three sites
  consistent — `embeddings.rs:19-21` (`global_config_dir().join("models")`),
  `main.rs:613` (`config_dir.join("models")`, where `config_dir` is the global
  config dir per the comment at 611-612), `rewire.rs:34`
  (`global_config_dir().join("models")`). Model binaries are per-machine and
  never committed to git. ✓
- Downloads go to public HuggingFace repos via `hf-hub` with no API key or
  secret; no credentials logged or emitted in events. ✓

---

## CONCURRENCY — no findings ✓

- **Download progress poller stops correctly:** breaks when
  `is_model_installed` (`embeddings.rs:130-132`) AND is aborted after the build
  completes (`:148`). Two independent stop mechanisms. ✓
- **Build runs on `spawn_blocking`:** `embeddings.rs:142-145`
  (`tokio::task::spawn_blocking(move || BundledEmbedder::new(...))`). ONNX init +
  HF download are blocking — correctly off the async runtime. ✓
- **No status-lock deadlock:** the poller writes status (`:127`) and the build
  writes status on completion (`:153/159/165`), but these are sequential
  write-lock acquisitions (acquire, release, acquire) — no nested locking, no
  guard held across `.await`. The build's final status write happens *after*
  `poll_handle.abort()` (`:148`), so the poller is stopped first. At worst a
  transient stale `Downloading` event is immediately overwritten by the final
  `Ready`/`Failed`. ✓
- **`reembed_all` holds no DB lock across `.await`:** loads all rows on
  `spawn_blocking` (lock released), then per-row `embed().await` (no lock) +
  `spawn_blocking` UPDATE (lock acquired fresh each iteration)
  (`src/memory/mod.rs:962-1001`). ✓
- **Fire-and-forget:** both re-embed spawns (`main.rs:649`, `rewire.rs:54`) use
  `tauri::async_runtime::spawn` and are never awaited — never blocks startup or
  the UI. ✓

---

## IDEMPOTENCY — no findings ✓

- **Schema migration idempotent:** `migrate_add_column_if_missing`
  (`src/memory/schema.rs:171-196`) guards each `ALTER TABLE` with a
  `PRAGMA table_info` check — no-op on fresh DBs (columns present) and on
  already-migrated DBs. Tests `schema_is_idempotent` +
  `migration_adds_fingerprint_columns_to_legacy_db` verify both paths. ✓
- **Config field round-trips:** `bundled_embedding_model: Option<String>`
  (`src/config/general.rs:73`) round-trips through TOML — test
  `bundled_embedding_model_round_trips` (`general.rs:413-433`) verifies parse +
  re-serialize + re-parse. Defaults to `None` (opt-in). ✓

---

## SUMMARY

The uncommitted diff is clean and correct; the prior review's critical
findings (C1/C2/C3 status-clobbering, B1/B2 download UX, CC2 dead command) are
all confirmed resolved in the current code. Security, concurrency, and
idempotency are clean.

Remaining findings are all **low severity and pre-existing** (not introduced by
this diff, but in changed files):

| ID | Severity | File:line | Issue |
|----|----------|-----------|-------|
| CC1 | low-medium | `embeddings.rs:68` | `let _config = ...lock().await` is dead code (lock never read); `_` silences the warning. Remove the line. |
| CC2 | low | `main.rs:636` | `if let Some(_model_id)` binds unused value; use `.is_some()`. |
| CR1 | low | `main.rs:657`, `rewire.rs:62` | Re-embed check tests presence of expected fingerprint, not absence of others; interrupted re-embed leaves stale rows. |
| CC3 | note | `embedder.rs:299`, `mod.rs:22` | `RemoteEmbedder` is dead-in-production (plan intended it dropped). |

None are blocking. CC1 is the most actionable (genuine dead code in a changed
file, a one-line deletion). The rest are optional polish.
