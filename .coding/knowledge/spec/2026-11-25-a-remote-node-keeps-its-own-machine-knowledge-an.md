+++
title = "a remote node keeps its own machine knowledge and writes typed knowledge into the worker's checkout"
created = "2026-11-25"
evidence = "src/remote/memory.rs (MEM_MIRROR_ROWS, is_machine_row, merge_hits), src/agent/factory.rs with_worktree_knowledge; tests mirror_rows_reach_the_hub_once, hub_recall_hides_machine_rows_from_nodes, node_recall_sees_its_own_machine_rows, worktree_knowledge_writes_into_the_agent_root"
+++

On a remote worker node, typed memory_write records (DECISION:/SPEC:/HOW:) land in <checkout>/.coding/knowledge because the node engine sets EngineBuilder::worktree_knowledge(true). They reach the hub with the branch and are indexed after merge. Every other engine writes to the project-wide store. The hub's MemoryHandler serves only nodes and never returns rows whose data.source is the machine corpus. The node's RoutedMemoryStore merges its OWN machine rows back into recall and list_filtered, so a worker sees the machine notes of the computer it runs on. The node's spend and savings rows are written locally first, then mirrored to the hub with mem.mirror_rows; the hub inserts them with INSERT OR IGNORE on the row UUID, so a retried flush is harmless. Queued rows are lost if the node process exits.
