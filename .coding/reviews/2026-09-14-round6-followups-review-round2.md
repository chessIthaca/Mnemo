## Verdict: FINDINGS (0 high, 2 low)

Round-2 verification of commit `2f543d3` (HEAD of `wt/mnemo`, plan f69317d4) against the five round-1 findings in `.coding/reviews/2026-09-14-round6-followups-review.md`. **All five are genuinely fixed** — verified in the shipped tree, not by test re-run. Two new LOW findings, both documentation-accuracy nits introduced by the fix itself (one stale sentence in the fresh BUG knowledge record, one imprecise cross-reference in the rewritten skill comment). No code defects; no constitution findings beyond the doc nits.

Tree state: `git diff HEAD` shows exactly one expected uncommitted change — the step-6 checkbox tick in `.coding/plans/f69317d4.md` (post-commit bookkeeping, not part of the reviewed change).

---

### HIGH-1 (landing sync tolerance) — FIXED, verified

**Code** (`src/project/worktrees.rs::land_item_branch_impl`): the sync block (:199-207) now first runs `git rev-parse --abbrev-ref --symbolic-full-name @{u}` and only treats the repo as tracking a remote when that succeeds (`tracks_a_remote`); `fetch origin` and `pull --no-rebase` run **only** inside that branch, each with `.map_err(LandError::Git)?`. The old blanket tolerance (`let _ = ...fetch; let _ = ...pull;`) is gone. The rationale comment (:192-198) documents why the narrow tolerance is correct — "no upstream" is a legitimate local-only-repo shape, while a configured-but-unreachable remote is an infrastructure failure. `LandError::Git` propagation to the caller is unchanged, and the caller's existing handling (git errors surface as infrastructure failures, conflicts keep the worktree) is unaffected.

**Test** (`land_fails_when_a_configured_remote_cannot_be_reached`, :317-345): builds a real bare upstream, `push -u origin main` (so `@{u}` resolves — `tracks_a_remote` is true), provisions the item worktree, then `remove_dir_all`s the upstream before landing and asserts `land_item_branch` is `Err`. Proven by neutralization (round-1 protocol): under the old blanket tolerance the sync would be swallowed and the landing would succeed on stale main, failing the `is_err()` assert. The companion tests are intact and still meaningful: `land_syncs_main_with_its_upstream_before_merging` (:271-314, asserts the upstream commit reaches main's log — fails without the sync) and `land_still_succeeds_without_any_remote` (:348-368, no remote → `@{u}` fails → sync skipped → landing succeeds — pins the tolerated half). Together they pin both sides of the new boundary exactly.

**Docs reconciliation — exact match.** `docs/FEATURES.md:70`: "the landing first syncs `main` with `origin` (`git fetch` + `git pull --no-rebase`), skipped when `main` tracks no upstream (local-only repos) while a genuine fetch/pull failure surfaces instead of landing on a stale `main` (2027-01-11)". `PLAN.md:320`: "The sync is skipped when `main` tracks no upstream; once it tracks one, a fetch/pull failure is surfaced rather than swallowed." Both now state the implemented semantics precisely (previously FEATURES.md promised this while the code was broader — round-1 HIGH-1; the code now matches the docs). I checked for other landing-sync claims: these are the only two spots plus agent.md (which describes only the skill's sync, agent-driven and unaffected).

### HIGH-2 (stale trace doc bullet) — FIXED, verified

`src/provider/trace.rs::write_records_to_file` doc, the "Deliberate debugging-aid tradeoffs" bullet (:1608-1613) now reads: "**The write is atomic** — the merged file goes to a temp file in the same directory and is renamed over the target, so a reader sees either the old file or the new one, never a truncated or half-written one (`write_mirror_atomically`). A failed temp write or rename leaves the previous mirror in place, intact, and the batch is retried on the next writer wakeup." That is exactly what the implementation does (:1713-1723: failed temp write → temp removed, error returned; failed rename → temp removed, target untouched). The lead-in also no longer claims the writer is synchronous (:1598-1599 rewritten as round-1 asked). Repo-wide search for "not atomic" over `**/*.rs`: zero matches.

### LOW-1 (skill file text) — FIXED, verified

`.coding/skills/merge_to_main.toml` rationale comment (:30-35) and the injected prompt's step 3 (:59) both now say the **git** tool must run the merge/push and that **both** tools force the core-operation prompt ("Both tools force the always-on prompt: ShellTool::never_auto_for was added 2027-01-11, closing the bypass this comment originally described" / "use the **git** tool, NEVER shell: both force the core-operation approval prompt, but the git tool is the structured, subcommand-checked path"). The false "shell does not force the prompt" claim is gone from both spots; the retained git-tool preference is now justified on structure grounds, which is the correction round-1 asked for. See LOW-2 below for a cross-reference nit in this comment.

### LOW-2 (ToolFilter doc rot) — FIXED, verified

`src/tool/mod.rs`: the `ExecutingResearch` doc (:204-226) now carries the "DISPATCH-ONLY since 2027-01-11" paragraph — no stale "chosen once at plan activation by plan kind" clause; the restriction statement is accurate ("still enforced here, at the gate"). The `PlanFrozen` doc (:227-253) now says the array is "the advertised surface for EVERY active plan, research plans included" with research write-restriction enforced at dispatch, matching `Workflow::schema_filter` (spot-checked in the final tree: `PlanFrozen` for `Executing|Reviewing` with plan, allow-list short-circuit, else `allowed_tools` — unchanged from round-1's verification). The ":210 advertisers" typo now reads "advertises"; repo-wide search for "advertisers" returns only the round-1 report itself quoting it.

### LOW-3 (temp name) — FIXED, verified

Production: `write_mirror_atomically` temp name is `format!("{file_name}.tmp{}", std::process::id())` (`trace.rs:1712`), with both round-1 suggestions in the doc: the Windows rename-failure caveat (reader holds the target open without FILE_SHARE_DELETE → rename fails, previous mirror intact, batch retried, :1696-1699) and the PID rationale (two app instances could interleave temp writes, :1700-1703). **Drift guard:** both tests use the identical expression — `:2578` (`assert!(!dir.path().join(format!("traces.jsonl.tmp{}", std::process::id())).exists())`) and `:2600` (`std::fs::create_dir(dir.path().join(format!("traces.jsonl.tmp{}", std::process::id())))`). They are literal copies of the production expression rather than derived from a shared constant, so they *could* textually drift — but the meaningful direction is pinned: in `mirror_write_failure_leaves_the_previous_mirror_intact` the pre-created directory only blocks the write if production still uses exactly `traces.jsonl.tmp<pid>`; if production's naming ever changed, the collision would vanish, the temp write would succeed, the rename would overwrite the target, and the assert (target unchanged) would fail. The fragment test's no-residue assert (:2578) would pass trivially under a renamed temp, but the failure test carries the name-pinning. Acceptable; both current expressions match production character-for-character.

---

## New defects from the fix commit

### LOW A — BUG knowledge record still describes the old temp name

`.coding/knowledge/bug/2027-01-11-traces-jsonl-mirror-rewrote-the-whole-file-with.md:10` (committed in this commit) describes the R6-3 fix as: "`write_mirror_atomically(path, content)` — write `<name>.tmp` in the same directory, `restrict_log_file` the TEMP …". The shipped code writes `<name>.tmp<pid>` — and the PID-uniqueness is precisely the mechanism that fixes the two-writers interleaving LOW-3 addressed, while the pre-created-directory test keys on the exact name. The record's FIX paragraph should say `<name>.tmp<pid>` (one clause). Everything else in the record (atomic rename, fragment drop on read, both test names + neutralization proofs) is accurate. One-line knowledge-file amendment; no code impact.

### LOW B — "(HIGH 1, review 2026-09-14)" citation in the skill comment is imprecise

The rewritten `.coding/skills/merge_to_main.toml` rationale (:30-35) cites "(HIGH 1, review 2026-09-14)" for "the prompt names the TOOL for every git step". The review it means is `.coding/reviews/2026-09-14-merge-to-main-remote-sync-review.md` (plan d826b9ad), whose HIGH 1 is indeed exactly about which tool executes the git steps — so the substance is right and the citation is real. The nit: three reviews are dated 2026-09-14 (this plan's own round-1 among them, whose HIGH 1 was the landing sync), so an unqualified "review 2026-09-14" can mis-point a future reader. Naming it ("d826b9ad review") or the file stem would remove the ambiguity. Cosmetic; no functional impact.

---

## Plan substance still holds (spot-checked in the final tree)

- **Frozen schema surface (R6-2):** `Workflow::schema_filter` returns `PlanFrozen` for `Executing|Reviewing` with an active plan (allow-list short-circuit first), else `allowed_tools`; `ToolFilter::ExecutingResearch` remains a dispatch-time gate (`schema_filter_is_stable_across_plan_kinds` still pins the byte-stable array). Round-1 verified this in depth; the fix delta did not touch it.
- **Shell core-op guard (follow-up A):** `command_is_core_git_op` (`src/tool/agent/shell.rs:72-111`) unchanged from round-1's verification: splits on `; | & LF CR`, requires `git`/`git.exe` as the segment's first token, steps over value-taking global options, matches subcommands case-insensitively against the shared `[git] core_operations` list — `git status`/`git log` never gate (non-core subcommands), and `git pull --no-rebase` gates only because `pull` is in the core list (documented, matching the git tool's own semantics). The conservative residual (alias/wrapper/`sh -c`) is documented at :67-71. No false positives introduced.
- **Landing sync:** verified above (HIGH-1).
- **StatusBar comments + docs:** the comment-only StatusBar/doc updates from the plan were verified in round 1; the fix delta did not touch them. `README.md` makes no landing/merge-into-main claims, so no sync owed there. The post-restart effect check exists (`cache-hit-6-report.md:124-146`, three concrete PASS criteria with the in-tree guard names) as plan step 6 required.

## Constitution checks

- **Documentation sync:** FEATURES.md, PLAN.md, agent.md, merge_to_main.toml, StatusBar comments, knowledge records, and the effect-check appendix are all present and consistent — except the two LOW nits above (knowledge-record temp name, citation precision).
- **Multi-platform neutrality:** the landing tests use `tempfile` + real git + `std::fs` only (no Windows-only paths or shell syntax); the trace temp/rename mechanism and its documented Windows caveat are platform-neutral descriptions of platform-accurate behavior; no new `cfg(windows)` code.
- **Warning-free build:** not re-run per instructions; judged by inspection — the new test code has no unused imports/dead code, doc-comment edits only where stated, and the tree's `cargo test` (2337 passed / 0 failed / 5 ignored + 16 integration) plus frontend (1106 tests, clean `npm run build`) is consistent with the code as read.
- **File-tools-first:** the fix delta is source/doc/comment edits; no shell-based file mutation anywhere in non-test code.