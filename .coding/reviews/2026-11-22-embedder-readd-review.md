# Review: Re-add real embedder (cloud + local) with failure protection

**Scope:** All uncommitted changes (`git diff HEAD`) — 18 files, +767/-45.
Backend Rust embedder/probe/config/IPC + frontend Settings tab, status banner,
and StatusBar badge.

**Overall:** The core design is sound — infallible `embed`, zero-vector
fallback, circuit breaker, OpenAI-compatible `/v1/embeddings` with auth, and a
startup probe. The infallible-trait migration is complete (all call sites
updated, no stale `Result`-returning embeds, no leftover no-arg
`build_embedder()`). The contract fixture is correctly updated. However there
is one significant correctness gap (hardcoded dimension vs. the UI's own model
suggestions) and several lower-severity items.

---

## Correctness

### C1 (medium-high) — Hardcoded `EMBEDDING_DIM = 768` silently breaks every non-768 model, including the ones the Settings UI recommends

`src/memory/embedder.rs:202` — `try_embed_once` rejects any response whose
embedding length ≠ `EMBEDDING_DIM` (768), returning `None` → zero vector:

```rust
if vec.len() == EMBEDDING_DIM { Some(vec) } else { None }
```

`EMBEDDING_DIM` is fixed at 768 (`nomic-embed-text`'s dimension). But the
Settings UI (`EmbeddingSection.tsx:167-169`) explicitly recommends:

- `text-embedding-3-small` — **1536-dim** (fails every embed)
- `text-embedding-3-large` — **3072-dim** (fails every embed)
- `bge-m3` — **1024-dim** (fails every embed)

So 3 of the 4 suggested models silently degrade to keyword-only recall — every
embed returns a zero vector, contributing nothing to cosine. The user follows
the UI's own recommendation, gets a green "Ready" badge (see C2), and believes
semantic recall is active when it is not. This directly defeats the feature's
purpose for the cloud path the plan emphasizes ("or any OpenAI-compatible
endpoint").

**Fix (pick one):** make the dimension dynamic (latch the first successful
embed's length and compare against that, so any single model works as long as
it's consistent), OR have the probe validate the response dimension and report
`Failed` on mismatch, OR at minimum remove the non-768 suggestions from the UI
and document the 768 constraint.

### C2 (medium) — Embedder status goes stale after startup; `RemoteEmbedder.status`/`set_status` are dead in production

The circuit breaker (`record_failure`/`record_success`, `embedder.rs:157-172`)
never calls `set_status`, and the startup probe does not call it on the
embedder either — `probe_and_autostart` returns an `EmbedderStatus` that
`main.rs:226-229` writes to the **IPC-level** `embedder_status` lock, not the
`RemoteEmbedder`'s own `status: RwLock<EmbedderStatus>` field. As a result:

- `RemoteEmbedder::status()` always returns `Checking` (the constructor
  default) in production — it is exercised only by unit tests
  (`embedder.rs:417-431`).
- If the service goes down *after* startup (circuit opens), the UI badge stays
  green "Ready" indefinitely. The status is probed exactly once at startup and
  never refreshed.

The `set_status` doc comment (`embedder.rs:146`) claims it is "used by the
startup probe + circuit-breaker transitions" but neither path actually calls
it. Not a build warning (public API), but misleading dead code + a real UX gap
(the whole point of the status badge is to surface degradation).

**Fix:** either wire `set_status` into `record_failure`/`record_success` and
have the IPC `get_embedder_status` read from the embedder (not a one-shot
probe), or drop the embedder-internal `status` field entirely and document
that status is startup-only.

### C3 (low) — Zero-vector memories are never re-embedded after service recovery

`src/memory/mod.rs:604` — `write()` guards on `memory.embedding.is_empty()`. A
failed remote embed returns `vec![0.0; 768]` (len 768, not empty), so it is
stored verbatim and never re-embedded — even after the service comes back,
those memories permanently carry a zero vector and match only via keyword. This
is the documented fallback ("contributes nothing to cosine"), so it is
acceptable degradation, not a crash. Noting it so it is a conscious choice.

---

## Bugs

### B1 (low) — Frontend subscribes to `embedder://status` *after* the initial fetch, risking a missed emit

`frontend/src/App.tsx:256-269` and `StatusBar.tsx:58-70` both do:

```ts
const status = await getEmbedderStatus();   // fetch first
setEmbedderStatus(status);
// ...then subscribe
onEmbedderStatus((s) => setEmbedderStatus(s)).then((fn) => { unlisten = fn; });
```

If the probe finishes and emits between the fetch-read and the
subscribe-complete, the emit is missed and the UI stays on the stale fetched
value (e.g. "checking" forever). The window is narrow (the probe's 2s sleeps
make it unlikely for local endpoints), but a fast cloud endpoint that returns
200 immediately could complete before the frontend even mounts, and a
subsequent quick transition could land in the gap. **Fix:** subscribe first,
then fetch (the fetch fills the current value; the listener catches future
emits).

### B2 (low) — `embedding_model` save validation does not check non-empty endpoint/model (inconsistent with `vision_model`)

`src-tauri/src/ipc/settings.rs:1020-1028` — the embedding validation only
checks that the endpoint *exists* in `endpoints.toml`; it does not reject
empty/whitespace endpoint or model strings. The `vision_model` path
(`settings.rs:973-976`) explicitly checks `vm.endpoint.trim().is_empty()` and
`vm.model.trim().is_empty()`. An empty `embedding_model: {endpoint:"", model:""}`
passes when no endpoints are configured (`has_endpoints` false skips the
check), then `build_embedder` falls back to hash (harmless), but the
inconsistency is a latent footgun. Mirror the vision_model non-empty checks.

### B3 (low, Windows) — `ollama serve` / `ollama pull` spawn may pop a console window on Windows

`src/memory/embedder_probe.rs:76-81` and `:96-102` spawn `ollama` with
`Stdio::null()` on all three streams but no `CREATE_NO_WINDOW` /
`DETACHED_PROCESS` creation flag. On Windows, a GUI app (Tauri) spawning a
console subprocess can allocate a visible console window. Best-effort and
non-fatal, but a flashing window on startup is poor UX. Consider
`.creation_flags(0x08000000)` (`CREATE_NO_WINDOW`) on Windows (gated behind
`#[cfg(windows)]`).

---

## Security

### S1 — No findings (clean)

- **API key resolution** (`client_factory.rs:24-31`, mirrored in
  `embedder_probe.rs:25-32`) follows the existing trusted chain: stored key →
  `OPENAI_API_KEY` → `ANTHROPIC_AUTH_TOKEN` → `"dummy"`. Sent as
  `Authorization: Bearer <key>`. Local Ollama ignores the dummy; cloud gets
  the real key. Correct.
- **No injection vector:** the model name is sent as a JSON string value
  (`{"model": self.model, "input": text}`), not interpolated into a URL or
  shell. The endpoint URL is `format!("{}/embeddings", base_url)` from trusted
  config. The `ollama pull <model>` spawn passes the model as a separate
  `Command` arg (no shell), so no command injection. The model name originates
  from user config (trusted), not untrusted remote input.
- The key-resolution duplication across two modules is a deliberate
  decoupling (documented in the probe's doc comment), not a security issue.

---

## Constitution compliance

### CC1 — No findings (clean)

- **Warning-free build under `#![deny(warnings)]`:** the removed
  `use crate::error::Result;` in `embedder.rs` is correctly dropped (no unused
  import). New imports (`AtomicI64`, `AtomicU32`, `Ordering`, `RwLock`,
  `Duration`, `SystemTime`, `UNIX_EPOCH`, `serde::{Deserialize, Serialize}`,
  `tauri::Emitter`) are all used. No `#[allow(...)]` suppressions were added.
  The `RemoteEmbedder::status`/`set_status` public methods are not dead-code
  warnings (public API), though they are functionally dead (see C2).
- **Doc comments:** all new public functions/structs/enums have doc comments
  (`RemoteEmbedder::new`, `status`, `set_status`, `EmbedderStatus`,
  `EmbeddingModel`, `EmbeddingModelWire`, `EmbeddingModelDto`,
  `build_embedder`, `probe_and_autostart`, `get_embedder_status`,
  `health_check`, `resolve_api_key`). `EmbeddingModelDto` fields lack
  per-field docs but mirror the existing `VisionModelDto` style — consistent.
- **Windows paths / PowerShell:** no shell commands in the diff; process
  spawns use cross-platform `std::process`/`tokio::process`. (See B3 for a
  Windows-specific spawn UX note.)
- **No commit to main:** this is a review of uncommitted changes; no commit
  occurs here.
- **Contract fixture:** `dto-get-settings.json` correctly adds
  `"embedding_model": null`, and `contract_fixtures.rs:229` builds
  `GetSettingsGeneral` with `embedding_model: None`. `serde_json::Value`
  comparison is order-independent (BTreeMap), so the field-order difference
  between the fixture file and struct declaration does not matter — the keys
  and values match. The two new `save_settings` DTO tests
  (`save_settings_sets_embedding_model`,
  `save_settings_clears_embedding_model`) correctly pin the set + clear paths.
- **Call-site migration complete:** all 3 `build_embedder` call sites pass
  config (`main.rs:608`, `rewire.rs:31`, `client_factory.rs:238/275`). All
  `.embed()` call sites use the infallible form (no `?`, no `.unwrap()`). No
  other `Embedder` implementors were missed (only `HashEmbedder` +
  `RemoteEmbedder`).

---

## Summary of action items (by priority)

1. **C1 (medium-high):** Fix the 768-dim hardcode so the recommended cloud
   models actually work, or stop recommending them. This is the only finding
   that silently breaks the feature for the documented cloud use case.
2. **C2 (medium):** Wire status updates into the circuit breaker (or drop the
   dead `status` field) so the badge reflects post-startup degradation.
3. **B1 (low):** Subscribe to `embedder://status` before the initial fetch in
   `App.tsx` + `StatusBar.tsx`.
4. **B2 (low):** Add non-empty endpoint/model validation to the
   `embedding_model` save path (mirror `vision_model`).
5. **B3 (low, Windows):** Add `CREATE_NO_WINDOW` to the `ollama` spawns.
6. **C3 (low):** Acknowledge that zero-vector memories are never re-embedded
   (already documented; no code change required unless re-embedding on
   recovery is desired).
