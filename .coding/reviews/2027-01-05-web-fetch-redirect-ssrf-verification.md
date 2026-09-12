## Verdict: PASS

Both LOW findings from the 2027-01-05 review (`.coding/reviews/2027-01-05-web-fetch-redirect-ssrf-review.md`, plan 152d8007) are correctly fixed, and the restructure introduced no new issues. F1: an unjoinable redirect `Location` now returns the 3xx response as-is — exact reqwest 0.12 parity — and the new test genuinely exercises the join-failure arm (`Url::join` does fail on port 70000). F2: the SSRF gate now runs at the top of the loop on every requested URL, `start` included, making `fetch_following_redirects` self-contained; the bottom gate is gone (no double-gating), and the doc comment states the invariant. All six redirect tests remain meaningful; the original review's clean areas are unregressed.

### Scope and method

- `git diff HEAD` (uncommitted): `src/tool/agent/web_fetch.rs` (+447/−38 net vs HEAD — the original SSRF fix plus both review-round fixes), `.coding/backlog.jsonl` (d187addc pending→in_flight, unchanged from the prior review's read). Untracked: the prior review report, plan file, knowledge file (all read).
- Full read of web_fetch.rs final state (882 lines): `execute` (135-248), `ensure_public_http_url` (250-285), `fetch_following_redirects` (287-355), the redirect test module (585-882).
- Cross-repo searches: `fetch_following_redirects` is referenced only inside web_fetch.rs (definition :305, one production call :198, five test calls); the removed F1 error text "invalid redirect Location" survives only in the prior review's historical record — no stale source or test references.
- Reviewer toolset has no shell: tests NOT re-run by me. Verified by inspection; the implementer's recorded runs (root 1998 passed — the prior round's 1997 plus the one new test; src-tauri 186+4; zero warnings under `deny(warnings)`) are consistent with the code as read.

### F1 — unjoinable `Location`: fixed, exact reqwest parity, test genuinely exercises it

**Fix (web_fetch.rs:345-348).** `current.join(location)` is now a `match`: `Ok(next) => next`, `Err(_) => return Ok((current, resp))`. A present, UTF-8, unjoinable Location returns the 3xx response as-is — the same outcome as the missing/non-UTF-8 Location path (let-else, :328-334). That is exact reqwest 0.12 parity ("If this fails, we won't error as the original response is still valid"): no error, no follow, the original response is the final one. The prior erroring `.map_err(...)?` is gone and nothing references its error text anymore (searched — only the prior review's historical record quotes it).

**Documented (:297-302, :340-344).** The `fetch_following_redirects` doc comment now lists the case in its parity contract ("a Location that cannot be joined (e.g. an invalid port) returns the 3xx response as-is — 'the original response is still valid'"), and the inline comment at the join repeats it with the malformed-IPv6 example. Because the fix restores parity rather than deviating, the plan's three-deliberate-changes list (152d8007.md:10) stays accurate — no fourth deviation exists to document.

**The test exercises the arm, and `Url::join` really fails on port 70000 (web_fetch.rs:769-786).** `unjoinable_redirect_location_returns_response_as_is` serves a 302 with `Location: http://127.0.0.1:70000/` and asserts: status 302 returned, `final_url` == the start URL, exactly 1 request. The url crate implements the WHATWG URL algorithm; its port parser accumulates digits into a u32 and fails with `ParseError::InvalidPort` once the value exceeds `u16::MAX` (65535) — 70000 > 65535, and `Url::join` parses an absolute reference in full (no fallback-to-base on error), so the join returns `Err`. The test comment (:773) is accurate. The test is discriminating: the 302 is in the followed set and carries a Location, and hop 0 ≠ MAX, so the ONLY route to `Ok` is the join-failure arm — if the join somehow succeeded, the loop would request `http://127.0.0.1:70000/` (nothing listening → connection refused → `Err` → the `.expect` at :782 panics), and under the pre-fix erroring behavior the `Err` likewise fails the `.expect`. It fails without the fix and passes with it; the 1-request assertion pins that no follow was attempted.

### F2 — self-contained gate: fixed at the top of the loop, bottom gate removed, invariant documented

**Gate placement (web_fetch.rs:311-316).** `ensure_public_http_url(&current, gate).await?` is the first statement of the loop body, before the function's only `client.get` (:317-318). Iteration 0 gates `current` = `start.clone()`, so `start` is gated inside the function — the caller-side precondition is gone. Every URL requested in the function passes the gate in the same iteration; there is no ungated request path.

**Bottom gate removed (:345-349).** After the join, the code assigns `current = next` and loops back to the top gate — the old bottom-of-body gate on `next` is gone. Each URL is gated exactly once inside the function; no double-gating of hop targets. `execute` still pre-gates the initial URL (:178-180) before calling (:197-198) — one redundant-but-idempotent re-check of the same parsed `Url` object (no parser-differential; cost = one extra DNS resolution of the start host per fetch), exactly the defense-in-depth option the prior report recommended (its fix option (a), which pre-verified compatibility with the loopback-exempt and `|_| false` test gates).

**Invariant documented (:287-295, :312-315).** The doc comment states: "running the full SSRF gate ([`ensure_public_http_url`]) on EVERY URL before requesting it, `start` included: the function is self-contained (no caller-side gate precondition — a future caller or refactor cannot silently reintroduce the bypass this fixed)"; the in-loop comment repeats it. A future caller cannot silently reintroduce the bypass.

**Mutation re-check consistency.** The implementer's recorded mutation (disabling the top-of-loop gate makes `redirect_to_internal_address_is_refused` fail) is consistent with the code as read: without the gate, iteration 1 would request 169.254.169.254 directly; the stub serves exactly one response, so the connection fails with a non-SSRF error and the `msg.contains("SSRF")` assertion fails.

### Restructure sweep — no new issues

- **Borrow/ownership of the `Err` arm (:345-347).** `location: &str` borrows `resp` (via `resp.headers()…to_str()`, :328-331); the match scrutinee `current.join(location)` is the last use of that borrow, and `join` borrows `current` only for the call — so moving both `current` and `resp` into the return value in the `Err` arm is sound under NLL. Compiles per the implementer's recorded run; no warning-generating leftovers (the removed `map_err` closure and bottom gate leave no dangling references — searched).
- **`unreachable!()` justification (:351-354) accurate.** On the last iteration (hop == MAX_REDIRECTS == 5) every path returns: gate/request error, non-followed status → Ok (:325-327), no Location → Ok (:328-334), `hop == MAX_REDIRECTS` → Err (:335-339), join failure → Ok (:347). The only continue path (`current = next`, :349) is unreachable on the last iteration because the hop check returns first. The comment enumerates exactly these.
- **Hop counting unchanged.** `for hop in 0..=MAX_REDIRECTS` = 6 requests (initial + 5 follows); the 6th followed-3xx triggers the too-many error. `too_many_redirects_errors` (:791-819) asserts exactly 6 requests — parity with `Policy::limited(5)` preserved. The top-of-loop gate makes no requests and consumes no hops, so request counts are unaffected.
- **All six redirect tests remain meaningful** (each re-verified against the final loop): `redirect_to_internal_address_is_refused` (:711-737, exactly 1 request, SSRF error), `redirect_to_non_http_scheme_is_refused` (:741-764, join of `file:///etc/passwd` succeeds, the top gate's scheme check refuses), `unjoinable_redirect_location_returns_response_as_is` (:769-786, new), `too_many_redirects_errors` (:791-819, 6 requests), `redirect_to_public_address_succeeds` (:823-848, 2 requests, final URL + body), `ensure_public_http_url_gates_ips_and_schemes` (:853-881, gate unit test, unaffected).

### Clean-area regression sweep — no regression

- Status set `301 | 302 | 303 | 307 | 308` (:325) — unchanged, exactly reqwest's followed set; a 3xx without a Location returned as-is (:328-334).
- Relative/absolute Location resolution via `current.join(location)` (:345) — unchanged.
- Fail-closed `spawn_blocking` (`unwrap_or(true)`, :269-274) — unchanged.
- `final_url` reporting: the loop returns `current` (last requested URL) ≡ old `resp.url()` after auto-follow; `execute` prints it (:197-210) with the original `args.url` in the output header (:232-233) — unchanged.
- Check order (status → Location → hop-cap → join) — unchanged from the prior-reviewed state.
- Module doc (:8-10) and `execute`'s SSRF/client-builder comments (:163-186) — still accurate.

### Constitution checks — no finding

- **Docs sync.** In-code docs updated (module doc, `fetch_following_redirects` doc + inline comments). The knowledge file's five-test list is not stale: the sixth test is an F1-parity test, not an SSRF regression test, and the file's gate description ("on every hop's Location BEFORE requesting it") remains true — no false statement. README.md/PLAN.md have no web_fetch-redirect claims (prior review verified; this delta touches neither file).
- **Multi-platform neutrality.** No Windows-only APIs; the stub binds `127.0.0.1:0` (fine on macOS and Windows); std::net + tokio + reqwest only.
- **Warning-free by inspection.** All imports/locals used; the restructure removed code without leaving dead paths. Implementer's runs on record (root 1998 passed = prior 1997 + the new test; src-tauri 186+4; zero warnings under `deny(warnings)`); I could not re-run cargo (read-only toolset).
