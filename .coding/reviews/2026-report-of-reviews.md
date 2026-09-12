# Report-of-Reviews — quality/accuracy audit of the 2026 myharness review round

This is a read-only review OF the five review reports produced for the myharness
codebase (Tauri 2 + Rust + React). I read all five reports, the baseline, `PLAN.md`,
and `agent.md` in full, then spot-checked the most consequential evidence citations
against the actual source. I edited **only** this file.

## Overall verdict

The round is **strong and trustworthy**. Four independent reviewers produced
detailed, file:line-grounded findings; the consolidated summary deduplicates them
faithfully and cross-references correctly. The one material inaccuracy is in the
Code Quality report (High H5): it claims the production JSON-serialization path
`build_request_json` has "no dedicated unit test" for the
multimodal/tool-call/assistant cases — this is **false**; that function is
directly and extensively unit-tested (see Code Quality section). Every other
High-severity citation I checked was accurate. No High finding was dropped or
contradicted in the consolidation, and the cross-perspective shadcn/a11y
convergence was deduplicated correctly rather than double-counted.

Spot-checks performed (citations I opened the source for):
- `commands.rs:94-103` (WorkflowStateInfo fields), `commands.rs:502-507` (state via
  `to_string()`), `workflow/mod.rs:38-46` (Display = capitalized) +
  `:20-22` (serde = lowercase) → **H2 casing bug confirmed**.
- `App.tsx:98` + `StatusBar.tsx:87` (`as never` casts) → **confirmed**.
- `approval.rs:65-77` (`cleanup_for_agent` clears all) + `events.rs:149,167`
  (called on both Finished and Exited) → **H4 confirmed**.
- `memory/mod.rs:103,114` (single `Mutex<Connection>`) + 12 `self.conn.lock().await`
  sites → **H3 confirmed**.
- `openai.rs:694,772` (`convert_message`/`convert_tool_choice`, test-only) +
  `:1536,1564,…` (`build_request_json` IS unit-tested) → **H5 partly wrong**.
- `main.rs:271-304,364-388` + `commands.rs:446-466` (triplicated key-resolution) → **M2 confirmed**.
- `run_all.rs:67-103` (checkpoint/commit_success/rollback) + `:122` test (only tests
  `first_line`) → **H7 confirmed** (the git path is genuinely untested).
- `project/mod.rs:47` (`agent_md: root.join("agent.md")`) → **PRD deviation #5 confirmed**.
- `globals.css:36-44` (light block omits `--code-*`) + `App.tsx:58-66` /
  `useAgentStore.ts:121-139` (`applyCodeColors` writes dark defaults to root style) → **M3 confirmed**.
- `PLAN.md:54` (shadcn locked-in) + `frontend/package.json` (no `@radix-ui`) → **H1 confirmed**.
- `types.ts:140-143` (TS missing `depth`/`parents`) → **confirmed**.

---

## Chief Architect report (`2026-chief-architect-review.md`)

### High / Critical
**No findings.** I verified the two flagship claims:
- Per-agent workflow independence "fixed": `factory.rs:411-413` (`two_builds_produce_independent_workflows`)
  exists and `build_inner` at `:198-205` builds a fresh `Arc<Mutex<Workflow>>`. ✅ accurate.
- `AgentLoop` 2376 lines / god-object: matches baseline inventory (`src\agent\mod.rs 2376`). ✅ accurate.

### Medium
**No findings.** The §4 `PendingApprovals::cleanup_for_agent` finding (mapped to H4)
is accurate: `approval.rs:65-77` clears the whole map; the comment at `:66-72` admits
the map can't target one agent. The §2 throughput caveat cites `events.rs:133-191`,
which I read in full — the manager is locked on Started (`:135`), Finished (`:142`),
Exited (`:162`), and the approval branch (`:184`), exactly as claimed. ✅

### Low
- **Low — minor framing imprecision in §3 / §4, not a citation error.** The §3
  "Observation" says `let _ = wf.load_latest()` at `factory.rs:203` swallows an I/O
  error. Verified: `factory.rs:203` is `let _ = wf.load_latest();` — citation correct,
  and the `let _` does discard the `Result`. The framing ("silently starts in Planning
  with no plan") is a fair interpretation. ✅ No action.

### Constitution compliance
The report is a document; it cites Windows-relative project paths implicitly via the
baseline and makes no PowerShell claims. Compliant.

**Architect verdict: no findings (clean, high-quality).**

---

## Code Quality report (`2026-code-quality-review.md`)

### High
- **Critical — H5's central evidence claim is FALSE.** §2 (and the consolidated H5)
  states: the dead `convert_message`/`convert_tool_choice` layer is test-only, AND
  "the production serialization path (raw JSON) has **no** dedicated unit test of
  message-→JSON shape for the multimodal/tool-call/assistant cases that
  `convert_message`'s tests purport to cover." The first half is correct
  (`convert_message` at `openai.rs:694` and `convert_tool_choice` at `:772` are only
  called from `#[cfg(test)]` code at `:1181,1197,1214,1233,1246,1252,1258` — verified).
  But the second half is **wrong**: `build_request_json` (the real production path,
  `openai.rs:347`, called at `:148`) **is** directly and extensively unit-tested:
  `build_request_json` appears in tests at `openai.rs:1285, 1316, 1352, 1390, 1419,
  1449, 1536, 1564, 1606, 1639`. Notably `build_request_json_sends_image_blocks_when_multimodal`
  at `:1536` and `build_request_json_strips_image_blocks_when_not_multimodal` at `:1577`
  cover the multimodal case, plus `build_request_json_includes_strict_when_some` / `_omits_strict_when_none`
  / `_includes_reasoning_effort_when_set` / `_plain_text_unchanged_when_not_multimodal`.
  So the production serialization path is NOT under-tested for the cases the report
  names. The report's *conclusion* — that the `async-openai` conversion layer is dead
  code kept alive only by tests, creating two parallel paths — is still valid and worth
  acting on, but the justification ("the real path has no dedicated shape test") is
  factually incorrect and inflates the severity. Recommend: downgrade the
  "no test" sub-claim to "the `convert_message` tests are redundant with the existing
  `build_request_json` tests" and restate the recommendation as *consolidate, not
  backfill*. The consolidated H5 repeats this inaccuracy verbatim and must be corrected too.

### Medium
**No findings.** Verified:
- M2 key-resolution triplication: `main.rs:271-304` (main), `main.rs:364-388` (vision),
  `commands.rs:446-466` (set_model) all repeat the identical
  `key_for().or(OPENAI_API_KEY).or(ANTHROPIC_AUTH_TOKEN).unwrap_or("dummy")` + `match ep.kind`
  block, with the comment at `commands.rs:445` literally saying "Mirror the api-key
  fallback chain from build_brain." ✅ accurate.

### Low
- **Low — §6 `MemoryTier::from_str` shadows `std::FromStr`.** Verified
  `memory/types.rs:36` `pub fn from_str(s: &str) -> Option<Self>` — correct, and it's an
  inherent method (not a trait impl), so it does shadow the name. The clippy note is fair. ✅
- **Low — `_dbg_test.rs` debug shim.** Verified `src/provider/_dbg_test.rs` (16 lines,
  `dbg_reqwest` test that `eprintln!`s and asserts nothing). ✅ accurate.
- **Low — `get_config` missing `///`.** Verified `commands.rs:587` `#[tauri::command]
  pub async fn get_config(...)` has no doc comment (the struct at `:92` does). ✅ accurate.

### Constitution compliance
The report claims `cargo clippy` was run (exit 0) and cites `cargo test` green via the
baseline. Doc-comment-rule spot-check (§7) is thorough. The one constitution-relevant
gap it itself flags (`get_config` missing `///`) is correctly attributed. Compliant.

**Code Quality verdict: ONE Critical finding (H5 "no test" claim is false). Everything
else verified accurate.**

---

## UI / Usability report (`2026-ui-usability-review.md`)

### High
**No findings.** Verified:
- Accessibility-absent (H1): the report claims a codebase-wide search for
  `aria-`/`role=`/`tabIndex`/`onKeyDown`/`prefers-reduced-motion`/`prefers-color-scheme`
  returned zero matches. I independently searched and found the same gaps in the
  components I read (`Sidebar.tsx:57`, `MainPanel` tabs, `RightPanel` tabs are plain
  `<button>`s with no ARIA). The root cause (no shadcn/Radix) is tied to
  `PLAN.md:54` and the missing `@radix-ui` dep. ✅ accurate.
- First-run empty state + buried plan (H6): `Conversation.tsx:56-67` renders only
  `transcript.map` + `streamingText` + `pendingApproval` — no empty-state branch. ✅
- Light-theme code highlighting (M3 in consolidation, "High→Medium-High" here):
  verified `globals.css:36-44` `.light` overrides only bg/text/border/muted, NOT the
  `--code-*` vars (defined in `:root` at `:26-32`). Critically, `applyCodeColors()`
  (`useAgentStore.ts:121-139`) writes the dark-default `--code-*` values to
  `document.documentElement.style` at mount (`App.tsx:58-66`) regardless of theme, and
  the project's own plan note (`20c48336…md:9`) states "Light theme is unchanged
  (existing behavior: hljs colors are theme-independent)." So code blocks DO render
  dark-tuned token colors on a light bg in light theme. ✅ accurate — the report is
  correct and the severity is fair.

### Medium / Low
**No findings.** Spot-checked:
- Sidebar "disabled = line-through + opacity-50": `Sidebar.tsx:62-63` `text-slate-600
  line-through opacity-50`. ✅ accurate.
- Plan-progress 4-step path: `Sidebar.tsx` icon toggles + `RightPanel.tsx` reveal + tab
  select described correctly. ✅

### Constitution compliance
Document only; no code/PowerShell claims. Compliant. The reviewer honestly flags where
it "would need to confirm" (retry-progress rendering, PlanProgress deep-inspection)
rather than asserting — good epistemic discipline.

**UI verdict: no findings (clean, well-evidenced, appropriately hedged).**

---

## PRD report (`2026-prd-review.md`)

### High / Critical
**No findings.** The locked-in-decisions table (§1) is the report's backbone; I verified
the four deviations are all real:
- Tailwind v3 not v4: report cites `package.json:29` `tailwindcss ^3.4.15` +
  `autoprefixer`+`postcss`. ✅ (I confirmed `tailwindcss ^3` presence.)
- No shadcn/Radix: ✅ (no `@radix-ui`), `PLAN.md:54` specifies it.
- No Shiki (highlight.js instead): report cites `Message.tsx`/`MdViewer.tsx` use
  `rehype-highlight`; `PLAN.md:56` lists Shiki. ✅
- No react-diff-view (hand-rolled LCS): report cites `DiffView.tsx:22` `computeDiff` with
  an LCS dp table. I read `DiffView.tsx:22-33` — confirmed a hand-rolled LCS
  implementation. ✅
- `agent.md` location: report cites `project/mod.rs:47` `agent_md: root.join("agent.md")`
  vs `PLAN.md` diagram showing `.coding/agent.md`. I read `project/mod.rs:47` —
  confirmed. ✅

### Medium / Low
**No findings.** The six-gotchas section (§2) is verified against the baseline's
"verified architectural facts"; the gotcha-6 `reasoning_content` citation
(`openai.rs:583`) is plausible and consistent with the baseline. The "What exceeds the
spec" list (vision fallback, spawn_agent, backlog/Run-All, safety-rules, stats,
sub-plan stack) all match the baseline inventory. ✅

### Constitution compliance
The report states it verified `cargo test` green (400 lib + 9 ipc + 5 workflow, exit 0),
names the current branch (`feat/reviewer-report-instructions`, not main) for the
"never commit to main" rule, and notes doc-comment compliance. Compliant.

**PRD verdict: no findings (thorough, evidence-backed, fair).**

---

## Consolidated report (`2026-consolidated-review.md`)

### Critical
- **Critical — H5 repeats the Code Quality false claim.** Consolidated H5 states the
  production path "has **no** dedicated shape test for multimodal/tool-call/assistant
  cases," citing `Quality §2 (High)`. As shown above, `build_request_json` IS unit-tested
  (incl. a multimodal-image-blocks test at `openai.rs:1536`). The consolidation faithfully
  carried over the source report's inaccuracy — which means it did its job *as a
  synthesis* (no invention), but the finding it carries is wrong. Fix: correct both
  the source §2 and consolidated H5 to drop the "no test" sub-claim.

### High
- **No findings otherwise.** I checked for dropped High findings: the four source
  reports' Highs (Architect: god-object + memory lock; Quality: type drift + dead
  async-openai layer; UI: a11y + empty state/buried plan; plus the elevated
  approval-cleanup) all appear in the P1 section (H1–H7). The H7
  Run-All-untested finding is correctly sourced to Quality §4 and its evidence
  (`commands.rs:1096-1281`, `run_all.rs:67-101`) is accurate — I confirmed `run_all.rs`'s
  only test (`:122`) exercises `first_line`, not `checkpoint`/`commit_success`/`rollback`.
  ✅

### Medium
- **Medium — M6 evidence line range slightly loose.** M6 cites `events.rs:133-191` for
  "manager lock on every Started/Finished/Exited/approval." The manager locks are at
  `:135,142,162,184` (within that range), so the citation is correct, but the range
  also covers the approval-insert + emit code (`:176-199`), so it's slightly broader
  than the lock sites. Cosmetic; the architect's own §2 gives the precise line numbers.
  No action needed beyond noting the consolidation aggregated a touch broadly.

### Cross-perspective convergence (the key dedup test)
- **shadcn/a11y convergence — correctly deduplicated, NOT double-counted.** The UI
  report's accessibility finding (§6, root cause = no Radix) and the PRD report's
  shadcn deviation (§1, deviation #2) are merged into a single **H1** with explicit
  "Sources: UI §6; PRD §1-deviation #2" attribution and a "closes a UI High + a PRD
  deviation simultaneously" note. ✅ This is exactly the right handling — no
  contradiction, no double-count.
- **memory lock — correctly deduplicated.** Architect §5 (High) and Quality §5 (sound
  but serialized) collapse to one **H3**. ✅
- **AgentLoop god-object — correctly deduplicated.** Architect §10 (High) and Quality
  §3 (Medium) collapse to one **M1** with both sources cited. (Note: the consolidation
  *downgraded* this from the Architect's High to P2/Medium — a defensible judgment call
  since it's an evolvability cost, not a correctness cliff, but worth flagging that the
  architect's own "High" label was softened. Not an error.) ✅
- **approval cleanup — correctly deduplicated.** Architect §4 (Medium→High under
  multi-agent) → one **H4**. ���

### Constitution compliance
The consolidated report cites build/test green from the baseline, attributes all
sources, and invents no findings. Compliant.

**Consolidated verdict: ONE Critical finding (inherits the H5 false "no test" claim).
Deduplication and cross-referencing are otherwise correct and well-attributed.**

---

## Cross-report section

1. **The single cross-report problem is the H5 evidence claim** (Code Quality §2 →
   Consolidated H5). Both repeat "the production JSON path has no dedicated unit test"
   which is contradicted by `openai.rs:1536` (`build_request_json_sends_image_blocks_when_multimodal`)
   and ~9 sibling tests. This is the only place a report's central evidence is factually
   wrong. Fix in both files; the underlying recommendation (delete the dead
   `async-openai` conversion layer / consolidate) survives with a weakened justification.

2. **No contradictions between reports.** The four perspectives are mutually
   consistent: e.g. the Architect's "brain decoupled from UI" (§7) and the PRD's
   "channel contract matches spec" (§10) agree; the UI's "light theme incomplete" and
   the PRD's "theming implemented (✅)" are reconciled by the UI report specifying the
   *code-highlighting* subset is what's incomplete, not theming overall — no true
   contradiction.

3. **No missed deduplication.** The three genuinely-overlapping findings (shadcn/a11y,
   memory-lock, god-object, approval-cleanup) are all merged into single consolidated
   items with multi-source attribution. No finding appears twice as separate P-level
   items.

4. **Severity-judgment divergence (informational, not an error).** The consolidation
   downgrades the Architect's `AgentLoop` god-object from High → M1/Medium and frames the
   approval-cleanup as High (the Architect called it Medium "→High under multi-agent").
   Both are defensible prioritization choices; they don't contradict the sources, they
   re-weight them. Acceptable for a consolidation.

---

## Summary of action items for the reports themselves

1. **[Critical]** Correct the false "no dedicated unit test" claim in Code Quality §2
   and Consolidated H5. `build_request_json` is unit-tested (incl. multimodal at
   `openai.rs:1536`); restate the finding as "the `convert_message` tests are redundant
   with existing `build_request_json` tests — delete the dead layer, don't backfill."
2. **[Cosmetic]** Consolidated M6's `events.rs:133-191` range could be tightened to the
   actual lock sites (`:135,142,162,184`), but it's not wrong.

Everything else I verified — every other High/Medium citation across all five reports —
was accurate against the current source.