+++
title = "Mnemo2 Phase 1 real lead runtime and model registry"
created = "2027-01-11"
+++

Mnemo2 Phase 1 landed on wt/mnemo at d46fd77. The lead is a real headless-engine session created via Engine::spawn(SpawnOptions) with the mnemo.workflow policy, not Engine::session(id). A minimal Tauri host exposes typed IPC for open/prompt/interrupt/snapshot/models/model changes/approvals/questions; the frontend adapter streams events, restores transcripts, and rejects stale generations. Persist the selected endpoint/model spec separately from the observed provider model id. Keep the lead busy through pending approvals/questions and gate send/model changes. Rust runtime tests cover real approval and question turns; the approval test uses a private policy-free seam because workflow Planning blocks shell execution. Workers/team runtime remain Phase 2. Pointers: .coding/plans/8509d764.md, src-tauri/src/mnemo2/runtime.rs, src-tauri/src/mnemo2/ipc.rs, mnemo2/src/lib/leadEngine.ts.
