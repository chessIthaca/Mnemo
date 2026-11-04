+++
title = "Manual model picks must mark route state before pinning the provider"
created = "2026-11-03"
+++

Root cause H1: `set_model` installed the human provider before calling `record_manual_model_choice`, leaving a window where an in-flight first classification still saw Pending and could overwrite the human pin before IPC marked Off{manual}. The ordering contract is now manual route-state transition FIRST (`record_manual_model_choice`, returning `(from, nudged)` from the same critical section that sets Off and bumps epoch), THEN the human provider pin, in both per-agent and global IPC branches. `mark_manual_before_pin` makes that order injectable/testable. Regression coverage: `manual_mark_before_human_pin_wins` and `set_model_marks_manual_before_pinning`. Fixed in commit `a4345703164f176bc0ca113bd255d797e1db0ce9`.
