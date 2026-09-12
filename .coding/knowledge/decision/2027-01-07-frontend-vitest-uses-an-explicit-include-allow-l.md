+++
title = "Frontend vitest uses an explicit include allow-list — register new test files in vitest.config.ts or they silently don't run"
created = "2027-01-07"
+++

frontend/vitest.config.ts pins an explicit `include` allow-list instead of vitest's default discovery glob — a new *.test.ts(x) file silently does not run unless registered. The curation is intentional and KEPT; since 2027-01-07 the trap is guarded: `src/lib/vitestInclude.test.ts` (registered in the list itself) fails loudly listing any test file under src/ not matched by `test.include`, so forgetting the registration step breaks the suite instead of silently skipping the file.
