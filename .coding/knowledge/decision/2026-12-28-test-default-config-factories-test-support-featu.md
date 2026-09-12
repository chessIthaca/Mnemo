+++
title = "test_default() config factories + test-support feature — MERGED into main (e11b76b)"
supersedes = "2026-12-24-test-default-config-factories-test-support-featu"
created = "2026-12-28"
+++

MERGED into main at e11b76b2a07b1cb9a9dbe801e5e57c24e165400e on 2026-09-04 (merge_to_main skill), branch wt/agenticcoding deleted (pre-merge tip 49e8110). Decision unchanged: test configs are built via test_default() factories (Endpoint/ModelSpec/OpenAiClientConfig), never raw struct literals; factories gated #[cfg(any(test, feature = "test-support"))]; the test-support cargo feature is enabled by src-tauri's dev-dependency, never compiled into release builds. Shipped in commit 7c7598b. Full detail: .coding/knowledge/decision/2026-12-24-test-default-config-factories-test-support-featu.md. Plan 7383a4d8, review PASS (.coding/reviews/2026-12-24-test-factory-helpers-review.md).
