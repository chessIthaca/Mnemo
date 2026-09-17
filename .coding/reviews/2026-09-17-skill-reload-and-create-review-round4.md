## Verdict: FINDINGS (0 high, 1 low)

Round 4 reviewed the working tree at `C:/Mnemo` (branch `wt/mnemo`, HEAD `630ec31`, everything uncommitted — `git diff HEAD` = 19 files, 1469 insertions; the untracked members of the change set are this plan's own artifacts: `.coding/plans/8a95a233.md`, the three prior review reports, and the feature's SPEC record). Scope: the round-3 fix round (sandbox directory gate, two new regression tests, one strengthened assertion, one knowledge record) plus the standing checks across the whole diff.

**All four round-3 findings are closed and verified by trace, not by assumption.** The one new finding is LOW and is a *test-coverage* gap, not a code defect: the round-3 LOW 2 refusal — the security-relevant half of the gate rewrite — is unpinned, so deleting its arm keeps the suite green.

## Closure of the round-3 findings

### HIGH 1 + LOW 2 — `validate_dir_creation` rewritten (src/tool/agent/sandbox.rs:203-232) — CLOSED

`is_link_free_as_spelled` is gone from the source (repo-wide walk over `src/**/*.rs`: zero hits; only the plan file and the SPEC record name it, as deleted). `validate_for_write` moved `:511` → `:497`, exactly the 14 lines the helper + its doc occupied — the ladder itself is byte-unchanged. The `Ok` arm is unchanged (canonical + `refuse_if_protected`); the `Err` arm now judges `canonical_existing_ancestor(dir)`: outside the root → `Error::PathOutsideRoot(ancestor)`; then a **retained** `PathOutsideRoot` on the dir itself → `Err(e)`; inside the root → `refuse_if_protected(&ancestor)?` (the ladder's step 2b); no ancestor → `Ok(dir)`.

**Every refusal the round-2/3 version had is preserved** (traced case by case):

| case | `validate(dir)` | current verdict |
|---|---|---|
| `<root>/.coding` is a link to an existing dir `D` outside | parent exists → canonicalize → `D/skills` not under root → `PathOutsideRoot` | ancestor = `<root>/.coding` → canonicalizes to `D` → outside root → **refused** (containment arm) |
| a dir **link leaf** pointing outside (`<root>/.coding/skills` → outside dir) | candidate exists (link follows) → canonicalize → outside → `PathOutsideRoot` | ancestor = `<root>/.coding` (inside root) → **retained error** → **refused** |
| a dir link into a protected tree (`…/skills` → `.coding/knowledge`) | candidate exists → canonicalize → `.coding/knowledge` → inside root → `Ok` | `Ok` arm → `refuse_if_protected` → **refused** |
| the Win32 8.3 spelling | not this function's surface (it is the *file* ladder's step 2b, untouched) | **refused** by `validate_for_write`'s canonical check (unchanged) |

**No new allowance.** The case round-3 LOW 2 named — an out-of-root dir whose parent chain is entirely missing — is now refused: `dir = D:/other/missing/sub/skills` → `validate` errors ("parent does not exist") → `canonical_existing_ancestor` walks `…/sub` → `…/missing` → `D:/other` (exists) → canonicalizes → not `starts_with(root)` → `PathOutsideRoot` **before any mkdir**. `..` spellings fold through the canonicalized ancestor the same way. A dangling dir link is still waved through the gate but `create_dir_all` fails on it (EEXIST + `!is_dir`), so nothing is created — the round-3 status quo for that case, deliberately unchanged. The theoretical odd spelling is unreachable from the tool: `dir` is `SkillLibrary::dir()`, i.e. the absolute `project.skills_dir` (`src-tauri/src/main.rs:1762-1767`), never caller-controlled. Note the returned path is *unused* by the caller (`src/tool/workflow/skill.rs:579` keeps the spelling), so the gate is a pure referee — as the round-2 design intended.

**macOS false refusal is fixed:** `<tempdir>/.coding/skills` with `.coding` absent → `validate` errors → ancestor = the tempdir, canonicalized to `/private/var/…` = the canonical root → `starts_with` ✓ → `refuse_if_protected` (tempdir unprotected) → `Ok` → the three happy-path `skill_create` tests stop being platform-dependent. The Windows `\\?\` verbatim-vs-plain trap is avoided because the `Err` arm compares only canonicalized things (ancestor-canonicalize vs canonicalized root) and never calls `validate_for_creation`.

**The retained-error arm is reachable and correct** — and it is pinned: `dir_creation_gate_refuses_a_link_component` (:1665-1669) plants `<root>/.coding/skills_outside` → an existing dir outside the root, which `validate` reports as `PathOutsideRoot` with the ancestor *inside* the root — i.e. the retention is what refuses it. Verified ordering: containment first, retention second, protection third; a `PathOutsideRoot` whose ancestor lies outside the root is refused either way (different payload, same refusal).

### LOW 3 — `create_skill_refuses_a_linked_skills_dir` is no longer vacuously green — CLOSED

`src/tool/workflow/skill.rs` (tests, the assertion added right after `!r.success`) now asserts the tool output contains `refused to create`. The emitting site is the directory gate alone (`src/tool/workflow/skill.rs:580`); a repo-wide search finds the string in exactly two places — the emit and that test. Pre-fix trace (gate deleted): `create_dir_all(dir)` on a junction at the skills dir returns `Ok` (`create_dir` → EEXIST, then `is_dir()` → true), and the refusal then comes from `validate_for_write`'s step-2 link-free gate — `"…traverses a symbolic link or junction…"` wrapped as `refused to write …` (`:591`). That output does **not** contain `refused to create`, so the new assertion is exactly what fails pre-fix (the old `!r.success` was already true pre-fix, which was the round-3 complaint). Wording and assertion match.

### LOW 4 — the SPEC knowledge record — CLOSED, fallback justification accepted

`.coding/knowledge/spec/2027-01-11-skill-reload-skill-create-over-a-reloadable-skil.md` (10 lines; **untracked** — `git log -- <path>` returns no commits, which is also why it is absent from `git diff HEAD`'s 19-file stat):

- Front matter intact (`title = "skill_reload + skill_create over a reloadable SkillLibrary"`, `created = "2027-01-11"`, no BOM).
- The gist (line 6) item (2) now carries the pre-mkdir `Sandbox::validate_dir_creation` clause.
- The round-1 amendment (line 8) no longer calls the "created BEFORE validation" ordering deliberate: it now states "The skills dir is validated BEFORE the mkdir by `Sandbox::validate_dir_creation` … which both reviews superseded", pointing at the end-of-record amendment.
- Line 10 is the appended `Amended 2027-01-11:` round-3 paragraph (grep-deleted helper, `canonical_existing_ancestor` containment + canonicalization of the existing ancestor, the `PathOutsideRoot` retention, the two new tests, the `refused to create` assertion — all accurate against the code I traced).
- Encoding intact: no mojibake, no BOM, and a `\r` search over that file returns zero matches → LF preserved, as disclosed.
- Items (1)/(3) of the gist are untouched (including the pre-existing `SKillStartTool` typo — cosmetic, pre-dating this round).

**Fallback justification: sufficient.** The record has **no row in the memory index** — an independent prefix browse (`SPEC: skill`) returns only the unrelated `SPEC: skill target_state validation layers …` record — so `memory_amend` had no id to target, and `memory_update` (including its new `find`/`replace_with` mode) is id-keyed as well. The file tools refuse `.coding/knowledge/**` by design (`src/tool/agent/sandbox.rs:378-385`). That is precisely the constitution's knowledge-file fallback case, and it was executed with the sanctioned staging writer (`file_write` into `.coding/tmp/*.txt`) plus explicit UTF-8 splicing. `.coding/tmp/` now holds **0 entries** (the directory itself pre-dates this session — a 2026-09-13 review references its scratch logs), and empty directories are invisible to git, so the closing commit cannot sweep residue. No other file's content changed in a way attributable to the splice: the other knowledge/plan diffs are the close-out bookkeeping for plans `ecdd67ac` / `b4812291` / `60f02c26` and match the merged commits' story (the `MEMORY … — MERGED into main (630ec31)` titles, the two b4812291 plan ticks, the backlog soft-deletes).

## NEW FINDING

### LOW 1 — the round-3 LOW 2 refusal is unpinned: no test exercises the containment arm, so it can be deleted with a green suite

- **Where:** `src/tool/agent/sandbox.rs:203-232` (the `Err` arm's out-of-root containment check) — the tests that should pin it live at `:1642-1720` (`dir_creation_gate_refuses_a_link_component` :1650-1685, `dir_creation_gate_allows_a_link_above_the_root` ~:1689-1720).
- **Why it is wrong:** the only *out-of-root* cases the suite exercises are **link-mediated**. `dir_creation_gate_refuses_a_link_component` plants `skills_outside` → an existing outside dir, which `validate` already reports as `PathOutsideRoot` while its ancestor (`<root>/.coding`) is inside the root — that refusal comes from the **retained-error** arm, not the containment arm. `skills_knowledge` → `.coding/knowledge` is refused by the `Ok` arm's `refuse_if_protected`. Grep confirms the only `validate_dir_creation` call sites in tests are :1656 (allow), :1667 (retained-error refusal), :1680 (Ok-arm refusal), :1707 (allow). Delete the `!ancestor.starts_with(&self.root)` arm and the suite stays green while the round-3 LOW 2 defect — an out-of-root `mkdir` with a missing parent chain — silently reappears. The project's code-style rule is explicit that every fixed defect gets a regression test that fails without the fix.
- **Concrete fix (test only — the code is correct as landed):** add to the sandbox test module, next to `dir_creation_gate_refuses_a_link_component`:

```rust
#[test]
fn dir_creation_gate_refuses_an_out_of_root_dir_without_any_link() {
    // No link anywhere: the escape is invisible to `validate` (the parent
    // chain is missing entirely), so only the canonical-ancestor CONTAINMENT
    // check can see it (round-3 LOW 2).
    let dir = tempdir().unwrap();
    let outside = tempdir().unwrap();
    let sandbox = Sandbox::new(dir.path()).unwrap();
    let target = outside.path().join("missing/sub/skills");
    assert!(
        matches!(
            sandbox.validate_dir_creation(&target),
            Err(Error::PathOutsideRoot(_))
        ),
        "an out-of-root mkdir must be refused by containment, not by a link walk"
    );
}
```

  Assert the **variant**, not a bare `is_err()`: pre-fix on Windows the call returned `Ok(dir)` (arm `_ => Ok(dir.to_path_buf())`, the exact path round 3 flagged) and pre-fix on macOS it returned the spelled walk's link refusal — a bare `is_err()` would be green pre-fix on macOS and pin nothing, whereas `Err(Error::PathOutsideRoot(_))` fails pre-fix on **both** platforms and passes now. `Error` is already in scope in that module (`use crate::error::{Error, Result}` + `use super::*`).

## Standing checks on the whole diff (all uncommitted changes)

- **Correctness / bugs / security.** The gate rewrite is analysed above; no new allowance, and the four prior refusals hold. `validate_dir_creation` has exactly **one** production caller (`src/tool/workflow/skill.rs:579`) plus the four test sites, and `validate_for_write` is unmodified by this round — its five-step ladder (step-2 link-free gate, step-2b canonical ancestor, step-5 canonical re-check, the one re-checked lexical fallback) is intact for `file_write`, `file_edit`, `file_append`, `convert_line_endings`, so nothing was weakened for their callers. `skill_create` still refuses `write_review_report`-adjacent surfaces and shipped skills (`is_shipped_skill`, `src/skill/mod.rs:297-299`).
- **Lock discipline.** No `std` guard can cross an `.await`: `SkillLibrary::read` (`src/skill/mod.rs:215-218`) keeps the guard inside the closure, `reload` (`:229-254`) builds the fresh registry *before* taking the write lock and swaps with a single assignment, `insert` (`:259-262`) is one scoped assignment. `enter_skill` uses the closure form on both reads (`src-tauri/src/ipc/agent.rs:845` and the `is_available_in` read) before any awaited lock. The fix round touched no async code at all.
- **Reviewer surface unchanged.** `src-tauri/src/ipc/spawn.rs` only adds `skill_reload`/`skill_create` to the *must-not-appear* assertion list; `REVIEWER_BASE_TOOLS` is untouched, and the reviewer-only `write_review_report` guard (`src/tool/mod.rs:340-342`) still fires before any arm.
- **Executing-only enforcement intact.** Hard guard `src/tool/mod.rs:349-353` denies `skill_create` under `ToolFilter::Skill`/`Reviewer` regardless of an allow-list naming it; per-state arms :433 (Executing), :479 (ExecutingResearch), :511 (PlanFrozen advertisement only); the test at `:2003-2029` pins Planning/Reviewing/Complete/Skill/Skill-with-name/Reviewer. `skill_reload` is in every arm (Planning :384, Executing :432, ExecutingResearch :478, PlanFrozen :510) and in the Skill arm's always-available set (its test at :1981-1994 also pins that a reviewer does **not** get it implicitly).
- **No new AutoRun escape hatch.** `skill_create` stays AutoRun (the design decision reviewed in rounds 1-3), but the write is sandbox-mediated end to end: the directory gate, `validate_for_write` (link-free creation ladder + the `.coding/**` hardlink guard) and the shipped-skill refusal. The one behaviour delta versus round 2 is *more permissive*, not less: an in-root **unprotected** dir link (e.g. `.coding/skills` → another directory inside the project) is followed, because the `Ok` arm only refuses a *protected* canonical target. That is defensible and consistent — the library reads through the same path, so refusing the write would make such a config un-authorable — and it is recorded below rather than raised, since it predates this round (the round-2/3 `Ok` arm was identical) and does not cross the sandbox's two invariants (containment, protection).
- **Path derivation.** `dir = library.dir()` = `project.skills_dir` (`src-tauri/src/main.rs:1762-1767`), the same tree `load_dir` reads, and the write target is `dir.join(format!("{name}.toml"))` with `validate_skill_name` restricting the stem to `[a-z0-9_-]`, alnum-first, ≤64 bytes — no traversal, no separators, no `..`/`:`/`~` can enter the filename.
- **Factory wiring.** `register_skill_tools(&mut registry, workflow, &sandbox)` (`src/agent/factory.rs:849`) receives the per-agent sandbox chosen at `:832-835` (a root-spec agent gets its worktree sandbox), and `SkillCreateTool` is constructed with `sandbox.clone()` (`:1084-1087`) — the HIGH-1 contract from round 1 stays in place.
- **Multi-platform neutrality.** The new gate code is platform-neutral (`canonical_existing_ancestor` + `Path::starts_with`); both new tests plant their links through the shared `link_fixture` (cfg-split *inside* the fixture), so no `#[cfg(windows)]` leaked into library or app code, and the macOS `/var` → `/private/var` regression is now exercised by both new tests. Skip paths are visible: both print an explicit `SKIP:` line and return before the assertions, and the fixture's own fallbacks do the same.
- **File-tools-first policy.** The knowledge-record fallback is the only shell mutation in this round and the disclosure around it is accurate and justified (see LOW 4). No `Set-Content`/`Out-File`/`sed -i`-style mutation appears in non-test, non-generated code.
- **Docs sync.** `PLAN.md:292-303`, `docs/FEATURES.md:20`, the `src/skill/mod.rs` module doc + `SkillLibrary`/`is_shipped_skill` doc comments, and the `src/agent/prompt.rs` skill hint all match the code; the mechanism-level contract lives in `validate_dir_creation`'s own doc comment, which is accurate for both the `Ok` arm and the `Err` arm. The fix round changed mechanism only, so no user-facing text went stale — with the one nuance recorded below.
- **Test genuineness.** See the table below.

| test | would it fail pre-fix? | skip visible? |
|---|---|---|
| `sandbox::tests::dir_creation_gate_allows_a_link_above_the_root` (~:1689-1720) | yes — plants `holder/alias` as a dir link and boots the sandbox on the *alias*, so the pre-fix walk from the filesystem root hits the `alias` component and refuses while the post-fix ancestor check canonicalizes it to the root | yes (`SKIP:` marker + early return) |
| `sandbox::tests::dir_creation_gate_refuses_a_link_component` (:1650-1685) | yes — pre-fix refusals came by accident, but with the *new* code the first case is refused by the retained-error arm and the second by the `Ok` arm; both still fail if either arm regresses | n/a (no link planted outside the fixture) |
| `tool::workflow::skill::tests::create_skill_works_under_a_symlinked_ancestor` | yes — pre-fix the gate refuses (`refused to create`) and `assert!(r.success)` fails; post-fix the artifact is asserted to land in `<real>/.coding/skills` | yes |
| `tool::workflow::skill::tests::create_skill_refuses_a_linked_skills_dir` (strengthened) | yes — pre-fix wording is `refused to write … traverses a symbolic link`, so the new `contains("refused to create")` assertion fails | n/a |

## Not findings — recorded for the maintainer

1. **Doc precision (optional tightening).** `PLAN.md:299` and `docs/FEATURES.md:20` say "a planted symlink … at the skills dir is refused". Exact for the two enforced cases (a link leaving the root, a link into a protected tree); a link pointing at an *unprotected in-root* directory is followed, as analysed above. Deliberate and pre-existing — tighten the sentence only if exactness matters.
2. **`.coding/tmp/` still exists, empty.** The fallback deleted its files (0 entries); the directory pre-dates this session and git ignores empty directories, so nothing can be committed from it.
3. **The SPEC record still has no memory row.** Startup reconciliation should re-derive it at the next project open (content-hash drift); until then `memory_amend` cannot target it — worth knowing if it needs another amendment before the commit.
4. **The code-graph index is stale for this area.** A live `graph_search` still reports `is_link_free_as_spelled` at `sandbox.rs:235-246` as a callee of `validate_dir_creation` (placed at `:197`), i.e. the pre-fix revision. Harmless — `codegraph.db` is a gitignored, rebuildable cache (rebuilds at project open, or Settings → Memory → "Rebuild index from files") — but no one should trust graph answers for `sandbox.rs` until then, and the closing sequence's graph-based checks should be read with that in mind.
5. **`Sandbox::new` on a not-yet-existing root** skips canonicalization, and the containment check then compares a canonical ancestor against that non-canonical root: an in-root dir under such a sandbox is refused. Unreachable for `skill_create` (the project root exists whenever an agent runs) and fail-closed in the safe direction; recorded so a future init-time caller is not surprised.

## Verification caveat

This reviewer is read-only by construction and has no shell, so the suites were **not** re-run here. I relied on the parent's evidence (root `cargo test`: 2406 run / 2401 passed / 0 failed / 5 ignored + 16 integration; `src-tauri`: 299 passed / 0 failed + aux; targeted `sandbox` 41 and `skill` 58 green; both crates `#![deny(warnings)]`) and on my own traces for the would-fail-pre-fix claims above. The knowledge record is untracked (no git history for that path), so a byte-level before/after comparison is impossible from git; the integrity checks listed under LOW 4 are what was verifiable, and they pass.
