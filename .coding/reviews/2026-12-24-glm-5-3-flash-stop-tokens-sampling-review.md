## Verdict: FINDINGS (2 high, 3 low)

Review of all uncommitted changes for plan 5ce79c00 (GLM-5.3-Flash stop tokens, sampling parameters, stream guard) across 11 files (+706/−51 lines). The config-layer design, resolution helpers, TOML→JSON converters, request payload injection, and stream guard logic are well-structured and correct. However, the Tauri app crate does not compile (missing `..Default::default()` in `settings.rs`), and the Settings save path silently drops all new fields (DTO layer not updated).

---

### HIGH-1: Build broken — `into_endpoint` in `settings.rs` constructs `ModelSpec` with 5 of 10 fields (compile error)

`src-tauri/src/ipc/settings.rs:121-127` constructs `mnemo::config::ModelSpec` with only the original 5 fields (`id`, `max_context`, `max_output_tokens`, `reasoning_efforts`, `multimodal`) and **no `..Default::default()`**:

```rust
.map(|d| mnemo::config::ModelSpec {
    id: d.id.clone(),
    max_context: d.max_context,
    max_output_tokens: d.max_output_tokens,
    reasoning_efforts: d.reasoning_efforts.clone(),
    multimodal: d.multimodal,
})   // ← missing: temperature, top_p, stop, stop_token_ids, extra_body
```

`ModelSpec` now has 10 fields (5 new ones added in `endpoints.rs`). This is a compile error: `missing fields: temperature, top_p, stop, stop_token_ids, extra_body`.

**Why `cargo test` catches this:** `Cargo.toml:144` declares `[workspace] members = ["src-tauri"]`, so `cargo test` (without `-p`) compiles the `mnemo-app` crate. The agent updated `console.rs` and `config_io.rs` (both in `src-tauri/`) to add `..Default::default()` to their `Endpoint` literals, but missed `settings.rs`. `into_endpoint` is NOT behind `#[cfg(test)]` — it's production IPC code.

**Fix:** Add `..Default::default()` after `multimodal: d.multimodal,` (line 126).

---

### HIGH-2: New sampling fields silently dropped on Settings save (data loss)

Even after fixing HIGH-1, the new fields cannot survive a Settings → Endpoints save:

1. **DTO layer not updated.** `EndpointDto`, `ModelSpecDto`, `EndpointWire`, and `ModelSpecWire` (all in `settings.rs`, NOT in the diff) carry only the original fields. The frontend never receives or sends `temperature`/`top_p`/`stop`/`stop_token_ids`/`extra_body`.
2. **`into_endpoint` drops ModelSpec-level fields.** After the HIGH-1 fix, `..Default::default()` sets the 5 new ModelSpec fields to `None`/empty — the DTO has no values to carry.
3. **`validate_endpoint` drops Endpoint-level fields.** `patch.rs:138-142` hardcodes `temperature: None, top_p: None, stop: Vec::new(), stop_token_ids: Vec::new(), extra_body: None`. The function signature doesn't accept these fields as parameters, so it can't propagate them.
4. **`apply_endpoints` replaces, not merges.** `patch.rs:234-241` constructs `Config { endpoints: built, ... }` — the `built` endpoints (from `validate_endpoint`) replace the existing ones entirely. `persist_and_reload` then writes the stripped config to `endpoints.toml`.

**Impact:** A user hand-edits `endpoints.toml` to add `temperature = 0.6` (the intended configuration path per the spec — the UI doesn't expose these fields). The app loads it correctly and the provider uses it. But the next time the user saves the Endpoints tab (even without touching that endpoint), all new fields are silently erased from disk. The GLM-5.3 stability fix reverts to defaults with no error or warning.

**Fix (either):**
- **(A) Full round-trip:** Add the 5 fields to `ModelSpecDto`/`ModelSpecWire`/`EndpointDto`/`EndpointWire` + `from_spec`/`into_spec` mappings, and add them as parameters to `validate_endpoint` (propagate instead of hardcoding `None`). This is the correct long-term fix.
- **(B) Preserve-on-save (smaller scope):** In `save_endpoints`/`apply_endpoints`, merge the new fields from the existing in-memory `Endpoint`/`ModelSpec` into the `built` endpoints before writing — the frontend doesn't own these fields, so they pass through untouched.

At minimum, document this as a known limitation if the DTO work is deferred to a follow-up.

---

### LOW-1: Stream guard cannot detect boundary tokens split across SSE chunks

`find_boundary_cutoff` (openai.rs:1081) checks each SSE event's content delta independently. If a boundary token like `<|endoftext|>` is split across two SSE `data:` events (e.g., delta 1 ends with `<|en`, delta 2 starts with `doftext|>`), neither chunk matches and the guard misses the boundary.

**Risk is low** in practice: GLM special tokens are single tokenizer units typically emitted in one delta, and the request `stop` field (server-side cutoff) is the primary mechanism — the stream guard is defense-in-depth. But the gap is real. An optional improvement: accumulate a small overlap buffer (length of the longest boundary) across deltas and check the concatenation.

---

### LOW-2: `extra_body` can silently overwrite critical request fields

The `extra_body` merge (openai.rs, end of `build_request_json`) runs **after** all other fields are set:

```rust
if let Some(extra) = &self.config.extra_body {
    if let Some(obj) = body.as_object_mut() {
        for (k, v) in extra {
            obj.insert(k.clone(), v.clone());
        }
    }
}
```

A user who puts `"stop"` or `"model"` in `extra_body` silently overwrites the carefully-constructed GLM stop sequences or model id. This matches the OpenAI Python client's `extra_body` semantics (explicitly "arbitrary"), so it's not a bug per se — but a one-line doc comment on the `extra_body` field noting "merged last; can override any request field including `model`/`messages`/`stop`" would prevent surprises.

---

### LOW-3: PLAN.md item 8 is stale

`PLAN.md` item 8 (the GLM-5.3 stop-boundaries entry) describes the stop list as "the three role-tag strings + `"\n\n\n"`" and `stop_token_ids: [151329, 151330, 151336]`. The implementation now has:
- **Four** role-tag strings (the `<|observation|>` tag was added).
- **Two** newline cascades: `"\n\n\n\n"` (4-newline, new) and `"\n\n\n"` (3-newline, pre-existing).
- New configurable `Endpoint`/`ModelSpec` fields (`temperature`, `top_p`, `stop`, `stop_token_ids`, `extra_body`) not mentioned anywhere in PLAN.md.

PLAN.md should be updated to reflect the expanded stop set and the new configurable fields. The spec document (`.coding/knowledge/spec/2026-12-21-...`) is already current.

---

### Multi-platform neutrality: PASS

All changes are in Rust library/app code using standard cross-platform APIs (`str::find`, `serde_json`, `tokio::net::TcpListener` for the test stub). No Windows-specific paths, APIs, or shell syntax. No `cfg(windows)` additions outside the sanctioned WebView2 gate. The `toml::Table`/`toml::Value` types are platform-neutral.

---

### Positive observations

- **Resolution helpers** (`temperature_for`, `top_p_for`, `stop_for`, `stop_token_ids_for`, `extra_body_for`) correctly implement model-override-takes-precedence-over-endpoint semantics, with sensible empty-collection fallbacks.
- **`extra_body_for` merge** correctly gives model-level keys precedence over endpoint-level keys.
- **`toml_value_to_json`** correctly handles `NaN`/`Infinity` floats by falling back to `Null` (JSON doesn't support them).
- **`find_boundary_cutoff`** is byte-index safe — `str::find` returns char-boundary positions, so `text[..idx]` never panics. The empty-prefix guard (`if !prefix.is_empty()`) prevents emitting empty `TextDelta` events.
- **Stream guard finish logic** correctly mirrors the trace log (`log.finish(*id, "stop")`) and emits `FinishReason::Stop` before returning, preventing the fallback finish from double-firing.
- **GLM stop injection** correctly merges configured stops with the GLM defaults (deduplication via `contains`), and the `else` branch correctly emits non-GLM configured stops only when non-empty.
- **`Eq` removal from `ModelSpec`** is safe — no code in the codebase requires `ModelSpec: Eq` (verified: no `HashSet<ModelSpec>`, `HashMap<ModelSpec, _>`, or `ModelSpec: Eq` bounds exist).
- **Tests are comprehensive**: TOML parsing + resolution, client factory propagation, `build_request_json` injection (GLM + custom), `find_boundary_cutoff` edge cases, and a full stream-guard integration test with a `StubServer`.
- All new public functions and struct fields have doc comments, satisfying the project's documentation requirement.
