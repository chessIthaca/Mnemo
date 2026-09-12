+++
title = "file_edit six modes (EOL-agnostic, batch, append) + per-path gate + memory_amend (be16ea36)"
created = "2027-01-07"
+++

SPEC (plan be16ea36, merged through 3 review rounds — round-3 PASS at .coding/reviews/2026-09-10-file-tool-reliability-be16ea36-round3-review.md; commits eb202c6 + 0a5eec6 + 9d4e293 on wt/agenticcoding):

file_edit has SIX modes: LITERAL / REGEX / LINE-RANGE / FUZZY / BATCH (`edits`) / APPEND (`append`). Literal+fuzzy matching is EOL-agnostic: both sides project to LF (`normalize_lf_with_offsets` byte-offset map), the needle matches in LF space, the replacement is spliced into the ORIGINAL bytes and re-emitted in the file's detected majority style — mixed-EOL files no longer false-drift on verified-identical text. BATCH is atomic: items applied in order via the shared `literal_splice` core, ONE write, any failing item aborts with the file untouched (error names item index + anchor excerpt); empty per-item anchors rejected (review H1); emission checks are per-item line-scoped + one combined brace-delta (review L2). APPEND adds new_string at EOF in the file's style (no extra prefix when the file already ends with a newline; empty file takes the text verbatim); missing file steers to file_write.

Stale-read gate is per-(agent, path) with direct read recording (`edit_drift` map; note_edit_drift/note_edit_success/note_file_read/clear_edit_drift_for): an interleaved non-read call no longer freezes the gate, and a successful file_write/file_append of P lifts P (review L3).

memory_amend: append-only knowledge-record amendment ({id, paragraph} → KnowledgeStore::amend appends "Amended <date>: …" to the file body, so the row-digest guard can't fire); refuses unknown ids + superseded records; registered ONLY in the factory's with-knowledge branch (DB-only branch deliberately lacks it — documented, review L5).

Constitution: file-tools-first policy now in the compiled prompt CORE PRINCIPLES + agent.md "File mutation policy" + reviewer expectations (shell surgery only for protected paths/verified freezes, justification stated). file_write/file_append chunk guidance ~4000 chars. Observability: per-session mutation counters by vehicle (file tools vs shell patterns — heuristic, may over-count) in SteeringStatsSnapshot → Trace panel. Tools-array budget ceilings raised with measured figures (Planning 16_000, Executing 28_000, ExecutingResearch 22_800, Reviewing 24_500, Complete 16_000).
