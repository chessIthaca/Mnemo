+++
title = "Bug-triggered features use implementation plans, not bug_fixing"
created = "2027-01-07"
+++

User correction (2027-01-11, plan c87093c1 "All-language coverage: finish-gate disk check + code-graph indexing"): a defect-triggered change whose fix adds capabilities, new dependencies, or spans multiple modules is a FEATURE, not a bug fix — file it as kind=implementation with the defect documented as motivation in goal/context. kind=bug_fixing (locked reproduce→root-cause→fix→verify skeleton, BUG: auto-capture) is for contained defect fixes only. The all-language coverage run (gate disk-check broadening + 10 grammars + HTML script indexing) ran as bug_fixing and was the wrong shape: a mega "Minimal fix" step with no sub-step progress tracking. Two backlog items queued 2027-01-11: sub-step progress tracking for bug_fixing plans, and create_plan/prompt steering for feature-scale fixes. Until the app enforces the steering, apply the rule manually when choosing plan kinds.
