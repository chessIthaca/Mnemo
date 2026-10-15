## Verdict: FINDINGS (1 high, 1 low)

One HIGH: the flipped `build_volatile_tail` is NOT byte-identical — six string literals use JSON-doubled `\\n` escapes (literal backslash-n text in the composed prompt, where the pre-flip bytes and every neighboring literal use single `\n`), and the CAPABILITIES header lost its leading newline separator. Invisible to the green suite: the order-pin golden is find()-based and every content assertion is newline-free. One LOW: stale doc comments on the kernel-side builders. The verbatim move, the head flip, the turn compose, and the two mirrored tests are verified clean.

### Scope & method
- Delta reviewed: boundary commit 6177d95 (7 files, +410/−295) vs base 6ece926 on wt/mnemo. The working tree's only uncommitted change is the harness Reviews stamp on `.coding/plans/339dfe72.md` (bookkeeping; verified accurate below) — the code delta is the commit itself.
- Read: full `src/app/workflow_policy.rs` (342 lines); `prompt.rs` head/tail builders + the test module (golden, imports, all flipped compositions) plus tree-wide sweeps of every `build_stable_head` (21 refs) and `build_volatile_tail` (17 refs) call site; `loop_impl.rs` `ConstitutionHolder::stable_head` + its 3 test callers (:2781/:2783/:2800); `turn.rs` `install_system_messages` (:3448–3684); `tests.rs` both mirrored attaches (:3360–3448, :3755–3796) + every `WORKFLOW STATE` assertion; both plan frames; git log/show of the delta and prompt.rs history; pre-flip bytes cross-checked three ways — the plan frame's verified anchors (:13-14 quote the old literals verbatim from the pre-flip tree), the plan's step-3 spec (:29), and the running pre-flip binary's own composed footer this session (real newlines + a blank line before `# CAPABILITIES`).
- I could not re-run cargo (read-only surface); instead I swept every tail-content assertion to establish exactly which failures the green suite could and could not have caught.

Reviewed-state: 6177d9574d66390d9696fe107b9410a8157074bd

### Finding 1 — HIGH — src/agent/prompt.rs :484, :486, :495, :497, :499, :502-504 — the rewritten `build_volatile_tail` is not byte-identical: literal backslash-n text + a lost separator

**Defect (a) — `\\n` doubled escapes.** All six string literals in the rewritten body are written with doubled backslashes:

```rust
484:    prompt.push_str("# RECALLED MEMORIES\\n\\n");
486:    prompt.push_str("\\n\\n");
495:    prompt.push_str("# CAPABILITIES\\n\\n");
497:    prompt.push_str("- Tool choice: supported (you may be directed to call specific tools).\\n");
499:    prompt.push_str("- Tool choice: not supported. Choose tools freely.\\n");
502-504: "- Finish reason may be unreliable; tool calls are detected by their presence.\\n",
```

In Rust source `"\\n"` is an escaped backslash + `n`: the composed volatile tail would carry the literal two-character text `\n` instead of line breaks — in exactly the memories and capabilities sections — while the policy-supplied `app_tail` (the moved `workflow_section`, single `\n` = real newlines) and `format_recall_entry`'s output (real `\n  {}\n\n`, :554-561) keep real newlines. The result is a mixed-separator, partially-run-on prompt section riding **every request of every agent** (main loops and subagents alike — the tail is the per-request volatile block), on both the WorkflowPolicy and NoPolicy arms.

Pre-flip bytes used single `\n` — three independent confirmations:
1. **The plan frame's own verified anchors** (.coding/plans/339dfe72.md :13-14, probed from the pre-flip tree): the memories block quoted as `"# RECALLED MEMORIES\n\n" + mem + "\n\n"`, the capabilities block as `"\n# CAPABILITIES\n\n"` — single backslashes.
2. **The plan's step-3 spec** (:29) prescribes the exact lines to land: `prompt.push_str("\n# CAPABILITIES\n\n");` etc. — single backslashes, leading separator present.
3. **The running (pre-flip) binary's composed footer, this session**: `# RECALLED MEMORIES`, `# CAPABILITIES`, and the bullet render with real newlines and real blank lines. (Display-faithfulness is controlled: the same read/search tools show single `\n` in every neighboring literal — `format_recall_entry` :554, `format_primer` :610, the moved `workflow_section`, turn.rs :3564 — and in the plan file's own quotes; a tree-wide literal search for the two-backslash sequence matches only these six production lines, everything else being deliberate backslash-handling code or JSON/SSE test fixtures in `line_endings.rs` / `edit_ops.rs` / provider tests.)

**Defect (b) — dropped leading separator.** :495 is `"# CAPABILITIES\\n\\n"` — the old block was `"\n# CAPABILITIES\n\n"` (leading newline; anchor :14, spec :29). The moved `workflow_section` ends with exactly one trailing newline (`push_state_text` normalizes the consts to one `\n` — workflow_policy.rs :195-198; `STATE_SUBAGENT` ends "applies to you.\n"), so the old composed tail had a blank line between the workflow section and CAPABILITIES; even with (a) fixed, the landed code composes `you.\n# CAPABILITIES` — one newline, no blank line — a second, independent byte drift.

**Why the green suite missed it:** `volatile_tail_section_order_pinned` (:638-663) is `find()`-based on bare headers — order only, no separator bytes; every memories/workflow/capabilities content assertion is a newline-free `contains` (swept: prompt.rs :1554/:1573/:1490, tests.rs :3407/:3797, the rest are negative asserts or hand-built fixtures like openai/tests.rs :1904). No test pins the tail's separator bytes, so the 3038-green run proves nothing about them. The in-code comment :493-494 ("same bytes workflow_section emitted pre-2a.4") and the boundary commit message's byte-identity claim are both contradicted by the code.

**Fix** — restore exactly the plan's step-3 text (single escapes + the leading separator):

```rust
484:    prompt.push_str("# RECALLED MEMORIES\n\n");
486:    prompt.push_str("\n\n");
495:    prompt.push_str("\n# CAPABILITIES\n\n");
497:    prompt.push_str("- Tool choice: supported (you may be directed to call specific tools).\n");
499:    prompt.push_str("- Tool choice: not supported. Choose tools freely.\n");
502-504: "- Finish reason may be unreliable; tool calls are detected by their presence.\n",
```

**Regression test (red today, green post-fix)** — extend the order-pin golden (fixed inputs: fresh Planning workflow, `Capabilities::openai()`, `Some("memory text")`) with separator-byte assertions using real newlines:

```rust
assert!(tail.contains("# RECALLED MEMORIES\n\n"), "memories header needs its real newline + blank line");
assert!(
    tail.contains("\n\n# CAPABILITIES\n\n- Tool choice: supported (you may be directed to call specific tools).\n"),
    "capabilities block needs the blank-line separator + real newlines"
);
assert!(!tail.contains("\\n"), "the tail must contain real newlines, not literal backslash-n text");
```

(the last is the general sentinel — any future `\\n` slip in any tail literal goes red; safe here because the golden's inputs are fixed, no memory snippet can inject backslash-n).

### Finding 2 — LOW — src/agent/prompt.rs :457-460, :515 — stale doc comments on the kernel-side builders (doc sync)

- **:457-460** — `build_volatile_tail`'s doc still describes the pre-flip body: *"The workflow section's static per-state guidance comes from the compiled STATE_* consts; the dynamic lines — CURRENT PLAN / GOAL / PROGRESS / CURRENT STEP, the skill goal, the sub-agent notes, CAPABILITIES — are interpolated from live workflow state."* Post-flip the consts live in `app/workflow_policy.rs`, this fn takes a pre-assembled `app_tail: &str` and interpolates nothing from workflow state, and CAPABILITIES is its own caps-driven block. Rewrite to the new reality: memories block + the policy-supplied app tail + this fn's capabilities block (the split per step file :639).
- **:515** — `build_system_prompt`'s doc: *"Production (`turn.rs`) calls [`build_stable_head`] and [`build_volatile_tail`] separately."* Post-flip production's head goes through `ConstitutionHolder::stable_head` → `policy.stable_head` → `assemble_stable_head` (turn.rs :3498); `prompt::build_stable_head`'s production referencers are now zero (sweep: def :434 + wrapper :526 + 17 test calls + this doc line). Say: the head composes via the holder's policy consult; this wrapper stays for tests / single-message callers.
- Minor, same family: `ConstitutionHolder::stable_head`'s doc (loop_impl.rs :480-485) was to gain the "the policy supplies the blocks" note per the plan's step 4 — accurate as written, but the new `policy` param goes unexplained.

### Harness checks
- **Documentation sync** — one LOW finding above; no README/PLAN.md implications (no intended behavior/config change — the HIGH is a bug against the intended bytes, not a documented behavior).
- **Multi-platform neutrality** — PASS: no `cfg(windows)` anywhere under `src/` (swept); the moved block, the delegate, and the test attaches are platform-neutral.
- **File-tools-first** — PASS: pure source edits across the delta; no shell-mutation signatures, no generated files.
- **Warning-free build** — n/a for the HIGH (semantic drift; `deny(warnings)` cannot see it); the parent's green cargo runs are taken as given for compile-level checks.
- **`.coding/**` accuracy (one line)** — the 339dfe72 frame matches what shipped (steps 1-4 checked, step 5 open; its verified anchors :13-14 are the ground truth Finding 1 cites — the frame is honest, the code deviates from it); the uncommitted 3-line diff is the harness Reviews stamp (base 6177d95, correct); ad14536e.md's 9-line delta is the 2a.3 cost-report tail, accurate.

### Verified clean (no action needed)
- **The verbatim move**: `workflow_policy.rs`'s `workflow_section` app-half + `push_state_text`/`short_step_text`/`short_context_line` + the 5 `STATE_*` consts — real-`\n` bytes throughout, caps param dropped exactly as declared (the app-half never reads caps); CAPABILITIES correctly stays kernel-side; the running binary's footer confirms identical workflow-section composition.
- **The head flip**: both holder arms consult `policy.stable_head(&KernelPromptBlocks::workflow(), …)`; L4 cache semantics (reload_if_changed + cached_head on the LIVE arm, rebuild-per-call on STATIC) unchanged; the 3 test callers pass `&NoPolicy` (:2781/:2783/:2800); the turn passes `&*self.policy` (:3498). No net-new `prompt::build_stable_head` production callers anywhere (all 21 refs accounted for).
- **The turn compose**: the wf guard correctly dissolves (the policy consult locks the workflow internally — holding the guard across it would deadlock); the nudge stays kernel-side with the re-locked state read (:3524-3528); the torn-read window is advisory-only per the documented nudge semantics (false positives/negatives harmless; STATE_COMPLETE guidance is the fallback) — the pre-declared resolution is sound.
- **The two mirrored tests** (tests.rs :3378-3385, :3769-3776): attach `WorkflowPolicy::new(workflow.clone())` — the SAME `Arc` the config carried (one shared workflow, not a copy); assertions unchanged (:3407/:3797 still prove the workflow section reaches the request); the `workflow.clone()` config passes are the minimal E0382 fix; no other content-asserting direct-constructed test exists (the remaining `WORKFLOW STATE` hits are negative asserts or these two tests).
- **Consult-site completeness**: `build_volatile_tail`'s production callers are exactly turn.rs :3506 + the wrapper :527; the 13 test compositions flipped mechanically to `&workflow_section(&wf)` with the module import (:629) and the `WorkflowState` test-module re-import (:630) — assertions unchanged.
- **WorkflowState import** dropped from prompt.rs's top-level imports (compiler-driven, correct — the match moved out with the fn).
