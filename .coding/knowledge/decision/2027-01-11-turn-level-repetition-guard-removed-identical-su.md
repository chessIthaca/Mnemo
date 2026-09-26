+++
title = "turn-level repetition guard removed — identical successful re-emission must not abort"
created = "2027-01-11"
+++

DECISION (2027-01-24, user choice during plan b9b12840): the turn-level repetition guard (3 byte-identical consecutive assistant responses) was REMOVED rather than weakened. Rationale: after the 7f72d3d7 interaction-aware fix, error-following responses reset the ring, so the guard's only reachable trip path was identical responses following SUCCESSFUL tool batches — which aborted legitimate re-reads (the 12:41 incident: the model re-emitted the same two read_files calls 3×, all succeeded, the guard killed the turn). The user judged identical successful re-emission wasteful but not abort-worthy. Remaining loop defenses: MAX_RETRIES owns same-error-across-three-error-interactions (a same-time failing batch = one interaction); the in-stream R10 guard (detect_repetition, src/provider/stream.rs) owns within-response loops. Landed at commit 29ee0b0 on wt/macos-fix; regression test identical_responses_after_successful_batches_do_not_abort; docs/FEATURES.md:54 carries the removal note.
