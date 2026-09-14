+++
title = "DeepSeek: stop folding the volatile tail into the leading system message (cache reset per check-off) — MERGED into main (d53546e)"
supersedes = "506b85e2"
created = "2027-01-11"
+++

MERGED into main at d53546e (d53546ee7e2ab45ffe10fcbb2f106410ed062e82) on 2027-01-11 via the merge_to_main skill; branch wt/mnemo deleted (pre-merge tip 7051da9) — supersedes this record's earlier "branch wt/mnemo @ d913638 (unmerged)" marker. Symptom: on DeepSeek-vendor models every plan-item check-off reset the provider's prompt-prefix cache. Root cause: the volatile tail (workflow state, plan rider, recall, footer) was folded into the leading system message, so any check-off mutated messages[0]. Fix: for fold vendors (DeepSeek-vendor + Local/Ollama) messages[0] stays the byte-stable cache prefix and the volatile tail rides as trailing user-role messages with no trailing system block. Regression test: deepseek_head_stays_byte_stable_across_plan_progress_bumps (src/agent/tests.rs). Full record: .coding/knowledge/bug/506b85e2.md.
