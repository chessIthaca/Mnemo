## Verdict: FINDINGS (0 high, 2 low)

The DNS-rebinding TOCTOU fix is correct, complete, and strictly strengthens the SSRF gate: the gate's validated address is pinned to the connection per hop (reqwest `ClientBuilder::resolve` — documented DNS-only override, so SNI/TLS/Host stay on the hostname), resolution now fails closed, and the regression test genuinely proves the connection cannot deviate from the gate's answer. Both findings are memory/bookkeeping hygiene (stale knowledge records describing the now-closed limitation as open; the BUG: record not yet in memory) — no code changes required.

## Scope reviewed

- `git diff HEAD`: `src/tool/agent/web_fetch.rs` (393 changed lines, +255/−140) and `.coding/backlog.jsonl` (item a09199b1 pending→in_flight with plan_id — expected bookkeeping, no other items touched).
- Untracked: `.coding/plans/aa3c8dae.md` (read in full — root cause documented in the Bug section, fix design matches the code, all 4 steps checked).
- Full read of the changed file post-fix (997 lines) — the changed code reviewed in context (`extract_host`, `is_internal_ip`, the redirect loop body, `RedirectStubServer`, all 21 tests).
- reqwest 0.12 confirmed in `Cargo.toml:79` (rustls-tls, no native-tls); `ClientBuilder::resolve` semantics confirmed against docs.rs.

## Fix correctness — verified point by point

**Pin semantics (the core mechanism).**
- `resolve_and_gate_host` (web_fetch.rs:432) resolves ONCE via `ToSocketAddrs` with the URL's effective port, rejects on ANY internal address (old behavior preserved), and pins the FIRST public address as `(host, addr)`.
- `build_fetch_client` (:410) applies `ClientBuilder::resolve(&host, addr)`. reqwest docs (verified on docs.rs): "Override DNS resolution for specific domains to a particular IP address… Ports in the URL itself will always be used instead of the port in the overridden addr." Only DNS is overridden — the URI authority is untouched, so the Host header, TLS SNI, and certificate hostname validation all keep using the original hostname (no manual SNI fiddling, no cert-check weakening). The pin addr's port always equals the URL's effective port (it was resolved from `format!("{host}:{port}")` with `port = url.port_or_known_default()` of that same URL), so the documented URL-port-wins rule can never diverge from the pin.
- **Key match — no silent DNS fallback:** the pin key is `extract_host(url.as_str())` of the very URL then requested — the serialized host (lowercased for special schemes by the url crate). reqwest's override lookup uses the same host string, so the override always hits. The regression test proves this end-to-end for the trickiest case (trailing-dot FQDN `rebind.invalid.`).
- **Cross-host redirects re-pin:** each loop iteration gates `current` (the joined URL) and builds a FRESH client with that hop's pin (:317-318) — hop N's client carries only hop N's host pin; no pin leakage across hops.
- **IP literals need no pin:** bracket-stripped, parsed, validated directly; `Ok(None)` — the URL already targets the literal and reqwest connects to literals without DNS. Correct, and it makes the literal tests DNS-free/deterministic.

**Fail-closed change.** Resolution error → `Err("DNS resolution failed for {host}: {e} (fail-closed SSRF gate)")`; zero addresses → `Err`. This closes the SERVFAIL-to-gate/private-to-connector attack. The availability delta for genuine DNS failures is nil — the fetch failed before too (at connect, with reqwest's DNS error); now it fails at the gate with a clearer, host-naming message. The `spawn_blocking` JoinError arm stays fail-closed (`unwrap_or_else(|_| Err(internal_addr_error()))`), matching the old `.unwrap_or(true)`.

**Per-hop client.** `build_fetch_client` carries every setting the old single client had — 30s timeout, `redirect::Policy::none()`, UA `mnemo-agent/0.1` — plus the pin. ≤6 clients per fetch; pool churn is irrelevant for a research fetch tool. Build failure surfaces the same "failed to build HTTP client" message as before.

**execute reshape.** The pre-flight gate + client build were dropped; the loop gates `start` at hop 0 (`0..=MAX_REDIRECTS`, gate at the top of every iteration). The only `client.get(...)` in the module is inside the loop, after `gate_and_pin_url` (:319-323) — **no ungated request path exists**. The self-containment invariant from the 2027-01-05 fix is preserved and re-documented in `fetch_following_redirects`'s doc comment.

**Redirect parity preserved.** The hop logic (301/302/303/307/308 + Location, join semantics, unjoinable → 3xx as-is, 6-request cap) is untouched by this diff.

## Security assessment

- The gate is strictly stronger: old = predicate with fail-open resolution; new = Err on internal / resolution failure / zero addresses, pin on success. The pin's `Some` arm only ever carries an address that passed `!is_internal_ip` — in production there is no path to pin an internal address (test stubs can, but they exist only under `#[cfg(test)]`).
- No new exposure: SNI/cert validation on the hostname means https pinning cannot bypass cert checks; the Host header stays the hostname.
- Edge cases checked: IPv6 zone-ID literals now fail closed at the gate (previously passed the gate and failed at connect — the 2027-01-05 review's noted residual, now strictly better); `extract_host` returning `None` keeps the pre-existing `Ok(None)` skip (unreachable for successfully-parsed http/https URLs, which always have a host); userinfo/bracket forms still handled by the url-crate-based `extract_host`; `trim_start_matches('[')`/`trim_end_matches(']')` on non-literal hosts is harmless (they never contain brackets).

## Tests

- `connection_uses_the_gate_validated_address` (:900) is a genuine red→green regression: pre-fix, the connection re-resolved `rebind.invalid.` (deterministic NXDOMAIN — RFC 2606/6761-reserved TLD, trailing dot prevents search-suffix rewriting) and failed with a DNS error; post-fix the pinned connection succeeds and returns the body. It also asserts the gate saw the right host/port. The plan correctly reasons that the live attacker-DNS scenario is not reproducible in unit tests and this proves the equivalent mechanism (the connection cannot deviate from the gate's answer).
- `resolve_and_gate_host_validates_literals_and_fails_closed` (:973) covers internal literals (incl. bracketed IPv6 + 169.254.169.254), public literals (v4+v6, no pin), and fail-closed unresolvable.
- The 5 redirect tests + gate test updated coherently; `loopback_exempt_gate` now delegates to the production gate for non-loopback hosts (redirect targets get real validation); `no_redirect_client` deleted with no dangling references (all touched functions are module-private; the green build confirms).
- 21 tests in the module — matches the reported 21/21.
- Minor robustness note (not a finding): the fail-closed case resolves `nonexistent.invalid.` for real — an NXDOMAIN-hijacking middlebox would fail the test loudly. RFC 6761 compliance is near-universal; acceptable.

## Constitution checks

- Doc comments on all new items (`HostPin`, `HostGate`, `internal_addr_error`, `gate_and_pin_url`, `build_fetch_client`, `resolve_and_gate_host`) ✓; no `#[allow]` suppressions ✓; multi-platform neutrality (std `ToSocketAddrs`/`SocketAddr`/`IpAddr` + reqwest `resolve` — no platform-specific code) ✓; file-tools-first (no shell mutation in the diff) ✓; warning-free per the deny(warnings) green build ✓.
- Documentation sync: module doc, `execute`'s SSRF comment, and the gate/loop doc comments updated; the "Known limitation" TOCTOU comment is gone. README.md / PLAN.md / docs/ carry no web_fetch SSRF claims (searched — the only docs/ SSRF mention is browser-context). The tool schema description makes no SSRF claims — accurate as-is.

## Findings

### LOW-1 — Stale knowledge records still describe the closed TOCTOU as an open limitation

- `.coding/knowledge/spec/2026-08-31-shared-repetition-guard-consolidation-dedup-ssrf.md:12` — "TOCTOU (DNS rebinding) documented as known limitation for desktop-only threat model."
- `.coding/knowledge/bug/2027-01-05-web-fetch-redirect-hops-bypassed-the-ssrf-blockl.md:12` — "Known residual: DNS rebinding (TOCTOU) stays documented in-code."

After this fix both statements are false about the code. Per the project's memory hygiene rules (never leave a contradicted record live; `memory_amend` for dated amendments to `.coding/knowledge/**`), amend both with a dated note that the TOCTOU was closed by plan aa3c8dae (gate-resolve-pin + fail-closed, `src/tool/agent/web_fetch.rs`). This is exactly the stale-claim class the project polices.

### LOW-2 — BUG: memory record for this fix is not yet written (plan step 2 marked complete)

Plan step 2 ("memory_write a BUG: record") is checked, but a memory browse (newest-first) shows no record for this fix and no new `.coding/knowledge/bug/` file is in the working tree. The finish gate auto-captures a BUG: record for bug plans — verify it lands, and make it supersede/cross-reference the stale residual claims from LOW-1 (a bare new record does not amend the old ones).

## Verification evidence

Read-only reviewer — suites not re-executed. The implementer's evidence (cargo test full workspace: 2278 + 16 passed, 0 failed, exit=0, warning-free under `#![deny(warnings)]`; 21/21 web_fetch tests; pin regression red→green) is consistent with the code as read: every reference to the old signatures is updated, the new types/functions are used coherently, and the test bodies match the described assertions.

**Recommendation:** fix LOW-1 (two `memory_amend` calls) and confirm LOW-2 at finish; no code changes required.
