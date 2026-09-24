## Verdict: FINDINGS (0 high, 4 low)

Summary: the Laya foundation ships the hard requirement correctly in CODE — the disabled/absent gate builds no client and no call site exists, both fallback arms and the rewire path stay Disabled/None, the wire protocol matches the pinned laya-serve shape, and the Rust↔TS contract is aligned end to end. All four findings are LOW: three doc-accuracy clusters (a save-time event that is promised but never emitted; NoClassifier docs describing a design that isn't the one shipped; an unconsumed snapshot field mis-documented) and one config-serialization convention deviation (an always-written `[general.laya]` section). No functional/behavioral bug in any acceptance-critical path.

### Reviewed scope
- All 21 tracked modified files + the 3 untracked new files (`src/memory/classifier.rs`, `frontend/src/components/settings/sections/ClassifierSection.tsx`, `.test.ts`) — full `git diff HEAD` plus direct reads of every touched region.
- Cross-checked against the established Embedder pattern (`src/memory/embedder.rs` trait/status/serde conventions, `build_embedder`, the embedder status plumbing in state/main/startup/rewire/settings).
- Verified via the code graph that `classify` has ZERO production call sites — only the trait method, the `NoClassifier`/`LayaClassifier` impls, and tests. Correct for the foundation item (items 2-5 are the consumers).

### Verified — the hard requirement (disabled ⇒ today's behavior), in the CODE
1. **The gate** — `build_classifier` (src/provider/client_factory.rs:350-390): `laya.enabled == false` or an absent/blank endpoint ⇒ returns `None` + status `Disabled`, and the `reqwest` client is **never constructed** (the client build sits behind the endpoint match). Enabled-without-endpoint logs a warning and stays `Disabled`; an unbuildable client degrades to `None` + `Failed`. The gate's regression pin (`build_classifier_disabled_returns_none_and_leaves_status_disabled`, :1088-1099) starts the status from `Ready` and asserts the `None` + `Disabled` overwrite — a gate regression fails it.
2. **All three AgentRuntimeContext arms** (src-tauri/src/main.rs): Ready arm (:521-522) shares `brain.classifier_status` and wraps `brain.classifier` (`None` when the gate produced none); NeedsProject arm (:670-673) and Err arm (:761-764) are both `Disabled` + `None` — the two fallback arms carry no classifier, exactly as today.
3. **The rewire path** (src-tauri/src/ipc/rewire.rs:79-101): rebuilds through the same `build_classifier` on the saved config and swaps the slot under the write lock; disabling ⇒ `None` + `Disabled` with no client; reads back through `IpcState::classifier()` for the log line. Enable/disable/endpoint change take effect without a restart, and the shared status Arc is written by `build_classifier` itself.
4. **No startup cost when off**: one `Arc<RwLock<…>>` + `None` per construction site; `LayaClassifier::new` does no I/O (client builder only — "no startup probe" is documented and true).

### Verified — failure-protected None contract
- `LayaClassifier::classify` (src/memory/classifier.rs:306-333): every transport/protocol failure maps to `None` (`ok()?` / `?` on the async block). The only `.expect`s are on the status `RwLock` — the repo's established idiom (byte-equivalent in spirit to `build_embedder`'s status writes); judged as sanctioned.
- `parse_answer` (:265-297): hostile bodies cannot panic — `get`/`as_str?`/`as_f64?` return `None`; a missing `confidence` reads as `0.0` (never trusted by confidence-gating callers, pinned); a mismatched primitive ⇒ `None` (pinned); `probabilities` non-numerics are filtered, missing ⇒ empty map.
- 10 s bound via `reqwest::Client::builder().timeout(LAYA_REQUEST_TIMEOUT)` covers the whole request incl. body read; no retries (reqwest default). A wedged endpoint ⇒ `None` + `Failed` (pinned by the hanging-server test). Status transitions: `Ready`⇄`Failed` per call; `Disabled` only via (re)build; poisoned locks map to `IpcError` in the IPC accessors (state.rs:344-369).

### Verified — wire protocol vs the pinned laya-serve shape
- `systemone_request` (:248-253): `POST {endpoint}/v1/systemone` body `{"state": …, "questions": {"question": {typed question}}}` — matches the pinned request exactly. `Question` serializes to `{type, instructions, criteria}` (tag `"type"`, lowercase rename) per primitive — pinned both by `questions_serialize_to_the_laya_wire_shape` and by the recorded-request assertion in the stub round-trip.
- `parse_answer` matches the pinned `answers` shape per primitive (`choice`/`confidence`/`probabilities`; `score`/`confidence`; `noul`); `usage` and `routing` are ignored. The one-place centralization is real: request + parse live together in this file. Trailing-slash endpoint normalization is exercised by the round-trip test.

### Verified — Rust/TS contract alignment
- `LayaWire` (settings.rs:471-479, always a nested object, never null) ↔ `AppSettings.general.laya` (tauri.ts:445-447) ↔ `dto-get-settings.json` (`laya: {enabled: false, endpoint: null}`) ↔ `contract_fixtures.rs` fixture construction — all four sides updated consistently, plus the settings.rs wire test asserting `v["general"]["laya"]`.
- Save patch: `laya_enabled`/`laya_endpoint` (settings_dto.rs:259-264; apply :554-564) — absent fields keep current values (pinned), a blank string clears the endpoint (pinned incl. whitespace-only), values trimmed. `ClassifierSection` always sends both fields and re-reads the live status after save (:120-126).
- Snapshot: `StartupSnapshot.classifier_status` (startup.rs:43-45, populated at :153) ↔ `StartupSnapshot.classifier_status: string` (tauri.ts:261) — lowercase serialization pinned on the Rust side; TS treats it as a plain string like embedder status.
- `get_classifier_status` registered in the handler list (main.rs:831); startup emission (main.rs:571-584) mirrors the embedder block — the status Arc is cloned out of `app.state::<IpcState>()` and read, no lock held across an await, `app.manage` already done (same pattern as embedder, no double-lock hazard).

### Verified — security & multi-platform neutrality
- Endpoint is user-configured only (Settings/config.toml); no auth/bearer is sent (matches the documented "no auth yet" — `LAYA_API_KEY` deliberately not part of this surface); warnings log only the endpoint URL (user-configured, not a secret); no secrets in any log line.
- No Windows-only APIs/paths anywhere in the Laya surface; `reqwest` + `tokio::net::TcpListener` tests bind `127.0.0.1:0` (portable); the emit/accessor code mirrors the already cross-platform embedder pattern. macOS-clean.

### Verified — constitution/style/docs
- Doc comments present on every new public item (trait, types, methods, command, accessors, `LayaWire`, JSDoc bindings); no `#[allow]` anywhere in the diff; no shell-based file mutation introduced (the knowledge-file amendment in the diff went through the sanctioned amend path).
- docs/CONFIGURATION.md's claims match `build_classifier`'s actual behavior (blank endpoint ⇒ warning + Disabled; the status triple; no-auth note). README highlights row + config note, docs/FEATURES.md bullet, and PLAN.md shipped-features entry all accurately describe the opt-in + failure-protected contract.

---

## Findings

### L1 (LOW) — `classifier://status` save-time emission is promised in docs but never emitted
- **Where:** frontend/src/lib/tauri.ts:660-663 (`onClassifierStatus` doc: "emitted at startup and after a settings save rewires the backend"); the same claim in ClassifierSection.tsx:90-92; partially in src-tauri/src/ipc/settings.rs:38 ("updated via the `classifier://status` event").
- **Reality:** the ONLY emit is at startup (src-tauri/src/main.rs:583). `save_settings` → `rewire_vision_embedder_and_classifier` writes the shared status lock + logs (rewire.rs:84-100) — no emit anywhere on the save path (grep-verified: zero `emit` calls in settings.rs/rewire.rs).
- **Impact:** no functional bug today — the ClassifierSection is the only listener and it re-polls `getClassifierStatus()` right after save (ClassifierSection.tsx:126), so the displayed status is correct. But the documented contract is false, and items 2-5 (or any future UI listening for save-driven status changes) would subscribe to an event that never fires post-save.
- **Fix (either):** emit `classifier://status` from the save path (rewire has no `AppHandle` — plumb one through `save_settings`/`resync_runtime_state`), or correct the doc comments to "emitted at startup; a settings save updates the shared status — re-poll `get_classifier_status` (the section does this after a save)".

### L2 (LOW) — the `NoClassifier` docs describe a design that is not the one shipped
- **Where:** src/memory/classifier.rs:10-13 (module doc: "the app builds [`NoClassifier`], which never answers"); :152-155 (`NoClassifier` doc: "Built when Laya is disabled (the default) or unconfigured"); :120 (`Disabled` doc: "`NoClassifier` serves this state"); :761-763 (test comment: "build_classifier returns the no-op classifier").
- **Reality:** the app NEVER constructs `NoClassifier` — `build_classifier` returns `Option`, disabled ⇒ `None`, and the `IpcState` slot holds `None` (state.rs:157-159 carries the accurate doc). The code graph confirms `NoClassifier` is constructed only by the two tests (:357, :765).
- **Impact:** the shipped design (no object at all — even stricter) is correct and safe; but an items-2-5 implementer reading the module docs would look for a live no-op backend and a status "served" by it. Doc-vs-code mismatch across four sites.
- **Fix:** reword the four doc sites to say the classifier slot is `None` while disabled (no backend exists at all); keep `NoClassifier` as the exported no-op (usable as a test double / explicit default), which is fine.

### L3 (LOW) — `[general.laya]` is always serialized into config.toml on every save, even for users who never touched Laya
- **Where:** src/config/general.rs:189-199 (`LayaConfig`) + :116 (the `GeneralSection::laya` field) — a non-Option struct without `skip_serializing_if`, so `GeneralConfig::save` (:837) writes `[general.laya]` with `enabled = false` into config.toml on the first settings save for EVERY user.
- **Contrast with the repo's own convention:** untouched sub-tables are omitted — `[ui.steering_notes]` skips the whole table via `skip_serializing_if = "SteeringNotesCfg::is_empty"` (general.rs:522), and `vision_model`/`embedding_model` are Options that skip when `None`. `laya` is the first always-serialized struct sub-table of `[general]`.
- **Impact:** behaviorally harmless — the written section parses back to exactly the default-disabled state (`laya_round_trips` proves the round-trip), so the opt-in invariant holds. This is config-file noise for hand-edited configs and a small blemish on the "untouched users see no trace" spirit, not a bug.
- **Fix:** `#[serde(default, skip_serializing_if = "LayaConfig::is_default")]` on the field plus a small `is_default()` fn (mirroring `SteeringNotesCfg::is_empty`), or document the always-written section in docs/CONFIGURATION.md if it is intentional.

### L4 (LOW) — the startup snapshot's `classifier_status` is unconsumed and its doc overstates its role
- **Where:** frontend/src/lib/tauri.ts:253-261 (`StartupSnapshot.classifier_status` doc: "Rendered by the Settings → Classifier section").
- **Reality:** nothing consumes `snap.classifier_status` — the section reads the live status via `getClassifierStatus()` on mount/save instead. Contrast: `snap.embedder_status` IS consumed (frontend/src/App.tsx:262 `setEmbedderStatus(snap.embedder_status)`).
- **Impact:** the field + population are required by the plan and correct on the backend; this is a doc nit today, but the "rendered by the section" claim steers a future reader to the wrong data flow.
- **Fix:** either seed a status store from the snapshot in App.tsx (mirroring the embedder line) or reword the doc ("seed value; the Settings section reads the live status via `get_classifier_status`").

---

## Test-quality assessment (requested)
- **Disabled pin:** `build_classifier_disabled_returns_none_and_leaves_status_disabled` genuinely fails if the gate regresses (starts from `Ready`, asserts `None` + `Disabled` overwrite). `disabled_path_never_calls_http` pins the no-op's silence (its comment mislabels `build_classifier` — see L2).
- **Stub round-trip:** a real `tokio` TCP listener + a real `reqwest` round-trip that asserts method (`POST`), path (`/v1/systemone`), and the full request body — a serializer or parser regression fails it; trailing-slash normalization is exercised; timeout (hanging server), non-200, malformed-body, and mismatched-primitive paths each pinned with the `Failed` status transition asserted.
- **Contract pins:** `laya_defaults_to_disabled` / `laya_round_trips` (absent section ⇒ disabled; TOML round-trip), `settings_save_dto` absent-field pins + `settings_save_dto_parses_laya`, `laya_patch_applies_enabled_and_endpoint` (trim, blank-clears, enable preserved), fixture tests both sides, and the frontend source-contract suite (command/event/patch/snapshot names, nav placement after Embeddings, forwardRef + dirty/save contract, serializeClassifier semantics).
- **What they would catch:** the acceptance criteria (disabled ⇒ no client/None/calls; enabled ⇒ end-to-end round trip) are both pinned by tests that exercise the real changed paths.

No high findings — the acceptance-critical paths (disabled invariant, failure-protected None, rewire without restart, wire protocol, Rust↔TS contract) all verify in the code.