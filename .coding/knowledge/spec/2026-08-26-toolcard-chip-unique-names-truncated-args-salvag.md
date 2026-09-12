+++
title = "ToolCard chip unique names + truncated-args salvage (exclusions enforced pre-parse)"
created = "2026-08-26"
+++

ToolCard header chips (frontend/src/lib/toolCardPaths.ts) now guarantee: (1) each file name appears exactly once — dedupe by normalized path + disambiguateDuplicateTexts qualifies colliding basenames with parent segments, bounded by maxDepth with a raw-path fallback so same-segment-sequence pairs ("/src/main.rs" vs "src/main.rs", "src//util.ts" vs "src/util.ts") terminate instead of hanging render; (2) truncated/streamed tool args salvage the first "path"/"file" literal via salvagePathLiteral, but the label-only tool exclusions (shell, spawn_agent, skill_start, git, git_read, search, search_read) are enforced BEFORE JSON.parse so excluded tools never gain a link chip. Regression tests: frontend/src/lib/toolCardPaths.test.ts (51 tests). Fix commits: 73a6c04 (plan fix, merged to main via de0f0bb) + 8112ecf on wt/toolcard-chips-salvage-review (round-2 review findings: hoisted exclusion, bounded disambiguate loop), review .coding/reviews/2026-12-toolcard-chips-salvage-verify-review.md PASS.
