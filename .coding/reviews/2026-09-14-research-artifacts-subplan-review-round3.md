## Verdict: FINDINGS (1 high, 3 low)

Round-3 verification of plan `ee65fd4b` ("Research plans may write .coding artifacts; close the sub-plan review leak", options A + D), branch `wt/mnemo`, working tree, no new commits. Scope: the round-2 fixes for L1/L2/L3, verified by **reading the code**, plus a fresh full-diff pass. **L1 is NOT fully closed:** the fail-open survives for a **dangling link as the leaf** component inside `.coding/`, because `research_write_verdict` trusts any `Ok` from `Sandbox::validate` as canonical — and `validate`'s "canonicalize the parent, append the raw file name" fallback returns a path whose *leaf is still a link*. The guard added in round 2 (`lexical_path_is_link_free`) is consulted only in the second branch, so that spelling reaches `ResearchWrite::Artifact`. L2 and L3 are genuinely closed (greps + reading), the legitimate new-tree artifact write still passes, the file tools' ladder is untouched, the gate order is unchanged, and cache invariant #1 holds. Three low findings: the L1 guard tests can silently no-op on this platform, the new sandbox test panics on a case-sensitive filesystem, and unrelated side-car drift rides the diff.

## H1 (high) — L1 is still reachable: a **dangling link as the LEAF** inside `.coding/` is granted, and the write follows it out of the root

**Exact path and call site:** `research_write_verdict` (`src/agent/dispatch.rs:77-105`) takes its FIRST branch on any `Ok` from `Sandbox::validate` and treats that result as canonical:

```rust
// src/agent/dispatch.rs:88-92
if let Ok(canonical) = sandbox.validate(Path::new(raw)) {
    return judge_research_write(sandbox, &canonical);   // <- no lexical_path_is_link_free here
}
```

`Sandbox::validate` (`src/tool/agent/sandbox.rs:82-113`) is **not** canonical for a leaf that is a reparse point whose target does not resolve:

```rust
// sandbox.rs:90-106
if candidate.exists() { ... }              // false: fs::metadata FOLLOWS the link; a dangling
                                           // symlink has no metadata -> exists() == false
let parent = candidate.parent()...;
if parent.exists() {                       // <root>/.coding exists (a real dir)
    let canonical_parent = parent.canonicalize()?;      // <root>/.coding  (canonical)
    let file_name = candidate.file_name()?;             // "dangling.md"   (raw, unresolved)
    let canonical = canonical_parent.join(file_name);   // <root>/.coding/dangling.md  <-- still a LINK
    return self.check_inside(&canonical);               // inside the root -> Ok
}
```

`strip_prefix`/`starts_with` only compare the string prefix, so the link path passes. `judge_research_write` (`dispatch.rs:111-119`) then asks `is_artifact_write_target` (`sandbox.rs:284-309`): first component `.coding` ✓, a second component exists ✓, `!is_protected_write_target` ✓ → **true → `ResearchWrite::Artifact`**. The gate (`dispatch.rs:231-238`) therefore does not deny, and the call proceeds to approval/execution.

**The write then follows the link out of the sandbox.** `file_write` uses `sandbox.validate_for_write` (`file_write.rs:140`, `sandbox.rs:362-387`): step 1 `validate` → `Ok(<root>/.coding/dangling.md)` via the same parent fallback; step 3 not protected; step 4 parent exists; step 5 revalidate → same `Ok`, so `validated` is the **link path**, and `std::fs::write(&validated, …)` (`file_write.rs:158`) makes the OS resolve the reparse point at open time → the bytes land at the link target, outside the root (on POSIX this creates/dirties `<outside>/missing.md`; on Windows the open follows the junction/symlink target as well). No other layer catches it: the protected check is a no-op on `.coding/dangling.md`, and `is_project_scoped` in the approval path also routes through `validate`, which returns `Ok` for the same reason.

**Why the sibling cases ARE closed (so this is a narrow, specific gap, not a re-opened F2a):** every case where the link is *intermediate* or where the target *exists* makes `validate` fail, which routes the call into the guarded lexical branch:

| spelling (all inside `.coding/`) | `validate` | branch | verdict |
|---|---|---|---|
| `.coding/link/x.md`, `link` → dir **outside** root | parent canonicalizes outside → `PathOutsideRoot` | lexical | link-free walk hits `link` → `NotApplicable` ✓ |
| `.coding/link/nested/x.md` (same link) | parent missing → Err | lexical | walk hits `link` → `NotApplicable` ✓ |
| `.coding/f.md` symlink → existing file outside | `exists()` → canonicalize outside → Err | lexical | walk hits the leaf → `NotApplicable` ✓ |
| `.coding/dangling.md` symlink → **missing** target outside | **Ok** (parent fallback, leaf unresolved) | **canonical** | **`Artifact` → GRANTED ✗** |
| `.coding/dangling-dir/x.md`, `dangling-dir` → missing dir | parent missing → Err | lexical | walk hits the link → `NotApplicable` ✓ |
| link → root's PARENT dir, chain `l1`→`l2`→outside, case variants, ADS/`..` mixes | see notes in §1 below | lexical | all refused ✓ (`..`-after-a-link is safe: the fold is lexical and the tool writes the folded path) |

So the remaining hole is exactly the case the round-2 fix claims to cover and whose regression test asserts `NotApplicable` — for a **dangling leaf**.

**Fix (one line, keeps every legitimate case):** in the canonical branch, also require `sandbox.lexical_path_is_link_free(&canonical)` before judging:

```rust
if let Ok(canonical) = sandbox.validate(Path::new(raw)) {
    if sandbox.lexical_path_is_link_free(&canonical) {
        return judge_research_write(sandbox, &canonical);
    }
    return ResearchWrite::NotApplicable;
}
```

The walk is a no-op for a genuinely canonical in-root path (every component exists and is a real file/dir → `true`), and returns `false` for a link leaf anywhere in the chain. (Alternative, narrower: make `validate` refuse when the leaf's `symlink_metadata` reports a symlink while `metadata` fails.) The existing test `research_write_verdict_fails_closed_on_a_link_out_of_the_root` already asserts the dangling-leaf case, so it will go red→green — **but only where the fixture can actually create a link; see L1 below.**

## L1 (low) — the L1 guard tests can silently no-op on this platform, and the junction spelling is never tested

The two fixtures that pin the round-2 fix both degrade to a no-op when link creation is refused:

- `research_write_verdict_fails_closed_on_a_link_out_of_the_root` (`src/agent/dispatch.rs`, tests module, added in round 2): `return`s early when `plant_dir_link` fails, and `continue`s per entry when `plant_file_link` fails.
- `dispatch_denies_research_write_through_an_escaping_link` (`src/agent/tests.rs`, added in round 2): `if !linked { return; }`.

Both use `std::os::windows::fs::symlink_dir` / `symlink_file`, which on Windows need Developer Mode; without it `CreateSymbolicLink` fails with `ERROR_PRIVILEGE_NOT_HELD`, `is_err()` → skip → the assertions never execute and the suite stays green. That is consistent with the reported green `cargo test`: on this machine the L1 guard is unverified rather than passing. It is also exactly why the H1 gap above survived three rounds. Note the asymmetry: the realistic Windows spelling of this attack is a **junction** (`mklink /J`, no elevation), which the fixtures never create (std has no junction constructor), so the claim the code leans on — "any symlink/junction (Windows junctions report `is_symlink()`)" (`sandbox.rs:311-339`) — is untested for its junction arm (`FileType::is_symlink` does cover `IO_REPARSE_TAG_MOUNT_POINT` in current std, so the code is plausibly right; nothing in the tree pins it).

**Suggestion:** make a refused fixture visible (an explicit skip marker/`eprintln`, not a silent `return`), and add a Windows junction fixture (`std::process::Command::new("cmd").args(["/c","mklink","/J", …])`) so the privilege-free spelling — and the junction arm of `lexical_path_is_link_free` — is actually exercised.

## L2 (low) — the new sandbox test panics on a case-sensitive filesystem

`artifact_target_is_the_coding_tree_minus_protected` (`src/tool/agent/sandbox.rs:633-684`) loops over `[".coding/analysis/cache-report.md", ".CODING/Analysis/Report.MD"]` and asserts BOTH the creation form and the canonical form; the canonical half is:

```rust
// sandbox.rs:644-653
let created = sandbox.validate_for_creation(Path::new(raw)).unwrap();   // portable
...
let canonical = sandbox.validate(Path::new(raw)).unwrap();               // :649 — panics off Windows
```

Only the lowercase `.coding/analysis` is created (`:639`). On a case-sensitive filesystem (Linux, case-sensitive macOS volumes) `.CODING/Analysis` does not exist, so `validate` takes the "parent directory does not exist" arm (`sandbox.rs:108-112`) and returns `Err` → `.unwrap()` panics → red on Linux/macOS. The review checklist requires multi-platform neutrality; the case-variant *predicate* assertion is portable, the case-variant *filesystem* resolution is not.

**Fix:** gate the `validate` half of the case-variant entry on `cfg(windows)` (or create the literal `.CODING/Analysis` directory on POSIX), keeping the `validate_for_creation` half unconditional.

## L3 (low) — unrelated side-car drift rides this diff

`git diff HEAD` also contains `.coding/plans/12284516.md` (+3 lines: a "## Regression test" section appended to a *different* plan's file) plus untracked artifacts of other plans run on this reused worktree (`.coding/analysis/cache-reset-*`, `.coding/knowledge/bug/12284516.md`, `.coding/knowledge/spec/2027-01-11-prompt-cache-resets-*`). Harmless in content and consistent with the new carve-out (these ARE `.coding/**` artifacts), but they will be swept into this commit; state deliberately whether they belong in it. Not raised by rounds 1-2.
