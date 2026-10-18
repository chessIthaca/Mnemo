+++
title = "Streamed answer stays plain until the next LLM request — markdown at turn end + late-delta hardening (backlog 076f601e) — MERGED into main (branch wt/stream-plain, 2026-10-18)"
supersedes = "3d40e529"
created = "2027-01-11"
+++

Symptom: The assistant's answer renders as plain text (no markdown) while it streams AND stays plain after the turn completes — headings/lists/code fences apply only once the next request is sent (backlog 076f601e, user-reported live late Jan 2027: ask for a markdown answer; it streams plain; the turn finishes — still plain; send the next prompt — now it is formatted). · regression test: regression076f601eLateDeltaSettlesIntoTranscript

Full record for plan 3d40e529 (see .coding/plans/3d40e529.md for the plan file).

regression test: regression076f601eLateDeltaSettlesIntoTranscript · path .coding/plans/3d40e529.md · landed at 0d16ab7 on branch wt/stream-plain (work tip 32b6f99, 2026-10-18) — a PR against main is opened from this branch, awaiting human review; the landing becomes part of main when the human merges the PR (the merge commit is then one `git log main --merges` away).
