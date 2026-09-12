+++
title = "DeepSeek echo/loop hardening: fold volatile tail for DeepSeek + generalize R10 repetition guard — FIXED, MERGED into main (3a2accd)"
supersedes = "a0eab9ca"
created = "2027-01-07"
+++

MERGED into main at 3a2accd (3a2accde38b70ee404ba3cdd1e75f9f0492fb6a4) on 2027-01-11 (merge_to_main skill), branch wt/agenticcoding deleted (pre-merge tip 951ded0). FIXED (plan a0eab9ca, review PASS round 3): (a) DeepSeek-vendor requests fold the volatile tail into the leading system message and omit the CONTEXT_FOOTER (ProviderPolicy::fold_volatile_tail — the trailing system blocks that triggered the context echo are no longer sent); (b) the R10 repetition guard (detect_repetition, src/provider/stream.rs) detects loops of ANY byte period ≤ 200 over the last 600 tail bytes (was exactly-200 only — the 74-byte exit-note period could never fire). Regression tests: detect_repetition_fires_on_exit_note_loop_with_74_byte_period + deepseek_vendor_folds_volatile_tail_no_trailing_system_messages (both verified failing pre-fix). Full suite 2256 + 16 passed, 0 failed, zero warnings; merged tree verified with npm run build + src-tauri cargo build.
