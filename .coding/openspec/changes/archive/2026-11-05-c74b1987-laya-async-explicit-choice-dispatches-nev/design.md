## Interfaces

- 2026-11-05: WorkerRouting field semantics: laya_tier/confidence/cleared are Option and may populate AFTER dispatch on explicit-choice dispatches; the UI already renders nulls — no schema change, only when fields populate. routed_by/lead_tier/ran_tier still resolve at dispatch time.
- 2026-11-05: File ownership: this plan owns src-tauri/src/mnemo2/team.rs and src-tauri/src/mnemo2/dispatch_routing.rs only; plan 7bc0b473 owns src/provider/**, src-tauri/src/mnemo2/trace_view.rs and mnemo2/src/** — no overlap.
