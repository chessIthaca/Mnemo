+++
title = "reviewer rounds — pre-flight self-check, commit verified code before a re-review"
created = "2027-01-11"
+++

Two conventions that pair with the harness-rendered reviewer preamble (backlog 85313a7e, plan f4636852). The app now renders the reviewer's contract itself (verdict lines, constitution checks, `.coding/**` rule) and, from round 2, the delta scope — so the dispatcher's job shrank.

1. PRE-FLIGHT SELF-CHECK — before spawning ANY reviewer, run agent.md's "Review expectations" checklist against the diff yourself and fix what it finds: documentation sync (README.md / PLAN.md / module docs), no `cfg(windows)`-only additions outside the sanctioned WebView2 `game_*` gate, file-tools-first (no shell file mutation where file_edit/file_write works), and for a bug fix a regression test genuinely red before the fix. Minutes of own work beats a whole review round — plan febcd6f5 spent three rounds (~33 min) on a 1-3 file fix, and its round 3 found nothing.

2. COMMIT VERIFIED CODE AT STEP BOUNDARIES on the working branch (`wt/*`), not only at the end. The delta scope is `git diff <base>` where `base` = the revision the previous round stamped at dispatch; it narrows ONLY for work committed since. Uncommitted carry-over re-appears in the next round's diff, so the preamble tells the reviewer to note it in one line rather than re-review. The closing sequence still commits last, with the report, so the final commit is never skipped.

Practicalities when spawning a reviewer: keep `task` to scope + acceptance criteria + risk focus — the preamble already carries the boilerplate, the reviewer can read `.coding/plans/<id>.md` itself, and hand-writing a preamble or a scope just bloats the prompt (the app renders it in the spawn path, `src/tool/agent/spawn_agent.rs`). Round stamps land in the plan file's `## Reviews` section, so a resumed session still knows the base revision; a reviewer that failed without a report is NOT stamped — the next round deliberately re-reviews from the older base.
