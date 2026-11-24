+++
title = "the SQLite MemoryStore is never released when an engine or the lead runtime shuts down"
created = "2026-11-24"
evidence = "Arc::strong_count / Drop instrumentation on 2026-11-24 (Drop for MemoryStore never fired); leftover temp folders containing exactly .coding/memory.db*."
+++

Symptom: after the documented teardown, the memory store's Arc stays alive. Headless `session.close_and_wait()` plus dropping the runtime leaves about 23 strong references; `LeadRuntime::shutdown()` plus drop leaves about 39. `.coding/memory.db` (with -wal/-shm, and codegraph.db where enabled) stays open, so on Windows the project folder can't be deleted. In tests this leaked about 213 temp folders per full `cargo test --workspace` (headless lifecycle tests, about 196 mnemo-app runtime/team/mcp_server tests, about 15 integration tests). In the running app it means open DB handles and memory held for the life of the process.
Known holders: HostRuntime.agent_loops (src/host/runtime.rs) holds Arc<AgentLoop> strongly and is never cleared on shutdown; clearing it released about 15 references. Each AgentLoop (src/agent/loop_impl.rs) holds `memory` plus one clone per memory/knowledge/plan tool in its tool set. Further clones sit in AgentLoopFactory.memory (src/agent/factory.rs) and in Brain (own_memory_store + memory_store). About 24 references remained even after clearing agent_loops, held by objects outside the runtime/engine tasks.
Status: being fixed (agent_loops cleared on shutdown, tasks aborted or joined, non-owning back-references made Weak), with tests memory_store_is_released_after_headless_close and memory_store_is_released_after_lead_runtime_shutdown. Update this record when the fix lands.

Amended 2026-11-24: Fixed (merge 07510ddc). Holders and fixes:
- HostSpawner's strong back-references, which formed cycles, now hold Weak.
- Engine::shutdown now aborts and joins the pump first, cancels every agent including the main one, drains agent_loops with a 5 s wait (LOOP_RELEASE_WAIT), and joins session-end consolidations concurrently with a 3 s grace (CONSOLIDATION_GRACE). Consolidations still running after the grace finish detached, never aborted, so no memory summary is lost. Both waits fit inside the app's 10 s SHUTDOWN_DEADLINE.
- Engine now has a Drop that aborts the pump.
- Consolidations are tracked in BackgroundTasks on AgentManager.
- MemoryWatcher, GraphWatcher and the symbol-embed thread hold Weak references.
- ConfigWatcher aborts its debounce task on Drop.
- LeadRuntime and TeamRuntime release their engine, board and stores in shutdown.
Tests: memory_store_is_released_after_headless_close, memory_store_is_released_after_lead_runtime_shutdown, background_join_is_concurrent_and_never_aborts_a_straggler.
Accepted trade-off: a consolidation that never completes (a stalled provider) keeps the store open until it ends or the app exits. Still open: the background re-embed pass (brain.rs, embed_download.rs, rewire.rs) holds the store strongly and is not joined, and is being fixed next.
