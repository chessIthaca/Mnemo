## Verdict: FINDINGS (1 high, 2 low)

Reviewed the full uncommitted change set on `wt/mnemo` (plan 8a95a233: `skill_reload` + `skill_create` over a reloadable `SkillLibrary`). The feature's design and its three headline contracts (every-state reload, Executing-only create, reviewer surface untouched) are correctly implemented and well pinned by tests. One security-relevant gap in the new write path, plus two doc/hardening nits.

---

## HIGH 1 — `skill_create`'s file write is not sandbox-mediated: a planted symlink or hardlink at the target redirects the write outside `.coding/skills/`

**Where:** `src/tool/workflow/skill.rs:536-550`

```rust
let dir = self.library.dir().to_path_buf();
let path = dir.join(format!("{}.toml", spec.name));
if path.exists() && !args.overwrite { ...refuse... }
if let Err(e) = std::fs::create_dir_all(&dir) { ... }
if let Err(e) = std::fs::write(&path, &text) { ... }
```

`SkillCreateTool` holds only `Arc<SkillLibrary>` — it has no `Arc<Sandbox>`, so the file tools' `Sandbox::validate_for_write` (`src/tool/agent/sandbox.rs:434`) never runs here, unlike `file_write` (`src/tool/agent/file_write.rs:158`, which writes to the validated path). Two reachable routes:

1. **Dangling symlink, no `overwrite` needed.** `Path::exists()` follows symlinks, so a *dangling* link at `.coding/skills/<name>.toml` reads as "does not exist" → the refusal at :538-544 is skipped → `std::fs::write` follows the link and *creates* the file wherever it points, including outside the project root.
2. **`overwrite: true` over a symlink or hardlink.** The write truncates the link's destination instead of the skill file — e.g. a symlink or hardlink at `.coding/skills/x.toml` pointing at `.coding/safety.toml`, `.coding/plans/<id>.md`, or `src/lib.rs`. That is exactly the bypass class closed for the file tools by plan b4812291 / backlog 1a5bffcf: the sandbox refuses a `.coding/**` path whose link count exceeds 1 (`sandbox.rs:302`) and re-checks protection on the canonical result in `validate_for_write` step 5. `skill_create` re-opens it for its own write path.

**Honest scoping:** this is NOT reachable through the name argument — `validate_skill_name` (`:341`) allows only `[a-z0-9_-]` with an alnum first char, so traversal (`..`), separators, Win32 8.3 aliases (`~`), ADS (`:`) and trailing-dot tricks are all impossible. The exposure is the *target path's identity* on disk, so it needs a pre-planted link — but `.coding/skills/` is a git-tracked, mergeable side-car, so a symlink can arrive with a clone/branch, and the sandbox's whole link discipline assumes planted links are in scope. `skill_create` is AutoRun (no approval prompt), so nothing else intercepts it.

**Concrete fix:** wire the sandbox in and route the write through it, mirroring `file_write`:
- `SkillCreateTool::new(library: Arc<SkillLibrary>, sandbox: Arc<Sandbox>)`; the factory has its sandbox in scope in `register_skill_tools` (`src/agent/factory.rs:1056-1074`).
- In `execute`: `let validated = match self.sandbox.validate_for_write(&path) { Ok(p) => p, Err(e) => return ToolResult::error(...) };` then `std::fs::write(&validated, &text)`. That single change inherits the hardlink guard, the canonical protected re-check, and the link-free creation ladder.
- Minimal fallback if reusing the sandbox is judged too broad: refuse when `std::fs::symlink_metadata(&path)` reports a symlink or `link_count(&path) > 1`, and require the canonicalized parent of `path` to equal `std::fs::canonicalize(&dir)` before writing.

Add a regression test next to the existing create tests: plant a symlink (and, Windows-gated, a hardlink) at the target and assert `skill_create` refuses; it fails without the guard.

---

## LOW 1 — The "always available inside a skill" enumerations omit `skill_reload`

**Where:** `src/tool/workflow/skill.rs:303-304` (the `SKILL_FILE_HEADER` comment written into *every* generated skill file) and `src/tool/workflow/skill.rs:428` (the `skill_create` tool description). Both say the always-available set is "the memory tools, ask_user, current_plan and the backlog tools".

This change adds `skill_reload` to that set (auto-granted under `ToolFilter::Skill`, `src/tool/mod.rs:628` arm; PLAN.md:289-300 now states it), so the two enumerations are stale on arrival — and the header is the in-file documentation a user reads when hand-editing a generated skill.

**Fix:** add `skill_reload` to both lists, e.g. "the memory tools, ask_user, current_plan, skill_reload and the backlog tools are always available and need not be listed here".

---

## LOW 2 — `skill_create` can overwrite a *shipped* skill (e.g. `merge_to_main.toml`) with no approval prompt

**Where:** `src/tool/workflow/skill.rs:341` + `:538-550`; `SHIPPED_SKILLS` in `src/skill/mod.rs`.

The shipped name `merge_to_main` satisfies `validate_skill_name`, so `skill_create { name: "merge_to_main", …, overwrite: true }` replaces the project's `.coding/skills/merge_to_main.toml` — the source of truth for the merge procedure (embedded via `include_str!`, seeded write-if-missing, so the rewrite persists and the seeded copy never returns). `skill_create` is AutoRun, so unlike an equivalent `file_edit` in Executing there is no approval prompt; the only signal is the tool card.

**Fix (maintainer's call):** refuse names present in `SHIPPED_SKILLS` in the create path with an actionable error ("'merge_to_main' is a shipped skill — edit it with the approval-gated file_edit, or pick another name"), or document the capability explicitly in the tool description. Leaving the `overwrite` escape hatch for user-authored skills is fine; the shipped set is the part worth pinning.

---

## Verified clean (the parent's explicit checklist)

**1. Name → path derivation (traversal / 8.3 / ADS / separators).** `validate_skill_name` (`src/tool/workflow/skill.rs:341`) runs before any path work and admits only `[a-z0-9_-]` with an alnum first char and ≤64 bytes; `dir.join(format!("{name}.toml"))` therefore cannot contain `/`, `\`, `..`, `.`, `~`, `:` or a trailing dot/space. `dir` is `SkillLibrary::dir()` — the same directory `load_dir` reads and `main.rs:1765` wires from `project.skills_dir`, so authoring and loading cannot disagree about location. No unvalidated input reaches the write. Only the *link identity* of the target is open → HIGH 1.

**2. Lock discipline.** `SkillLibrary::read<R>(&self, f: impl FnOnce(&SkillRegistry) -> R) -> R` (`src/skill/mod.rs:197-201`) is higher-ranked over the registry borrow, so `R` cannot carry the guard out — `lib.read(|r| r.iter())` does not compile; the guard is created and dropped inside `read`. All four readers call it synchronously: `SkillStartTool::execute` (`skill.rs:139`), `SkillCreateTool::execute` (`skill.rs:536/563`), `SkillReloadTool::execute` (`skill.rs:616`), and Tauri `enter_skill` (`src-tauri/src/ipc/agent.rs:846` `.cloned()`, `:863` bool) — no `std` guard can be live across an `.await`, so `clippy::await_holding_lock` has nothing to catch. `reload` (`src/skill/mod.rs:222`) and `insert` (`:255`) take the write lock for a synchronous build-then-swap (`load_dir` runs *before* the lock; the swap is one assignment), and poisoning is recovered (`unwrap_or_else(|e| e.into_inner())`) rather than panicking. Lock order is always workflow → registry (no async runs under the registry guard), so no inversion exists.

**3. `skill_create` Executing-only.** The guard at `src/tool/mod.rs:349-353` denies `skill_create` whenever the filter is `Skill(_)` or `Reviewer(_)`, *before* the arm match and independent of list contents — the same hard-rule shape as `write_review_report` (`:340-342`). Subagent allow-lists are `ToolFilter::Skill` (`src/workflow/mod.rs:412` via `set_tool_allowlist`), so a subagent list naming it is denied too; a skill file listing it likewise. `available_in` and `target_state` go through `is_lifecycle_state` (Planning | Executing | Complete), so `"subagent"`, `"skill"` and `"reviewing"` are refused at the tool boundary with an actionable message (`skill.rs:477-498`) — serde accepting the variants is exactly why that filter is there. Tests pin all of it: `skill_create_allowed_only_in_the_executing_surfaces` (`src/tool/mod.rs:2003-2029`; the eight-state matrix incl. `Skill(["skill_create"])` and `Reviewer(["skill_create"])`).

**4. Reviewer surface unchanged.** `REVIEWER_BASE_TOOLS` (`src-tauri/src/ipc/spawn.rs:432+`) names neither tool, and the must-not-include assertion list now pins both (`spawn.rs:1050-1054`), so a future addition to that const fails the test. `ToolFilter::Reviewer` is the literal-list arm, and `skill_create` is refused even if listed (test `:2025-2028`). `skill_reload` is *not* implicitly granted to a reviewer (`!v(ToolFilter::Reviewer(vec!["file_read"]))` at `:1991-1994`); it would take a deliberate `REVIEWER_BASE_TOOLS` edit plus breaking that pin to change. Verified unchanged.

**5. Reload round-trip / behavior tests.** `library_reload_reports_added_removed_and_changes` (`src/skill/mod.rs`) exercises the unchanged dir (nothing added/removed), then add + edit + delete in one reload, asserting `added`/`removed`/sorted `names` AND that the *edited content* is live (`reg.get("alpha").prompt == "Do alpha, revised."`) — a count-only implementation fails it. `library_reload_drops_a_file_that_stops_parsing` asserts a malformed file leaves the registry (`removed`, `total == 0`, `get(...).is_none()`). The tool-level tests cover the reload report text through a directory-less library ("0 skill(s)") and the create matrix (validation, no-write-on-invalid, overwrite refusal leaving the file byte-identical, re-registration of an overwritten spec, round-trip parse). Both claimed contracts (every-state reload; Executing-only create) are pinned in both directions per state. One mild weakness: `create_skill_rejects_invalid_arguments_without_writing` checks emptiness via `read_dir(...).unwrap_or_default()`, which also reads as empty when the dir is missing — harmless here (nothing creates it) but it would not detect a write *outside* the dir, i.e. HIGH 1's territory.

**6. Multi-platform neutrality.** Only `std::fs` / `std::path` / `tempfile` were added; no `cfg(windows)`, no platform API, no shell syntax. The `PathBuf::from("C:/nonexistent/skills/dir")` literal in `library_reload_on_missing_dir_is_empty` is sound: on macOS/Linux it is merely a relative path whose `read_dir` fails (empty registry), and the assertion is an equality on the stored `PathBuf`, so it is platform-independent and never touches disk. `std::fs::create_dir_all` + `write` on the target dir behave identically on both platforms.

**7. File-tools-first / registration / docs.** No shell-surgery artifacts in the diff; all changed files are coherent (the tool history itself is not observable from the diff). Registration is conditional on the library (`src/agent/factory.rs:1064-1074`) and the "no skills → no skill tools" test (`:2416`) plus the fully-wired expected-name set (`:2290-2296`) both cover the two new tools. Docs match the implementation: PLAN.md:42-46 and :289-300, docs/FEATURES.md:20, the module + tool doc comments, `prompt.rs:769-775`, and the frontend `toolCardPaths` change (skill_create excluded from `argPaths`, labelled by `name`; skill_reload stays a bare card — consistent with the arg-less convention, since it needs no path chip and the salvage can no longer produce a bogus one). The two deliberate no-ops (no new IPC command → `tauri.ts` untouched; no frontend label for `skill_reload`) are correct. README.md/`docs/` carry no skill-tool enumeration that goes stale.

**Not findings, recorded for the maintainer:** (a) `SkillRegistry::load_dir` still validates only `target_state`, not `available_in` — a hand-edited file may list `reviewing`, but `skill_start` is not visible in the Reviewing filter, so the model cannot act on it (pre-existing, unchanged by this plan); (b) a reload during an active skill does not disturb it, because `Workflow` snapshots the spec at `start_skill` — intended and safe.
