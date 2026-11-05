+++
title = "the plan board notes thin steps as advisory \"Spec notes\" and never refuses them"
created = "2026-11-03"
+++

Board plan_create and plan_update add_steps append "Spec notes (advisory; nothing was refused):" with one line per thin step `- step N: <gaps>`. Gaps come from spec_gaps in src/app/tool/workflow/board.rs, in order: under 120 words; no signatures (none of `fn `, `->`, `=>`, `name(args):`); no tests named (no "test"); no done checklist (no "done"). Steps containing "no-code" are exempt. Gaps never return an error. The steps/add_steps schema descriptions ask for a full implementation spec.
