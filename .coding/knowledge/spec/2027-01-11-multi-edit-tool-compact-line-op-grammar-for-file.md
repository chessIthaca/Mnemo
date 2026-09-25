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

Amended 2027-01-11: Landed: the 2027-01-11 merge_to_main landing of wt/mnemo (merge 4cbc3b0, pre-merge tip 4c08077) carried 88577b4 into main together with this knowledge file — the "UNMERGED" header above is historical. Re-verified green on the re-dispatch (plan edf8c0cf, backlog 2e27f896): root cargo test 2,602 passed / 0 failed / 6 ignored under #![deny(warnings)] (all 9 multi_edit tests green, including the atomicity regression), frontend `tsc --noEmit` exit 0, vitest 91 files / 1,273 tests passed. src-tauri remains blocked by the Application Control policy (os error 4551) — the known environment fact recorded in plan e3d0758b's FINAL VERIFY.
