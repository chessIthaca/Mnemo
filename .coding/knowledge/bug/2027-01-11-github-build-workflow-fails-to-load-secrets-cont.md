+++
title = "GitHub build workflow fails to load — secrets context in step if: conditionals (L111/121/141)"
created = "2027-01-11"
+++

Symptom (user report, this session): GitHub Actions build workflow fails to load — "Unrecognized named-value: 'secrets'" at .github/workflows/build.yml L111, L121, L141 (the three Apple-secrets step `if:` conditionals). Surfaced after the L52 YAML-syntax fix (56132d3) let the file parse far enough for expression validation; the workflow had never loaded successfully.

Root cause: the `secrets` context is NOT available in step `if:` conditionals — the official docs context-availability table lists jobs.<job_id>.steps.if as github, needs, strategy, matrix, job, runner, env, vars, steps, inputs (no secrets), while jobs.<job_id>.env DOES allow secrets (the file's step-level env blocks were legal — only the if: lines errored).

Fix (commit b5d8ef8 on wt/mnemo, plan 35434040): job-level env gate mirrors on the macos job — APPLE_SIGNING_GROUP_SET / APPLE_NOTARY_GROUP_SET / APPLE_GROUPS_INCONSISTENT computed from secrets — and the three ifs switched to ${{ env.<GATE> == 'true' }}. Gate names deliberately differ from the tauri-facing APPLE_* names (set-but-empty avoidance preserved; the GITHUB_ENV export pattern in the gated steps untouched).

Regression test: build_workflow_step_ifs_never_reference_secrets (tests/integration/ci_workflow.rs, registered via `mod ci_workflow;` in tests/integration/main.rs) — asserts no `if:` line in build.yml references `secrets.`; fail-proof demonstrated (the assertion against the pre-fix content flags exactly the 3 lines), passes on the fixed tree.

Convention: never reference `secrets.` in an `if:` conditional (job or step level) — compute the gate as job-level `env` and check `env.*` in the step.
