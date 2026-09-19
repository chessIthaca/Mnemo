+++
title = "shell tool — purpose required, PowerShell && / || auto-translate"
created = "2027-01-11"
+++

The shell tool (src/tool/agent/shell.rs) enforces two calling contracts since plan 358656a1 (commit d136026 on wt/mnemo, 2027-01-24, backlog 79a2755d):

- `purpose` is REQUIRED at deserialization (ShellArgs.purpose: String, no #[serde(default)]) — the schema's `required` list is now enforced at runtime; a missing field errors with "parameter 'purpose' is required". Still consumed by the frontend from raw tool-call args, never read in Rust (#[expect(dead_code)] retained).
- On Windows (PowerShell 5.1), top-level `&&` / `||` are auto-translated to `if ($?)` gates before the child spawns (translate_powershell_chaining — quote-aware scan; single `&`/`|`, `2>&1`, and quoted operators untouched; degenerate chains — leading/trailing operator or empty segment — return None so the original PowerShell parser error surfaces). `a && b || c` → `a; if ($?) { b }; if (-not $?) { c }` — left-to-right short-circuit semantics preserved. The ORIGINAL command is what approval/classification saw; the translated string is only what runs. CHAIN_TRANSLATION_NOTE rides the displayed output only (raw stdout/stderr in data stay note-free).
- The schema description carries the empty-call trap ("no zero-argument form") and the chaining advisory ("chain with ; not && / ||"); the sh path is untouched (translation is cfg(windows)-only).

Budget: ExecutingResearch tools-array ceiling 26_800 (raised from 25_950 for the ~+350 description growth — dated raise comment in src/agent/factory.rs).

Regression tests: missing_purpose_is_rejected, powershell_chain_translation_rewrites_both_operators, powershell_chain_translation_leaves_valid_commands_alone, powershell_chain_translation_runs_and_notes (cfg windows), schema_description_names_the_calling_traps. Review: .coding/reviews/2026-09-19-shell-powershell-chaining-review.md (PASS, 0 high / 3 low — L2/L3 fixed).
