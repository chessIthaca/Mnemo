## Verdict: FINDINGS (0 high, 3 low)

Round-2 verification of the uncommitted tree on `wt/mnemo` against `.coding/reviews/2026-09-17-skill-reload-and-create-review.md`. **All three round-1 findings are genuinely closed** — I traced each fix in the current source (I ran no tests myself; the suite evidence is the parent's — root `cargo test` 2397+16, `src-tauri` 299, vitest 83 files, `tsc --noEmit`, both crates `#![deny(warnings)]`).

The three new findings are low-severity residuals in the fix round itself: (1) the new write ordering leaves one pre-validation filesystem mutation (a `mkdir`, never a file write) outside the sandbox gate; (2) one new regression test can skip silently on Windows, against the repo's own documented fixture convention; (3) the user-facing docs state the overwrite rule without its new shipped-skill exception. None of them re-opens the round-1 HIGH's data-write hole.

---

## Round-1 closure — verified

**HIGH 1 — closed (write is now sandbox-mediated; the link is refused before the overwrite question).**
- `SkillCreateTool` holds a `Sandbox` (`src/tool/workflow/skill.rs:400-417`) and the factory passes the **agent's** sandbox — `self.register_skill_tools(&mut registry, workflow, &sandbox)` (`src/agent/factory.rs:849`), `SkillCreateTool::new(Arc::clone(skills), sandbox.clone())` (`:1085-1088`), so a worktree agent gets its worktree root, not the main tree.
- `execute` refuses a leaf whose `symlink_metadata` reports a symlink **before** the `Path::exists`-based overwrite check (`skill.rs:546-558`), which is what closes the dangling-link route that `exists` cannot see; then routes the write through `validate_for_write` and writes to the returned path (`:581-587`), inheriting the `.coding/**` hardlink guard (`is_protected_write_target` → `link_count > 1`, `sandbox.rs:289-304`), the canonical protected re-check (step 5, `sandbox.rs:479-499`) and the link-free creation ladder (`:440-463`).
- **Ordering audit (the parent's explicit question).** `create_dir_all(&dir)` at `skill.rs:574` runs before `validate_for_write`, but the *write* is not re-opened in any named case:
  - **symlinked skills dir** — `validate` step 1 canonicalizes the parent and lands outside the root → `Err`, then the creation fallback's `lexical_path_is_link_free` walks the path and returns `false` at the link component → refusal (`sandbox.rs:442-450`). A `.coding/skills -> .coding/knowledge` link canonicalizes *inside* the root instead, and step 3 then refuses the protected canonical path (`:464-467`). A dangling link **at** the final component makes `create_dir_all` itself fail (`mkdir` → EEXIST, `is_dir()` false → error), so nothing is created through it.
  - **8.3-alias parent** — the dir comes from `project.skills_dir`, never from model input (the name is `[a-z0-9_-]`, `skill.rs:343-372`), so there is no model-controlled alias; and an alias in the config path is resolved by step 1's canonicalize, whose result step 3 re-checks by name (`sandbox.rs:465-467`) — a protected tree behind the alias is refused.
  - **protected canonical target** — refused by step 3/step 5 as above.
  - **normal case still works, and the ordering is what makes it work on Windows** — after `create_dir_all`, step 1 `validate` canonicalizes the (now existing) parent spelling-independently, so the `\\?\`-prefixed canonical root comparison never runs. With the `create_dir_all` removed, a *non-existent* skills dir on Windows would fall into `validate_for_creation`, whose `normalized.starts_with(&self.root)` compares the plain spelling against the verbatim root and refuses a valid path. Because the dir exists by then, the ladder's step 4 sets `created_parent = false` (`sandbox.rs:470-475`), so every step-5 failure propagates rather than returning a lexical path — the fail-closed arm is not reachable from this tool.

**LOW 1 — closed.** `SKILL_FILE_HEADER` now reads "the memory tools, ask_user, current_plan, skill_reload and the backlog tools are always available" (`skill.rs:299-314`) and the `skill_create` schema description names `skill_reload` in the same set (`skill.rs:432`). The two stale enumerations are gone; `prompt.rs:769-775` already carried the mid-skill reload hint.

**LOW 2 — closed.** `pub fn is_shipped_skill` (`src/skill/mod.rs:289-299`) over `SHIPPED_SKILLS`, refused at `skill.rs:481-486` **before any disk access** and regardless of `overwrite`, with an actionable error naming the approval-gated file tools and the re-seed rule. The refusal also covers the "file was deleted" case, which is the right call (the seed returns a missing file).

---

## LOW 1 (new) — the pre-validation `create_dir_all` can still `mkdir` outside the sandbox (or into a protected tree) through a planted link component

**Where:** `src/tool/workflow/skill.rs:565-576`.

The comment claims the ordering "matches the ladder's own order (step 4 creates parents, step 5 revalidates)". It does not: the ladder's step 4 runs **after** the link-free gate (step 2) and the canonical-ancestor protection check (step 2b), so `file_write` can never create a directory through a link. `skill_create` creates the directory first and validates after, so a link at a **non-final** component still gets a `mkdir` through it.

Concretely: `.coding` is a symlink to an existing directory `D` elsewhere and `D/skills` does not exist (the `Path::exists`/`symlink_metadata` probes at `:553-559` only cover the leaf, and `create_dir_all` resolves each component as the OS does). `create_dir_all(<root>/.coding/skills)` issues one `mkdir` that the OS places at `D/skills` — an out-of-root directory creation. Pointed at `.coding/knowledge` instead, the same step plants a new `skills` subtree **inside** a protected tree, which is the shape the sandbox's step 3 refuses "before any filesystem mutation" for the file tools (`sandbox.rs:464-467`).

**Scoping:** no file content is written — `validate_for_write` then refuses (`lexical_path_is_link_free` / canonical protected check), and the mutation is an empty directory. It needs a pre-planted link (the same assumption round-1's HIGH rested on) and it is strictly narrower than the pre-fix behaviour (which wrote the file through the link). It is a real remaining filesystem-escape mutation on the one path this fix was meant to bring under the sandbox, so it should not be left undocumented-by-code.

**Fix (mirrors the ladder's first two gates, ~10 lines before `create_dir_all`):**

```rust
match self.sandbox.validate(&dir) {
    Ok(canonical_dir) => {
        if let Err(e) = self.sandbox.refuse_if_protected(&canonical_dir) {
            return ToolResult::error(format!("refused to create {}: {e}", dir.display()));
        }
    }
    // Refuse a link component BEFORE mkdir — the ladder's step-2 rule.
    Err(_) if !self.sandbox.lexical_path_is_link_free(&dir) => {
        return ToolResult::error(format!(
            "refused: {} traverses a symbolic link or junction — name the resolved skills dir instead",
            dir.display()
        ));
    }
    Err(_) => {} // does not exist yet and no link component — safe to create
}
```

(All three calls are already `pub` on `Sandbox`: `validate`, `refuse_if_protected`, `lexical_path_is_link_free`.)

**Test gap that goes with it:** no test plants a link at the skills **dir** itself — the three new link tests only cover the leaf, so the ordering's risk surface is unpinned. Add one next to `create_skill_refuses_a_linked_target`: `plant_dir_link(&outside, &library.dir())` (the fixture picks a privilege-free junction on Windows), then assert `skill_create` refuses and that the link target gained nothing.

---

## LOW 2 (new) — `create_skill_refuses_a_dangling_symlink_target` can skip silently, against the fixture's documented convention

**Where:** `src/tool/workflow/skill.rs:1043-1045`

```rust
if !crate::tool::agent::sandbox::link_fixture::plant_file_link(&escaped, &target) {
    return; // platform refused to plant the link — nothing to assert
}
```

`link_fixture` documents the opposite rule in its own header ("a fixture that silently gives up … is how the dangling-leaf hole survived three review rounds … so **every caller can see — and must say** — when it could not plant the link", `sandbox.rs:666-674`), and every other caller honors it: `sandbox.rs:1572-1574`, `dispatch.rs:2008-2009`, `file_write.rs:684`. `plant_file_link` on Windows is `symlink_file`, which needs Developer Mode — so on a Windows box without it this regression asserts **nothing** while looking green (the other two link tests are safe: the hardlink half always runs and `unwrap`s, and the symlink half rides the same already-run hardlink branch).

**Fix:** reuse the sandbox's own two-step spelling (`sandbox.rs:1546-1575`) — try `plant_file_link`, else `plant_dir_link` (privilege-free junction) for the dangling target, and when neither succeeds print `eprintln!("SKIP: could not plant a dangling link at the skill target")` before returning.

---

## LOW 3 (new) — the user-facing docs state the overwrite rule without its new exception

**Where:** `docs/FEATURES.md:20` — "…`skill_create` authors a validated skill file, hot-added as startable (Executing only; never overwrites an existing file unless asked)". After this fix a shipped skill is never rewritten even **with** `overwrite: true`, and the write is link-refusing — so the documented rule is now incomplete in exactly the case a user would hit (running `skill_create` on `merge_to_main`). `PLAN.md:292-300` likewise describes authoring/visibility but not the write policy that motivates the sandbox wiring.

The model-facing surface **is** documented (`skill.rs:432`: "The write is sandbox-mediated: a planted symlink or hardlink at the target is refused, and a skill shipped with the app (e.g. merge_to_main) is never rewritten — edit those with the approval-gated file tools"), and the code docs are thorough — only the two prose docs lag.

**Fix:** append one clause to `docs/FEATURES.md:20` ("…; its write is sandbox-mediated and a shipped skill — `merge_to_main` — is never rewritten, even with `overwrite`") and one parenthetical to `PLAN.md:295-298` naming the refusal.

---

## Test genuineness — fail-without-fix analysis (parent's item (a))

- **`create_skill_refuses_a_linked_target`** (`skill.rs:989-1031`). Hardlink half: always runs (`std::fs::hard_link(...).unwrap()` fails loud, never silently skips). Pre-fix, `std::fs::write(&path, &text)` follows the hardlink and truncates `victim.txt` while returning success → both the `!r.success` and the `"do not touch"` assertions fail. Symlink half: pre-fix the write follows the link into the same victim → fails too. Genuine.
- **`create_skill_refuses_a_dangling_symlink_target`** (`:1033-1053`). Pre-fix, `path.exists()` is `false` for a dangling link, so the overwrite refusal is skipped and `std::fs::write` **creates** `escaped.toml` outside the skills dir → `r.success == true` and `escaped.exists() == true` → both assertions fail. Genuine (with the silent-skip caveat reported as LOW 2).
- **`create_skill_refuses_to_rewrite_a_shipped_skill`** (`:1055-1078`). Pre-fix, `overwrite: true` writes the file and returns success → all three assertions fail. Genuine.
- **`shipped_skill_names_are_recognized`** (`src/skill/mod.rs:588-592`) — the predicate exists only for this fix, so it cannot pass pre-fix (compile error). Trivially genuine.
- **Hardened invalid-args test** (`:963-971`) — `!library.dir().exists()` is now a real filesystem assertion; the old `read_dir(...).unwrap_or_default()` read empty for a missing dir too. Nothing else in that test creates `.coding/skills`, so it genuinely pins "validation runs before any disk access" (the post-validation `create_dir_all` would otherwise create the dir).

## Test helper (parent's item (b))

`make_library_tools` (`:847-862`) builds `SkillLibrary::load(dir.join(".coding/skills"))` plus a real `Sandbox::new(dir)` rooted at the tempdir — production-shaped (main.rs wires `project.skills_dir`; the factory passes the agent's sandbox), and the `.coding/` placement is what makes the `.coding/**` hardlink guard actually fire. No assertion is weaker than it looks: the create tests verify the file through an **independent** `SkillRegistry::load_dir(library.dir())` (`:893-901`, `:920-927`, `:983-986`) rather than the hot-added copy, so a hot-add that skipped the write would fail; the reload tests assert content (the revised prompt) and not just counts (`src/skill/mod.rs:565-580`), and the tool-level reload test asserts the report text + the live spec (`:1094-1117`).

## Earlier contracts re-verified (parent's item (c))

1. **Lock discipline** — `read<R>` (`src/skill/mod.rs:215-218`) is higher-ranked over the registry borrow, so the guard cannot escape the closure; every reader is synchronous: `skill_start` (`:124`, `:130`, `:140`), `skill_create` (`:544` `dir()`, `:600` `insert`), `skill_reload` (`:654`), Tauri `enter_skill` (`src-tauri/src/ipc/agent.rs:846` `.cloned()`, `:863` bool). `reload` builds the fresh registry *before* the write lock (`:229-231`) and swaps in one assignment (`:247`); `insert` (`:259-262`); poisoning is recovered, never a panic. No `std` guard can be live across an `.await` anywhere on these paths.
2. **`skill_create` Executing-only** — the guard (`src/tool/mod.rs:349-353`) precedes the category match, so it is independent of list contents; arms allow Executing / ExecutingResearch / PlanFrozen (`:433`, `:479`, `:511`) and deny Planning / Reviewing / Complete; the matrix test (`:2004-2029`) pins `Skill([])`, `Skill(["skill_create"])` and `Reviewer(["skill_create"])` as denied.
3. **Reviewer surface unchanged** — `REVIEWER_BASE_TOOLS` (`src-tauri/src/ipc/spawn.rs:432-466`) names neither tool; the mutation-exclusion loop now lists both (`:1050-1054`), so a future addition to that const fails the test; `skill_reload` is not implicitly granted to a reviewer (`src/tool/mod.rs:1991-1994`).
4. **Multi-platform neutrality** — the fix adds no `cfg(windows)` and no platform API; tests use `std::fs` only (`hard_link` is privilege-free on NTFS; the symlink spelling degrades through the fixture). The `"C:/nonexistent/skills/dir"` literal (`src/skill/mod.rs:639`) only feeds a failing `read_dir` and an equality assert on the stored `PathBuf` — it never touches disk on either platform.
5. **Registration** — all five skill tools sit under one `if let Some(skills)` (`src/agent/factory.rs:1064-1089`) and `conditional_tools_absent_without_their_wiring` (`:2414-2436`) still pins the absent case; it enumerates the three lifecycle names, the two new tools riding the same single guard (adding them to that loop would pin it explicitly — not a finding).
6. **Frontend** — `argLabel` derives the skill_create chip from `name` (`frontend/src/lib/toolCardPaths.ts:847-860`) and `argPaths` excludes the tool (`:62-70`), so the truncated-args salvage cannot fabricate a path chip; `skill_reload` stays a bare card (arg-less). Unchanged by the fix and still correct.

## Not findings — recorded for the maintainer

- **run-all worktree agents**: `skill_create` now returns an "outside the sandbox root" refusal for the main tree's library dir, since the tool takes the agent's worktree sandbox (`src/agent/factory.rs:1077-1083` documents this as by design). Honest error, nothing written — but a run-all item whose task is to author a skill no longer can; worth a deliberate decision if unintended.
- `.coding/skills/**` is not a protected target (`comps.get(1) == "skills"` is absent from `sandbox.rs:305-322`), so the sandbox rightly allows the write itself — consistent with the file tools.
- The TOCTOU window between the validated path and `std::fs::write(&validated, …)` is the same pre-existing one `file_write` has; unchanged.
- `is_shipped_skill` refuses even when the file is missing (intended — the seed restores it); the error names `file_edit` while `file_write` is the tool that can create a missing shipped file — cosmetic.
