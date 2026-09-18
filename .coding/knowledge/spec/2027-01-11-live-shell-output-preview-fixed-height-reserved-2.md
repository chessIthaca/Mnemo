+++
title = "Live shell-output preview — fixed height, reserved, toggleable — MERGED into main (dcc7a93)"
supersedes = "2027-01-11-live-shell-output-preview-fixed-height-reserved"
created = "2027-01-11"
+++

MERGED into main at dcc7a93 (dcc7a93ff84f7dc59934087c773ea7b052623b9b) on 2027-01-16 via the merge_to_main skill; branch wt/mnemo deleted (pre-merge tip 21c6c13) — supersedes this record's earlier "on wt/mnemo" hint. The live shell-output preview renders at a FIXED h-[10em] (exactly six lines at the element's own em), reserved from the first paint of a running shell card (gated on showShellPreview && name === "shell" && running), toggleable via Settings → Chat (localStorage mh.showShellPreview, default on, localStorage-only persistence). Full detail: .coding/knowledge/spec/2027-01-11-live-shell-output-preview-fixed-height-reserved.md.
