## Verdict: PASS

Round-1 delta review of backlog 627ee5d9 (plan 07ff4ea6), branch `wt/mnemo`, range `e61a31d..HEAD` (caaddf6, c463e48, c6e6f8b). Both L4 holes are closed exactly as scoped; no defects found in the delta.

### What I actually read

- `git_read op=diff` (working tree) and `op=show e61a31d..HEAD` (stat + full diffs of all three commits).
- `tests/integration/ci_workflow.rs` — full final state (286 lines).
- `.github/workflows/` directory listing (2 entries: build.yml, codeql.yml) and the `permissions:`/`contents:`/`uses:` lines of both real workflow files (build.yml:40–99 read in full for the permissions block).

The working tree diff is `.coding` bookkeeping only (backlog.jsonl + untracked knowledge/plan files) — out of scope per the task; accuracy-checked below.

### Axis 1 — enumeration is genuinely directory-wide (caaddf6)

- `read_dir` over `{CARGO_MANIFEST_DIR}/.github/workflows`, entry paths collected, filtered by `eq_ignore_ascii_case("yml"|"yaml")` — case-insensitive extension filter confirmed.
- `paths.sort()` before iteration → stable, deterministic messages regardless of OS directory order.
- Missing directory → `read_dir` panics with the path and the io error; directory present but no `*.yml`/`*.yaml` → the `!paths.is_empty()` assert fails with an explicit anti-vacuous-pass message. Loud failure both ways, never a vacuous green. ✓
- The two real workflows (build.yml, codeql.yml) are covered by enumeration; the hardcoded two-file list is gone. ✓
- `ALLOWED_UNPINNED` unchanged (`["dtolnay/rust-toolchain@stable"]`); both real occurrences (build.yml:59, :108) match the exact-string entry after the `- `/`uses:` strips, so the guard stays green on the real tree — consistent with the parent run's "6 passed, exit 0". ✓
- Edge case considered, not a defect: a subdirectory whose name ends in `.yml` would pass the filter and `read_to_string` would panic — a loud failure, not a false pass (GitHub reads only top-level workflow files anyway, so there is no false-green path).
- The pin-rationale doc comment (mutable-tag risk, dtolnay exception, repo-setting rationale) moved intact onto `workflows_pin_actions_to_full_shas` (lines 207–226) and gained the enumeration note; the draft-release test keeps its own rule text (lines 153–170). Nothing lost, nothing misplaced. ✓

### Axis 2 — value assertion is exact (c463e48)

- Block detection unchanged: first column-0 `permissions:`, continuation = following lines that are blank or space-indented (`take_while`). In build.yml that is lines 46–47 (`permissions:` / `  contents: read`); the release job's indented override at :239–240 is correctly not the workflow-level block. ✓
- The value now comes from that block's `contents:` entry via `find_map(strip_prefix("contents:"))`, then trim + quote-strip (`trim_matches('"')`/`trim_matches('\'')`), asserted `== Some("read")`. Verified against the real file: `" read"` → `"read"`. ✓
- `write` / `write-all` / any other value → `Some("write") ≠ Some("read")` → red (matches the recorded `left: Some("write"), right: Some("read")`). Missing block → `None ≠ Some("read")` → same actionable message, which names the fix, the CodeQL alert origin, the write/write-all hazard, and the sanctioned release-job widening. A block that declares other scopes but omits `contents:` also fails — correct, the scope must be explicit. ✓
- Release-job per-job `contents: write` override assertion (`text.contains("permissions:\n      contents: write")`) untouched and still matches build.yml:239–240. ✓

### Axis 3 — documentation

- PLAN.md paragraph (c6e6f8b) now names the enforcement: the guard enumerates every workflow file under `.github/workflows` and asserts the workflow-level `contents:` value exactly, release-job override as the only widening. Accurate against the shipped code; no other claims altered. ✓

### Axis 4 — test honesty / constitution

- The guards ARE the regression tests, and the red/green triangle is consistent with the committed code: a third workflow with an unpinned `uses:` is now enumerated and panics at its file:line (pre-fix it was never read); a workflow-level `write` now fails the exact-value assert. Pre-fix-green-on-doctored is exactly the recorded hole behavior. ✓
- No vacuous test, no `#[allow]`, no new dependencies (delta touches only `tests/integration/ci_workflow.rs` and `PLAN.md`). Multi-platform neutral: `std::fs::read_dir`/`PathBuf`/`eq_ignore_ascii_case`; the `/`-joined path works on both Windows and macOS (and matches the file's pre-existing `concat!` style). Warning-free by the parent run's green `cargo test` under `#![deny(warnings)]`. ✓
- File-tools-first: no shell-based mutation in the delta. ✓

### Bookkeeping accuracy (one line, out of scope)

The uncommitted `backlog.jsonl` diff marks the budget item (a25a5323) done with plan_id 07ff4ea6 / this item's plan title — the plan link landed on the wrong item, which is the already-recorded run-all dispatch bug (BUG memory 69093a21), not a defect of these commits.

### Observation (not a finding — pre-existing, unchanged by this delta)

The permissions guard remains build.yml-only by design (the item scoped hole (b) to the value assertion, not enumeration); if directory-wide permission coverage is ever wanted, that is a separate backlog item.

Reviewed-state: c6e6f8b7cac510cfca3eae9fe31e62fce27fbb1c
