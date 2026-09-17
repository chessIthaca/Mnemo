+++
title = "reviewer failure recovery — narrow the retry task + incremental report writes"
created = "2027-01-11"
+++

When a reviewer subagent ends WITHOUT writing a report, the protocol is ask_user (retry on another model / abandon) — never a blind respawn. Verified path (2027-01-16, plan 8a95a233 on wt/mnemo, commit f2d03d3): the user chose a SAME-MODEL retry, and it succeeded once the task was narrowed and the report written incrementally.

What failed: the first round-5 reviewer was asked to re-read four prior review reports plus a 19-file uncommitted diff and write one report at the end — it ended silently with no report file (the parent's latch reported "failed to produce a review report").

What worked on the retry: (1) scope to the DELTA since the last review (three concrete items: one new test, two doc sentences) and state explicitly that everything the previous round verified is the base — do not re-derive it; (2) cap reads to named line ranges instead of whole files/reports; (3) instruct it to call write_review_report FIRST with the verdict line + a two-line summary, then append evidence in one or two further calls — the call appends, and round 3's report had landed in four chunks the same way, so a partial run still leaves a parseable verdict. Result: round 5 = PASS, no findings.

Also durable: the feature's SPEC record `.coding/knowledge/spec/2027-01-11-skill-reload-skill-create-over-a-reloadable-skil.md` has NO memory row until the next project-open reindex (content-hash drift), so memory_amend cannot target it in-session; the sanctioned `.coding/knowledge/**` fallback (text staged with file_write into .coding/tmp/*.txt, spliced with .NET UTF-8 APIs, anchors verified to match exactly once, LF preserved) is the repair path meanwhile — round 4 accepted that justification.

Pointers: plan 8a95a233 (SPEC: skill_reload + skill_create over a reloadable SkillLibrary — dir gate judges the deepest existing CANONICAL ancestor, so a link above the sandbox root cannot refuse a valid dir and an out-of-root mkdir with a missing parent chain is refused by containment); reports .coding/reviews/2026-09-17-skill-reload-and-create-review.md + round2..round5.
