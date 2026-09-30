+++
title = "CI workflow guards — directory-wide pin coverage + exact contents:read (backlog 627ee5d9)"
created = "2027-01-11"
+++

CI workflow guards (backlog 627ee5d9, plan 07ff4ea6, branch wt/mnemo, commits caaddf6+c463e48, PLAN.md note c6e6f8b) — what tests/integration/ci_workflow.rs now enforces:

- workflows_pin_actions_to_full_shas ENUMERATES .github/workflows itself (read_dir, *.yml/*.yaml case-insensitive, sorted, empty dir fails loudly — never a vacuous pass) instead of a hardcoded file list. INVARIANT for future work: a NEW workflow file is automatically under the SHA-pin guard — no test edit needed; every `uses:` must be a full 40-hex commit SHA with a `# vN` comment, the sole exception ALLOWED_UNPINNED = ["dtolnay/rust-toolchain@stable"] (selects toolchain by ref; the repo-level "require SHA pinning" setting stays off because of it).
- build_workflow_scopes_token_permissions asserts the workflow-level `permissions:` `contents:` value is EXACTLY `read` (quotes tolerated; `write`/`write-all` red), build.yml-only by design; the release job's per-job `contents: write` override is the only sanctioned widening.
- Red-proof convention (both items so far): doctored copies under gitignored .coding/tmp/redcheck + temporary uncommitted concat!/root pointers in the guard — pre-fix green on doctored (hole demonstrated), post-fix red on doctored, green on the real tree; evidence recorded in the commit messages; scratch tree deleted after.
- Related: memory 0caccd28 (the dtolnay exception); PLAN.md:42-48 posture paragraph; next guard item 6e071b07 (Cargo.lock vendored patches) lands in the same file.
