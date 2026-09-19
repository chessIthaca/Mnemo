+++
title = "shell tool — purpose required, PowerShell && / || auto-translate — MERGED into main (5a86e9d)"
supersedes = "2027-01-11-shell-tool-purpose-required-powershell-auto-tran"
created = "2027-01-11"
+++

MERGED into main at 5a86e9d (5a86e9d1157a497a8f6fdc567c29d584dda774eb) on 2027-01-24 via the merge_to_main skill; branch wt/mnemo deleted (pre-merge tip 33ad6ec) — supersedes this record's earlier unmerged marker. WHAT: the shell tool (src/tool/agent/shell.rs) enforces two calling contracts since plan 358656a1 (commit d136026, backlog 79a2755d): `purpose` is required at deserialization, and top-level && / || chains auto-translate to PowerShell 5.1 if ($?) gates (quote-aware scan, CHAIN_TRANSLATION_NOTE prepended, approval/classification on the ORIGINAL command). Knowledge file: .coding/knowledge/spec/2027-01-11-shell-tool-purpose-required-powershell-auto-tran.md.
