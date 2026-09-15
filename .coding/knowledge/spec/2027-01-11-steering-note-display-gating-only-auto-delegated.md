+++
title = "Steering-note display gating (only AUTO-DELEGATED is toggleable today)"
created = "2027-01-11"
status = "superseded"
+++

User report 2027-01-14: steering notes in tool output are only partially controllable. Settings → Chat → "Show auto-delegation notes in search results" = `[ui].show_delegation_notes` (default false, `src/config/general.rs:388`) strips ONLY the `note: AUTO-DELEGATED …` line — the filter is `frontend/src/lib/delegationNotes.ts` (prefix list `["note: AUTO-DELEGATED", "AUTO-DELEGATED"]`), applied at the two ToolCard render sites in `frontend/src/components/chat/Message.tsx` (~line 510, ~line 811). The tool-result text (the model's context) is never modified — display-only filter.

Every OTHER steering line renders unconditionally. Marker set: `MarkerKind::marker()` in `src/agent/steering_stats.rs:113-126` (labels at `label()` 166-179) — search-nudge, shell-tip, graph-miss, recall-rider, read-nudge, literal-tip, known-memory-hit, consolidation-due, shell-redirect, edit-stale-read. Example: the literal TIP from `literal_tip()` in `src/tool/agent/search.rs:809` ("TIP: pattern has no regex metacharacters — literal:true would use the content-index engine") rides `merged_note`/`with_note` as `note: TIP: …`.

Per-note toggles were requested → backlog item "Per-note toggles for steering notes in the Chat config" (2027-01-14).
