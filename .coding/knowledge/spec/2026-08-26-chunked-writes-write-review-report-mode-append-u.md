+++
title = "chunked writes — write_review_report mode:append + update_plan append=true"
created = "2026-08-26"
status = "superseded"
+++

Long plan and review-report bodies are written in CHUNKS (backlog 0085ccc0, plan 96e2601f, commit d095881 on wt/toolcard-chips-salvage-review, review .coding/reviews/2026-12-chunked-writes-verify-review.md PASS): (1) write_review_report (src/tool/agent/write_review_report.rs) takes mode:"overwrite"|"append" — append adds to the existing report (creating it if absent); the verdict contract is enforced whenever a call CREATES the report (fail closed), an existing verdict-less file is never grown, sandbox guards hold in both modes; protocol = verdict+summary first, finding sections appended per chunk. (2) update_plan (src/workflow/mod.rs Workflow::update_plan, signature now has `append: bool` before regression_test) with append=true ADDS steps after the remaining ones (no resending) and extends context as {old}\n\n{new}; bug_fixing skeleton lock is total (steps replace AND append refused). create_plan stays atomic; chunking = create with first steps → update_plan(append=true) per chunk. Schemas + PLAN.md teach the protocol.
