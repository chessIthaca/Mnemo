# Review: Bundled in-process embedder (fastembed) — feat/memory-learning-agent

Reviewed ALL uncommitted changes (`git diff HEAD`) across 18 files. The plan
set out 7 goals: add `BundledEmbedder` (fastembed/ONNX), a `bundled_embedding_model`
config field, model-fingerprint schema columns, rewire `build_embedder` to prefer
the bundled model, download-progress IPC, startup + rewire re-embed checks, and a
frontend model picker.

---

## CORRECTNESS

### C1 (critical) — The `Failed` status from `build_embedder` is overwritten by the legacy startup probe, defeating plan goal #4

**Files:** `src-tauri/src/main.rs:231-239`, `src/memory/embedder_probe.rs:47-51`

Plan goal #4 explicitly requires: "On load failure, falls back to HashEmbedder +
sets status Failed (does NOT overwrite to Ready)." `build_embedder`
(`src/provider/client_factory.rs:187-188`) correctly sets `EmbedderStatus::Failed`
when a bundled model fails to load. **But this status can never reach the UI** —
it is immediately clobbered by the startup probe.

The setup closure (`main.rs:217-240`) unconditionally spawns
`embedder_probe::probe_and_autostart(&config)`. That probe
(`embedder_probe.rs:47-51`) checks the **legacy** `config.general.general.embedding_model`
field (the old remote/Ollama path), which is now always `None` (the bundled path
uses `bundled_embedding_model` instead). So the probe hits the early return:

```rust
let em = match &config.general.general.embedding_model {
    Some(em) => em,
    None => return EmbedderStatus::Ready,   // ← always taken now
};
```

…returns `Ready`, and `main.rs:236-238` writes `Ready` over the `Failed` status
and emits it to the frontend. The user configures a bundled model that fails to
load (not yet downloaded, corrupt, unknown id) and the badge shows green ◆
"Ready" instead of red ⚠ "Failed" — the exact failure the plan said must surface.

**Fix:** The probe is legacy dead-weight for the new architecture. Either remove
the probe spawn entirely (the bundled path has no "probe" — `build_embedder`
already determined the status), or gate it on `embedding_model.is_some()`. At
minimum, do not overwrite a `Failed` status: `if status != Failed { *status_lock.write() = probe_status; }`.

### C2 (critical) — Startup re-embed task uses the CONFIG model_id (not the embedder's), causing an infinite re-embed loop + status overwrite when the bundled model fails to load

**File:** `src-tauri/src/main.rs:645-677`

When a bundled model is configured but fails to load, `build_embedder` returns a
`HashEmbedder` fallback (model_id `"hash"`, dim 768). The re-embed check then
runs (because `bundled_embedding_model` is still `Some`):

```rust
let expected_model = model_id.clone();              // CONFIG value: "all-MiniLM-L6-v2"
let expected_dim = embedder_for_reembed.dim();      // HashEmbedder: 768
```

It looks for a stored fingerprint `("all-MiniLM-L6-v2", 768)` — which can **never**
match (real MiniLM vectors are 384-dim; hash vectors are stored as `("hash", 768)`).
So `needs_reembed` is always true. `reembed_all` then re-embeds every row with the
HashEmbedder, storing fingerprint `("hash", 768)`. On the **next** startup the same
mismatch is detected (expected `"all-MiniLM-L6-v2"`, stored `"hash"`) → re-embeds
again. **Every startup, forever**, when the configured bundled model fails to load.

Additionally, the task sets status `Checking` (line 668-669) then `Ready`
(line 674-675), overwriting the `Failed` status — a second path that defeats
plan goal #4.

**Contrast with `rewire.rs:50`:** the rewire path correctly uses
`embedder_for_reembed.model_id().to_string()` (the actual embedder's id), so it
does not loop — but it still overwrites `Failed`→`Ready` (see C3).

**Fix:** (a) Use `embedder_for_reembed.model_id()` instead of the config value,
matching rewire.rs. (b) Skip the entire re-embed task when the embedder is the
hash fallback (status is `Failed`) — there is nothing useful to re-embed with a
fallback embedder, and doing so corrupts the fingerprint semantics.

### C3 (medium) — Rewire re-embed task overwrites `Failed`→`Checking`→`Ready`

**File:** `src-tauri/src/ipc/rewire.rs:55-67`

Same status-overwrite pattern as C2 but in the config-save rewire path. When a
config save changes the model and the new model fails to load, `build_embedder`
sets `Failed`, but the re-embed task unconditionally sets `Checking` then `Ready`
(lines 61, 67), clobbering `Failed`. It uses the correct model_id (so no infinite
loop), but the user still won't see the failure.

**Fix:** Guard the status writes: only set `Ready` if `reembed_all` succeeded AND
the embedder is not the hash fallback. Or skip the re-embed task when the new
embedder's `model_id() == "hash"` but a bundled model is configured (the fallback
case).

### C4 (low) — `reembed_all` is O(N) sequential; a large store re-embeds slowly

**File:** `src/memory/mod.rs:984-1000`

Each row: `embed().await` (CPU-bound, on the async runtime) then a `spawn_blocking`
UPDATE — one at a time, no batching or concurrency. For thousands of memories
this is slow (though fire-and-forget, so non-blocking). Not a correctness bug;
note for future. The embed call itself correctly runs ONNX on `spawn_blocking`
(`embedder.rs:207`) — that part is right.

---

## BUGS

### B1 (medium) — `downloadBundledModel` resolves immediately but `handleDownload` treats it as awaiting completion

**Files:** `src-tauri/src/ipc/embeddings.rs:104-195`, `frontend/src/components/settings/sections/EmbeddingSection.tsx:96-111`

The Rust `download_bundled_model` command spawns the download on the blocking
pool and returns `Ok(())` **immediately** (line 194) — it is fire-and-forget by
design. But the frontend `handleDownload` `await`s it as if it blocks until
completion:

```ts
await downloadBundledModel(modelId);          // resolves instantly
const catalog = await listBundledEmbeddingModels();  // premature — model not installed yet
setModels(catalog);
} finally {
  setDownloading(null);  // clears the progress bar while download still runs
  setProgress(0);
}
```

Result: (1) the catalog refresh runs before the download completes (model still
shows "not installed"); (2) the progress bar is cleared by the `finally` block,
then re-set moments later when the first `embedder://status` `Downloading` event
arrives via the listener — a visible flicker. The `tauri.ts` doc comment
("then `ready` on completion") implies the call blocks, which is misleading.

**Fix:** Either make `download_bundled_model` not return until the build
completes (await the spawned task), or remove the `finally` clear + premature
catalog refresh and let the `onEmbedderStatus` listener drive the progress bar
until a `Ready`/`Failed` event arrives (then refresh the catalog).

### B2 (low) — `is_model_installed` can return true before the download completes, stopping the progress poller early

**File:** `src-tauri/src/ipc/embeddings.rs:27-44, 153`

`is_model_installed` returns true if **any** file exists in the model's
`snapshots/<rev>/` dir (`files.count() > 0`, line 37). During a download,
`hf-hub`/`fastembed` writes files incrementally — a partial snapshot can satisfy
this check before the model is fully downloaded + loadable. The poller
(line 153) then `break`s, stops emitting progress, and the progress bar freezes
at its last value until the build task completes and emits `Ready`/`Failed`.

Not catastrophic (the build still completes correctly), but the progress bar can
stall mid-download. A more robust check would verify the expected ONNX model file
exists (e.g. `model.onnx` or `model_quantized.onnx`) rather than "any file."

### B3 (low) — `RemoteEmbedder` + `embedder_probe` were intended to be dropped (plan goal #4) but remain

**Files:** `src/memory/embedder.rs:299-471`, `src/memory/embedder_probe.rs`, `src/memory/mod.rs:10,22`

Plan goal #4: "drops the old RemoteEmbedder/Ollama path." But `RemoteEmbedder`
is still fully defined (`embedder.rs:299-471`), still re-exported
(`mod.rs:22: pub use embedder::{..., RemoteEmbedder}`), and still has live tests.
It is no longer constructed by `build_embedder` — so it is dead-in-production
(survives `deny(warnings)` only because it's `pub` + test-used). `embedder_probe.rs`
is still `pub mod` + still called from `main.rs:234`, but it checks the legacy
`embedding_model` field and is the direct cause of C1. The plan intended both to
go; their continued presence is the root of the status-clobbering bug.

**Fix:** Remove `embedder_probe.rs` + its call site (C1 fix) and either remove
`RemoteEmbedder` or leave a note that it's retained intentionally. As-is, it's
confusing dead weight that contradicts the plan.

---

## SECURITY

**No findings.** Confirmed: the model cache dir is `global_config_dir().join("models")`
in all three sites (`main.rs:624`, `embeddings.rs:21-23`, `rewire.rs` via
`myharness::config::global_config_dir().join("models")`) — consistent and NOT
under `.coding/` (which is committed to git). Model binaries are per-machine and
never committed. Downloads go to public HuggingFace repos via `hf-hub` with no
API key or secret. No credentials are logged or emitted in events.

---

## CONSTITUTION COMPLIANCE

### CC1 (low) — `to_wire` has an unused `_selected` parameter (dead code)

**File:** `src-tauri/src/ipc/embeddings.rs:230`

`fn to_wire(m, cache_dir, _selected: &Option<String>)` — the `_selected` param is
never read (underscore-prefixed so no warning, but it's dead). `list_bundled_embedding_models`
fetches `selected` from config (line 74) only to pass it here unused. Either use
it (e.g. flag the selected model in the wire form) or drop the param + the fetch.

### CC2 (low) — `get_bundled_model_status` command appears unused in the frontend

**Files:** `src-tauri/src/main.rs:392` (registered), `frontend/src/lib/tauri.ts:425-428`

The `get_bundled_model_status` command + its TS wrapper `getBundledModelStatus`
are defined and registered but never called from any frontend component
(`EmbeddingSection` uses `listBundledEmbeddingModels` + `downloadBundledModel`
only). Dead command/wrapper. Either wire it in or remove it.

### CC3 — No `#[allow(...)]` suppressions — compliant

Searched all changed files: zero `#[allow(` attributes added. The only
`#[allow(...)]` in the tree is the pre-existing `#[allow(clippy::too_many_arguments)]`
in `src/agent/loop_impl.rs` (untouched). ✓

### CC4 — Doc comments on all new public functions — compliant

Every new public item has a doc comment: `Embedder::model_id()`,
`BundledEmbedder::new`/`available_models`, `BundledModelInfo` (all fields),
`hf_cache_subdir`, `MemoryStoreTrait::stored_model_fingerprints`/`reembed_all`,
`build_embedder`, the three IPC commands, and the private helpers
(`embedder_cache_dir`, `is_model_installed`, `cache_dir_progress`, `dir_size`,
`to_wire`). ✓

### CC5 — Windows paths + PowerShell — compliant

All paths in comments/code use Windows conventions; `embedder_probe.rs` correctly
uses `#[cfg(windows)]` + `creation_flags(0x0800_0000)` for CREATE_NO_WINDOW on
the (legacy) ollama spawns. ✓

### CC6 — `EmbedderStatus` `Eq`→`PartialEq` drop — no breakage

Dropping `Eq` (because `Downloading` carries `f64`) is safe: no code uses
`EmbedderStatus` as a `HashMap`/`BTreeMap` key or in any `Eq`-bound context.
`assert_eq!` requires only `PartialEq` + `Debug` (both still derived). The
frontend handles the object form at every comparison site (App.tsx:476-504,
StatusBar.tsx:694-728, EmbeddingSection.tsx:74-84) — all check
`typeof === "object" && "downloading" in` before accessing. ✓

### CC7 — Schema migration idempotency — compliant

`migrate_add_column_if_missing` (`schema.rs:171-196`) guards each ALTER with a
`PRAGMA table_info` check — no-op on fresh DBs (columns already present) and on
already-migrated DBs. Test `migration_adds_fingerprint_columns_to_legacy_db`
verifies the legacy-DB path. ✓

### CC8 — `reembed_all` holds no DB lock across `.await` — compliant

`reembed_all` (`mod.rs:963-1002`) loads all rows on `spawn_blocking` (lock
released), then per-row: `embed().await` (no lock held) + `spawn_blocking` UPDATE
(lock acquired fresh each iteration). No `MutexGuard` held across `.await`. ✓

---

## SUMMARY

The most actionable findings are **C1 + C2 + C3** (the `Failed` status cannot
survive to the UI — three separate code paths overwrite it, directly defeating
plan goal #4) and **B3** (the legacy `embedder_probe`/`RemoteEmbedder` that the
plan intended to drop are the root cause). Fix C1 by removing/gating the probe,
fix C2/C3 by using the embedder's actual `model_id()` and skipping the re-embed
when the embedder is the hash fallback. B1 (premature catalog refresh) is a
straightforward frontend fix. Everything else is low-severity.
