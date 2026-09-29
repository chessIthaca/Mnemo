# GitHub security defects — snapshot 2026-09-29 (HEAD `a2522a4`)

Fetched with the authenticated `gh api` path documented in
`.coding/knowledge/how/2027-01-11-read-mnemo-s-github-security-code-scanning-pages.md`.
The fetcher script is `.coding/tmp/gh-security-report.ps1` (raw dump to
`.coding/tmp/gh-security-report.txt`).

CodeQL runs on **default setup** (`dynamic/github-code-scanning/codeql:analyze`;
languages: actions, javascript, javascript-typescript, python, rust, typescript;
`query_suite=default`, `threat_model=remote`, weekly + on push to main). Every alert
below is therefore computed at commit `a2522a4` (`refs/heads/main`, latest analysis
2026-09-28T14:55Z), so the line numbers are exact for that revision.

## Totals

| Surface | Open | Fixed | Dismissed |
|---|---|---|---|
| Code scanning (CodeQL) | 13 | 2 | 0 |
| Dependabot alerts | 1 | 2 | 0 |
| Secret scanning | 0 | — | — |
| Repository security advisories | 0 | — | — |

Repo posture (context for the severities below): public repo; secret scanning +
push protection enabled (validity checks and non-provider patterns off); Dependabot
alerts and security updates enabled (`automated-security-fixes.enabled=true`,
`paused=false`); private vulnerability reporting enabled; Actions default workflow
token permission already `read` (`can_approve_pull_request_reviews=false`); **no**
`.github/dependabot.yml`, **no** `SECURITY.md`, no rulesets.

## Open code-scanning alerts (13)

### MEDIUM — `actions/missing-workflow-permissions` (CWE-275) — 2

| # | Location | Message |
|---|---|---|
| [1](https://github.com/chessIthaca/Mnemo/security/code-scanning/1) | `.github/workflows/build.yml:43` (`windows` job, 43-80) | `GITHUB_TOKEN` permissions not limited |
| [13](https://github.com/chessIthaca/Mnemo/security/code-scanning/13) | `.github/workflows/build.yml:81` (`macos` job, 81-226) | `GITHUB_TOKEN` permissions not limited |

The only `permissions:` in the file is on the sibling `release` job
(`contents: write`, lines 231-232, added by `6ba1720` on 2026-09-20); the two build
jobs have no `permissions:` key at all. Alerts 2 and 12 are **not** evidence of a fix:
they are earlier instances of this same finding on the `macos` job (line ranges 96-209
and 115-259) that GitHub closed and immediately re-raised as alert 13 when the job was
restructured — `9063715` (`ci(macos): gate the macOS job on a dispatch input +
capture test output`, 2026-09-21T08:03Z) matches the 08:04:38Z close/re-open pair
exactly. Practical risk is low because the repo-wide default is already `read`, but it
is a real defence-in-depth gap: flipping that repo setting would silently grant both
build jobs a write token.

**Fix:** add a workflow-level block (below `concurrency:`), leaving the job-level
override on `release` in place:

```yaml
permissions:
  contents: read
```

### HIGH — `rust/cleartext-logging` (CWE-312/359/532) — 5

| # | Location | Sink | Class |
|---|---|---|---|
| [8](https://github.com/chessIthaca/Mnemo/security/code-scanning/8) | `src/memory/consolidation.rs:208` (col 19) | `eprintln!("mnemo: failed to delete working events for session {session_id}: {e}")` | **production code** |
| [6](https://github.com/chessIthaca/Mnemo/security/code-scanning/6) | `src/tool/steering.rs:513` (col 60) | `assert!(note.contains("15 working-memory events"), "{note}")` | test (`mod tests`, line 276+) |
| [7](https://github.com/chessIthaca/Mnemo/security/code-scanning/7) | `src/tool/steering.rs:514` (col 54) | `assert!(note.contains("memory_consolidate"), "{note}")` | test (`mod tests`) |
| [15](https://github.com/chessIthaca/Mnemo/security/code-scanning/15) | `src/workflow/mod.rs:1527` (col 33) | `assert_eq!(id.len(), 8, "plan ids are short 8-hex handles, got {id}")` | test (`mod tests`, line 1344+) |
| [14](https://github.com/chessIthaca/Mnemo/security/code-scanning/14) | `src/workflow/mod.rs:1530` (col 13) | `"the id is hex, got {id}"` (id tainted by `uuid_hex`, `create_plan` at line 650) | test (`mod tests`) |

Only alert 8 is production code: the session id reaches stderr verbatim on a failed
working-memory cleanup. Impact is low (local log, a session handle — not a
credential), which is why the taint chain in the four test assertions is the bulk of
the count: CodeQL treats the `assert!`/`assert_eq!` panic messages as log sinks and
the literal `"session-f11"` / uuid-derived plan id as the sensitive source.

**Fix options:** (a) log a non-identifying prefix (e.g. first 8 chars) or a count
instead of the full `session_id` in `consolidation.rs`; (b) for the test-only
assertions, drop the interpolated value (`assert!(note.contains(...))`) or assert on
a redacted form — both close the alert without weakening the pin.

### HIGH — `rust/insecure-cookie` (CWE-319/614) — 3 — vendored library, false positive

| # | Location | Class |
|---|---|---|
| [9](https://github.com/chessIthaca/Mnemo/security/code-scanning/9) | `vendor/wry/src/webkitgtk/mod.rs:972` (col 5, `cookie_builder.build()`) | library |
| [10](https://github.com/chessIthaca/Mnemo/security/code-scanning/10) | `vendor/wry/src/webview2/mod.rs:1589` (col 8, `cookie_builder.build()`) | library |
| [11](https://github.com/chessIthaca/Mnemo/security/code-scanning/11) | `vendor/wry/src/wkwebview/mod.rs:1103` (col 5, `cookie_builder.build()`) | library |

Verified in the vendored source: all three modules DO set `Secure` on the builder
from the *source* cookie before building — `webkitgtk/mod.rs:949-950`
(`cookie.is_secure()`), `wkwebview/mod.rs:1073-1074` (`cookie.isSecure()`),
`webview2/mod.rs:1559-1561` (`cookie.IsSecure(&mut secure)`). The rule only accepts a
literal `.secure(true)`, so a runtime-propagated value reads as missing. wry is a
pass-through cookie plumbing layer; the attribute is preserved.

**Fix:** dismiss as "false positive" (documented here) rather than patch vendored
code — `vendor/wry` is a pruned mirror with its own `PATCHES.md` regeneration flow,
and `vendor/wry/src/webkitgtk/**` is dead code for this app (Windows + macOS only).

### HIGH — `js/redos` (CWE-1333/400/730) — 2

| # | Location | Flagged sub-pattern |
|---|---|---|
| [3](https://github.com/chessIthaca/Mnemo/security/code-scanning/3) | `frontend/src/components/settings/sections/ModelCombobox.test.tsx:118` (cols 64-70) | `(?:\s|\/\/[^\n]*)*` in a `source`-pinning regex |
| [4](https://github.com/chessIthaca/Mnemo/security/code-scanning/4) | `frontend/src/components/views/gitLanes.ts:359` (cols 35-65) | `(?:-(?:C\|c)\s+\S+\s+\|-\S+\s+)*` in the shell-command git-verb heuristic |

Both are overlapping alternatives inside a quantified group (`\s` vs `//[^\n]*`;
`-C <val>` vs `-\S+`), i.e. genuinely exponential for adversarial input — but the
inputs are a checked-in test file's own text and the agent's own shell command
string, so exploitability is nil.

**Fix:** (a) `ModelCombobox.test.tsx` — strip comments first, then match with a plain
`\s*` bridge: `const stripped = source.replace(/\/\/[^\n]*/g, "");` followed by
`expect(stripped).toMatch(/onClick=\{\(\) => \{\s*setOpen\(\(o\) => !o\);\s*if \(document\.activeElement !== inputRef\.current\) \{\s*skipOpenRef\.current = true;\s*inputRef\.current\?\.focus\(\);/);`
(b) `gitLanes.ts` — make the two alternatives disjoint by forbidding a *value*
token from looking like a flag (a leading `-`), which removes the overlap:
`/(?:^|[|;&,(]\s*)git\s+(?:-(?:C|c)\s+(?![\s-])\S+\s+|-\S+\s+)*(?:commit|merge|checkout|branch)\b/`
— verified equivalent to the current regex on 310 000 random commands (0 differing
results) and flat at 0.004 ms where the current one needs 5.2 s at n=28 growing ~4x
per extra token (see "Fix validation" below). Note the naive "collapse the
alternation" rewrite `(?:-\S+\s+(?:\S+\s+)?)*` is **worse** than the original
(1.9 s at n=20 where the original needs 25 ms) — do not use it.

### HIGH — `js/insecure-randomness` (CWE-338) — 1

| # | Location | Detail |
|---|---|---|
| [5](https://github.com/chessIthaca/Mnemo/security/code-scanning/5) | `frontend/src/components/settings/sections/ProvidersSection.tsx:151` (cols 17-26) | `const uid = makeUid();` |

The `Math.random()` fallback lives in `frontend/src/components/settings/types.ts:227`
(`makeUid()` — `crypto.randomUUID()` when available, else
`Math.random().toString(36).slice(2, 10)`). The value is used only as a React key /
`Set` key for endpoint rows (`ProvidersSection.tsx:153-154, 343-355`), and the doc
comment above `makeUid` already says "React list keys only — never reuse it for
anything security-adjacent". The rule fires because the id sits next to the
`apiKeys` state.

**Fix:** make the primary path unconditional — this app runs in WebView2 (Chromium)
and WKWebView, both of which ship `crypto.randomUUID` in a secure context — e.g.
keep only `` `ep-${crypto.randomUUID().slice(0, 8)}` `` and delete the `Math.random`
fallback (or replace it with a module-scope counter). Removes the taint source.

## Open Dependabot alerts (1)

| # | Package | Advisory | Severity | Vulnerable | Patched | Scope |
|---|---|---|---|---|---|---|
| [1](https://github.com/chessIthaca/Mnemo/security/dependabot/1) | `rust:glib` 0.18.5 (`Cargo.lock`) | GHSA-wrw7-89jp-8q8g — unsound `Iterator`/`DoubleEndedIterator` impls for `glib::VariantStrIter` (UB; NULL-pointer crashes under optimisation) | medium | `>= 0.15.0, < 0.20.0` | 0.20.0 | runtime |

Not reachable for this app's targets: `cargo tree -i glib --target
x86_64-pc-windows-msvc --offline` reports `package ID specification 'glib' did not
match any packages`, and every in-lock dependent (`gtk`, `gdk`, `gio`, `atk`, `soup3`,
`javascriptcore-rs`, `libappindicator`, …) sits behind
`[target.'cfg(any(target_os = "linux", target_os = "dragonfly", …))'.dependencies]`
in `vendor/wry/Cargo.toml` (gtk 0.18, line 209) and `vendor/tao/Cargo.toml` (gtk 0.18,
line 270). CI builds `windows-latest` + `macos-latest` only, so the crate is never
compiled here.

**Fix:** escaping the vulnerable range needs `glib` 0.20, which drags the whole
gtk-rs 0.18 → 0.20 stack (gtk/gdk/gio/soup3) with it — a Linux-port project, not a
patch. Dismiss as "vulnerable code not reachable" for the Windows/macOS targets and
re-evaluate only if a Linux target ever lands (see
`.coding/analysis/2026-08-20-macos-port-research.md` for the platform-scope backdrop).

## Closed in the same window (no action)

- Code scanning 2, 12 — `actions/missing-workflow-permissions`: state `fixed`, but
  that is re-raise churn from the 2026-09-21 workflow restructures, **not** a
  remediation — the identical finding is still open as alert 13. Only the Dependabot
  pair below is a genuine fix.
- Dependabot 2, 3 — GHSA-82fw-gwwq-j7x9 / CVE-2026-84373 (Vitest + `@vitest/mocker`
  path traversal / arbitrary file read; fixed 2026-09-26 by the 4.1.11 bump).

## Fix validation (run locally against HEAD)

Scripts (scratch, `.coding/tmp/`): `redos-fix-validation3.mjs`, `redos-timing.mjs`,
`redos-fuzz-extended.mjs`; evidence dumps `redos-valid3.txt`, `redos-timing.txt`,
`redos-fuzz-extended.txt`.

**ModelCombobox pin.** The regex was extracted from the test file programmatically
(an earlier hand-transcription was wrong because CodeQL escapes `{` as `{{` in alert
messages — the real pin is `onClick=\{\(\) => \{\s*setOpen...`, single braces).
Running that file at HEAD is **14/14 green**
(`cd frontend; node ..\node_modules\vitest\vitest.mjs run
src/components/settings/sections/ModelCombobox.test.tsx`, vitest 4.1.11), so the pin
currently matches; the finding is a latent ReDoS, not a broken test.

| Check | Result |
|---|---|
| current pin matches `ModelCombobox.tsx` | true |
| strip-comments + `\s*`-bridge pin matches | true |
| still rejects a source with the guard deleted | true |
| still tolerates an interposed `//` comment line | true |
| rejects other code (`doSomethingElse();`) in the gap | true |

**gitLanes heuristic** — `'git ' + '-C -! ' * n`, milliseconds:

| n | current regex | candidate C |
|---|---|---|
| 20 | 24.9 | 0.013 |
| 24 | 317.3 | 0.004 |
| 28 | 5215.0 | 0.004 |
| 256 | (hours) | 0.026 |
| 1024 | (hours) | 0.121 |

Differential fuzz, current vs candidate C: 60 000 commands of 1-5 tokens → 0
differences; 250 000 commands of 1-9 tokens → 0 differences. Real-command sanity:
`git merge`, `git -C path commit`, `git -c x=y merge`, `git --no-pager commit` all
still true; `echo git commit` still false.

## Fixes applied (2026-09-29) — all 13 code-scanning + the 1 Dependabot finding

| Finding | Fix | Regression net |
|---|---|---|
| 1, 13 `actions/missing-workflow-permissions` | workflow-level `permissions: contents: read` in `build.yml`; the `release` job keeps its `contents: write` override | new `build_workflow_scopes_token_permissions` — verified RED with the block removed, green with it |
| 5 `js/insecure-randomness` | `settings/types.ts` `makeUid` fallback is a module-scope counter; no `Math.random` in the file's code | new `makeUid` contract test in `settings/types.test.ts` — verified RED with `Math.random` reintroduced as code, green now |
| 4 `js/redos` (`gitLanes.ts:359`) | disjoint alternatives via the `(?![\s-])` value lookahead | `gitLanes.test.ts` (29 tests) + the 310 000-command differential fuzz |
| 3 `js/redos` (`ModelCombobox.test.tsx:118`) | line comments stripped before the pin; plain `\s*` bridge | the pin itself + the strength checks under "Fix validation" |
| 6, 7 `rust/cleartext-logging` (test asserts) | the session-bearing `note` is no longer interpolated into the failure messages | the assertions themselves; lib suite |
| 14, 15 `rust/cleartext-logging` (test asserts) | the `uuid_hex`-derived plan id is no longer interpolated | `create_plan_mints_a_short_collision_checked_id` |
| 8 `rust/cleartext-logging` (production) | `consolidation.rs` logs the error only, not the session id — the `mnemo: failed to delete working events for session` needle stays intact | `durable_write_failures_log_instead_of_vanishing` |
| 9, 10, 11 `rust/insecure-cookie` | **dismissed** "false positive" by chessIthaca on 2026-09-29 (vendored wry propagates `Secure`; see the section above) | — |
| Dependabot 1 `glib` GHSA-wrw7-89jp-8q8g | **dismissed** `not_used` by chessIthaca on 2026-09-29 (Linux/GTK-only lock entry, never compiled here) | — |

Local verification at the time of writing: `cargo test -p mnemo --lib --test integration`
→ 2903 + 20 passed, 0 failed; `vitest run` over the whole frontend → 92 files /
1314 tests passed; `tsc --noEmit` clean.

The 10 still-*open* code-scanning alerts (1, 3-8, 13-15) are fixed in the working tree;
GitHub closes them once the change is pushed and default-setup CodeQL re-analyses (push
to `main`, a PR, or the weekly run). Until then the security tab keeps showing them.

## Reproduce

```powershell
gh api "repos/chessIthaca/Mnemo/code-scanning/alerts?per_page=100" --paginate --jq ".[]"
gh api "repos/chessIthaca/Mnemo/dependabot/alerts?per_page=100" --paginate --jq ".[]"
gh api "repos/chessIthaca/Mnemo/secret-scanning/alerts?per_page=100" --paginate
gh api "repos/chessIthaca/Mnemo/security-advisories"
gh api "repos/chessIthaca/Mnemo/code-scanning/alerts/<n>/instances"   # commit_sha + exact cols
gh api "repos/chessIthaca/Mnemo/code-scanning/default-setup"
```
