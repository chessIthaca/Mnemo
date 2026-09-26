+++
title = "Laya managed-only English-only clean-up — landed design (plan 2f74e10a)"
created = "2027-01-11"
status = "superseded"
+++

Landed 2027-02 on branch wt/mnemo (plan 2f74e10a; commit follows this session's closing sequence). Laya is now MANAGED-ONLY + ENGLISH-ONLY: `[general.laya]` carries only `enabled` + the consumer flags (`auto_type_memories`, `steer_tool_choice`, `failure_triage`, `failure_triage_knn`, `auto_finetune`).

Removed: `LayaMode` enum + `LayaConfig::{mode, endpoint, checkpoint}` (src/config/general.rs); the `laya_mode`/`laya_endpoint`/`laya_checkpoint` patch fields (src/config/settings_dto.rs); `build_classifier` + its 4 tests (src/provider/client_factory.rs — `LayaClassifier` stays, used by `build_managed_classifier`); the external branches in src-tauri/src/main.rs and src-tauri/src/ipc/rewire.rs; the multilingual catalog entry + `find_checkpoint` (→ `english_checkpoint()`) + `laya_setup`'s argument (src-tauri/src/ipc/laya.rs); the LayaWire mode/endpoint/checkpoint fields (src-tauri/src/ipc/settings.rs) and the contract fixture keys (src-tauri/src/ipc/contract_fixtures.rs + frontend/src/lib/ipc-fixtures/dto-get-settings.json); the frontend mode radios, endpoint input, checkpoint radios and `laya_*` patch keys (frontend/src/lib/tauri.ts, settings/types.ts, settings/sections/ClassifierSection.tsx).

Invariants to preserve: absent/disabled `[general.laya]` ⇒ no client, no server, no downloads (unchanged); enabled ⇒ the app downloads/owns the loopback sidecar and serves English; a legacy config.toml with the removed keys still LOADS and drops them on the next save (`config::general::tests::laya_removed_mode_endpoint_and_checkpoint_keys_are_ignored`). Verification: root `cargo test` exit 0, src-tauri `cargo test -p mnemo-app` exit 0 (328 tests), frontend `npx tsc --noEmit` 0 + `npx vitest run` 92/92.
