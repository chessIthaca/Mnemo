## Verdict: FINDINGS (0 high, 2 low)

The implementation is correct, to-spec, and constitution-clean: the advisory badge matches security review 2026-09-09 LOW-2's prescription exactly (token set, shell-only scope, advisory-only, no gating change), the helper is sound, the 8-test suite is well-designed and registered, and all four project-specific checks pass. Two LOW findings: (1) the scan reads only `args.command` — a shell call whose `cwd` points into a protected dir (e.g. `cwd: ".git"`) with a clean command text silently skips the badge; (2) a doc-comment overstatement in the new helper about what the file tools enforce. Both are one-line fixes or justifiable skips; neither blocks.

### Scope reviewed

All uncommitted changes on wt/agenticcoding (git diff HEAD + untracked files):

- `frontend/src/lib/protectedPathWarning.ts` (NEW) — token list + scan helper
- `frontend/src/lib/protectedPathWarning.test.ts` (NEW) — 8 vitest tests
- `frontend/src/components/chat/ApprovalPrompt.tsx` (M) — badge wiring
- `frontend/vitest.config.ts` (M) — test registration
- `.coding/plans/6d50d160.md` (NEW), `.coding/backlog.jsonl` (M) — plan/bookkeeping only

Cross-checked against: `.coding/reviews/2026-09-09-security-review.md` (LOW-2 prescription), `src/tool/agent/sandbox.rs` (`is_protected_write_target`), `src/tool/agent/shell.rs` (schema + `resolve_cwd`), `frontend/src/hooks/agentState.ts` (`PendingApproval`), `frontend/src/lib/vitestInclude.test.ts` (registration guard), README.md / PLAN.md (doc-sync bar).


### Verified correct

**1. Helper (`protectedPathWarning.ts`).** `protectedPathTokens` lowercases the command once and filters the all-lowercase token tuple with `String.includes` — case-insensitivity is symmetric, `filter` preserves `PROTECTED_PATH_TOKENS` order and cannot duplicate (canonical order, deduped by construction), `[]` for clean or empty input. The `.git/` trailing slash correctly excludes `.gitignore`/`.gitattributes`, mirroring the Rust sandbox's own component-equality intent (`sandbox.rs:177`). Case-insensitivity matches the Rust protected-target style (`sandbox.rs:198-205` lowercases for NTFS) — good parity between the advisory scan and the enforcement it advertises. Both exports carry doc comments citing the review, the advisory-only nature, and the design rationales. No platform APIs.

**2. Wiring (`ApprovalPrompt.tsx`).** The `command?: string` addition to the args cast is type-only; the guard `approval.toolName === "shell" && typeof args?.command === "string"` is null-safe and correctly scoped to shell — the documented residual (file tools enforce the sandbox; the git tool is argv-based with a fixed op set, so it cannot plant hooks). The field name matches the shell tool's schema (`shell.rs:180`, `command` required) and `PendingApproval.args` already carries it (`agentState.ts:25`, `args: unknown`) — the no-Rust-changes design holds. The badge renders below the header row / above the diff block, only when tokens matched. Advisory-only verified: `protectedTokens` is read solely in JSX — it never touches the approve/deny handlers, so no gating change. Token strings render as React text children (escaped — no injection surface).

**3. Tests (8, as claimed).** The backlog acceptance case (`rm -rf .git/hooks/pre-commit` → `[".git/"]`); per-token coverage; case-insensitivity in both directions (`.GIT/`, `.CODING/`); the `.gitignore` negative; clean + empty command; multi-token canonical order (order independent of token position in the command); token-list shape; plus the `?raw` source contract pinning the import, the shell gate, the render condition, the icon, and the rationale text — all assertions verified present in the component source. The lib-test-imports-component-`?raw` pattern has 34 precedents (e.g. `markdownRendering.test.ts`, `attachImages.test.ts`). Registered at `vitest.config.ts:70` in correct alphabetical position; the `vitestInclude.test.ts` drift guard would fail the suite if it were missing.

**4. Design decisions (a)–(e) all verified.** (a) Shell-only scope is right: scanning file-tool args would false-positive on diff content that merely mentions `.git/`, while the file tools already refuse those writes (`sandbox.rs:196-249`, including any `.git` component since the HIGH-1 fix). (b) Case-insensitive is required for Windows paths and matches the Rust guard. (c) The trailing slash avoids the `.gitignore` false positive (tested). (d) Advisory only — verified no gating influence. (e) No Rust changes — verified payload sufficiency.

**5. Project-specific review expectations.**
- **Documentation sync: sufficient, no finding.** README.md:46 documents the approval gate at the feature level; no approval-dialog sub-feature (Mark Safe, Deny all, the A/D/Shift+D shortcuts — verified absent from README) has its own bullet, so an advisory badge needs none; the helper + wiring doc comments cite the review and rationale, which is the established coverage level for this granularity. PLAN.md's approval-flow section (Phase A, :908) covers IPC mechanics, unaffected.
- **Multi-platform neutrality: clean.** Pure TS string scan, no platform APIs/paths/assumptions; case-insensitivity is cross-platform (and required by the Windows path reality the Rust side already handles).
- **File-tools-first: clean.** No shell-based file mutation anywhere in the change; all edits are ordinary source changes through the file tools.
- **JSX/classname fit: good.** The strip's red palette (`border-red-500/40 bg-red-950/30 text-red-300`) matches the dialog's existing red elements (Deny all: `border-red-500/60 bg-red-950/40`; resolveError: `text-xs text-red-300`); `h-3.5 w-3.5` icon sizing matches every other icon in the dialog; `shrink-0` prevents flex squish; `mt-2` matches the container's spacing rhythm; placement below the header row keeps it visible at decision time. The red-on-yellow severity contrast against the yellow-tinted container is intentional and reads correctly.


### Findings

**LOW 1 — the scan reads only `args.command`; a `cwd` inside a protected dir (and slash-less `.git` phrasings) evades the badge.**
- **Location:** `frontend/src/components/chat/ApprovalPrompt.tsx:73-76` (scan input); `src/tool/agent/shell.rs:142-155` (`resolve_cwd`).
- **Symptom:** `resolve_cwd` validates `cwd` only for root-containment (`Sandbox::validate`) — it does not consult `is_protected_write_target` — so `cwd: ".git"` or `cwd: ".coding/reviews"` (existing dirs inside the root) is accepted. A command like `cp /tmp/evil hooks/pre-commit` with that `cwd` writes the protected location while the command text stays clean → no badge. Similarly, command phrasings without the trailing slash (`cd .git && …`, `git -C .git config …`) do not match the `.git/` token. This matches the prescribed scope exactly (the review's LOW-2 and the backlog item both say "a static string scan of the command" / "when the command text mentions …"), and the badge is advisory with the residual documented and accepted — so it is an optional tightening, not a spec violation. Mitigating: the badge is client-side and invisible to the agent, so there is no adversarial evasion pressure; the cwd route would be accidental phrasing. But it is precisely the plant-a-hook / forge-a-review scenario the badge exists to surface, and the fix is cheap.
- **Suggested fix:** include the cwd in the scanned text — e.g. scan `` `${args.command} ${args.cwd ?? ""}/` `` (add `cwd?: string` to the args cast; the appended slash makes a bare `.git`/`.coding` cwd match the slash-bearing tokens, and the space separator prevents junction false-positives — a trailing `" /"` on the no-cwd path matches no token) — plus a test (`cwd: ".git"` + clean command → badge shown). Alternatively, if the command-text-only scope is to stand, document the coarseness explicitly in the helper doc ("`cwd` is not scanned; phrasings without the trailing slash do not match").

**LOW 2 — helper doc overstates the file-tool protection for the `.coding/` token.**
- **Location:** `frontend/src/lib/protectedPathWarning.ts:9` ("The file tools enforce the sandbox against all three").
- **Symptom:** Accurate for `.git/` (any `.git` component, `sandbox.rs:241-249`) and `safety.toml` (`sandbox.rs:227`), but the file tools do not refuse every path under `.coding/` — they protect the enumerated control-plane set (`.coding/plans|reviews|knowledge/`, `safety.toml`, `backlog.jsonl`, the DBs; `sandbox.rs:220-240`); e.g. `.coding/notes.txt` is file-tool-writable. A maintainer reading the sentence could conclude `.coding/` is wholesale protected. The badge token being broader than the protected set is fine (advisory, deliberately coarse); only the doc sentence is loose.
- **Suggested fix:** reword to the accurate claim, e.g. "The file tools refuse writes to the control-plane locations under them (`.coding/plans|reviews|knowledge/`, `safety.toml`, `backlog.jsonl`, the DBs, and any `.git` component), but shell cannot be sandboxed the same way — an approved shell command can write any of them."

Both findings are LOW: neither is a correctness bug in what was built, and each has a one-line fix (or a written justification to skip, since the implementation matches the review's prescribed heuristic).

### Test evidence

Per the plan (step 4, at review time): frontend vitest 79 files / 1095 tests passed (including the new 8 and the include guard), exit=0; `cargo test` 2278 + 16 passed, 0 failed, warning-free under `#![deny(warnings)]`. Not re-run by this reviewer (read-only); registration is verified in-tree (`vitest.config.ts:70`) and enforced by the `vitestInclude.test.ts` drift guard, and the Rust workspace is untouched by the diff, so the reported green `cargo test` is unaffected by this change.
