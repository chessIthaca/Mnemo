+++
title = "Close web_fetch DNS-rebinding TOCTOU (pin the validated IP)"
created = "2027-01-11"
+++

Symptom: web_fetch's SSRF gate validates the resolved IP (rejecting private ranges, link-local 169.254.169.254 — commit 8957b12), but reqwest re-resolves the hostname at connect time: a rebinding DNS server can return a public IP for the gate check, then 127.0.0.1/169.254.169.254 for the actual connection — bypassing the gate (time-of-check to time-of-use). The gap is documented as a known limitation in src/tool/agent/web_fetch.rs (lines 171-177): "the hostname is resolved once per gate check, then again by reqwest when it connects". Root cause: resolve-then-connect — the gate runs on one DNS answer, the connection uses another. · regression test: connection_uses_the_gate_validated_address

Full record for plan aa3c8dae (see .coding/plans/aa3c8dae.md for the plan file).

regression test: connection_uses_the_gate_validated_address · path .coding/plans/aa3c8dae.md · branch wt/agenticcoding @ e786556 (unmerged — exists only on this branch)
