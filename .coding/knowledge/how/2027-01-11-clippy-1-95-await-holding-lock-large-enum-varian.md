+++
title = "clippy 1.95 await_holding_lock + large_enum_variant fix conventions"
created = "2027-01-11"
+++

Plan efd1f4c3 (clippy 1.95 sweep), learned 2027-01-20 on wt/mnemo while fixing batch 7:

1. await_holding_lock (std MutexGuard across .await): explicit `drop(guard)` BEFORE the await does NOT satisfy clippy 1.95 — the lint is a scope-based analysis and still fires at the original lock line (verified: runtime/agent.rs 3 sites + spawn_agent.rs stayed RED after drop() insertion, help text "ensuring the MutexGuard is dropped before calling await" notwithstanding). The fix that works: replace the guard BINDING with a snapshot clone — `let calls = mutex.lock().unwrap().clone();` — the guard is then a statement temporary, dead at the semicolon, and the owned Vec serves the asserts. Alternatives: async-aware Mutex (tokio::sync::Mutex) or scoping the guard in a block.

2. large_enum_variant: boxing only the LARGEST variant can leave the second-largest still over the 200 B difference threshold — BrainOutcome { Ready(Brain), NeedsProject(Box<Config>) } still fired at "216 bytes" (Brain ~216 B vs Box 8 B) after boxing Config (~1.7 KB). Fix: box BOTH variants (Ready(Box<Brain>)). Always re-run the gate after a Box change before assuming the site is clean.

3. items_after_test_module can have MULTIPLE stragglers after the tests mod — workspace.rs had both ensure_minimal_project AND find_coding_dir_ancestor below mod tests; the first gate re-run only surfaced the lint position again. Read the WHOLE file tail after a test-mod boundary fix.

4. Consuming methods named to_* fire wrong_self_convention; rename to into_* (StepInput::to_text -> into_text) rather than switching to &self + clones — the by-value semantics were correct, only the prefix was wrong.
