+++
title = "Tool-arg decode errors: echo received bytes for one-shot recovery — MERGED into main (branch wt/stream-plain, 2026-10-18)"
supersedes = "81a1b70d"
created = "2027-01-11"
+++

Symptom: Truncated/malformed tool-call argument payloads (model emission decay — the documented 2026-12-20 root cause) reject with bare messages that never show what the executor actually received — "ops is required", "its arguments are not valid JSON (…). Please retry with a correctly-formed JSON object" — so the model blind-retries the identical payload and fails identically (observed 2027-01-17 on wt/mnemo, jobs 3a.6/3a.8: repeated "ops is required" rejections and two identical timeout corrections on ~2.5-4 KB multi_edit/file_edit payloads; each blind retry costs a full round-trip). · regression test: arg_shape_rejections_carry_received_bytes_context

Full record for plan 81a1b70d (see .coding/plans/81a1b70d.md for the plan file).

regression test: arg_shape_rejections_carry_received_bytes_context · path .coding/plans/81a1b70d.md · landed at 58effb0 on branch wt/stream-plain (work tip 32b6f99, 2026-10-18) — a PR against main is opened from this branch, awaiting human review; the landing becomes part of main when the human merges the PR (the merge commit is then one `git log main --merges` away).
