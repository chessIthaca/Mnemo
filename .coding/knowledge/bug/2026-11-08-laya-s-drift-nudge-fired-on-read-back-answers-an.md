+++
title = "Laya's drift nudge fired on read-back answers and fix prompts, which add no work"
supersedes = "2026-11-08-laya-message-nudges-fire-on-plain-go-ahead-repli"
created = "2026-11-08"
evidence = "Both regression tests failed on the old production code and with the gate forced open, and pass with the fix (run by the lead on 2026-11-08); the 13-nudge count comes from joining .coding/laya/dispatch_routing.jsonl to .coding/mnemo2/team.json."
+++

Symptom: a lead's reply to a read-back ("proceed as specified", "your proposals stand") and a team_route_fix prompt drew "this got harder (Laya reads the message as involved, 1.00); consider team_resume with tier medium". Each misfire also spent the worker's one nudge, so a later genuine strike or review nudge could not fire, and it added a nudge_ignored row to the routing log.

Root cause: the drift check ran on every delivered message of 20+ words and asked "how hard is this text as a task?" about text that is not a task description. These messages read like implementation specs, and the kNN overlay votes on them against logged step specs (confidences of exactly 0.80 and 1.00 are 5-neighbour vote shares, not Laya's probabilities).

Evidence: joining the routing log to the team ledger, 13 of 19 drift nudges could be matched to their message: 10 read-back answers, 3 fix prompts, none adding scope. In 4 of the 10, Mnemo2's plan-changed notices arrived before the answer, because the lead records answers as plan decisions before replying, so "first inbox entry" is the wrong test.

Fix: message_can_show_drift (src-tauri/src/mnemo2/team/nudge.rs), checked in message_inner_with_kind (team/control.rs) on the record read before the message joins the inbox. MessageSender::Mnemo text is never read, nor is a read-back worker's first message from anyone but Mnemo2. Any other lead or user message is still classified. These messages are skipped outright rather than classifying only their new requirements, because nothing in free text separates new requirements from restated ones, while the sender and the worker record say exactly what a message is.

Accepted limits: a read-back answer that does add scope draws no nudge, since the lead wrote that scope and picks the tier; a second reply to a read-back worker is classified as an ordinary message; a long continuation message can still misfire on its text.

Regression tests: read_back_answer_draws_no_drift_nudge and route_fix_prompt_draws_no_drift_nudge in src-tauri/src/mnemo2/team/tests.rs. Requirement: "Drift nudges read only new guidance" in .coding/openspec/specs/dispatch-routing/spec.md; design section 4.6 of .coding/design/mnemo2-laya-routing.md.
