## Verdict: FINDINGS (0 high, 3 low)

The .git sandbox-escape fix is correct and complete against the reviewed attack surface: the predicate change covers all five write gates (agent file tools + IPC mirror, including the approval-preview paths), the creation gap is closed for exact `.git` components, reads stay allowed, and the regression tests genuinely fail without the fix. The `--no-verify` addition is correct for the hooks it targets. Three low findings: a narrow Windows trailing-dot/space residual in the creation path, an overstated code comment (`--no-verify` does not suppress `prepare-commit-msg`), and doc-sync (two call-site comments + the README security bullet). Bookkeeping files are undamaged.

### Scope reviewed

- `git diff HEAD` at c4d7afd + untracked `.coding/plans/eda38891.md`: `src/tool/agent/sandbox.rs` (+128), `src-tauri/src/ipc/files.rs` (+16), `src/tool/agent/git.rs` (+9/−3), `.coding/backlog.jsonl` + `.coding/plans/4422c64c.md` (bookkeeping).
- Cross-checked against `.coding/reviews/2026-09-09-security-review.md` (HIGH-1), the executive summary BUG 1, and git's published githooks semantics (web_fetch, git-scm.com/docs/githooks).
- Tests: not re-run by this reviewer (read-only); the parent's stated green runs (root 2157+16, src-tauri 292+4+2, exit 0 unpiped) are relied upon, and the new tests' assertions were hand-verified against the code.

### Verification — what holds

**Predicate coverage (all write gates).** Graph-verified callers of `is_protected_write_target`: exactly `validate_for_write` (used by `file_write::execute` + `file_append::execute`) and `refuse_if_protected` (used by `file_edit` execute + `prepare_for_approval`, `file_write::prepare_for_approval`, `convert_line_endings::execute`, and the IPC mirror `write_sandboxed`, src-tauri/src/ipc/files.rs:131). One predicate change covers every write path; the approval-preview paths refuse too, so a `.git` write never even reaches an approval prompt. No other write-ish tool in `src/tool/agent/` bypasses the predicate (`write_review_report` writes only `.coding/reviews/` through its own gate; the memory tools write `.coding/knowledge/` via their own writer; `shell` is the documented approval-gated standing residual).

**The check itself.** `rel_str.split('/').any(|c| c == ".git")` (sandbox.rs:248) runs on the root-stripped, backslash-normalized, ASCII-lowercased relative path — so it sees `.git` as a component for the root control-plane dir and all its contents, a nested repo's `.git` (`vendor/repo/.git/config`), and the linked-worktree plain `.git` FILE (single component). Component equality keeps `.gitignore`/`.gitattributes`/`.gitmodules` (tested), `.github`, and bare-repo-style names (`foo.git`) writable. This is broader than the security review's suggested root-prefix check, matching the backlog item's instruction ("any target whose canonical path contains a .git component").

**Creation gap.** `validate_for_write` checks the predicate at step 3, BEFORE `create_dir_all` (the new test asserts `.git` is not created) — a non-existent `.git/hooks/pre-commit` is refused lexically via `validate_for_creation`. Closed for exact components, including case variants (`.GIT/config` — caught by the pre-existing lowercase; the test is purely lexical, no NTFS dependence, so it is platform-neutral) and ADS forms (`.git/hooks/x:evil` — caught by the pre-existing colon guard).

**Existing-path variants.** For an existing `.git`, `validate` canonicalizes: `.git./config` resolves through Win32 normalization to the real `.git/config` and is caught. 8.3 short names are not a vector (`.git` is already a valid short name; canonicalization resolves any alias for existing paths; a newly created `GIT~1`-style dir is a distinct name, not an alias). A user-created `.git` symlink to an in-root directory canonicalizes to the target and escapes the component check — but the file tools cannot create symlinks, so it is outside the auto-approve threat model (noted for completeness only).

**Reads unaffected.** The predicate gates writes only; `read_files`/`file_read`/`search` never call it — matching the security review's explicit ask that `.git/config` stay readable.

**--no-verify.** Correct for what it targets: per githooks(5), `--no-verify` bypasses pre-commit, commit-msg (commit and merge), and pre-merge-commit. Both merge arms and the commit arm carry it; argv order is fine; no test pinned the old commit/merge argv (search-verified — the only `no-verify` matches outside git.rs are plan/review/backlog docs).

**Refusal message.** The new wording keeps "protected"; no test pinned the old full string (a search for the old phrase hits only historical plan/review docs and two code comments — see finding 3).

**Bug-plan checks.** The regression tests exercise the changed path at every layer (predicate via `validate` + `validate_for_creation`; ladder via `validate_for_write` including the no-mkdir assertion; `refuse_if_protected` gate; IPC mirror including the not-over-blocked `.gitignore` assertion) and fail without the fix (before the change `is_protected_write_target(.git/config)` returned false, the ladder created dirs and succeeded, and `write_sandboxed` succeeded). The root cause is documented in the plan's Bug section (symptom → root cause → fix, complete) — sufficient for the finish-time BUG: auto-capture.

**Bookkeeping sanity.** `backlog.jsonl`: the new item 0296d448 matches the plan text; two dispatch deletions carry timestamps; all lines are well-formed JSON. `4422c64c.md`: one checkbox flip. No accidental damage.

**Multi-platform neutrality.** The fix is pure string matching; no Windows-only APIs; the case-variant test relies on the predicate's lowercase, not NTFS behavior; tempdir-based tests are neutral. (Finding 1 is a Windows-only *gap* in coverage, not an added platform assumption.)

### Findings

**1. LOW — Windows trailing-dot/space component variant reopens the .git creation gap (narrow)**

- Location: `src/tool/agent/sandbox.rs:248` (the `.git` component check) + `:125-138` (`validate_for_creation`).
- Symptom: Win32 path normalization strips trailing `.` and ` ` from every path component, so `mkdir(".git.")` creates the real `.git`. `file_write(".git./hooks/pre-commit")` (or `.git./config`, `.git /...`) in a project root whose `.git` — or the intermediate dir — does not yet exist: `validate` fails (nothing resolves) → `validate_for_creation` accepts `.git.` lexically (component ≠ `.git`) → predicate passes → step 4 `create_dir_all` creates the REAL `.git/hooks` → the revalidated canonical path is written → hook/config planted. In an ordinary repo this is closed by existence (`.git/config` and `.git/hooks` exist, so `validate` resolves `.git.` → `.git` through parent canonicalization and the predicate catches the canonical path); the gap needs a not-yet-initialized project root (the app supports it — the Diff tab's "Initialize Git Repository" button; `git init` on a pre-existing `.git` preserves planted hooks/config) or a repo missing `.git/hooks`. Windows-only (macOS/Linux treat `.git.` as a distinct name). Same hardening class as the case-lowercase and ADS-colon guards the predicate already carries.
- Root cause: the lexical creation-path normalization does not model Win32 trailing-dot/space stripping, so a component the FS will resolve to `.git` never equals `.git` in the comparison.
- Suggested fix: trim trailing `.` and ` ` per component before the comparison — `rel_str.split('/').any(|c| c.trim_end_matches(['.', ' ']) == ".git")` — plus a regression test (`validate_for_creation(".git./config")` → protected; ideally also a trailing-space variant). Applying the trim to the whole `rel_str` would give the `.coding` exact matches the same robustness (their dirs always exist in practice, so that part is belt-and-braces). The only over-block is a file literally named `.git.`/`.git ` on macOS/Linux — negligible.

**2. LOW — the `--no-verify` comment overstates the guarantee: `prepare-commit-msg` (and post-commit/post-merge) still execute**

- Location: `src/tool/agent/git.rs:690-692` (comment above the commit arm).
- Symptom: the comment claims "planted hooks must never execute via the agent's git path", but per githooks(5) `--no-verify` bypasses only pre-commit, commit-msg, and pre-merge-commit — `prepare-commit-msg` "is not suppressed by the --no-verify option" (it runs on every `commit -m`, source `message`), and post-commit/post-merge always run. A `prepare-commit-msg` hook planted via the documented approval-gated shell residual would still execute arbitrary code through the agent's commit/merge despite this change.
- Root cause: the comment was written from the intent (no hook execution) rather than the flag's documented semantics.
- Suggested fix: reword to name what is actually bypassed ("--no-verify skips pre-commit/commit-msg/pre-merge-commit; prepare-commit-msg and post-* hooks still run — acceptable because .git writes are refused by the sandbox and shell is the documented approval-gated residual"). No behavior change required; alternatively also note the residual in the git.rs module doc.

**3. LOW — doc sync: two ladder call-site comments and the README security bullet don't mention the .git protection**

- Location: `src/tool/agent/file_write.rs:137-139`, `src/tool/agent/file_append.rs:105`, `README.md:33`.
- Symptom: file_write's ladder comment still enumerates the refused set as "protected .coding state/bookkeeping files (memory DB, safety.toml, backlog.jsonl, plan stack)" and file_append's as "protected .coding state/bookkeeping files" — both now stale (the refusal also covers the .git control plane; the file_write enumeration was already incomplete pre-change — reviews/, knowledge/, codegraph.db — so a set-enumeration there keeps drifting). README.md:33's security bullet says "protected `.coding/` live state" — after this change the sandbox also protects the `.git` control plane, which is exactly the kind of security-posture fact the feature list should carry.
- Root cause: the refusal semantics changed in one place (the predicate + shared message) but the per-tool comments and the README bullet were not touched.
- Suggested fix: one-line updates — both comments to "refuses protected paths (.coding state/bookkeeping or the .git control plane)" (or just point at `is_protected_write_target`'s doc instead of enumerating), and README.md:33 to mention the `.git` control plane alongside `.coding/` live state.

### Observations (no finding — for the parent's closing sequence)

- **Two commits:** the plan (and backlog item) require the `--no-verify` change to land as a SEPARATE commit from the sandbox protection. Currently one working-tree delta — split at commit time (1: sandbox.rs + files.rs; 2: git.rs).
- **App-internal git calls:** the app's own git invocations (run-all checkpoint commits, the merge_to_main skill) do not pass `--no-verify` either. Out of this plan's scope (the planting vector is now closed; shell remains the documented approval-gated residual) — noted for awareness only.
- **BUG: memory:** plan step 2 ("memory_write a BUG: record") is checked, but no BUG record for this fix is findable in the memory store (two targeted searches). The finish-time auto-capture will write it from the plan's complete Bug section — verify it lands.
- **IPC UX:** the FileViewer's Save (`write_file` IPC) now refuses `.git/*` edits with the protected-path error — consistent with the security decision; a user who ever needs to hand-edit `.git/config` does it in an external editor.
- **Test status:** not re-run by this reviewer (read-only); the parent's green runs (root 2157+16, src-tauri 292+4+2, exit 0) are relied upon, and the new tests' assertions were hand-verified against the code.
