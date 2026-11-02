+++
title = "src-tauri crate split — ipc/** is Mnemo1-binary-only; the lib crate (mnemo2 host) cannot reach it"
created = "2026-11-02"
+++

Fact: in the src-tauri crate, the `ipc` module tree (src-tauri/src/ipc/** — Mnemo1's Tauri bridge: laya.rs, finetune.rs, settings.rs, rewire.rs, …) is declared ONLY in src-tauri/src/main.rs (`mod ipc;`), i.e. it is compiled into the Mnemo1 BINARY target. src-tauri/src/lib.rs (crate name `mnemo-app`) declares only `webview_udf`, `instance_marker`, and `#[cfg(feature = "mnemo2")] pub mod mnemo2;` — the mnemo2 module and everything it can reach (the mnemo-app BINARY `src/bin/mnemo2.rs` links that lib) therefore CANNOT resolve `crate::ipc::…` (E0433). lib.rs's own doc comment documents the intent: "the intentionally separate Mnemo2 host without pulling Mnemo1's larger IPC surface into the Mnemo2 binary".

Implication: any Mnemo1 helper living under src-tauri/src/ipc/** (fine-tune gates, catalog/setup commands, rewire machinery) is unreachable from Mnemo2 code until it is lifted into the lib crate (lib.rs + main.rs + ipc/mod.rs change) or reimplemented in the mnemo2 tree. Kernel-side equivalents that ARE reachable from both hosts: everything re-exported from the `mnemo` crate (e.g. mnemo::laya::LayaManager/start_managed_server, mnemo::headless::Setup's attach_laya/classifier_slot/live_config).
