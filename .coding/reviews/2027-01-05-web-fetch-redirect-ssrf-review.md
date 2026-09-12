## Verdict: FINDINGS (0 high, 2 low)

The SSRF redirect fix is correct and airtight as wired: every hop target passes the full gate (scheme + host, fail-closed) before being requested, hop counting and the followed-status set match reqwest 0.12's `Policy::limited(5)` semantics, and the regression tests drive the real loop with the production gate. Two LOW findings: one undocumented reqwest-parity deviation (an unjoinable redirect `Location` now errors instead of returning the 3xx response), and one defense-in-depth gap (the loop function itself never gates its `start` URL — safe today, single gated caller). Neither blocks the security goal.

### Scope and method

- `git diff HEAD` + untracked: `src/tool/agent/web_fetch.rs` (substantive, +414/−38), `.coding/backlog.jsonl` (d187addc pending→in_flight), untracked `.coding/knowledge/bug/2027-01-05-web-fetch-redirect-hops-bypassed-the-ssrf-blockl.md` + `.coding/plans/152d8007.md` (both read, sanity-checked).
- Full read of web_fetch.rs final state (849 lines). reqwest 0.12 (Cargo.toml:79) parity checked against reqwest 0.12's redirect implementation semantics.
- Reviewer toolset has no shell: tests were NOT re-run by me. Verified by inspection; the implementer's recorded runs (root 1997 passed, src-tauri 186+4, zero warnings under `deny(warnings)`) are consistent with the code as read (no unused imports/vars/dead code spotted).

### Findings

**F1 (LOW) — invalid redirect `Location` now errors instead of returning the 3xx response as-is: a fourth, undocumented deviation from the stated reqwest-parity goal.**

Evidence: `src/tool/agent/web_fetch.rs:334-336` — `current.join(location).map_err(|e| format!("fetch failed: invalid redirect Location {location:?}: {e}"))?`.

reqwest 0.12's auto-follow treats a `Location` that fails `to_str()` **or** `Url::join()` (or `http::Uri` conversion) as "no redirect" and returns the original response — reqwest's own source comment: "If this fails, we won't error as the original response is still valid." The new code matches that for a missing/non-UTF-8 Location (`web_fetch.rs:319-325`, returned as-is — parity ✓) but **errors** when the Location is present, UTF-8, and unjoinable — e.g. `Location: http://host:70000/` (invalid port) or `http://[/` (malformed IPv6). Old behavior: the 302 itself was returned (status 302, typically empty body). New: `ToolResult::error("fetch failed: invalid redirect Location ...")`.

The plan (`.coding/plans/152d8007.md:10`) documents exactly three deliberate behavior changes; this is a fourth. Severity LOW: broken-server edge case, fail-safe direction (nothing unsafe is fetched), but the tool result differs from the stated parity contract and the deviation is undocumented.

Fix (either): (a) exact parity — on join failure return `Ok((current, resp))` like the no-Location path; or (b) keep erroring (arguably the clearer tool result) and document it as a fourth deliberate change in the plan + the `fetch_following_redirects` doc comment. No test covers this path either way — add one with whichever behavior is chosen.

**F2 (LOW) — `fetch_following_redirects` does not gate its `start` URL; the gate precondition lives only in the caller (defense-in-depth).**

Evidence: `src/tool/agent/web_fetch.rs:301-312` — the loop's first `client.get(current.clone())` (lines 308-309) requests `start` (assigned at line 306) without any gate check; only hop targets are gated (line 337). `execute` gates `start` before calling (line 178), so the current wiring is airtight — but the function's doc comment (lines 287-300) doesn't state the precondition "caller must have gated `start`", while the function advertises the invariant "this loop is the only thing that follows redirects, so no hop can bypass the gate." A future caller, or a refactor that moves/removes the initial gate in `execute`, silently reintroduces the exact bug class this plan fixes.

Fix (either): (a) one-line hardening — `ensure_public_http_url(&current, gate).await?` at the top of the loop body (idempotent for the already-gated start; costs one extra DNS lookup per fetch; verified compatible with all five new tests — the loopback-exempt and `|_| false` gates pass the mock start URL); or (b) minimum: document the precondition in the `fetch_following_redirects` doc comment. Private fn, single caller — no exploitable path today.

### Checked clean — no finding

**1. Correctness (redirect-loop semantics vs reqwest parity) — no finding.**
- Status set: `matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)` (web_fetch.rs:316) is exactly reqwest's followed set; 300/304/305 and any 3xx without a Location are returned as-is (lines 314-325) — parity ✓.
- Hop counting: `for hop in 0..=MAX_REDIRECTS` = 6 requests (initial + 5 follows); the 6th redirect response triggers the error (lines 326-330) — identical to `Policy::limited(5)` (6 requests, then error). `too_many_redirects_errors` asserts exactly 6 requests (web_fetch.rs:781-785) ✓.
- Relative Locations: `current.join(location)` (lines 334-335) — url-crate join, the same resolver reqwest uses; protocol-relative `//host/x` and absolute cross-host Locations both land in the gate ✓.
- `unreachable!()` (line 343) is sound: the `hop == MAX_REDIRECTS` iteration always returns (final response / no-Location response / too-many-redirects error) — every path in the last iteration exits ✓.
- Borrow/ownership: `start.clone()` into `current` (start is used later in the error message), `current.clone()` into `get()` — clean; `gate: fn(&str) -> bool` fn-pointer injection works with both the named gate and the non-capturing test closure ✓.
- `final_url` reporting: `current` (last requested URL) ≡ old `resp.url()` after auto-follow (web_fetch.rs:210) ✓; the output header still prints the original `args.url` ✓.
- Method/header parity: web_fetch only issues GETs with a client-level UA — reqwest's 303/301/302 method rewrite and cross-host sensitive-header stripping are inapplicable ✓.

**2. Security (per-hop gate) — no finding beyond F2.**
- No ungated request path in `execute`'s wiring: the only `client.get` is on `current`, which is either the pre-gated `start` (gated at line 178) or a `next` that passed `ensure_public_http_url` immediately before assignment (lines 337-338). The gated `Url` object is the exact object then requested — no parser-differential between gate and fetch ✓.
- Scheme re-checked on every hop: `ensure_public_http_url` checks scheme first (lines 261-267); `file://` via Location is refused — covered by `redirect_to_non_http_scheme_is_refused` ✓.
- Fail-closed preserved: `spawn_blocking(...).await.unwrap_or(true)` (line 274) — JoinError ⇒ treated as internal ⇒ refused, same as the old initial gate ✓.
- Location edge cases: non-UTF-8 → `to_str().ok()` → None → returned as-is (reqwest parity) ✓; multiple Location headers → first (reqwest same) ✓; whitespace-padded Location → the url crate strips it (WHATWG parsing) ✓.
- Initial-URL gate equivalence: same `extract_host` → `hostname_resolves_to_internal` path with the same fail-closed semantics; the prefix scheme check (lines 144-151) is retained, so the initial URL is now checked twice (prefix + parsed scheme) — strictly stronger than the old single check ✓.
- Pre-existing residuals, unchanged by this diff and correctly still documented in-code: DNS-rebinding TOCTOU (web_fetch.rs:171-177), `hostname_resolves_to_internal` returning false on resolver error (the request then fails naturally at connect time), and the IPv6 zone-ID literal edge (gate passes but the connection itself cannot be established). None is a regression from this change.

**3. Behavior changes beyond the three deliberate ones — only F1's class found.** The three documented changes (invalid-URL error text, whitespace-trim parsing, clear too-many-redirects error) are as described; everything else (size caps, HTML stripping, output format, 30s timeout, UA, error message shapes for send/body failures) is equivalent.

**4. Constitution — no finding.**
- Doc comments present on every new item: `MAX_REDIRECTS` (30-32), `ensure_public_http_url` (250-259), `fetch_following_redirects` (287-300), `StubResponse` (576-583), `RedirectStubServer` + methods (585-602, 667-670, 672-675), `no_redirect_client` (678-679), `loopback_exempt_gate` (687-689), and all five test fns ✓. No new public functions.
- Warning-free by inspection: all imports/fields/locals used; the implementer's `deny(warnings)` runs (root 1997, src-tauri 186+4) are on record — I could not re-run cargo (read-only toolset).
- Regression tests exercise the changed path: `redirect_to_internal_address_is_refused` (web_fetch.rs:700-726) drives the real `fetch_following_redirects` with the production gate (loopback exempted for the mock host only) and asserts the internal target is never contacted (exactly 1 request); the mutation check (disabling the per-hop gate call makes it fail) is recorded in the knowledge file ✓.
- Root cause documented: `.coding/knowledge/bug/2027-01-05-web-fetch-redirect-hops-bypassed-the-ssrf-blockl.md` — symptom → root cause → fix → regression tests, accurate against the code ✓. BUG: memory written (semantic record id e0bea94c) ✓.

**5. Multi-platform neutrality — no finding.** No Windows-only APIs; the stub server binds `127.0.0.1:0` (fine on macOS and Windows); only std::net + tokio + reqwest. `expect()` panics live inside the spawned test-server task only (test scope, acceptable).

**6. Docs sync — no finding.** Module doc updated (web_fetch.rs:5-13) to describe manual redirect following with per-hop gating ✓. README.md and PLAN.md searched for redirect/web_fetch claims: the only hits are unrelated (OAuth loopback redirect, `file_edit_redirect`, shell redirection) — nothing to update ✓. No source-contract test pins the removed "Redirect-to-internal … not re-checked" comment (searched the tree; only historical `.coding/reviews/` records quote it, correctly left as history) ✓.

**7. Bookkeeping sanity-check — no finding.** backlog.jsonl d187addc pending→in_flight with plan_id 152d8007 + checkpoint note (consistent with a plan complete but not yet finished/merged); the plan file matches the shipped design; the knowledge file matches the code.

### Recommendation

Fix F1 and F2 (both are one-liners or one doc-comment edit), or justify-skip with a written note. Neither blocks the security goal of the plan — the SSRF redirect bypass itself is closed, parity-verified, and regression-tested.
