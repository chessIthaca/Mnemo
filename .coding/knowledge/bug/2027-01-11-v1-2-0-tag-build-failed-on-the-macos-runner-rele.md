+++
title = "v1.2.0 tag build failed on the macOS runner — release shipped with no assets (fix 4b0764a)"
created = "2027-01-11"
+++

Symptom: the v1.2.0 tag build's macOS runner went red — state_override_survives_429_fallback exceeded the 10 s shutdown_agent window (src/runtime/agent.rs:2922) — the release job was skipped, and the published v1.2.0 GitHub release has 0 assets (releases are immutable, so it can never gain installers). Fix: window widened to 30 s (commit 4b0764a, merged 71e214b); the failed jobs were re-run. Consequence for cuting 1.3.0: it must be the first release with installers since v0.1.1 — verify the assets after the tag build. Pointers: .coding/analysis/2026-09-29-github-security-defects.md (follow-up table), CHANGELOG.md [1.2.0] entry.
