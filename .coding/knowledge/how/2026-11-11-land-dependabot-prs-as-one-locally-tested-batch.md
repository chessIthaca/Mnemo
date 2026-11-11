+++
title = "land Dependabot PRs as one locally tested batch; jsdom 31 breaks 6 mnemo2 style tests"
created = "2026-11-11"
evidence = "PR #116 merged 44f92686; local runs on the batch tree 1cfef97d; mnemo2 vitest with jsdom 31: 6 failed / 1198 passed, with jsdom 30: 1204 passed"
status = "superseded"
+++

PR CI on chessIthaca/Mnemo runs only CodeQL (build.yml is tag/manual only), so a green Dependabot PR proves nothing about build or tests. The Cargo PRs share Cargo.lock and the npm PRs share package-lock.json, so after one merges the rest conflict. What worked (PR #116, 2026-11-11): worktree off origin/main, `git merge --no-ff` each dependabot branch, run npm ci + both builds + both vitest suites + `cargo test --lib` + `cargo test -p mnemo-app --features mnemo2 --lib` + `cargo check` of both bins, then one PR merged with a merge commit. GitHub then marks every included Dependabot PR as merged. github/codeql-action init and analyze bumps must land together, because CodeQL fails when the two run different versions. jsdom 31 (PR #110, still open): (1) Dependabot's lock nests jsdom under mnemo2/node_modules, where the root vitest cannot import it. Fix: `npm uninstall jsdom -w mnemo2 --package-lock-only`, then `npm install -D jsdom@^31 -w mnemo2 --package-lock-only`. (2) jsdom 31's getComputedStyle resolves var(--token), so 6 tests that assert the raw token fail: LeadChat.test.ts:645 (fontSize var(--text-body)), WorkerRow.test.ts:42 (3 tests: --font-ui, --text-meta, --muted), and 2 in WorkerThread.test.ts:94 (color var(--danger)). Rework those tests before landing jsdom 31.
