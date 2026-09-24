+++
title = "multi_edit tool + compact line-op grammar for file_edit"
created = "2027-01-11"
+++

UNMERGED — on branch wt/mnemo, commit 88577b4 (plan e3d0758b, completed 2026-09-24; not yet merged to main).

WHAT SHIPPED
- `multi_edit` (src/tool/agent/multi_edit.rs): edits N files in ONE approval/call, atomically — every entry validated & prepared in memory (sandbox validate → duplicate guard on the sandbox-VALIDATED PathBuf, so `a.txt`/`./a.txt` collide → dir/protected refuse → read → apply ops → diff) BEFORE any write; any prepare error aborts with zero bytes written. Write-phase failures name exactly what landed and call the failed file out as possibly truncated (never "byte-identical").
- Previews: `ApprovalPreview::MultiDiff { paths, diff }` (src/provider/mod.rs) — ONE combined diff with per-file `---`/`+++` sections; rendered inline in ApprovalPrompt and on the tool card, plus the Diff tab (`multi_diff` branch in DiffViewer.tsx; approval auto-reveal covers file_edit/file_write/file_append/multi_edit).
- `file_edit` gained the compact line-op grammar (`ops`: i/b/d/r verbs with counts + anchors, fuzzy whitespace) alongside the classic anchor form, plus the `edits` batch alias; shared op engine in src/tool/agent/edit_ops.rs (also powers multi_edit). Dispatch/steering/strict schemas wired.

POINTERS
- Plan: .coding/plans/e3d0758b.md · Reviews: .coding/reviews/2026-09-24-e3d0758b-multi-edit-review.md, -round2.md (1 low: Diff auto-show) and -round3.md (PASS) — all findings fixed.
- Tests: root `cargo test` green; frontend `tsc --noEmit` + `vitest` green (1253).
