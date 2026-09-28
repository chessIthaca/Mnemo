## Verdict: FINDINGS (2 high, 1 low)

Review of commit 1fc1d92 (branch wt/mnemo, plan e7a80fbb — Day-1 social card + launch post). Scope: every public-facing claim checked against README.md, docs/FEATURES.md, agent.md and the source; SVG markup; card-to-png.html; security sweep; .gitignore/tracking. Read fully: assets/social/day1-card.svg, docs/marketing/day1-post.md, docs/marketing/card-to-png.html, plus source citations below. No source code changed in this commit, so the warning-free build state is untouched (cargo test not re-run by this reviewer — read-only).

## Findings

### F1 (HIGH) — "6 token levers" is wrong: the product has SEVEN

Occurrences of the stale count: SVG pill (day1-card.svg L68 "6 token levers · live savings dashboard"), SVG desc (L3 "six token levers"), post long-form (L18 "✂️ **Six token-saving levers, on by default.**"), short variant (L38 "6 token levers"), alt text (L70 "six token levers").

Evidence: docs/FEATURES.md L56 says "seven independent context-economy levers", and README's own `[general.optimizer]` TOML block (L175-182) lists seven flags: `delta_reads`, `compress_output`, `archive`, `compaction_survival`, `quality_score`, `lean_output_nudge`, `recall_delta`. README L165's prose "Six independent context-economy levers" is stale (the `recall_delta` lever post-dates it) and contradicts its own flag list ten lines below — the marketing assets copied the stale word. Additionally, post L18 names only five levers under a "Six" header (delta re-reads, command-output compression, archive-and-expand, compaction survival, cache-safe nudges — `quality_score` and `recall_delta` unnamed), which is internally sloppy on its face even before counting.

Fix: change to "7 token levers" / "Seven token-saving levers" in all five places (SVG pill + desc, post long-form, short variant, alt text), re-render the PNG (same Inkscape/harness route), and fix README L165 "Six" → "Seven" (documentation sync — this commit propagated the stale number into public artifacts; fixing the artifacts without fixing the README leaves the trap armed for the next card).

### F2 (HIGH) — "Credentials are redacted before the model sees command output" overstates the coverage

Post L24 states this as an unconditional guarantee. The redaction code is real — `redact_secrets` (src/tool/agent/output_compactor.rs:499-532, redacting URI credentials, bearer values, key=value secrets, well-known token prefixes) — but its call sites are conditional:
- shell.rs L650-671: applied only when `compress_output` is ON **and** the filtered output ≥ `compress_min_chars` (default 2000) **and** the command matches a known family / `compress_extra_commands` pattern **and** the compact form is meaningfully smaller. The comment at L640-642 says it plainly: "a non-family command or a compact form that is not meaningfully smaller leaves `combined` byte-identical" — i.e. the model sees that output **unredacted**.
- archive previews (src/agent/turn.rs:3520-3525) and `expand_result` serves (src/tool/agent/expand_result.rs:177).

So a short output (`echo $GITHUB_TOKEN`), an unknown-family command (`terraform plan`), or any fail-open path reaches the model verbatim. The README scopes this correctly ("credential redaction in output compression"); the post generalized it into an unconditional security claim. A public safety claim that doesn't hold is exactly the class of risk this review was commissioned for.

Fix: reword to the scoped truth, e.g. "The output compressor redacts credentials before the model sees long command output." — or, if the unconditional claim is wanted, implement blanket redaction on the shell serve path (apply `redact_secrets` to `combined` unconditionally at shell.rs, which is cheap and by its doc comment "err toward redaction" in spirit). Rewording is the contained fix.

### F3 (LOW) — "a file you've already read is never re-sent whole" is too strong

Post L18. `read_one_delta` (src/tool/agent/read_files.rs:520-630) serves the FULL content on re-read in several by-design fail-open paths: unchanged `.html` files (L551-556 — no line signatures, skeleton would be noise), tiny files where the header/note boilerplate outweighs the full serve (L576-582), small files where the diff+header tips over the full serve (L603-611), and mostly-rewritten files where the diff costs more than a full re-serve (L621-627). "Never" is false; "rarely" is true. Fix: drop "never" (e.g. "re-served as a skeleton or a diff") — one word, same line as F1's fix.

## Verified true (no finding)

- **"4-tier memory that survives sessions"** — README L12-13 "four-tier persistent memory"; post L20's working → episodic → semantic → procedural matches the actual tier system; persistence across sessions is real (`.coding/knowledge/` + `memory.db`).
- **"tree-sitter code graph · 12 languages"** — README L40 lists exactly 12 (Rust, TS/TSX, JS, Python, Go, Java, C/C++, C#, Ruby, PHP, HTML), pinned against the 12 tree-sitter grammars in Cargo.toml (cross-checked by the 2026-09-12 readme review round-3 record). Card, post L22, alt text all consistent.
- **"no edits without a plan"** — the enforced plan-first state machine keeps mutation tools absent from the tool list until a plan is on disk (this reviewer's own per-state allow-list is a live demonstration).
- **"git push always gated"** — `GitTool::never_auto_for` (src/tool/agent/git.rs:527-548) forces the approval prompt for `merge`/`push` regardless of safety mode; constitution + tests pin it.
- **"read-only reviewer that can't approve its own work"** — reviewer subagents are read-only by construction (this session is one: no mutation tools in the allow-list), the main agent can never author a review, and the author cannot self-approve (main-protection ruleset, required approving review).
- **"sharp model plans, cheap model executes"** — per-state model routing + effort resolution, README L78, `model_resolver.rs`.
- **"open source · MIT" / "Rust + Tauri · Windows + macOS" / `github.com/chessIthaca/Mnemo`** — LICENSE is MIT; README badges confirm both platforms; the footer URL matches README L118's clone URL and the origin remote.
- **"live savings dashboard"** — `savings_events` ledger + Settings → Savings / Dashboard (README L192-198), with per-lever rows.

## Other checks

- **SVG internal consistency (criterion 2)** — all `<text>` lengths vs their boxes at the stated font sizes: longest pill "no edits without a plan · git push always gated" ≈ 353px at 15px, starting x=118 in a 512px pill → fits; badge "open source · MIT" ≈ 133px in the 140px pill → fits; headline ≈ 700px < 1056px available; all box captions/bodies fit with margin; arrows have clean 8px gaps with correct marker geometry; the `<desc>` is complete prose. No overflow risk found. (PNG pixel fidelity was pre-verified per the task note — not re-checked.)
- **card-to-png.html (criterion 3)** — 800×420 CSS px × 1.5 = 1200×630 device px ✓; 1× and 2× alternates documented with correct math; `?src=` handled via `URLSearchParams` (works over file:// too); magenta sentinel rationale is sound; relative `../../assets/social/day1-card.svg` resolves correctly from `docs/marketing/`. The 150% default is documented in-file and the post offers two device-independent routes (Inkscape, `npx svgexport`) — the machine-specific-scale risk is adequately mitigated for a dev tool. No finding.
- **Security (criterion 4)** — no absolute local paths, usernames, tokens, or private URLs in the SVG, PNG's source SVG, post, or harness; all links relative or the public repo URL. The commit author identity is ordinary git metadata, not artifact content.
- **.gitignore / PNG tracking (risk c)** — `.gitignore` has no PNG rule; the 177KB `day1-card.png` is deliberately tracked as an upload asset ("committed and ready to post") and future re-renders won't need `-f`. Correct.
- **Multi-platform neutrality** — no code changed; the harness is platform-neutral browser tooling. ✓
- **File-tools-first** — no shell-based file mutation in scope; the PNG was produced via the sanctioned browser/Inkscape/Node routes documented in the post. ✓
- **`.coding/**` bookkeeping accuracy (one line)** — plan e7a80fbb.md matches what shipped (card, post, harness, PNG); note its context quotes README's stale "six levers" phrasing, which is the root of F1 — the plan's accuracy-check passes, the number it inherited doesn't.

## Recommended fix order (small, contained)

1. SVG: pill L68 "6"→"7", desc L3 "six"→"seven" — re-render PNG.
2. Post: L18 "Six"→"Seven" (optionally name all seven or drop the enumeration), drop "never" in the delta-reads parenthetical; L24 reword the credential claim; L38 short variant "6"→"7" (char count becomes 263; re-check the X-weighting note); L70 alt text "six"→"seven".
3. README L165 "Six"→"Seven" (documentation sync).

Reviewed-state: 1fc1d92babc47b5db9e37607fd3affdfe5dc32a6
