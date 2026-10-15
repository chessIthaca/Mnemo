## Verdict: FINDINGS (1 high, 0 low)

The delta is exactly the briefed close-out (src surface = prompt.rs 43+/12−, loop_impl.rs +4; everything else is `.coding` bookkeeping), and all of it except the HIGH's six literals is correctly landed: the three regression asserts are the round-1 prescription verbatim (:681/:686/:691) and all three LOW doc rewrites are accurate to the post-flip reality. But the six literals themselves landed as **raw-newline multi-line plain string literals** — an unterminated-string syntax error in Rust — so the committed tree cannot compile, the round-1 HIGH is not validly closed, and the dispatcher's green-run claims cannot describe the committed bytes.

Reviewed-state: HEAD = fcaba75 (src identical to 1746b2d; prompt.rs/loop_impl.rs clean vs HEAD per git status — the working tree IS the committed content).

Reviewed-state: fcaba75bf06b9fae2807f358a383d90fe576eb5e

### Scope & method
- Delta since base 6177d9574d66390d9696fe107b9410a8157074bd enumerated with git log: **1746b2d** (the close-out) + **fcaba75** (.coding-only side-car: backlog.jsonl +1, the escape-trips HOW record +13) + the uncommitted remainder (3 modified `.coding` files + 2 untracked bug records — de-noise bookkeeping).
- Read: git show 1746b2d (stat: src = exactly prompt.rs + loop_impl.rs; the other two paths are the plan frame +3 and the round-1 report +79 — bookkeeping) and its diff tail (the golden's three new asserts as raw git bytes: single-line, `\n`-escaped); git show fcaba75 (stat); the full uncommitted diff; every touched region in the current tree — prompt.rs :440-548 (build_volatile_tail + both rewritten docs + the wrapper compose) and :628-700 (the golden), loop_impl.rs :458-505 (the holder doc + both L4 cache arms), the 339dfe72 plan frame in full, the HOW record in full, the round-1 report in full (fix block :38-45 and assert prescription :50-58 cross-checked byte-for-byte); plus the graph view of build_volatile_tail (span :475-519, 11 callers).
- **Byte-triangulation (load-bearing for Finding 1):** read_files, the content index (rebuilt this session from the physical file), and the graph index (tree-sitter, same physical file) all agree on the identical physical line structure and numbering (build_volatile_tail :475-519; the golden fn :650; asserts at :681/:686/:691). A display-decode-and-renumber artifact would desync these indexes (they are built from the file's physical lines) — they do not desync. The multi-line literals are physical bytes in the file, not a rendering artifact.
- I could not run cargo (read-only surface). The parse-level claim in Finding 1 is established from the source bytes plus the Rust grammar via three independent read channels, not from a test run.

### Finding 1 — HIGH — src/agent/prompt.rs :485-487, :489-491, :500-503, :505-506, :508-509, :512-515 (build_volatile_tail) — the six restored literals landed as raw-newline multi-line strings: invalid Rust, the tree cannot compile, the round-1 HIGH is not closed

The fix was supposed to restore the six single-`\n`-escaped literals (round-1 report :38-45). The committed file instead carries the intended bytes as **raw newlines inside plain `"…"` literals**. Physically (all three indexes agree):

```
485: prompt.push_str("# RECALLED MEMORIES
486: (empty)
487: ");
489: prompt.push_str("
490: (empty)
491: ");
500: prompt.push_str("
501: # CAPABILITIES
502: (empty)
503: ");
505: prompt.push_str("- Tool choice: supported (you may be directed to call specific tools).
506: ");
508: prompt.push_str("- Tool choice: not supported. Choose tools freely.
509: ");
512: prompt.push_str(
513:     "- Finish reason may be unreliable; tool calls are detected by their presence.
514: ",
515: );
```

while every neighboring literal and the new asserts are proper single-line `\n`-escaped text (nudge :392 `push_str("\n# PLAN NUDGE\n\n")`; format strings :566/:625; assert needles :681/:686; sentinel :691 `!tail.contains("\\n")`).

**Why it cannot compile:** a plain Rust string literal may not contain a raw newline — the literal is unterminated at the line's end. The only legal multi-line forms are raw strings (`r#"…"#` — absent here) and the `\<LF>` line-continuation escape, which requires a trailing backslash on the physical line. The file's own consts show that legal style (:238-252 — every physical line ends with a visible `\`, e.g. :244 `… if you \`), and these six lines demonstrably lack it: the content index shows :485 physically ending at `…MEMORIES` and :505 at `…specific tools).` — no `\n");` tail on either line. Six unterminated string literals = parse failure for the crate. Consequently `cargo build --workspace`, the 3010-test suite, the feature-gated trees, and the "goldens trio" claimed in the commit message and the dispatch brief cannot have run against the committed bytes; they necessarily describe an earlier, pre-mangling state (git status: prompt.rs clean vs HEAD — the working tree IS 1746b2d's content).

**The intent is right; only the delivery is wrong.** The multi-line content IS the round-1 prescription — real newlines, the restored leading separator before `# CAPABILITIES` (`\n# CAPABILITIES\n\n`), the `\n`-terminated bullets — i.e. the pre-flip composed bytes, semantically exactly what the HIGH's fix should produce. The transport decoded the payload's escapes into the file instead of splicing the escape text. The HOW record committed in fcaba75 (`.coding/knowledge/how/2027-01-11-file-edit-string-literal-escape-trips-typed-deli.md`) prescribes "type ONE backslash per real escape in new_string" — followed literally, a typed `\n` payload is a JSON-encoded newline and lands a REAL newline in the file, which is precisely what the bytes show. That working pattern is itself the trap, and the record's "green post-fix (the plan 339dfe72 fix landed them)" claim contradicts the tree.

**Fix** — re-deliver the six literals in single-line escaped form (ONE physical line each; the payload string must carry backslash+n as two characters so the file receives the two-character escape):

```rust
prompt.push_str("# RECALLED MEMORIES\n\n");
prompt.push_str("\n\n");
prompt.push_str("\n# CAPABILITIES\n\n");
prompt.push_str("- Tool choice: supported (you may be directed to call specific tools).\n");
prompt.push_str("- Tool choice: not supported. Choose tools freely.\n");
prompt.push_str("- Finish reason may be unreliable; tool calls are detected by their presence.\n");
```

then ground-truth with the HOW record's own backslash-count probe (expect exactly 1 backslash per escape and 0 raw newlines inside these literals), `cargo build --workspace`, and the golden green (`cargo test -p mnemo --lib -- volatile_tail_section_order_pinned`); re-run the suite battery on the fixed tree. Amend the HOW record in the same close-out (memory_amend): the typed payload must carry `\\n` (double-backslash-n) so the payload string delivers the two-char escape — one backslash typed lands a raw newline — and correct its "green post-fix … landed them" line.

### Verified in the delta (no action needed)
- **Delta composition matches the brief mechanically**: 1746b2d's src surface is exactly prompt.rs + loop_impl.rs (43+/12− and +4); the other two paths in that commit (plan frame +3, round-1 report +79) are bookkeeping; fcaba75 is .coding-only (+14); the uncommitted remainder is .coding-only. Nothing else touched src since the base.
- **The three regression asserts are the round-1 prescription verbatim** (prompt.rs :675-693): the memories-header real-newline+blank-line assert (:681), the capabilities blank-line-separator assert (:686 — needle `"\n\n# CAPABILITIES\n\n- Tool choice: supported (you may be directed to call specific tools).\n"`), and the literal-backslash-n sentinel (:691 — `!tail.contains("\\n")`) — on the golden's fixed inputs (fresh Planning workflow via `workflow_section(&wf)`, `Capabilities::openai()`, `Some("memory text")` — nothing can inject backslash-n, sentinel safe, generalizes as designed). Statically they are red on the round-1 defect bytes (the doubled-escape tail carries `\n` text: the real-newline needles fail, the sentinel fires) and green on the intended fixed bytes — the red→green design holds. But nothing can have RUN on the committed tree (it does not parse), so the executed red/green-proof claims refer to a pre-mangling state.
- **No weakening anywhere**: the original order-pin asserts (:667-674) intact; build_volatile_tail's signature `(caps, app_tail: &str, memories)` untouched by this delta; the wrapper's compose via `crate::app::workflow_policy::workflow_section(workflow)` (:539-543) untouched; the 13 test compositions (flipped at the boundary, round-1-verified) untouched here; the caller set unchanged (graph: the one production caller turn.rs install_system_messages :3484, the wrapper, and 9 test callers — the round-1 consult-site set).
- **All three LOW doc fixes landed and are accurate to the post-flip reality**: build_volatile_tail's doc (:454-474) — the workflow section rides in via `app_tail` (WorkflowPolicy::volatile_tail wraps the same bytes the kernel-side workflow_section produced pre-2a.4), CAPABILITIES stays kernel-side per the step file :639, this fn owns the memories + caps blocks — matching the moved consts' reality (workflow_policy.rs, round-1-verified). build_system_prompt's doc (:521-531) — "Production (`turn.rs`) consults the agent's policy for the head (the constitution holder's `policy.stable_head`, 2a.4) and composes the tail via [`build_volatile_tail`]" — the round-1 :63 prescription. The holder's doc (loop_impl.rs :486-489) — the policy param explained (both NoPolicy and the factory-attached WorkflowPolicy produce today's bytes via `assemble_stable_head(&KernelPromptBlocks::workflow(), …)`), with the L4 arms (:496-505) reading unchanged.

### Harness checks on the delta
- **Documentation sync** — PASS: the doc rewrites ARE the sync, accurate to the shipped split; no README/PLAN.md implications (the HIGH is a byte-delivery bug against already-documented intent, not a documented behavior).
- **Multi-platform neutrality** — PASS: nothing platform-specific in the hunks; no `cfg(windows)` additions; the literals/docs/asserts are platform-neutral bytes.
- **File-tools-first** — PASS mechanically (no shell-mutation signatures anywhere in the delta); note the failure mode here is a file-tool DELIVERY trip, covered by Finding 1.
- **Warning-free build** — moot at parse level: the tree fails before warnings; the green-run claims are part of Finding 1.

### `.coding/**` accuracy (one line)
Accurate except where Finding 1 implicates it: the plan frame's Reviews stamps (round-1 base 6177d95; round-2 dispatch state fcaba75) are harness-correct, and the uncommitted supersede marks + the two new closed-bug records match the standing closed/landed state of both bugs; but the 1746b2d commit message ("Fixed to single \n escapes … green post-fix") and the HOW record's "the plan 339dfe72 fix landed them" do not match the landed bytes — correct both with the re-delivery (the HOW record via memory_amend per the fix).

### Process remark (one line)
The uncommitted `.coding` carry-over (bug-record supersede marks, the two new closed-bug records, the round-2 Reviews stamp) is accurate de-noise/bookkeeping, not re-reviewed line-by-line.
