+++
title = "DeepSeek echo/loop hardening: fold volatile tail for DeepSeek + generalize R10 repetition guard"
created = "2027-01-07"
status = "superseded"
+++

Symptom: deepseek-v4-flash turns degenerate into an unbounded repetition loop: (a) the trailing volatile-tail + CONTEXT_FOOTER system messages (placed after the user message for prompt-cache reasons) trigger the model's context-echo degeneration — it echoes the recalled-memories block, fabricates sections, and loops; (b) the R10 repetition guard (detect_repetition) only fires when the accumulated tail is exactly 200-byte-periodic, so the 71-byte-period exit-note loop (366 repetitions, 6,194 tokens, 64.5 s, ~1 MB) could never be detected and ran until the user manually cancelled. · regression test: detect_repetition_fires_on_exit_note_loop_with_74_byte_period

Full record for plan a0eab9ca (see .coding/plans/a0eab9ca.md for the plan file).

regression test: detect_repetition_fires_on_exit_note_loop_with_74_byte_period · path .coding/plans/a0eab9ca.md · branch wt/agenticcoding @ 15fd691 (unmerged — exists only on this branch)
