# Review: Scoped one-shot `merge_to_main` action

**Reviewer:** read-only closing-sequence reviewer (spawn_agent)
**Date:** 2026-04-04
**Scope:** ALL uncommitted changes in the working tree (`git status` / `git diff HEAD`):
`src/tool/workflow/merge.rs` (NEW), `src/tool/workflow/mod.rs`, `src/agent/factory.rs`,
`src/tool/mod.rs`, `src-tauri/src/ipc/commands.rs`, `src-tauri/src/main.rs`,
`frontend/src/lib/tauri.ts`, `frontend/src/components/layout/MergeToMainDialog.tsx` (NEW),
`frontend/src/components/layout/StatusBar.tsx`, `agent.md`
(plus plan bookkeeping: `.coding/plans/da512219-….md`, `.coding/plans/stack.json`, `.coding/plans/940491fd-….md`).

**Verified green locally:** `cargo test --lib merge` → 5 passed, 0 failed
(`merge_to_main_gating`, `refuses_dirty_tree`, `source_equals_target_errors`,
`merges_feature_into_main_and_returns`, `merge_conflict_returns_to_original_branch`).
The two `unused mut` warnings in the output are in `src/runtime/agent.rs:511` — pre-existing, not from this diff.

---

## Summary + verdict

The git sequence in `merge.rs` is correct, honest about partial failure, and leaves no
half-merged state on any path I could trace. The TS wiring (camelCase arg mapping, the
`"complete"` serde comparison, the single dialog-gated caller) is correct. The UI
command path is genuinely gated behind the Radix confirmation dialog.

**However, the central safety claim — "always approval-gated, never auto-run,
regardless of safety mode" — is NOT actually enforced for the agent-initiated path.**
`safety() == NeedsApproval` is necessary but not sufficient: the dispatch layer can run
the tool without any prompt in **Autonomous** mode, and the **safety-rules** shortcut can
auto-approve it in any mode. The feature's documentation, schema text, constitution
amendment, and the design intent all assert an unconditional gate that the code does not
provide. This is the one finding that must be resolved before commit (either enforce the
gate for this specific tool, or correct the over-strong claims).

**Verdict: CHANGES REQUIRED** — one High (the gating guarantee), one Medium (git arg
flag-injection hardening), one Low (missing doc comment). Everything else is clean.

---

## 1. Git sequence correctness & safety — `merge.rs::execute`

Traced every path:

- **Dirty-tree refusal FIRST** (lines 155–165): `git status --porcelain` runs before any
  checkout; a non-empty tree returns an error without moving. ✓
- **Branch recorded BEFORE checkout** (168–174): `rev-parse --abbrev-ref HEAD` captured
  into `original_branch` prior to `checkout`. Detached HEAD (`HEAD`) is rejected (175–177). ✓
- **source==target rejected before checkout** (182–184) — no-op merge refused early. ✓
- **Merge conflict** (198–205): on merge failure it runs `merge --abort` then
  `checkout original_branch` before reporting. Covered by
  `merge_conflict_returns_to_original_branch` (asserts branch is `feat`, clean tree). ✓
- **Push failure** (209–216): merge already committed on target; it returns to the
  original branch and reports "Merge commit is local on '<target>'" — honest, not
  half-merged. ✓
- **Final return-checkout failure** (221–233): reports `success:false` with
  `returned:false` in `data` and an explicit "Run `git checkout <branch>` manually"
  message. Honest about the one state it could not auto-recover. ✓
- **Nonexistent source branch** (e.g. agent passes `source_branch:"nope"`): caught at the
  merge step — `git merge --no-ff nope` fails, `merge --abort` is a harmless no-op
  (errors discarded), `checkout original_branch` restores. Returns to original branch. ✓
- **Checkout target succeeds but merge fails**: covered above (abort + return). ✓

**No path leaves the repo on `main` or mid-merge.** The only unrecoverable-without-
user-action case (final return-checkout fails) is reported truthfully.

**Finding 1.1 — Medium — git argument flag-injection (hardening).**
`merge.rs:187` `&["checkout", &target]` and `:198` `&["merge","--no-ff",&source,"-m",&merge_msg]`
pass `source`/`target` straight through as git args. These are discrete `Command` args
(not a shell string), so there is **no shell injection** — but there is no validation or
`--` separator, so a branch argument beginning with `-` (e.g. `target="-b"`,
`source="--strategy=ours"`, `source="--no-commit"`) would be parsed by *git* as a flag,
altering the command's behavior. The caller is the local user/agent (already trusted to
run git), so this is not a privilege boundary — but the tool's whole purpose is to be a
*narrow, predictable* merge. Fix: validate that `source`/`target` match a branch-name
shape (reject a leading `-` and whitespace), or insert `--` where git supports it
(`git checkout <target> --` is invalid for branch checkout, so validation is the right
fix here). At minimum reject `arg.starts_with('-')`.

**Otherwise: no findings** in the git sequence.

## 2. Approval gating actually holds — **THE claim is not enforced**

- `MergeToMainTool::safety()` (merge.rs:138–144) returns `SafetyLevel::NeedsApproval`
  unconditionally. ✓ (as far as it goes)
- Not registered as `AutoRun` anywhere; `is_project_scoped("merge_to_main",…)` hits the
  conservative `_ => false` arm (approval.rs:113), so **AutoApproveProject** mode *does*
  gate it. ✓
- ToolFilter exposes it only in Executing/Complete, never Planning (mod.rs:154–160,
  174). The `merge_to_main_gating` test asserts visibility + `NeedsApproval`. ✓

**Finding 2.1 — High — the "never auto-run, regardless of safety mode" guarantee is
false for the agent-initiated path.** Two bypasses in `src/agent/dispatch.rs::execute_tool_call`:

1. **Autonomous mode.** `approval::needs_approval` returns `false` for *every* tool under
   `SafetyMode::Autonomous` (approval.rs:80: `SafetyMode::Autonomous => false`). So when the
   app is in Autonomous mode and the agent emits a `merge_to_main` tool call,
   `execute_tool_call` skips the approval block entirely and dispatches directly
   (dispatch.rs:150). No prompt, no gate. This directly contradicts the tool's own schema
   text ("ALWAYS requires approval, regardless of safety mode"), the module doc ("never
   auto-run"), and the design intent in the task.

2. **Safety-rules shortcut.** Even when `needs_approval` returns `true`
   (ApproveEachAction / AutoReadApproveWrites / AutoApproveProject), dispatch.rs:79–87
   auto-approves if `safety_rules.is_safe(name, args)` matches. `SafetyRules::signature`
   for `merge_to_main` is `"merge_to_main:"` (empty key arg — it is not in `key_argument`'s
   match, safety_rules.rs:267). A user who runs `add_safety_rule` on a `merge_to_main` call
   (or hand-adds `tool="merge_to_main"` to the rules file) creates a standing rule that
   auto-approves **every** subsequent `merge_to_main` call — a persistent bypass of the
   gate, exactly what the feature is designed to prevent.

   **Why this matters:** the constitution carve-out (agent.md) sanctions the action only
   as "a single, always-approval-gated merge… invoked only when the user asks." If the
   tool can run with no prompt (Autonomous) or via a stored rule, that invariant is broken
   and the agent *can* land commits on main without a contemporaneous user sanction.

   **Fix (pick one; the first is preferred):**
   - (a) Enforce the gate for this tool specifically: in `execute_tool_call`, treat
     `merge_to_main` (and conceptually any "never auto-run" tool) as always requiring the
     interactive prompt — i.e. bypass the `Autonomous` short-circuit **and** the
     safety-rules shortcut for it. The cleanest mechanism is a property on the tool (e.g.
     a `fn never_auto(&self) -> bool { false }` on `Tool`, overridden to `true` by
     `MergeToMainTool`) that `needs_approval`/dispatch consult before applying the
     Autonomous/rules bypasses. This makes the guarantee real rather than documentary.
   - (b) If enforcement is out of scope, correct every over-strong claim so the docs match
     behavior: the schema text (merge.rs:111–117), the module doc (merge.rs:1–23), the
     `safety()` comment (merge.rs:139–143), the ToolFilter comments (mod.rs:152, 172), and
     the task's design statement all assert an unconditional gate. They must be weakened to
     "approval-gated except in Autonomous mode / unless a matching safety rule exists."
     Option (b) leaves the constitution carve-out's "always-approval-gated" wording
     inaccurate, so (a) is strongly preferred.

## 3. The Tauri command doesn't bypass the gate — no findings

- `merge_to_main` (commands.rs:782–812) calls `tool.execute` directly — no tool-call
  approval. This is acceptable **only because** the sole caller is the explicit user
  click+confirm. Verified: the only frontend reference to `mergeToMain`/`merge_to_main`
  is `StatusBar.tsx:62` inside `handleMergeConfirm`, which is wired exclusively to
  `MergeToMainDialog`'s `onConfirm`. No agent path, no other component invokes it. ✓
- The button is rendered only when `workflowState === "complete" && gitBranch &&
  gitBranch !== "main" && gitBranch !== "no-branch"` (StatusBar.tsx:514) — i.e. only in
  the Complete state and not already on main. ✓
- The dialog cancel is inert while `merging` (`onCancel={() => { if (!merging) setMergeOpen(false); }}`),
  and both buttons are `disabled={merging}`, so the user can't double-fire or dismiss
  mid-merge. ✓
- The command is registered in `invoke_handler` (main.rs:203). ✓
- `Workflow::new(plans_dir: impl Into<PathBuf>)` (workflow/mod.rs:68) matches the call
  `Workflow::new(project_root.join(".coding").join("plans"))`. ✓
- `IpcState.project: Arc<tokio::sync::Mutex<Project>>` (state.rs:43) with `Project.root:
  PathBuf` (project/mod.rs:20); `state.project.lock().await.root.clone()` is correct. ✓

  **Note (informational, not a finding):** the UI command path relies on the dialog being
  the *only* caller. That holds today. If a future caller is added, the gate is only as
  strong as the convention — the agent-path enforcement gap (Finding 2.1) is the real
  issue; this path is correctly gated by construction.

## 4. TS wiring — no findings

- **camelCase mapping:** Tauri converts snake_case Rust command params to camelCase on the
  JS side. Confirmed against an existing command: `send_prompt(agent_id: AgentId, …)`
  (commands.rs:114–116) is invoked as `{ agentId, … }` (tauri.ts:32). So
  `source_branch → sourceBranch`, `target → target`, `push → push`. The wrapper passes
  `{ sourceBranch: …, target: …, push: … }` (tauri.ts:289–293) — all three map correctly.
  Passing `null` for the `Option` args deserializes to `None`. ✓
- **WorkflowState comparison:** `WorkflowState` is `#[serde(rename_all = "lowercase")]`
  (workflow/mod.rs:21), so the serialized value is `"complete"`; the comparison
  `workflowState === "complete"` (StatusBar.tsx:514) matches. ✓
- `MergeResult` type matches the command's returned JSON (`success`/`output`/`data`). ✓

## 5. Security — no findings beyond 1.1

- The git commands run via `tokio::process::Command` with `cmd.args(&[...])` — discrete
  argv, never a shell string (merge.rs:71–95). No shell injection. ✓
- `current_dir(&self.project_root)` pins git to the repo; that is the intended scope
  (the merge *is* a repo operation). The sandbox is not implicated because this is not a
  file-path tool. ✓
- The only arg-handling concern is the git-level flag injection in **Finding 1.1** (a
  hardening gap, not a remote/shell exploit). ✓

## 6. Constitution compliance — one Low

- **Amendment wording (agent.md:32–38):** reads correctly and scopes the carve-out to the
  explicit, user-initiated, always-approval-gated action — but note the "always-
  approval-gated" phrase is currently aspirational for the agent path (see Finding 2.1).
  If 2.1 is fixed via enforcement (option a), the wording becomes true; if via docs
  (option b), this line must be amended too.
- **Finding 6.1 — Low — missing doc comment on a public fn.** `MergeToMainTool::new`
  (merge.rs:63) is `pub fn` with no doc comment; the constitution requires doc comments
  on all public functions. The struct's other items are documented; add a `///` line to
  `new`. (The other public items — the `impl Tool` trait methods — inherit trait docs and
  are fine.)
- **Windows/PowerShell:** `CREATE_NO_WINDOW` handled under `#[cfg(windows)]` (merge.rs:77–81). ✓
- **Plan-first:** work was done under a plan; the plan files are updated. ✓

---

## Must-fix before commit (prioritized)

1. **High — Finding 2.1:** make the "always approval-gated, regardless of safety mode"
   guarantee real for the agent-initiated path. Enforce it in `execute_tool_call` for
   `merge_to_main` (recommended: a `never_auto`/`always_prompt` property on the `Tool`
   trait that defeats both the Autonomous short-circuit and the safety-rules shortcut),
   OR downgrade every claim (schema, module doc, `safety()` comment, ToolFilter comments,
   agent.md "always-approval-gated") to match actual behavior. Enforcement is strongly
   preferred so the constitution carve-out stays accurate.
2. **Medium — Finding 1.1:** validate `source`/`target` in `merge.rs::execute` to reject
   arguments beginning with `-` (and ideally whitespace), preventing git flag-injection
   from a crafted branch argument.
3. **Low — Finding 6.1:** add a doc comment to `MergeToMainTool::new` (merge.rs:63) to
   satisfy "all public functions must have doc comments."

After fixing, re-run `cargo test` (and `tsc --noEmit` / `vite build` if frontend changes
are made) and include this report in the commit.
