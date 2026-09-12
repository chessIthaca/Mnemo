+++
title = "output rendering strips special tokens — codepoint-dump + PowerShell write workaround"
created = "2026-12-24"
status = "superseded"
+++

The tool-output rendering layer strips LLM special tokens like the GLM end-of-text token (13 chars: < | e n d o f t e x t | >) from read_files output, test panic output, and possibly agent-authored file_edit payloads. Symptom seen (plan 7383a4d8, 2026-12-24): patch.rs test apply_endpoints_preserves_sampling_and_stop_parameters — reads showed the ModelSpec stop literal as an empty string, a reconstructed old_string failed to match, a line-range edit wrote a wrong value, and the test panic rendered both sides as [""] while they differed. Diagnosis: dump codepoints with PowerShell ($line.ToCharArray() | ForEach-Object { "U+{0:X4}" -f [int]$_ }) — non-ASCII scan alone finds nothing because the token is pure ASCII. Fix pattern: construct the token from explicit codepoints in PowerShell ([char]0x3C,[char]0x7C,...) and do a targeted string Replace + [System.IO.File]::WriteAllText (UTF8 no BOM, preserves LF); never type the literal token in a tool call. General rule: when a literal file_edit fails with "not found" despite an apparently-identical read, suspect rendering-layer token stripping — verify with a codepoint dump instead of re-reading.
