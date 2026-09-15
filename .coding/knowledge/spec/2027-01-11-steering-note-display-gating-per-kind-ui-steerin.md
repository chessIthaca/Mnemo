+++
title = "Steering-note display gating per kind ([ui.steering_notes], Settings → Chat)"
supersedes = "2027-01-11-steering-note-display-gating-only-auto-delegated"
created = "2027-01-11"
+++

Every steering note in tool output is now individually show/hide-able: Settings → Chat → "Steering notes in tool results", persisted as `[ui.steering_notes]` (one optional boolean per kind). Landed for plan 3ae367bd — full design + invariants in .coding/plans/3ae367bd.md; reviews .coding/reviews/2026-09-15-steering-note-toggles-review.md (round 1: 0 high / 4 low, all fixed) and …-round2.md (branch wt/mnemo).

Eleven kinds: the 10 stable `MarkerKind` labels (search_nudge, shell_tip, graph_miss, recall_rider, read_nudge, literal_tip, known_memory_hit, consolidation_due, shell_redirect, edit_stale_read) + the `auto_delegated` display FAMILY (code-graph delegation and its memory twin, which carries no marker of its own).

Invariants to preserve:
1. DISPLAY-LAYER ONLY — the tool-result text the model reads (including a note's re-issue escape hatch) is never modified; no Rust consumer reads the hidden set (config + IPC DTO only). Filter sites: the two store reads in frontend/src/components/chat/Message.tsx — collapsed note chip (`filterSteeringNote`), collapsed error summary (`toolErrorSummary` → `filterSteeringNote`), shell stdout/stderr, generic `<pre>`.
2. Defaults preserve the old single toggle: only `auto_delegated` hidden (`DEFAULT_HIDDEN_STEERING_NOTES`). `[ui].show_delegation_notes` still parses and seeds auto_delegated's default via `effective_steering_note_visible` (src/config/general.rs); the frontend hydration passes that legacy boolean as `hiddenKeysFromConfig(cfg, legacyShowNotes)` for a backend with no table.
3. The TS registry `STEERING_NOTES` (frontend/src/lib/delegationNotes.ts) is a PROSE MIRROR of the Rust emission sites — steering_stats.rs `MarkerKind::marker()`, search.rs (delegation blocks, `symbol_nudge`/`alternation_nudge` incl. the UNQUOTED name==branch entry, `literal_tip`, known-memory-hit), codegraph.rs `miss_hint` (JSON field value), shell.rs GREP_NUDGE/REDIRECT_NOTE, file_edit.rs `with_fresh_read_nudge`, steering.rs recall rider + consolidation note. Keeping them in lockstep is a review duty; an uncovered emission shape = a checkbox that does nothing.
4. The AUTO-DELEGATED graph header IS the search_nudge emission — the toggles UNION (either hides it); deliberate and tested.
5. Merged notes (`"; "`-joined) split only at a RECOGNIZED note start, so dropping one component leaves the remainder byte-identical.
6. Matching is anchored to line start / the note's marker shape: only location-prefix-free raw streams (shell output, generic `<pre>`) can hide a note quoted verbatim at column 0 — worded that way in docs/CONFIGURATION.md + docs/FEATURES.md.
