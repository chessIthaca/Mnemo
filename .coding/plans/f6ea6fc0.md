# Plan: Vitest include allow-list guard — unregistered test files fail loudly

## Goal
A meta-test guard that makes the vitest include allow-list trap LOUD: every *.test.{ts,tsx} file under frontend/src must be registered in vitest.config.ts's test.include — the guard fails with an actionable message listing any unregistered file, so a green suite again proves the whole suite ran. The allow-list design itself stays (intentional curation); the guard doubles as documentation. Regression proof: with a scratch unregistered file present the guard goes red listing it; removed, the suite is green with the guard itself running.

## Kind
bug_fixing

## Context
Explored: frontend/vitest.config.ts (environment "node", explicit 65-entry include list — one glob "src/components/settings/**/*.test.ts", the rest literal paths; src/lib/tauri.test.ts already registered at :81 = the shipped proof the registration step works). frontend/tsconfig.json: allowImportingTsExtensions + moduleResolution "bundler" + noEmit — importing "../../vitest.config.ts" as a module type-checks under tsc (the build gate is "tsc && vite build"). package.json: test = "vitest run". Repo pattern for source-reading tests: BacklogView.test.ts uses ?raw imports; ipc-contract.test.ts reads golden fixtures. The folklore memories (HOW/DECISION: register new test files in the include list) exist but are not a guard — this plan adds the guard (task's preferred option (a): keep the allow-list, make the trap loud).

## Steps
- [x] 1. **Reproduce with failing regression test** — Write a regression test that reproduces the defect (constitution: every defect gets a regression test that fails without the fix and passes with it). Run it and confirm it FAILS.
- [x] 2. **Document root cause** — Investigate and document the root cause. memory_write a BUG: record (symptom → root cause → fix + regression test name, ≤600 chars).
- [x] 3. **Minimal fix** — Apply the minimal fix that makes the regression test pass. Do not refactor unrelated code.
- [x] 4. **Verify** — Run the regression test + the full test suite (cargo test unpiped, warning-free). Record the regression test name via update_plan (regression_test field) — finish is blocked without it.

## Bug
New frontend test files silently never run: frontend/vitest.config.ts pins an explicit `include` allow-list instead of vitest's default discovery glob, so an unregistered *.test.ts(x) is invisible to the runner — `npm test` reports green while the file's tests are never executed (live case 2027-01-07: src/lib/tauri.test.ts was written, the suite total stayed flat at 71 files / 1007 tests before AND after, and the gap was caught only by noticing the file missing from the run list).

## Regression test
everyTestFileUnderSrcIsRegistered
