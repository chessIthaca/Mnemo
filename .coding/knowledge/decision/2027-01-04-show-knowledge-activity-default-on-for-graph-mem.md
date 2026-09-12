+++
title = "show_knowledge_activity (default ON) for graph + memory + auto-recall chat cards"
created = "2027-01-04"
status = "superseded"
+++

DECISION (2027-01-04, plan bc09914f, backlog 68c4c9a5, branch wt/agenticcoding, commit a228276): graph + memory + auto-recall activity cards are now VISIBLE BY DEFAULT in the chat transcript via a NEW `show_knowledge_activity` setting (default ON), rather than riding the existing `show_tool_activity` toggle (default off).

Rationale: the user wants to SEE graph + memory + auto-recall activity out of the box (the current default hid everything). A dedicated toggle lets the user hide ONLY knowledge cards without revealing the full noisy tool-card set (shell/read_files/file_edit) that show_tool_activity governs.

Implementation (reuses existing card components — nothing was missing, only visibility):
- Backend: UiConfig.show_knowledge_activity (bool, default true) + full patch/dto/IPC plumbing + contract fixture sync (follows the show_tool_activity precedent, plan 0e71e4f9). Files: src/config/general.rs, patch.rs, settings_dto.rs, src-tauri/src/ipc/settings.rs + contract_fixtures.rs.
- Frontend: store field showKnowledgeActivity + setShowKnowledgeActivity setter (useAgentStore.ts); App.tsx hydration `settings.ui.show_knowledge_activity !== false` (absent field reads ON); ChatDraft + Settings→Chat checkbox (ChatSection.tsx); new isKnowledgeActivityEntry predicate in agentState.ts — `entry.kind === "memory" || (entry.kind === "tool" && entry.name.startsWith("graph_"))`; Conversation.tsx render filter: `showToolActivity || !isActivityEntry(entry) || (showKnowledgeActivity && isKnowledgeActivityEntry(entry))`.
- The `memory` entry kind covers BOTH memory tool calls (memory_search/write/etc. via MEMORY_TOOLS in agentEventReducer.ts) AND auto-recall entries (reduceMemoryRecalled pushes kind:"memory", name:"auto-recall"). graph_* tool calls are kind:"tool" (not in MEMORY_TOOLS) → matched by name prefix.
- Prompt: TOOL_STRATEGY bullet added (prompt.rs:227) — "Auto-recalled memories are injected each turn — use them silently; if they are not useful to the current task, do not narrate or apologize for them." Pure insertion (byte-stable head preserved); no steering-marker substrings.

Render-filter truth table: showToolActivity=ON → everything; OFF + knowledge=ON → knowledge cards + conversation; OFF + knowledge=OFF → hide all activity cards.

Review: .coding/reviews/2027-01-04-show-knowledge-activity-review.md (PASS, no findings).
