+++
title = "Fix build workflow: secrets context in step if: conditionals (env-mirror gates) — MERGED into main (3d058b9)"
supersedes = "35434040"
created = "2027-01-11"
+++

MERGED into main at 3d058b9 (3d058b99ad037f567fd35b1e5b0aea30477565da) on 2026-09-20 via the merge_to_main skill; branch wt/mnemo deleted (pre-merge tip c57d192) - supersedes this record's earlier "branch wt/mnemo @ 3d7ba85 (unmerged - exists only on this branch)" marker. Symptom: GitHub Actions build workflow fails to load - "Unrecognized named-value: 'secrets'" at .github/workflows/build.yml L111/L121/L141. Root cause: the secrets context is not available in jobs.<job_id>.steps.if (docs context-availability table); jobs.<job_id>.env allows it. Fix: job-level env gate mirrors + env-based ifs. Regression test: build_workflow_step_ifs_never_reference_secrets. Full record: .coding/knowledge/bug/2027-01-11-github-build-workflow-fails-to-load-secrets-cont.md (superseded with its own merged-status successor); plan: .coding/plans/35434040.md.
