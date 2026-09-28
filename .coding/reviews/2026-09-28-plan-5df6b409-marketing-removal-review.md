## Verdict: FINDINGS (0 high, 1 low)

Commit 004bd91 on wt/mnemo (the removal of the day-1 marketing work) verified against source and git history. All four acceptance criteria pass except one low bookkeeping inaccuracy inside the commit's own .coding records. The restored how-to knowledge file is judged an ACCEPTABLE resolution of the "marketing out of git" instruction. Details below.

Reviewed-state: 004bd9149744b4071bf7a826a59da948ed51d7c3

## Scope — what was actually read

Working tree is clean, so the review target is branch tip commit **004bd91** alone; earlier commits (5b7f99b..f8aa63d, plans 428a9f1e and d3aedfee) are context only and were not re-reviewed, per task. Read: full commit message + stat (12 files, 102 insertions), the removal plan `.coding/plans/5df6b409.md`, branch history via `git log`, path history via `git log -- assets/social` and `-- docs/marketing` (both empty), the four salvaged docs at their changed lines, the source backing each claim (`src/config/general.rs` OptimizerConfig, `src/tool/agent/output_compactor.rs`, `src/tool/agent/read_files.rs`), `.gitignore` lines 70-75, and every tracked mention of `docs/marketing` / `assets/social` (repo-wide text search). The full `git show` diff (66k chars) was archived by the harness; the stat's 12-file list was cross-checked against the plan's step-6 salvage list and commit message.

## Acceptance criterion 1 — marketing paths out of git: PASS

- `git log -- assets/social` and `git log -- docs/marketing`: **zero commits** — no ref reachable from HEAD touches either path.
- Branch history from the fork point is exactly 5b7f99b..f8aa63d + 004bd91; none of the six dropped marketing commits (1fc1d92, c48f934, fe89c97, 1af4549, 6367f85, 42f34c7) appear, and 004bd91's parent is f8aa63d — the `git reset --mixed f8aa63d` provenance claim is accurate.
- Repo-wide text search: `docs/marketing` / `assets/social` appear only in `.gitignore` and in `.coding/` records (plan, decision record, how-record, project-root spec — the spec's mention is historical evidence in an unrelated salvaged record, and remains true: the files exist on disk). README.md, PLAN.md, docs/FEATURES.md, docs/CONFIGURATION.md contain **no** reference to any marketing path.
- The artifacts still exist on disk (the search walker read content from `docs/marketing/day1-post.md` and `card-to-png.html`), confirming "removed from git, kept locally".

## Acceptance criterion 2 — salvaged docs are accuracy fixes, verified against source: PASS

- **Seven levers**: `OptimizerConfig` (src/config/general.rs:423-482) has exactly seven boolean levers — `delta_reads`, `compress_output`, `archive`, `compaction_survival`, `quality_score`, `lean_output_nudge`, `recall_delta` — all default-on, checked in `is_default()`. README L45/L165 ("Seven"), PLAN.md L596, FEATURES.md L56, CONFIGURATION.md L9 all say seven. ✓
- **Credential redaction scoped to compressed output**: `redact_secrets` is applied only inside the compactor's line-serving arms (`output_compactor.rs:378-382`: Error/Warning/Keep each bump `redact_secrets(line)`), and the module doc (line 17) scopes redaction to the compressed surface. The doc wording "redacts credentials on every line the compactor serves (output it never sees passes through verbatim)" in PLAN.md:607, FEATURES.md:56 and CONFIGURATION.md:9 matches the code exactly. ✓
- **Delta reads not absolute**: the lever-1 doc comment (general.rs:424-428) scopes the skeleton/diff serve to *re-reads*: "First reads and ranged reads still serve full content"; `read_files.rs:1315-136` carries the pre-lever fail-open fallback. The docs say "a `read_files` re-read … serves a skeleton … or a unified diff" — correctly scoped to re-reads, no absolutist claim. ✓
- **No marketing copy**: the four docs' changed lines are lever-count/redaction-scope wording only; nothing promotional anywhere in the commit's non-.coding content.

## Acceptance criterion 3 — nothing unsanctioned; .gitignore correct: PASS

- The 12 committed files are exactly the plan's step-6 list (.gitignore, README.md, PLAN.md, docs/FEATURES.md, docs/CONFIGURATION.md, .coding/plans/d3aedfee.md, .coding/backlog.jsonl, .coding/knowledge/bug/d3aedfee.md, .coding/knowledge/spec/2027-01-11-project-root-is-c-mnemo-…, the new decision file, the plan file itself) plus the restored how-record — the deviation the plan itself documents at L33. Nothing else; no screenshot, no marketing artifact, no stray file.
- `.gitignore:74-75` carry `assets/social/` and `docs/marketing/` — trailing-slash directory patterns anchored at the repo root. They cover both a direct file (`assets/social/day1-card.png`) and any nested path (`docs/marketing/sub/x.png`); with a slash in the pattern they cannot accidentally match unrelated deeper trees of the same name. Since nothing under those dirs is tracked, the patterns fully prevent re-entry via normal `git add .`. ✓

## Acceptance criterion 4 �� .coding record coherence: ONE low finding

The restored how-record, the plan file, and the salvaged records are internally coherent and truthful. The one inaccuracy is in the **DECISION record**:

### Finding L1 (low): the committed DECISION record contradicts the commit's own content

`.coding/knowledge/decision/2027-01-11-marketing-artifacts-are-not-tracked-in-git-local.md` states: "the marketing process records (plan file, three e7a80fbb review reports, **the render/verify how-record file**) were deleted." That same commit restores and commits the how-record (`.coding/knowledge/how/2027-01-11-render-verify-a-social-card-png-docs-marketing-c.md`, +20 lines) — so the decision record misdescribes what shipped. The plan (L33) correctly documents the walk-back, and the commit message correctly says "the restored render/verify how-to"; only the decision record was written before the repair and never amended. Fix: amend the decision record (memory_amend / targeted update) to say the plan file and three review reports were deleted, while the how-record file was *restored with corrected content* (local-only status + CodeQL ?src= caveat) so the live memory row stays updatable — then commit the amendment.

## Deviation judgment — restored how-to file: ACCEPTABLE

Committing the restored how-record is an acceptable resolution of "marketing out of git", for three reasons: (1) the instruction targets the marketing *artifacts* (card, post, harness) and their *landing* — all are out of git; the how-record is process documentation, and `.coding/` is by project convention the mergeable side-car that travels with git. (2) The alternative left a live-but-broken memory record: with the file gone, `memory_supersede`/`memory_update` both fail with os error 2, so the record could never be corrected or retired — strictly worse for record hygiene than restoring it. (3) The restored content was rewritten to state the local-only status, point at the DECISION record, and carry the CodeQL ?src= security caveat with an explicit "drop that feature before this file ever goes back into git" warning — it does not smuggle marketing copy (no card text, no post copy) and it actively discourages re-entry. The deviation is documented in the plan (L33) and named in the commit message. Not a finding.

## Risk focus

- **(a) Tracked doc pointing at an untracked marketing path** — none. The four product docs are clean; mentions exist only in `.gitignore` and `.coding/` records whose job is to describe the local-only status (the project-root spec's mention is historical evidence that remains true — the file exists on disk).
- **(b) Marketing content riding in the salvage commit** — none. Doc changes verified line-by-line against source; .coding additions are status/procedural records, not marketing copy.
- **(c) The restored how-to as a hole** — judged acceptable, see above; it is the documented deviation and the record is now truthful and security-annotated.
- **(d) Provenance accuracy** — commit message verified: six dropped commits absent from history, parent is f8aa63d, salvaged file list matches the stat, artifacts confirmed on disk. One provenance slip inside the DECISION record's "what stands" narrative = Finding L1. Note: the remote state (PR #13 CLOSED, remote branch deleted) is asserted by the plan and memory records but not independently verifiable with my read-only toolset (no shell/gh); every locally checkable claim held.

## Constitution checks (one line each)

- **Documentation sync**: docs updated accurately against source (seven levers, scoped redaction, re-read-only deltas) — verified, not prose-trusted. ✓
- **Multi-platform neutrality**: no library/app code touched; the how-record's `file:///C:/Mnemo/...` URL is a bookkeeping record's environment note, not app code. ✓
- **File-tools-first**: shell used only where the file tools refuse by design (.coding/plans, .coding/reviews deletions; .coding/knowledge restore — the sanctioned writer for that path); `.gitignore` edited via file tools. ✓
- **Warning-free build**: no source code changed by 004bd91; nothing here can affect `cargo test` or the `deny(warnings)` gate. ✓
- **Bookkeeping exclusion**: `.coding/**` accuracy-checked, not line-reviewed — one accuracy miss found (L1).