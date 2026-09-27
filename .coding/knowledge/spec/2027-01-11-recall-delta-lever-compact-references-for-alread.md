+++
title = "recall_delta lever — compact references for already-injected memories (commit f38e56b)"
created = "2027-01-11"
+++

SHIPPED (commit f38e56b on wt/mnemo, plan 6d8cbe94, backlog 30bacfa2; round-1 review FINDINGS 0 high/2 low both fixed, round-2 delta review PASS).

What it does: the `recall_delta` lever in `[general.optimizer]`, ON by default, stops the volatile tail re-sending a memory's 160-char snippet on every re-recall. A repeat renders `[tier] Title (id: X, score: 0.40) — unchanged since injection (turn N); re-read with memory_search if needed.` instead. Elision requires same memory id AND same content hash AND same compaction epoch; a new, changed (update/supersede), or post-compaction recall still arrives in full, and the flag-off path is byte-identical to before. Each elision writes a `savings_events` row, kind `recall_delta`, detail "{n} memories ({k} elided}", measured=false — visible in the Dashboard per-kind table.

Where: src/agent/recall_delta.rs (RecallDeltaCache {entries: id -> (content_hash, epoch, full_turn), turn, epoch}; content_hash over tier+title+content), SessionState.recall_delta (loop_impl.rs:57 — chosen over an AgentLoop field so ~all AgentLoop::new call sites stay untouched), prompt::format_recall_context_delta + format_recall_entry (prompt.rs), AgentLoop::recall_block (turn.rs; read lever -> classify -> render -> `after >= before` guard -> record_savings_event), begin_turn at the top of run_turn, invalidate() at BOTH `*messages = summarized;` sites.

Two facts worth keeping: (1) the volatile tail is popped right after every request, so an elided reference is a POINTER the model dereferences with memory_search — never text still in the conversation; the user explicitly accepted this tradeoff over eliding nothing. (2) Because the reference line is ~164 chars, elision only pays off for memories whose content approaches the 160-char render cap — for a short memory the guard serves the full block, which is correct, not a bug.
