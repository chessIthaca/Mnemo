## Verdict: PASS

# Round-2 re-review — job-4 review-fix commit (wt/mnemo, delta 1bb38b6..3373096)

All three round-1 findings are fixed correctly and completely at 3373096; both regressions genuinely exercise the fixed paths and are red pre-fix; no new defects in the delta. One non-blocking observation and two process remarks are noted at the end.

## What I read (empty-diff rule)

The working tree is 3373096 plus the uncommitted round-2 plan stamp and the untracked knowledge BUG file (both bookkeeping). The delta is exactly one commit (13 files, 349+/62−); I read its stat, its archived full diff (head/tail), and then verified every hunk in the landed tree: `src/host/brain.rs:51-80` (the field + doc), `src-tauri/src/console.rs:1210-1239` (the twin), `src-tauri/src/ipc/laya.rs:50-104 + 122-149` (the sink wrappers + `laya_setup`), `ipc/embeddings.rs:1-25`, `ipc/mcp.rs:1-25`, `ipc/settings.rs:1090-1107 + 1215-1279 + 1596-1640` (wire, save path, unit test), `src/headless/setup.rs:500-799` (`for_engine` wiring, `patch_settings`, `classifier_setup`, `live_laya_enabled`), `src/headless/error.rs:115-149` (the `unsupported_patch` ctor), `tests/integration/headless.rs:1100-1176` (the F2 regression), `tests/integration/headless_setup.rs:385-419 + 455-549 + 810-841` (the D4 patch fixture, both converted classifier fixtures, the F3 regression), `src-tauri/src/ipc/rewire.rs:95-124` (the GUI leg's lock order), the plan file, the step file §F 4.9 (:700-766), and the round-1 report in full. Targeted sweeps: every `classifier_setup` / `patch_settings` call site across src, src-tauri and tests; `brain.memory_store` remnants in src-tauri (zero); the `SettingsSaveWire` shape; `unsupported_patch`; `classifier_slot` holders; `Option<&Arc`.

## F1 (high) — fixed, complete

- `brain.rs:61` — `pub own_memory_store: Option<Arc<MemoryStore>>`; the doc comment names the app consumers (the IpcState wiring + the two main.rs startup hooks + the console twin) and the round-1 provenance — accurate against the landed consumers.
- `console.rs:1226` — the twin now reads `(&brain.pending_model_load, &brain.own_memory_store)`, mirroring main.rs's hooks and handing `spawn_startup_load_console` the concrete store its signature takes (the old read was the `Arc<dyn Trait>` slot — type mismatch).
- The four further app-crate breaks, all fixed coherently: `embeddings.rs:21` restores `State` in the `tauri::{AppHandle, Emitter, State}` import; `mcp.rs:19-20` is down to `serde::Serialize` + `tauri::State` (the unused `Deserialize`/`McpServerDef` are gone — under `#![deny(warnings)]` a green workspace battery proves both survivors are used); `settings.rs:1221` + `:1265-1266` carry the G9 `.engine.` nesting (`patch.engine.safety`, `patch.engine.trace_*`); the stale UI-field unit test now parses `SettingsSaveWire` and asserts both flattened halves (engine :1603-1614, ui :1608-1613).
- `ipc/laya.rs` ×2: `start_managed_server` / `start_server_models` keep the pre-D5 `&Option<AppHandle>` signature and build the sink via `status_sink(&app)` before calling the lib core (:56-77); the `spawn_setup_task` wrapper does the same (:81-90), matching `laya_setup`'s call site (:141-147).
- Completeness: zero remaining `brain.memory_store` reads in the app crate; the wire payload shape is untouched (`#[serde(flatten)] engine: SettingsSaveDto` + `#[serde(flatten)] ui: SettingsUiPatch`, :1094-1102) — the split stays type-level only.
- Red check: the app-crate compile itself — red at 1bb38b6 was independently established in round 1; green at 3373096 is inspection-verified (see Battery). The root cause (root-only `cargo test` never builds src-tauri) is durably fixed: the battery is now `cargo test --workspace`, recorded in the step file §F 4.9, the knowledge BUG file, and the agent.md constitution.

## F2 (high) — fixed, complete, no deadlock

- `classifier_setup` is now `async` — this realigns the code with the master spec's designed signature (`headless-free-agent.md:554`, `pub async fn classifier_setup`); the D5 plan line had omitted the async. Callers: only fixtures — the two existing classifier fixtures converted (`#[tokio::test]`, headless_setup.rs:465/:504, `.await` at the call sites); **no production caller missed** (the GUI's Download path is `laya_setup` → lib `spawn_setup_task` directly, never `Setup::classifier_setup`; src-tauri references to either method are comments only).
- Both arms persist → LIVE-APPLY (`*live.config.lock().await = config.clone()`, :666) **before** the guard (:693): the flip is in the Arc even on guard reject, and a failed save can never wedge the guard (the guard is claimed only after persist + apply succeed).
- Lock ordering: the config tokio-Mutex is locked in exactly one statement per path and released at statement end — never held across another lock; the disable leg takes the slot std-RwLock then the status RwLock (:680 → :683), the SAME order the app's GUI leg uses (rewire.rs:104-110 and :117-123, verified) — no cycle, no nesting, no deadlock. `patch_settings` locks the config last (:603) after all other writes; `live_laya_enabled` takes the single lock alone (:733).
- Disable leg: stop → CLEAR `live.classifier_slot` (:679-682, the GUI leg's Disabled ⇒ no client — no calls to a dead loopback port) → park Disabled → terminal Done. It never touches the guard, so a mid-setup disable always wins the race — the completing pipeline re-reads the Arc it just flipped.
- Standalone enable = install-only (:705-711): the spawn receives a fresh disabled-view Arc (slot `None`); disk keeps the flag; the docs (:626-629, :701-704) state exactly this, and the next engine start honors the disk flag via the pre-existing attach path — the spawn's disabled view contradicts nothing the docs promise.
- Regression `classifier_setup_flip_reaches_the_live_config_before_the_guard` (headless.rs:1129-1176): guard pre-claimed so NO network pipeline runs; provider-injected engine bypasses the not_configured gate; asserts Arc == Some(false) before, the `setup_in_flight` reject, then **Arc == Some(true) after the reject — the exact assertion that is red pre-fix** (pre-fix only disk flipped, the Arc stayed stale) — then the disk persist, then the disable leg flips the Arc back to Some(false) and yields the terminal Done. It genuinely exercises the fixed path on both arms, network-free.
- Observation (not a finding): the disable leg's slot-clear Some→None transition executes in the fixture (the branch runs over a None slot) but isn't observably asserted — `LiveHandles` is private, so pinning that transition would require new pub API purely for test observability. The code mirrors the GUI leg's order and is correct by inspection.

## F3 (low) — fixed, complete, no app-side break

- `patch_settings` rejects `laya_enabled` at :547-552 with the new ctor `EngineError::unsupported_patch` (error.rs:132-137 — stable code `unsupported_patch`, message names `Setup::classifier_setup`) — checked BEFORE load/validate/persist, so "rejected whole, nothing applied" holds with no partial state. The shared validator is untouched.
- No existing save path breaks: no src-tauri code calls `Setup::patch_settings` (mentions are comments only); the GUI's save owns the full classifier leg through its own `save_settings` + rewire; the one pre-existing headless patch fixture sends only `summarize_at_fill_rate` (:404-408) and is unaffected.
- Both doc comments rewritten to the one-owner rule (:536-545, :615-645) and now match landed behavior.
- Regression `patch_settings_rejects_the_classifier_flag_with_a_stable_code` (headless_setup.rs:828-841): **red pre-fix** (the patch was accepted and half-applied, so `expect_err` fails); post-fix asserts the stable code, the message pointing at `classifier_setup`, and that nothing was applied (disk still reads disabled).

## Constitution checks

- **Documentation sync** ✓ — every changed surface carries a doc comment matching landed behavior (the brain field, patch/classifier/accessor docs, the ctor); README/PLAN.md need nothing (internal architecture — round-1's ruling stands; the battery correction is documented in the step file, the knowledge BUG file, and the agent.md constitution).
- **Multi-platform neutrality** ✓ — no `cfg(windows)` additions in the delta; all paths config_dir-derived; fixtures tempdir-based.
- **File-tools-first** ✓ — no shell mutation in the delta.
- **Warning-free** ✓ — no `#[allow(...)]` added anywhere.
- **Bookkeeping (accuracy)** ✓ — step file §F 4.9 matches what shipped, including the honest four-more-breaks list and the root-cause note; the plan's Reviews section carries the round-2 stamp; the untracked knowledge BUG file (root-only cargo test) matches the shipped battery correction.

## Battery

Read-only review — cargo was not re-executed. The claimed `cargo test --workspace` exit 0 is consistent with everything verified: each F1 hunk is the exact mechanical fix the compile errors demanded, both async conversions are coherent end to end (`#[tokio::test]` fixtures, `.await` at every call site, no missed production caller), and the `--features embeddings` and tauri-tree claims are unaffected by the delta (no tauri import in any touched lib file).

## Process remarks (not findings)

1. Uncommitted carry-over: the plan-file round-2 stamp and the untracked knowledge BUG file — expected mid-review bookkeeping; they ship with the closing commit.
2. Cosmetic only: a few jammed comment starts in the delta (`}/// Fire-and-forget` at ipc/laya.rs:78; `}// -- D5:` / `}// -- D6:` at headless_setup.rs:460/:532) — rustfmt-would-fix whitespace with no format gate and no functional impact.

Reviewed-state: 3373096d500340be50503f2f5be4f255d7bade1c
