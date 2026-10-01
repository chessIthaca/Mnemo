## Verdict: PASS

The change is exactly what the plan claims: a `|| name.starts_with("image_")` third term in both `ToolCategory::Agent` arms of `ToolFilter::allows()` (Planning `src/tool/mod.rs:583-595`, Complete `src/tool/mod.rs:816-828`), each under a rationale comment, plus regression test `image_family_visible_in_planning_and_complete` (`src/tool/mod.rs:1897-2014`). No approval relaxation, no surface left half-fixed, no stale docs.

## What I read

- `git_read` diff HEAD (stat + full diff) and `git status` — the working-tree delta is `.coding/backlog.jsonl` (status flip), `src/tool/mod.rs` (+153/−0: pure insertion), untracked plan file.
- `src/tool/mod.rs` — Planning arm (:570-634), Complete arm (:790-859), Skill arm (:863-938), `hidden_groups_for` (:1163-1188), `schemas` (:1230-1267), the whole new test (:1881-2014), sibling tests.
- `src/agent/factory.rs:2825-2933` — image registration + the pre-existing visibility tests (Executing-only — unaffected).
- `src/agent/dispatch.rs:262` — the per-call filter re-check.
- `PLAN.md:452-465`, `PLAN.md:1146-1155`, `docs/FEATURES.md:80-89`.
- Searches: `starts_with("image_")` across the tree; `\.allows\(` across `src/agent/**`; `AutoRun && !name.starts_with` across `src/**/*.rs`; write-path scan of `src/tool/agent/image_tools/**`; backlog item `e1f9df86`.

## Risk focus, point by point

**(a) Prefix admits only the read-only family.** Exactly seven tools carry an `image_` name (registered at `factory.rs:2846-2853`; asserted again by the factory test at :2914-2918). No other name source can collide: dynamic tools are MCP-only and namespaced `mcp__<server>__<tool>` by construction (`PLAN.md:1148`), and skills select from registered tools via allow-lists, they don't mint names. All production code in `src/tool/agent/image_tools/` is read-only w.r.t. the project — the only `write` hits are `#[cfg(test)]` fixtures and in-memory PNG encoding (`zoom.rs:199-200` into a `Vec`); the family reads an image inside the sandbox and calls the vision model.

**(b) Other arms untouched; mcp__ exclusion intact.** `src/tool/mod.rs` is +153/−0 (pure insertion), and `image_` appears in code at only three sites: the two arm clauses (:594, :827) and the test's enumeration filter (:1941). The Skill arm's separate AutoRun read-grant (:898-902) and the Reviewer arm (:939+) are unchanged. The `!name.starts_with("mcp__")` term survives in all three gate expressions (:584, :817, :899); `PLAN.md:1148`'s claim (MCP hidden in Planning/Complete) still holds — an MCP tool is NeedsApproval + `mcp__`-named, so all three terms of both arms are false for it.

**(c) Visibility path complete.** Both consulters of the gate — `hidden_groups_for` (:1180-1184) and `schemas` (:1257) — call the same `allows()`, so the one arm fix restores both the index advertisement and the emitted schemas (the test asserts each separately). The only other production caller is the dispatch-time re-check (`src/agent/dispatch.rs:262`), which uses the same `allows()` — so advertisement and dispatch cannot desync. `mcp_reveal_allowed` probes with an `mcp__probe__probe` name, unaffected. `factory.rs:3240+` are test-side skill-filter helpers. No third surface exists.

**(d) The test genuinely bites.** It builds the real seven tools via real constructors, enumerates the family from `r.iter()` (never a hardcoded list) with an anti-vacuity `assert_eq!(len, 7)` (:1943-1947), asserts the `image` group is advertised in both states (:1954-1964), loads it, adds an Executing fixture-sanity pass (:1970-1982) so a broken fixture can't pass vacuously, asserts all seven present in both states' schemas (:1994-1996), the four mutating names absent (:1999-2004), and `NeedsApproval` per tool (:2006-2012). The recorded RED panic ("the image group must be advertised in Planning") matches the assertion message at :1962 verbatim (the :1940 in the RED log is the pre-arm-edit line shift); RED exit=101 → GREEN exit=0.

**(e) NeedsApproval preserved.** No safety() body was touched; the test asserts it for all seven (`src/tool/mod.rs:2006-2012`), and the arm comments state the invariant. No silent approval relaxation.

**(f) Docs sync — agree with the no-edit judgment.** `PLAN.md:457-460` and `docs/FEATURES.md:84/87` describe the surface categorically ("read agent tools" / "read tools only"), and the image family *is* read-only w.r.t. the project; the long-standing `spawn_agent` NeedsApproval exception is likewise unenumerated there (summary-level convention). `PLAN.md:1148` is untouched by this change and remains true.

**(g) Neutrality + file-tools-first.** Pure boolean Rust in the filter; no OS, path, or shell surface — holds identically on macOS and Windows. No shell-based mutation in the diff; no `#[allow(...)]` anywhere in it.

## Constitution checks (one line each)

- *Documentation sync* — verified no stale sentence; see (f).
- *Multi-platform neutrality* — no platform surface; PASS.
- *File-tools-first* — diff is clean `file_edit`-shaped insertions; no shell mutation.
- *Warning-free build* — no `#[allow]` added; plan records unpiped `cargo test` exit=0 (green under `#![deny(warnings)]`).
- *Bookkeeping accuracy (one line)* — backlog `e1f9df86` flipped to `in_flight` with `plan_id: fe83f10d` attached, matching the shipped state at review time; the plan file's FIX AS LANDED section matches the tree (arm line refs :583-595/:816-828 verified).
- *Empty-diff rule* — not applicable: the working tree is dirty and I read the full uncommitted delta (scope of the task) plus the surrounding unchanged code for each risk focus.

## Notable good properties

The Executing fixture-sanity block (`src/tool/mod.rs:1970-1982`) is above the bar for this kind of gate test — it distinguishes "the state gate denies" from "the fixture never registered anything" — and the pre-existing factory tests (`factory.rs:2873-2928`, unavailability + deferral in Executing) still pass untouched, confirming the change widened exactly two arms and nothing else.

Reviewed-state: ba704f8fa0993555482bba957ae8d7f6f7e3be80
