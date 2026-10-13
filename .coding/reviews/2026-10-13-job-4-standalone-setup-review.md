## Verdict: FINDINGS (2 high, 1 low)

# Review — Job 4: standalone setup + config (wt/mnemo, delta 90b8ad5..1bb38b6)

The lib-side surfaces are solid — the Setup API, G4 isolation, G8 injection, G9 split, the D3/D8 cores, and the 14 new fixtures all check out — but the D7 Brain field split left the app crate uncompilable as committed (`pub(crate)` field read cross-crate + the console twin still on the old field), and `Setup::classifier_setup` never applies its enable flip to the live config Arc, so an engine-backed enable installs but never lights the sidecar and a mid-setup disable loses the documented race.

## What I read (empty-diff rule)

The working tree is clean except `.coding/plans/e4fada21.md` (bookkeeping) — the committed delta 90b8ad5..1bb38b6 IS the review surface. I walked all 8 commits (1146f02 → 1bb38b6) with `git show --stat` and their messages; read the plan `e4fada21.md`, the step file `job-04-standalone-setup.md` in full (713 lines incl. §F landing notes 4.1–4.8), and the master-spec anchors §5.8 :486–589, §5.9.8 :725–731, §5.9.9 :733–738, §7 row 33. Code read in depth: `src/headless/setup.rs` (all 1560 lines), `src/host/{laya,rewire,brain,embed_download}.rs`, `src/headless/{engine,error,mod}.rs`, `src/config/{mcp,keys,settings_dto,general}.rs`, `src/safety_rules.rs`, `src/project/scaffold.rs` + `src/app/scaffold.rs`, `src-tauri/src/{main.rs,console.rs}`, `src-tauri/src/ipc/{settings,laya,embeddings,mcp,rewire,state}.rs`, `tests/integration/headless_setup.rs` (14 fixtures), plus targeted verification passes (literal searches for `global_config_dir(`, `own_memory_store`, `\.memory_store\b`, `laya_enabled`) and the crate layout (`Cargo.toml`, `src-tauri/Cargo.toml`).

Reviewed-state: 1bb38b6820d47aa5b0e14fdfa9af6baeb455c978
## Finding 1 (HIGH) — the D7 Brain field split left mnemo-app uncompilable as committed

Two independent compile errors in the app crate at 1bb38b6, both introduced by commit 698ddff (D7) and untouched by D8:

**(a) Cross-crate access to a `pub(crate)` field.** `src-tauri/src/main.rs:32` re-exports the lib's `Brain` (`mnemo::host::brain::Brain`), and the D7 rewire points the app at the new concrete slot:

- `main.rs:627` — `memory_store: brain.own_memory_store.clone(),` (the IpcState wiring)
- `main.rs:710` / `main.rs:726` — `if let Some(store) = &brain.own_memory_store {` (the two startup hooks)

But `src/host/brain.rs:58` declares it `pub(crate) own_memory_store: Option<Arc<MemoryStore>>` — and mnemo-app is a separate crate (`src-tauri/Cargo.toml`, package `mnemo-app`, depends on `mnemo` by path). A `pub(crate)` field is invisible outside the defining crate; these three reads are field-is-private compile errors. The §F 4.7 note itself lists "main.rs's IpcState wiring + the two startup hooks" among `own_memory_store`'s consumers — the visibility contradicts the landed consumer list (the doc comment at brain.rs:57, "pub(crate) for the same reasons the old single field was", is wrong too: the pre-split `memory_store` field was `pub`, which is exactly why the app could read it).

**(b) The console twin was missed by the rewire.** `src-tauri/src/console.rs:1226` — untouched since job 3c (git log) — still reads:

```rust
if let (Some(model), Some(store)) = (&brain.pending_model_load, &brain.memory_store) {
    crate::ipc::embeddings::spawn_startup_load_console(store.clone(), ...)
```

`brain.memory_store` is now `pub memory_store: Option<Arc<dyn crate::memory::MemoryStoreTrait>>` (brain.rs:51) after the D7 split, but `spawn_startup_load_console(store: Arc<MemoryStore>, ...)` (embeddings.rs:163-169) still takes the concrete store — an `Arc<dyn Trait>`-to-`Arc<Concrete>` type-mismatch compile error. It is also the wrong slot per the D7 design: the console's startup embedder load is own-store machinery, exactly like main.rs's two startup hooks that WERE rewired to `own_memory_store`.

**Impact.** mnemo-app (the shipped Tauri app) does not compile at 1bb38b6. Corollary: every "Battery: workspace green" claim since 698ddff — and the task's "full workspace `cargo test` exit 0" — cannot have actually compiled src-tauri: plain `cargo test` at the root package builds only `mnemo` (root package semantics), never the app. The four batteries since D7 (D7's own, D8's, and the final verification) all missed a hard app-crate break.

**Fix.** Make `own_memory_store` `pub` (with the doc comment corrected — the app is a documented consumer) and rewire `console.rs:1226` to `brain.own_memory_store`, mirroring main.rs's two hooks. The red/green check is the compile itself: `cargo build -p mnemo-app` (or a true `cargo test --workspace`) is red today and green after. Going forward the closing battery must include an app-crate compile — root-only `cargo test` structurally cannot catch app-side regressions.
## Finding 2 (HIGH) — `Setup::classifier_setup`'s flip never reaches the live config Arc: engine-backed enable installs but never lights; a mid-setup disable loses the documented race

`classifier_setup` (setup.rs:622-677) persists `[general.laya].enabled` to **disk** (:629-633), then for an engine-backed Setup passes `Arc::clone(&live.config)` — the brain's shared config — to `spawn_setup_task` **without ever applying the flipped `config` to that Arc** (:658-661). `spawn_setup_task` re-reads exactly that Arc at pipeline completion for the autostart decision + the classifier-slot swap (host/laya.rs:833-836 — "never from a snapshot taken at download start").

**Consequence A — engine-backed enable installs a runtime nothing lights.** For an engine started with laya disabled (the typical fresh-install state, since this method is the enable surface), the completing pipeline reads `enabled=false` from the stale Arc → no autostart, no `live.classifier_slot` swap, status restored to the pre-setup snapshot (Disabled). The kernel brain deliberately leaves the classifier slot empty at build (brain.rs:539-542 — "the host fills the slot"), so the for_engine'd `classifier_setup(true)` is the only headless surface that can ever wire a live classifier — and through it the engine NEVER gets one from the documented one-call flow. The consumer sees progress events and a terminal Done; the disk says enabled; the sidecar is dark until a future engine start happens to pick the flag up. The method doc (:605-612) promises "persist enabled = true … and autostart the sidecar when the LIVE config still selects it at completion" — but this method never puts the flip INTO the live config, so via this surface alone it never lights. There is a working engine-backed flow — `patch_settings(laya_enabled=true)` first (its :590 Arc swap reaches the config), then `classifier_setup(true)` — but nothing tells the consumer: the doc for the patch says the flip does *not* ride it (see Finding 3).

**Consequence B — a mid-setup disable loses the race it was built for.** `classifier_setup(false)` (:635-644) persists `enabled=false` to disk, stops the sidecar, parks `Disabled` — but never touches the Arc. A completing enable pipeline therefore reads the stale `enabled=true` and **autostarts anyway**, contradicting the spawn's own contract ("a mid-setup disable save must always win"). The only disable that wins is one flowing through `patch_settings(laya_enabled=false)` — the surface whose doc disclaims the field. The disable arm also leaves the dead classifier handle in `live.classifier_slot` while the GUI leg sets it to `None` ("Disabled ⇒ None + status Disabled", ipc/rewire.rs:59-60, :104-110) — post-disable tool calls hit a dead loopback port instead of a clean "no client".

Sub-note: on a fresh (non-engine) Setup the same arm spawns a sidecar nothing consumes (a fresh slot Arc no factory reads) — install-only would be the coherent behavior there; the engine-backed arm is the one that matters.

The GUI twin has none of these problems because its flag flip rides `save_settings` (persist_and_reload swaps the live config + the rewire classifier leg swaps the slot/starts the sidecar) and the Download button only installs — the flip and the sidecar control share one surface there.

**Fix direction.** In the engine-backed arm, apply the flipped config into `live.config` (lock+swap, as `patch_settings` :590 does) before spawning — `classifier_setup` is sync, so either make it async or push the Arc-swap to the head of the spawned task. In the disable arm, clear `live.classifier_slot` (mirror the GUI leg). Regression: a fixture asserting the live Arc + slot state immediately after an engine-backed `classifier_setup` call — red today (Arc unchanged, slot stale), green after; the pipeline itself stays network-bound and out of test reach, as documented.
## Finding 3 (LOW) — `patch_settings`' doc contradicts the patch surface on `laya_enabled` (silently half-applies)

`setup.rs:536-539` states: "The managed-CLASSIFIER flip does not ride this patch (D5): it has its own surface — `Setup::classifier_setup`…". But the shared `SettingsSaveDto` carries `laya_enabled: Option<bool>` and `validate_and_apply_settings_patch` applies it (settings_dto.rs:704 — the same function the headless `patch_settings` calls). So a headless `patch_settings` patch that includes `laya_enabled` DOES persist the flip and swap it into the live config Arc (:590) — while owning none of the classifier leg (no sidecar stop/start, no slot swap, no status update), diverging from the GUI's `save_settings`, which owns the whole leg via `rewire_vision_embedder_and_classifier`. The headless consumer is left in a half-applied state (config says enabled, engine behaves disabled) with no signal — and the only working engine-backed enable flow (patch first to reach the Arc, then `classifier_setup` to install, per Finding 2) relies on exactly the behavior the doc disclaims.

**Fix.** Either reject `laya_enabled` at the headless `patch_settings` boundary (stable-code error pointing at `classifier_setup`), or own the classifier leg there (the Laya core is lib-side since D5, so the leg is now shareable — it stayed app-side only for the AppHandle emit). Either way the doc must match the landed behavior.
## Constitution checks

- **Documentation sync** — module doc comments landed with every move (setup.rs, laya.rs, rewire.rs, embed_download.rs, mcp.rs, keys.rs, scaffold.rs, and each app thin-layer); README/PLAN.md untouched by the delta and genuinely need nothing (internal architecture; the master spec carries the design). One doc finding: the `patch_settings`/`classifier_setup` doc comments misdescribe their own surfaces (Findings 2-3).
- **Multi-platform neutrality** — clean. `uv_asset_name` covers all six OS/arch pairs (win/mac/linux × x86_64/aarch64, unit-tested at laya.rs:1042+); `hide_console_window` is `cfg(windows)` with a no-op elsewhere (laya.rs:237); embed_download.rs gates by feature only, no platform cfg; the Setup paths are all config_dir-derived. No Windows-only assumptions outside the sanctioned gates.
- **File-tools-first** — clean; no shell-mutation patterns in the delta.
- **Warning-free build** — no `#[allow(...)]` added anywhere in the delta. (Lib-side green per the parent's battery; the app crate is superseded by Finding 1 — it cannot compile at all.)
- **Bookkeeping (accuracy, not line review)** — the step file's §F notes 4.1-4.8 match what shipped, including the honest en-route-fix list; the §F 4.7 consumer list is accurate about intent but its landed state is exactly Finding 1 (the console twin missing, the visibility contradicting the listed main.rs consumers). The plan file is bookkeeping-dirty (uncommitted) as expected mid-review.

## Verified clean (risk focus)

1. **G4 install isolation** ✓ — no `global_config_dir(` anywhere under src/headless (my own walk + the close-grep test in mod.rs); the Setup derives everything from its explicit config_dir: embedder cache `<dir>/models`, laya `<dir>/laya`, agent.md Global scope, mcp.toml paths, projects.toml. The app twins pass `config_dir: None` (machine-global) — byte-identical GUI behavior.
2. **Brain split (lib side)** ✓ — the factory funnel gets `store_trait` (brain.rs:1133, row 33 leg 1: recall+consolidation+capture through the injected store); `Engine::memory()`, `spawn_startup_embedder_load`, and `LiveHandles` read `own_memory_store`; `Engine::memory()` errors `memory_disabled` for injected stores per spec (app-side consumers are Finding 1).
3. **3c F1 lesson** ✓ for writes — no method writes through a slot a fresh Setup shares: persist paths write only to `self.config_dir`; live writes go only through `for_engine`'s Arc-cloned handles. (The classifier-control read side is Finding 2.)
4. **Frontend payload shapes frozen** ✓ — `SettingsSaveWire` = `#[serde(flatten)] engine: SettingsSaveDto` + `#[serde(flatten)] ui: SettingsUiPatch` (shape unchanged; lib patch lost the 14 [ui] fields); `[ui]` is carried by the lib's `GeneralConfig.ui` (general.rs:29) so lib saves preserve it and the GUI reads it unchanged; `McpServerWire` moved lib-side with the same flatten+default shape; the legacy-payload + wire tests pin both.
5. **GUI thin layers** ✓ — `ipc/{laya,embeddings,mcp}.rs` are glue only: Option<AppHandle> signatures, sink-vs-None console twins, the `classifier://status` + `embedder://status` emits, and the D8 save delegation (config.mcp sync + manager swap).
6. **D7 sqlite fallback** ✓ — the fallback keeps the RESOLVED embedder (brain.rs:576-582, `open_in_memory(embedder)`, found by the fixture per §F).
7. **D8 semantics** ✓ — `save_split` with `restore_shadowed_globals` (shadowed-baseline restore unit-tested), a purely-global save never materializes a project file, `write_rules` matches the kernel parser (FILE_HEADER + the mtime re-read), and the scaffold kernel + app re-export shim follow the 2b pattern.
8. **EngineError codes** ✓ — `not_configured` + `setup_in_flight` (and `memory_disabled`) as stable codes per row 15; the not_configured gate + the non-fatal embedder leg are pinned by fixtures.
9. **Fixtures** ✓ — 14 new in `headless_setup.rs` covering D1-D8: open/status/ordering, the ready rule, key-source semantics (file/Env/InMemory), upsert validation + key preservation, the settings/ui preservation, patch persistence, the classifier disable + guard, the config-dir-rooted manager + catalog + cache install, the no-surprise-downloads guarantee, init_project scaffolding + registration, agent.md both scopes, the kernel-format safety rules, and the app-parity MCP split.

## Fix order

1. Finding 1 first (make `own_memory_store` `pub`; rewire console.rs:1226) — nothing else can be verified on the app until it compiles; the closing battery must include `cargo build -p mnemo-app` / a true `cargo test --workspace`.
2. Findings 2-3 (the classifier-control surfaces): apply the flip to `live.config` in the engine-backed arm, clear the slot on disable, reconcile the `laya_enabled` doc/behavior, add the Arc/slot-state fixture.