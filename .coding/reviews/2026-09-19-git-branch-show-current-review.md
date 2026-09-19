## Verdict: FINDINGS (0 high, 3 low)

Review of ALL uncommitted changes (git diff HEAD: src/tool/agent/git.rs, .coding/backlog.jsonl, .coding/knowledge/how/2026-08-26-git-tool-per-subcommand-parameter-reference.md; untracked .coding/plans/29baa080.md) for plan 29baa080 "git branch list: allow the read-only --show-current flag (backlog 41cd5ad0)", kind bug_fixing.

Summary: the functional fix is correct, minimal, and safe — `--show-current` (exact) and `--points-at=` (prefix) are genuinely read-only, the `=` form provably cannot smuggle a positional or inject a flag (argv is passed via `Command::new("git").args(...)`, no shell), the mutating path stays pinned, and the regression test exercises the changed path end-to-end (validate + argv). Three LOW hygiene findings: the rejection message still omits allowlisted `-q`/`--quiet`; the plan-promised assertion pinning the message's flag enumeration is missing; the HOW amendment is dated 2027-01-11 while the landing is recorded as 2027-01-24.


## Scope reviewed

- `git diff HEAD` (3 files: src/tool/agent/git.rs +41/−9 net, .coding/backlog.jsonl item 41cd5ad0 pending→done, HOW knowledge file +2 lines) plus untracked `.coding/plans/29baa080.md`.
- Code read in context: `validate_branch_list_args` + `branch_list_argv` (git.rs:259-336), the spawn path (git.rs:406-407), the schema `args` description (git.rs:507-511), the module doc (git.rs:21-28), the test module (git.rs:1436-1535), the full HOW file, the plan file, and README.md (searched for branch-list flag enumerations — none exist).

## Check-by-check verification

**(a) --show-current / --points-at= are genuinely read-only — PASS.** `git branch --show-current` only prints the checked-out branch's name (git ≥2.22); `git branch --points-at=<object>` only filters the listing to branches whose tip equals the object. Neither touches refs, the index, or config — no mutation path in git's branch subcommand. Both are exactly as read-only as the already-allowed `--list`.

**(b) The prefix form cannot smuggle a positional — PASS.** The check is `a.starts_with("--points-at=")` on a single argv element; the value rides inside the `=` form and never becomes a separate element. The bare two-arg form `--points-at HEAD` is doubly refused (`--points-at` is not in `safe_exact`; `HEAD` fails the `!a.starts_with('-')` positional check) — same treatment as `--merged`'s value form today. Value injection is impossible: git is spawned via `Command::new("git").args(args).current_dir(...)` (git.rs:406-407) — no shell — so even `--points-at=--force` or a value containing `;`/spaces stays one argv element that git parses as the option's value (a harmless "malformed object name" at worst). Same trust model as the pre-existing `--format=`/`--sort=`/`--color=` prefixes. `--show-current` is exact-match only — no prefix risk.

**(c) Surfaces in sync — PASS with Finding 1.** safe_exact (git.rs:294) and safe_prefix (git.rs:305) carry the new flags; the doc comment's Allowed list (git.rs:274-283) covers both plus the audited exclusions; the rejection message (git.rs:317-320) names both; the schema `args` description (git.rs:510) and module doc (git.rs:21-28) carry no flag enumeration, so correctly untouched; README has no enumeration to sync. One pre-existing gap survives this edit — see Finding 1.

**(d) Mutating path still refused — PASS.** `branch_list_allowlist_rejects_positionals_and_write_flags` (git.rs:1436-1468) is unchanged and still pins: positional refused with "positional" in the message; `--set-upstream-to=origin/main`, `--unset-upstream`, `--edit-description`, `-d`, `-m`, `--delete`, `--move`, `--force` all refused with "not allowed". `args_rejected_on_write_subcommands` additionally pins branch create with args. Combined short flags (`-av`) remain unlisted and refused.

**(e) HOW amendment accurate — PASS with Finding 3.** The amendment's content matches the code exactly (exact vs prefix form, the `=` rationale, the audited exclusions, mutating flags refused, message kept in sync). The date heading is muddled — see Finding 3.

**(f) Multi-platform neutrality / file-tools-first / documentation sync — PASS.** Pure Rust string matching — no platform APIs, paths, or shell syntax. The src change is a clean file-tools diff; the knowledge file was amended via the sanctioned `memory_amend` writer (clean appended paragraph); backlog.jsonl via the backlog tools; no shell mutation anywhere. Docs: doc comment + rejection message synced; schema/module doc/README correctly need no change; backlog item closed with an accurate note naming the plan, both flags, the exclusions, the regression test, and the suite count.

**(g) General correctness / bugs / security — PASS.** The regression test `branch_list_allowlist_accepts_show_current` (git.rs:1507-1521) exercises the changed path end-to-end (validate passes AND `branch_list_argv == ["branch", "--show-current"]` — the flag provably reaches git); it was written first and confirmed failing pre-fix per the plan. `branch_list_allowlist_accepts_read_only_flags` extended with `--show-current` in the exact loop and `--points-at=HEAD` in the prefix asserts. Deny-by-default allowlist preserved; no new attack surface. Reported 2480 passed / warning-free is consistent with the diff (no unused imports, no dead code; doc comment sits on the function it documents).

## Findings

### Finding 1 (LOW) — rejection message still omits allowlisted `-q`/`--quiet`

`validate_branch_list_args`'s rejection message (git.rs:317-320) enumerates the allowlist but omits `-q`/`--quiet`, which ARE on `safe_exact` (git.rs:295-296) and in the doc comment's Allowed list (git.rs:276). Pre-existing (the old message omitted them too), but this diff edited exactly that enumeration under the plan's "message synced with the allowlist" goal — the sync is still incomplete, and an agent refused on `-q` won't learn it's allowed. Fix: add `-q/--quiet` to the enumeration (one word).

### Finding 2 (LOW) — the plan-promised message-sync assertion is missing

The plan's checked-off detailed step "Regression tests" says: "add an assertion that the refusal message for an unknown flag names --show-current (message/allowlist sync)". No such assertion exists — the refusal tests only check `err.contains("not allowed")` (git.rs:1466). The message's flag enumeration is therefore unpinned by tests: a future edit that drops `--show-current` or `--points-at=` from the message breaks no test. Fix: in `branch_list_allowlist_rejects_positionals_and_write_flags` (or the new regression test), assert the refusal message for an unknown flag contains `"--show-current"` and `"--points-at="`.

### Finding 3 (LOW) — HOW amendment date contradicts the landing records

The new amendment is headed "Amended 2027-01-11" — identical to the previous amendment's heading — while the landing is recorded as 2027-01-24 everywhere else (backlog note "Done via plan 29baa080 (2027-01-24)", the plan, the sibling items' notes). The three dates in play are mutually inconsistent (this review report itself was clock-stamped 2026-09-19, matching the backlog `created_at` epochs — the environment's dates are internally muddled), but the amendment duplicating the prior amendment's exact date cannot be right for a change landed in the 2027-01-24 session: the record's chronology reads as if both amendments landed the same day. Fix: correct the amendment heading to the landing date used by the plan/backlog records (2027-01-24) via `memory_update` find/replace, so the two amendments are distinguishable.

## Conclusion

The defect is fixed correctly and safely: the flag is read-only, the value form cannot smuggle anything, the mutating path stays pinned, and the regression test fails pre-fix / passes post-fix on the real code path. All three findings are documentation/test-hygiene level (message completeness, an unpinned message enumeration, a record date) — none blocks the fix's correctness or security. Fix them, re-run `cargo test`, then commit.
