+++
title = "land Dependabot PRs as one locally tested batch; mnemo2 tests assert tokens via sentinels on jsdom 31"
supersedes = "2026-11-11-land-dependabot-prs-as-one-locally-tested-batch"
created = "2026-11-11"
evidence = "PR #116 merged 44f92686. Plan a659d5ad, worker jsdom31Tests01, tip 95fb9c5b: mnemo2 vitest 117 files / 1254 tests pass, svelte-check 0/0. Standalone jsdom 30.1.3 vs 31.0.0 runs showed the var() and blockification differences."
+++

PR CI on chessIthaca/Mnemo runs only CodeQL (build.yml runs only on tags or manually), so a green Dependabot PR says nothing about the build or tests. The Cargo PRs share Cargo.lock and the npm PRs share package-lock.json, so once one merges the rest conflict.

What worked (PR #116, 2026-11-11): make a worktree off origin/main, `git merge --no-ff` each dependabot branch, then run npm ci, both builds, both vitest suites, `cargo test --lib`, `cargo test -p mnemo-app --features mnemo2 --lib`, and `cargo check` of both bins. Land it as one PR merged with a merge commit; GitHub then marks every included Dependabot PR as merged. The github/codeql-action init and analyze bumps must land together, because CodeQL fails when the two run different versions.

jsdom (mnemo2 devDependency, on jsdom 31 since plan a659d5ad, merge 9345bf87 on wt/mnemo):
(1) Keep jsdom at the ROOT of package-lock.json. vitest is hoisted to the root and cannot import a copy nested under mnemo2/node_modules; every test file then fails with ERR_MODULE_NOT_FOUND. Dependabot and a first `npm install` both nest it. Fix: `npm uninstall jsdom -w mnemo2 --package-lock-only`, then `npm install -D jsdom@^N -w mnemo2 --package-lock-only`.
(2) jsdom 31 resolves var(--token) in getComputedStyle, and the tests never load app.css. A test that proves a component uses a token therefore calls defineTokenSentinels() from mnemo2/src/test/tokens.ts, which sets distinctive values on :root (--font-ui sentinel-ui, --text-meta 17px, --text-body 14px, --muted rgb(1, 2, 3), --danger rgb(4, 5, 6)), and expects tokenSentinels[name]. Add a token to that map when a new test needs it.
(3) jsdom 31 blockifies flex items, as browsers do: an inline-flex child of a flex container computes display "flex", and inline-block computes "block".
