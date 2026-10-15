+++
title = "job 1 host extraction complete — mnemo::host landed (wt/mnemo @ef65642)"
created = "2027-01-11"
+++

Job 1 (host extraction) of the headless-free-agent master spec is COMPLETE — all three steps executed, reviewed (round-1 findings fixed/queued, round-2 PASS each), and finished on branch wt/mnemo.

Landed architecture (zero behavior change; app crate compiles via pub(crate) re-exports):
- src/host/mod.rs — ungated modules: index_progress, maintenance, notifier, runtime, startup; brain gated on all(feature = "browser", feature = "embeddings"). Module doc enumerates all; nothing may depend on Tauri or the app crate.
- src/host/startup.rs, maintenance.rs, index_progress.rs (step 1.1); notifier.rs (HostEvent/HostNotifier/SharedNotifier seam) + brain.rs (Brain/BrainOutcome/build_brain, gated) (step 1.2); runtime.rs (step 1.3): AgentLoopMap alias, WorktreeBinding, parent_root_binding, pub spawn_agent_shared, REVIEWER_BASE_TOOLS/ALWAYS_SAFE_ROLE_TOOLS, compute_subagent_allowlist, pub cleanup_inactive_subagents, HostSpawner (ex-ConsoleSpawner) + LifecycleBookkeeping, plus the 7-test module at EOF.
- App side: src-tauri/src/ipc/{spawn,events,state}.rs carry pub(crate) re-exports (spawn_agent_shared+WorktreeBinding; cleanup_inactive_subagents; AgentLoopMap); console.rs imports mnemo::host::runtime::{AgentLoopMap, HostSpawner, LifecycleBookkeeping} with #[cfg(test)] ParentAwareSpawner; spawn.rs keeps the spawn_agent command, spawn_run_all_agent, IpcSpawner.

Commits (oldest→newest, all on wt/mnemo): f21edf0 specs · 322139e 1.1 · f7ac4d1 + d374bc9 1.1 close · f960be8 1.2 · a1796b3 + a30dc75 1.2 close · fad6163 1.3 · 84f4c70 + ef65642 1.3 close (job tip). Reviews: .coding/reviews/2026-10-11-job1-step1.{1,2,3}* (+round2 each). Plans: a9ddfaee (1.1), 041d867b (1.2), eeb9dd91 (1.3), all closed.

Deferred to later jobs: HostRuntime type → 3a (D2). Open queue: backlog #4eba8f03 (src/host doc-link/stale-prose cleanup pass — covers the IpcSpawner/spawn_run_all_agent doc links + runtime.rs :222-224/:587/:634-636 prose), #e6b15418 (clippy-1.95 drift restore; workspace clippy -D warnings red, known). Next per master spec: job 2a (AgentPolicy seam) — NO step file exists yet; coordinator writes .coding/specs/job2a-*.md from the master spec §job-2 first, anchor-verifying every quoted region against the post-job-1 tree (job-1 moves shifted many line numbers).
