## Verdict: PASS

All three round-2 findings are verified RESOLVED at HEAD 71b4cb1 (wt/agenticcoding, working tree clean). The two code-comment sites and PLAN.md carry the exact sibling wording requested; the deck family carries the .git control plane in all three forms (rendered markdown, deck source, narration) — verified on disk, since docs/ is gitignored by design. Commit 71b4cb1 contains exactly the four expected paths (the three tracked doc-sync files + the round-2 report) and its source-file hunks are comment/doc lines only — no behavior change, no source drift. The stale-enumeration sweep is clean: no living source or doc site still describes the pre-fix protection set (remaining hits are historical .coding artifacts and an unrelated .gitattributes comment). No new findings; this gate closes the plan.

### Scope reviewed

- HEAD 71b4cb1 on wt/agenticcoding; `git diff HEAD` and `git status --short` both empty — working tree clean (verified). Commit sequence confirmed linear and as described: f4a2e6d → 8f435d5 → 0b55b23 → 71b4cb1 (tip).
- Full diff of 71b4cb1 reviewed (git show, stat + full): 4 files, +74/−8 — .coding/reviews/2026-09-10-git-sandbox-escape-review-round2.md (new, +64), PLAN.md (+3/−1), src-tauri/src/ipc/files.rs (+4/−4), src/tool/agent/file_edit.rs (+3/−3).
- Direct on-disk reads of all six finding sites (file_edit.rs, files.rs, PLAN.md, docs/why-mnemo-deck.md, docs/deck_content.py, docs/narration.py). The deck family was verified from disk, not git — docs/ is gitignored (local presentation artifacts), intentionally absent from 71b4cb1, and the commit message itself documents that omission so it is not silent.
- Tests: not re-run (read-only reviewer); the parent's stated green runs relied upon (root 2158+16 / 0 failed; src-tauri 292+4+2 / 0 failed; exit 0 unpiped). 71b4cb1 is comment/doc-only, so no test-count movement is expected from it — consistent.

### Round-2 finding verification

**1. RESOLVED — file_edit call-site comment + write_sandboxed IPC doc use the sibling wording.**

- src/tool/agent/file_edit.rs:892-893 now reads "Reject edits to protected paths (.coding state/bookkeeping or the .git control plane) with the shared refusal message." — the old enumeration ("the memory DB, safety.toml, backlog.json, the plan stack") is gone (diff-confirmed removal; the `sandbox.refuse_if_protected(&validated)` call below it is unchanged context).
- src-tauri/src/ipc/files.rs:120-123 (`write_sandboxed`'s doc) now reads "…protected paths (.coding state/bookkeeping or the .git control plane) are refused with the shared message, so a UI edit can't desync live workflow state." — the old enumeration ("the memory/codegraph DBs, `safety.toml`, `backlog.jsonl`, the plan stack and `.coding/plans/*.md`") is gone (diff-confirmed removal; signature and body unchanged context).
- Both now match the shared refusal message (sandbox.rs:321) and the file_write/file_append ladder comments (file_write.rs:137-138, file_append.rs:105-106) — the protection-description sites are wording-consistent.

**2. RESOLVED — PLAN.md's protection sentence extends to the .git control plane.**

- PLAN.md:125-130 now reads: "Protected `.coding` write targets (memory DB, `safety.toml`, `backlog.jsonl`, the `knowledge/` corpus, the `plans/` tree) are refused by the file agent tools, as is the `.git` control plane (any path containing a `.git` component - planted hooks/`core.fsmonitor`); `keys.toml` is written with user-only permissions (Unix `0600` / Windows DACL)." — exactly the requested extension, in the constitution-listed as-built doc.

**3. RESOLVED (on disk) — the deck family includes the .git control plane.**

- docs/why-mnemo-deck.md:677-679: "**The agent can't edit its own guardrails** — memory DB, `safety.toml`, `backlog.jsonl`, the knowledge corpus, the plans tree, the reviews tree, the .git control plane (planted hooks/`core.fsmonitor`) are all refused by the file tools."
- docs/deck_content.py:367: "…the plans tree, the reviews tree, the .git control plane (planted hooks/core.fsmonitor) are all refused by the file tools."
- docs/narration.py:236-237: "…the plans tree, reviews tree and the .git control plane (planted hooks) are all refused by the file tools."
- All three forms (rendered/source/narration) are in sync with each other and with the code-level wording.

### Commit 71b4cb1 audit — no source drift

- Exactly 4 paths: the round-2 report (new, +64), PLAN.md, src-tauri/src/ipc/files.rs, src/tool/agent/file_edit.rs — matching "the three tracked doc-sync files + the round-2 report" exactly; nothing else in the commit.
- Both source-file hunks are comment/doc lines only. Zero behavior change: the predicate (sandbox.rs), all five write gates, and every test are byte-identical to the round-2-verified state at 0b55b23 (71b4cb1 touches no code line — the `refuse_if_protected` calls and the `write_sandboxed` body appear as unchanged diff context).
- Working tree clean at HEAD → on-disk == committed for all tracked files; the gitignored deck fixes are the only on-disk-only state, by design.

### Sweep — no remaining stale sites

- ".git control plane" now appears at every protection-description site in living source: sandbox.rs (predicate doc :171, rationale :241, shared message :321, test section :740-744), src-tauri/src/ipc/files.rs (:122 doc, :670/:674 test parity), file_write.rs:138, file_append.rs:106, file_edit.rs:893 — plus PLAN.md:128, README.md:33 (backticked variants, unchanged since round 2's verification — not touched by 71b4cb1, tree clean), and the three deck files.
- "memory/codegraph" (the old files.rs enumeration phrase) survives only in historical artifacts (.coding/reviews/2026-04-20-md-editor-review.md, .coding/reviews/ui-freeze-lock-audit.md, two .coding/plans files) and an unrelated .gitattributes:42 comment about SQLite sidecars — no living doc claims the pre-fix protection set. fullreview.md (:19/:224) remains as round 2 classified it: a dated historical artifact, not a living doc — not a finding.

### Observations (no finding — for the parent)

- **Search-index lag (methodology note):** the content index served pre-71b4cb1 content for file_edit.rs during this round's literal sweeps (the "protected paths" and ".git control plane" searches missed the file's on-disk wording), and gitignored docs/ sits outside the index. Every conclusion above is therefore based on direct file reads + the git diff, which agree — no verification relied on the stale index.
- **Multi-platform neutrality:** 71b4cb1 touches only comments/docs — no platform surface at all; round 2's verification of the predicate (pure ASCII string matching, lexical regression tests, no NTFS dependence) stands unchanged.
- **Test status:** not re-run (read-only); the parent's green runs relied upon. A comment-only commit implies no test-count change, and none was claimed.
- **Plan closure:** the full commit set stands as planned — f4a2e6d (protection + 8 regression tests incl. the IPC mirror), 8f435d5 (--no-verify on commit/merge arms), 0b55b23 (round-1 report + bookkeeping), 71b4cb1 (round-2 doc-sync + report). Three review rounds, all findings resolved, nothing further outstanding.
