## Verdict: FINDINGS (1 high, 3 low)

Round-3 verification of the uncommitted tree on `wt/mnemo` (plan 8a95a233), focused on the fix round for the three LOW findings in `.coding/reviews/2026-09-17-skill-reload-and-create-review-round2.md`. I read the whole diff, traced every fix in the current source, and did not run any suite (the test evidence in my task brief — root 2404/2399+16, src-tauri 299 + aux, vitest 83 files/1176, `tsc --noEmit`, both crates `#![deny(warnings)]` — is the parent's).

**Fix-round outcome.** Round-2 LOW 2 (silent skip) and LOW 3 (docs) are genuinely closed, and the LOW 1 remedy is materially correct *on Windows*: the gate now runs before the mkdir, it refuses all three named escape shapes, and the model cannot influence `dir`. But the new helper `is_link_free_as_spelled` does **not** faithfully reproduce the ladder's gate — it walks path components **above** the sandbox root, which the ladder deliberately never does — and that (a) falsely refuses the ordinary "skills dir does not exist yet" case on macOS, where `/var`, `/tmp` and `/etc` are symlinks, breaking three of the new create tests there, and (b) drops the ladder's lexical root-containment check, so an out-of-root mkdir is still possible when the dir's parent chain is entirely missing. The test added for the LOW 1 fix is also vacuously green: it passes with or without the gate.

---

## HIGH 1 — `is_link_free_as_spelled` walks ancestors **above** the sandbox root: the ordinary "skills dir does not exist yet" case is falsely refused on macOS

**Where:** `src/tool/agent/sandbox.rs:235-246` (the helper), reached from `validate_dir_creation` (`sandbox.rs:197-226`, the walk at `:206`) and therefore from `skill_create` (`src/tool/workflow/skill.rs:579`).

```rust
fn is_link_free_as_spelled(path: &Path) -> bool {
    let mut prefix = PathBuf::new();          // <-- the filesystem root, not the sandbox root
    for comp in path.components() {           // <-- EVERY component, drive/root included
        prefix.push(comp.as_os_str());
        match std::fs::symlink_metadata(&prefix) {
            Ok(meta) if meta.file_type().is_symlink() => return false,
            ...
```

The ladder's own gate is **root-relative** — `lexical_path_is_link_free` (`sandbox.rs:460-475`) does `path.strip_prefix(&self.root)` and walks from `self.root`, so nothing above the project can ever influence it. The new helper deliberately drops that (its doc at `:228-234` says "walked exactly as spelled"), but the spelled walk starts at `PathBuf::new()`, so a symlink **anywhere in the project's ancestry** — a component that is not part of the project at all — returns `false` and the gate refuses.

**Concrete failure, macOS.** `/etc`, `/tmp` and `/var` are symlinks into `/private/…` on macOS by design, and `tempfile::tempdir()` hands back the un-canonicalized `$TMPDIR` (`/var/folders/…/T/.tmpXXXX`) or `/tmp/…`. In the create tests the library dir is `<tempdir>/.coding/skills` with `.coding/` absent, so `validate(dir)` fails (parent missing, `sandbox.rs:139-143`) and the `Err` arm runs the spelled walk: `prefix` reaches `/var` at its **second** component, `symlink_metadata("/var").file_type().is_symlink()` is true → refuse. Three tests then fail on macOS, each of which is the happy path with the dir absent:

- `create_skill_writes_a_loadable_file_and_hot_adds_it` (`src/tool/workflow/skill.rs:884`) — `assert!(r.success)` fails.
- `create_skill_refuses_to_overwrite_without_the_flag` (`:915`) — first create fails, then `on_disk.get("greet").unwrap()` panics.
- `create_skill_dedupes_states_and_tools` (`:983`) — `assert!(r.success)` fails.

The refusal text is the misleading one ("`<dir>` traverses a symbolic link or junction — name the resolved directory instead") for a directory with no link in it.

**This is a regression introduced by the fix round, not a pre-existing condition.** Before round 3, `skill_create` called `create_dir_all(&dir)` first, so `validate()` succeeded by canonicalizing the now-existing parent (`/private/var/…` agrees with the canonical root) and no spelled walk ran — those three tests passed on macOS. `sandbox.rs`'s own new unit test does not catch it because its positive case (`dir_creation_gate_refuses_a_link_component`, `sandbox.rs:1666-1673`) has `.coding/` **already created**, so it takes the `Ok(canonical)` arm and never exercises the walk.

**Honest scoping.** In production a normally opened project is unaffected: `Project::init` seeds the skills and creates `.coding/` (`src/project/mod.rs:151/164`), so the parent exists and the `Ok` arm runs. The reachable production case is a project/skills dir whose parent chain is missing while an ancestor is a symlink — an uninitialized tree, a worktree agent pointing at a not-yet-initialized main tree, a checkout under a symlinked `~/Documents` / external-volume path, or a `/tmp/...` checkout. Narrow, but it is a false refusal of a legitimate operation, and the test-suite breakage on macOS is immediate. (The round-2 concern that motivated the helper — the verbatim `\\?\` canonical root — is genuinely solved: the `Err` arm never consults `validate_for_creation`, and the tests exercise the walk on this Windows box with no link above the tempdir. Good — the fix is right for Windows and wrong for symlinked ancestors.)

**Concrete fix — fold the ladder's step-2b ancestor check into the `Err` arm instead of walking the whole path:**

```rust
Err(e) => {
    // Not existing yet. Judge the DEEPEST EXISTING ANCESTOR (canonicalized)
    // rather than every component of the spelling: canonicalization resolves
    // the OS-level links the sandbox ignores (`/var` → `/private/var` on
    // macOS), so an ancestor outside the project can never refuse a valid dir.
    match canonical_existing_ancestor(dir) {
        // Outside the root — a skills dir configured outside the tree, or a
        // worktree agent pointing at another tree. Refuse even with no link:
        // the mkdir would leave the project (this also covers the missing
        // parent chain `validate` cannot see — see LOW 2).
        Some(ancestor) if !ancestor.starts_with(&self.root) => {
            Err(Error::PathOutsideRoot(ancestor))
        }
        // Inside the root: refuse an ancestor that resolves into a protected
        // tree (the ladder's step 2b). Everything BELOW the deepest existing
        // ancestor does not exist yet, so it cannot be a link — no walk needed.
        Some(ancestor) => {
            self.refuse_if_protected(&ancestor)?;
            Ok(dir.to_path_buf())
        }
        // Nothing on the chain exists at all — nothing to traverse.
        None => Ok(dir.to_path_buf()),
    }
}
```

(If the spelled walk is kept instead, it must at minimum skip every component at or above the sandbox root, i.e. walk root-relatively — but the ancestor form is strictly better: it also closes LOW 2 and keeps the property that the mkdir base is *checked*, not just spelled. `canonical_existing_ancestor` already exists at `sandbox.rs:732` and is private-to-the-module, so it is callable here.)

**Test that goes with it:** root the library + sandbox at a path reached through a symlinked ancestor and assert the create happy path still succeeds, e.g. on Unix `let alias = tmp.join("alias"); std::os::unix::fs::symlink(&real, &alias)` then build `make_library_tools(&alias)` — today it refuses on macOS and, with the fix, succeeds. (Windows equivalent: the fixture's junction.)

---

## LOW 2 — `validate_dir_creation` still allows an out-of-root `mkdir` when the dir's parent chain is missing, and its doc comment claims it refuses

**Where:** `src/tool/agent/sandbox.rs:213-223`, the `_ => Ok(dir.to_path_buf())` arm at `:222`.

`validate()` can only detect an out-of-root target when the parent chain exists — it canonicalizes the parent and runs `check_inside` (`sandbox.rs:127-137`), and returns `Err(InvalidInput("path's parent directory does not exist"))` when the parent is missing (`:139-143`). That error lands in the `_` arm and is waved through, so `create_dir_all(dir)` still creates the whole chain **outside** the root. The doc comment two lines above claims the opposite:

> Outside the root (a skills dir configured outside the tree, or a worktree agent pointing at another tree's skills dir): the mkdir would leave the project, so refuse it even though no link is involved. **Its parent may well exist**, which is why this cannot be waved through as "merely absent".

The parenthetical is where the reasoning stops: the `PathOutsideRoot` arm covers only the parent-exists case.

**Reachability (honest scoping).** `dir` is never model-controlled (`SkillLibrary::dir()` ← `project.skills_dir`, `src-tauri/src/main.rs:1766`, or test wiring; the model's only input is the validated name). It needs a wiring whose dir sits outside the agent's sandbox root *and* whose parent chain is entirely missing — e.g. a worktree agent whose shared library points at a main tree that has no `.coding/` yet (`SkillLibrary::load` tolerates a missing dir). No file content is written (the file write is still refused by `validate_for_write`, exactly as before), so this is the empty-directory half of round-2 LOW 1 — narrower than that finding, but the function's contract as documented is wrong.

**Fix:** the ancestor check already proposed for HIGH 1 closes it exactly — with `canonical_existing_ancestor(dir)` in hand, `!ancestor.starts_with(&self.root)` is decidable regardless of whether the parent exists. (If the ancestor form is rejected, the alternative is to apply the ladder's lexical containment test on the canonicalized root, not on `self.root`'s verbatim spelling.)

---

## LOW 3 — `create_skill_refuses_a_linked_skills_dir` is vacuously green: it passes with or without the fix it was added for

**Where:** `src/tool/workflow/skill.rs:1041-1069`.

The fixture plants the link at the skills **dir** pointing at `outside.path()`, which **exists** and is empty (`:1049-1057`). Traced on the pre-fix tree (i.e. with the `validate_dir_creation` call deleted):

1. `create_dir_all(<root>/.coding/skills)` — the path is the link, its target is an existing directory, so `mkdir` returns EEXIST and Rust's `create_dir_all` takes its `Err(_) if path.is_dir() => Ok(())` branch. **Nothing is created anywhere**; the call succeeds.
2. `validate_for_write(<root>/.coding/skills/greet.toml)` then refuses: step 1 `validate` canonicalizes the parent *through the link* → outside the root → `PathOutsideRoot`; on Windows step 2's lexical fallback also fails on the `\\?\`-prefixed root. No write happens.

So pre-fix `!r.success` holds and `read_dir(outside).count() == 0` holds → the test is green before the fix. The hazard round-2 LOW 1 described requires the link's **target directory to be absent** ("`D/skills` does not exist") so that `create_dir_all` genuinely issues the out-of-root `mkdir` — this fixture never creates that situation, and (as the sandbox's own `dir_creation_gate_refuses_a_link_component` shows) the gate's real behaviour is only observable when the mkdir would have done something.

**Fix (pick either, (a) is sufficient and robust):**
(a) assert the refusal came from the dir gate, i.e. `assert!(r.output.contains("refused to create"), "the dir gate must be what refuses: {}", r.output);` — pre-fix the error is the ladder's "refused to write …", so the assertion fails without the gate, and the test then genuinely pins the ordering it was written for.
(b) additionally/instead, point the link at a nonexistent target so pre-fix `create_dir_all` mkdirs at the destination and `read_dir(outside)… == 0` fails — note a junction to a missing target is not always plantable, so this variant must keep the visible `SKIP:` marker (the Developer-Mode `symlink_dir` spelling is the one that works).

---

## LOW 4 — the feature's SPEC record still carries the superseded ordering ("the skills dir is deliberately created BEFORE validation")

**Where:** `.coding/knowledge/spec/2027-01-11-skill-reload-skill-create-over-a-reloadable-skil.md:8` (the round-1 amendment), in a knowledge record that is the durable documentation of this feature (the file is the truth; the index only carries a digest).

> The skills dir is **deliberately created BEFORE validation**: `validate` canonicalizes an EXISTING parent, which is what makes the check spelling-independent — … this also matches the ladder's own create-then-revalidate order.

Round-2 LOW 1 established that the ordering did **not** match the ladder's gates, and round 3 changed it: `skill_create` now runs `validate_dir_creation` before `create_dir_all` (`src/tool/workflow/skill.rs:579-584`). The record therefore documents the pre-fix contract as a deliberate design decision, which is exactly what a resuming session would read. The gist at `:6` likewise still describes the round-1 write policy without the dir gate.

**Fix (docs only, no code):** `memory_amend` the record with a dated paragraph — the skills dir is pre-validated through `Sandbox::validate_dir_creation` (link-free walk + canonical protection/containment check on the DIRECTORY) *before* the mkdir, because the mkdir itself is the mutation; the file write stays on `validate_for_write`; cite `.coding/reviews/2026-09-17-skill-reload-and-create-review-round3.md`. (All other doc carriers are current — see the closure section for round-2 LOW 3.)

---

## Round-2 findings — closure status

**LOW 1 (the pre-validation `create_dir_all`) — closed for every escape shape, on Windows; see HIGH 1 for the ancestry gap.** The gate now runs at `src/tool/workflow/skill.rs:579`, before `create_dir_all` at `:582`, and every earlier step of `execute` is read-only (validations → render → leaf `symlink_metadata` at `:553` → `exists`/overwrite at `:559`), so the mkdir is the first mutation and it is gated. Traced cases:

- **`.coding` → an existing directory `D` elsewhere** — `validate` canonicalizes the parent to `D` → `PathOutsideRoot` → the `Err` arm's walk hits the `.coding` component (`symlink_metadata` → symlink) → refuse before the mkdir ✔.
- **`.coding/skills` → a protected tree** — `validate` resolves the link, the canonical path is inside the root, and `refuse_if_protected` (`sandbox.rs:605-610`) refuses ✔ (pinned by `sandbox.rs:1690-1699`).
- **`.coding/skills` → outside the root** — same path, refused ✔ (pinned by `:1678-1686` and by the tool test).
- **dangling link AT the dir** — `validate` returns `parent.join("skills")` for a dangling leaf (`sandbox.rs:127-137` does not treat a dangling leaf as an error), the canonical path is unprotected, so the gate returns `Ok` and `create_dir_all` itself fails (`mkdir` → EEXIST, `is_dir()` false for a dangling link) → the tool errors; nothing is created through the link ✔ (safe, though the message is the generic "failed to create …").
- **`dir` provenance** — never model-controlled: `SkillLibrary::dir()` is wired from `project.skills_dir` (`src-tauri/src/main.rs`, `SkillLibrary::load(project.skills_dir.clone())`) or test wiring; the model's only input is the name, and `validate_skill_name` (`skill.rs:343-372`) admits only `[a-z0-9_-]`, ≤64 bytes, alnum first char, so no traversal, separator, 8.3 alias, ADS or trailing-dot spelling can reach the dir ✔.
- **Windows false-refusal concern — resolved.** `validate_dir_creation`'s `Err` arm never calls `validate_for_creation`, so the verbatim `\\?\` canonical root is never compared against the plain spelling of `project.skills_dir`; the `Ok` arm gets its spelling-independence from `validate` canonicalizing an existing parent. The plain "skills dir does not exist yet" case passes on this Windows box (the parent's suite evidence) and the sandbox's own positive case pins it (`sandbox.rs:1666-1673`). The remaining false refusal is platform-specific and above the root — HIGH 1.

**LOW 2 (silent skip) — closed.** `create_skill_refuses_a_dangling_symlink_target` (`skill.rs:1071-1100`) now tries `plant_file_link`, falls back to `plant_dir_link` (`:1086-1087`) and prints a visible `eprintln!("SKIP: …")` (`:1089`) before returning — the fixture's documented rule ("every caller can see — and must say — when it could not plant the link", `sandbox.rs:743-751`) and the same two-step spelling its other callers use. The `plant_dir_link` fallback also keeps the dangling semantics (its target `missing-dir` does not exist), so the junction spelling exercises the same hole. Recorded, not a finding: the symlink half of `create_skill_refuses_a_linked_target` (`:1030-1038`) still has no `else` marker, but its hardlink half always runs and `unwrap`s, so that test cannot be silently vacuous (round 2 reached the same conclusion).

**LOW 3 (docs) — closed for both named files.** `docs/FEATURES.md:20` now reads "…never overwrites an existing file unless asked, never rewrites a skill shipped with the app such as `merge_to_main`, and its write is sandbox-mediated — a planted symlink or hardlink at the target or at the skills dir is refused", and `PLAN.md:296-301` carries the same policy (Executing-only + the constructor-granted allow-lists + link-mediated write + the shipped-skill exception). Sweep for other carriers: `README.md` has **no** skill/tool enumeration (zero matches for "skill") ✔; `src/agent/prompt.rs:769-775` names `skill_reload` and nothing stale ✔; the module + tool doc comments describe the current write path ✔; the frontend `toolCardPaths` label/exclusion behaviour is unchanged and correct (`skill_create` excluded from `argPaths`, labelled from `name`, `skill_reload` bare) ✔. The one remaining carrier is a knowledge record, reported as LOW 4.

## Standing checks on the whole diff

- **The ladder's contract for the file tools is unchanged.** The `+124` line sandbox.rs delta is exactly `validate_dir_creation` + its private helper + doc comments + one test: every pre-existing anchor shifts by a uniform `+77` (round-2 cited `validate_for_write` at `:434` → now `:511`; the `link_fixture` header `:666` → `:743`; the sandbox's own SKIP caller `:1572` → `:1649`), and `validate_for_write` (`:511-599`), `is_protected_write_target` (`:316-400`), `lexical_path_is_link_free` (`:460-475`) and `link_count` (`:659-665`) are the round-2 text verbatim. So `file_write`/`file_append`/`file_edit`/`convert_line_endings` behave exactly as before, and `validate_dir_creation` has exactly one caller (`skill_create`). No `#[allow]`, no dead code, no new dependency.
- **Ordering / TOCTOU.** `validate_dir_creation` is the last read-only step before the mkdir; the file write still goes through `validate_for_write` (`:589`) and writes to the returned path (`:593`). The window between the dir gate and `create_dir_all` is the same residual class the ladder has between its step-2 gate and step 4 (needs an approval-gated shell call inside the window) — unchanged risk, recorded not as a finding.
- **Lock discipline.** `validate_dir_creation` and `is_link_free_as_spelled` are synchronous; `SkillLibrary::read` is still closure-scoped (guard cannot escape), `reload` still builds the fresh registry outside the write lock and swaps in one assignment, `insert` is one assignment, poisoning recovers. No `std` guard can be live across an `.await` on any path this diff touches.
- **`skill_create` Executing-only.** The name guard (`src/tool/mod.rs:349-353`) still precedes the arm match and is independent of list contents; the matrix test (`:2004-2029`) pins Executing/ExecutingResearch/PlanFrozen true and Planning/Reviewing/Complete/`Skill([…])`/`Reviewer([…])` false. Unchanged.
- **Reviewer surface unchanged.** `REVIEWER_BASE_TOOLS` names neither tool, the must-not-include list pins both (`src-tauri/src/ipc/spawn.rs:1050-1054`), and `skill_reload` is not implicitly granted (`src/tool/mod.rs:1991-1994`). A review report still cannot be authored outside a reviewer, and `.coding/reviews/**` stays protected.
- **No new AutoRun escape hatch.** Both tools are AutoRun as designed, but the authoring write is sandbox-mediated, the dir is gated, and the git merge/push gates are untouched. `write_review_report` remains refused in a skill's tool list (`skill.rs:515-519`) and by the filter.
- **Path derivation.** `dir.join(format!("{name}.toml"))` with the name validated first and `dir` never model-supplied; the rendered text is parsed back before the write (`render_skill_file`, `:322-335`).
- **File-tools-first.** No shell-based mutation in non-test code; the only shell use is the pre-existing test-only Windows junction fixture inside `#[cfg(test)]`. The changed frontend/docs files are coherent; no test-file registration was missed (`toolCardPaths.test.ts` is an existing registered file).
- **Multi-platform.** No new `cfg(windows)` anywhere outside the shared test fixture; the new test uses `std::fs` + the fixture only, and both SKIP markers are visible on stderr. The one genuine platform divergence is HIGH 1 (macOS symlinked ancestors).

## Test genuineness (fail-without-fix)

| Test | Verdict |
| --- | --- |
| `create_skill_refuses_a_linked_target` (`skill.rs:997-1039`) | **Genuine.** The hardlink half always runs and pre-fix `std::fs::write` truncates the victim's inode (both the `!r.success` and the "do not touch" assertion fail); the symlink half rides the same guard. |
| `create_skill_refuses_a_dangling_symlink_target` (`:1071-1100`) | **Genuine.** Pre-fix `Path::exists` is false for the dangling leaf, the overwrite refusal is skipped, and `std::fs::write` *creates* the link's target (`escaped.toml`, or `missing-dir` as a file in the junction spelling) → both assertions fail. Skip is visible (LOW 2 fix). |
| `create_skill_refuses_to_rewrite_a_shipped_skill` (`:1102-1125`) | **Genuine.** Pre-fix `overwrite: true` writes `merge_to_main.toml` and returns success → all three assertions fail. |
| `shipped_skill_names_are_recognized` (`src/skill/mod.rs:545-550`) | **Genuine** (the predicate is new — pre-fix is a compile error). |
| `dir_creation_gate_refuses_a_link_component` (`sandbox.rs:1655-1700`) | **Genuine by construction** (the fn is new) and its positive case pins the Windows-spelling worry on the `Ok` arm; its SKIP markers are visible. It does **not** cover the ancestor walk (HIGH 1). |
| `create_skill_refuses_a_linked_skills_dir` (`skill.rs:1041-1069`) | **Vacuously green** — passes without the gate (LOW 3). |
| `create_skill_rejects_invalid_arguments_without_writing` (`:938-980`) | Genuine (`!library.dir().exists()` genuinely pins "validation before any disk access"). |

## Not findings — recorded for the maintainer

- A dangling link at the skills **dir** is refused with the generic "failed to create …: File exists" rather than a named link refusal; it is safe (nothing is created) but not self-explanatory.
- `is_link_free_as_spelled` fails **open** on any `symlink_metadata` error (`Err(_) => return true`) — the same shape as the pre-existing `lexical_path_is_link_free`; unchanged risk, no new exposure.
- Run-all worktree agents still cannot author into the main tree's skills dir (round-2 "not finding", unchanged): honest refusal, nothing written.
- `.coding/skills/**` remains unprotected by name (correct — the file tools may legitimately write there), and the TOCTOU window between validation and the write remains the pre-existing one `file_write` also has.
