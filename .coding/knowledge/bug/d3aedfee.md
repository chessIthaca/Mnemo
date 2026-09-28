+++
title = "handle_bad_json: batch isolation — run well-formed siblings, targeted correction one-liner"
created = "2027-01-11"
+++

Symptom: One malformed tool call poisons its well-formed siblings: when ANY call in a batch arrives with empty/malformed JSON arguments, the whole batch is failed with "arguments malformed or truncated — not run" and retried, so valid calls are never dispatched (their work is wasted for a round-trip and the visible failure count doubles). Observed live in the 2027-01 failure wave; the empty-args root cause was fixed separately (commit 5b7f99b) but a malformed call can still occur and the whole-batch failure is the wrong containment. Secondary defect in the same path: the first-failure retry guidance is a fixed ~85-word generic preamble (bad_json_retry_message, src/agent/turn.rs:4583-4596) that was empirically ineffective in the 2027-01 session — it should be a per-call one-liner naming the tool and its required fields. · regression test: bad_json_batch_isolation_runs_valid_siblings

Full record for plan d3aedfee (see .coding/plans/d3aedfee.md for the plan file).

regression test: bad_json_batch_isolation_runs_valid_siblings · path .coding/plans/d3aedfee.md · branch wt/mnemo @ f8aa63d (unmerged — exists only on this branch)
