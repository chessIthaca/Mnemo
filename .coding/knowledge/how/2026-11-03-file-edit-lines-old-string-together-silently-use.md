+++
title = "file_edit lines+old_string together silently uses line-range mode — verify struct edits with git diff"
created = "2026-11-03"
+++

file_edit hazard: when BOTH `lines: [start, end]` and `old_string` are passed in one call, LINE-RANGE mode wins and old_string is silently IGNORED — the whole range is replaced by new_string, destroying any intermediate lines that were not part of the intended edit. This nearly deleted the `audit_target` field and the findings doc comment from WorkerRecord in src-tauri/src/mnemo2/team.rs (the field vanished; compile errors E0560/E0609 exposed it — 'struct has no field named audit_target', read sites still referencing it). Recovery: `git diff -U12 -- <file>` showed the exact original lines, and a plain LITERAL edit (old_string only, no lines) restored them.

Rules going forward:
- Never pass `lines` together with `old_string`; pick one mode. Prefer literal anchors (old_string) for content edits; use `lines`/ops only when whitespace makes literal matching impractical, and re-read the range first so the replacement is complete.
- After any structural edit to a struct/enum, run `git diff -U12 -- <file>` on the hunk before compiling — a wholesale range replacement loses doc comments and attrs with no tool error.
- A field silently missing from a struct surfaces as E0560/E0609 at every construction/read site — trust those errors over the edit tool's success message.
