+++
title = "web_fetch SSRF gate is resolve-validate-PIN per hop, fail-closed"
created = "2027-01-11"
+++

DECISION: web_fetch's SSRF gate is resolve-validate-PIN per hop, fail-closed (plan aa3c8dae, commit e786556 on wt/agenticcoding). The gate resolves the hostname ONCE, rejects on ANY internal address, and pins the validated address to the connection (reqwest ClientBuilder::resolve — a DNS-only override: SNI/TLS/Host stay on the hostname), so a rebinding DNS server cannot swap the address between check and connect. DNS-resolution failure is fail-closed (was fail-open). IP literals need no pin (Ok(None)); each redirect hop re-gates and re-pins with a fresh client. Code: src/tool/agent/web_fetch.rs (resolve_and_gate_host + build_fetch_client + gate_and_pin_url); regression test connection_uses_the_gate_validated_address. Closure notes amended into .coding/knowledge/spec/2026-08-31-shared-repetition-guard-consolidation-dedup-ssrf.md and .coding/knowledge/bug/2027-01-05-web-fetch-redirect-hops-bypassed-the-ssrf-blockl.md.
