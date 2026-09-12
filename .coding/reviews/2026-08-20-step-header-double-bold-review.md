# Review — `fix/step-header-double-bold` (backlog #61: `****bold****` plan-step headers)

Reviewed ALL uncommitted changes (git diff HEAD + 4 untracked files) against the stated
goal: stop double-wrapping already-bold map headers, make header extraction tolerant of
2+ asterisk runs, fix the frontend step-body strip for legacy files, and audit the
markdown renderer.

## Verdict: CLEAN — one LOW-severity doc/behavior nit (non-blocking)

No correctness, regression, security, or constitution findings. The three-layer fix is
correct, byte-safe, and each layer has a regression test that genuinely fails without
its fix. Details of what was verified below, then the one nit.

---

## LOW — `unwrap_bold` strips only ONE layer; doc + schema overpromise "always exactly one layer"

**Where:** `src/tool/workflow/plan.rs:106-108` (doc comment), `plan.rs:115-125`
(`unwrap_bold`), `plan.rs:70-72` (`Map.header` field doc), `plan.rs:212` /
`plan.rs:376-378` (create_plan / update_plan schema descriptions).

A single-pass unwrap handles the realistic input (`"**Add X**"` → `"Add X"`) but not an
already-double-wrapped header: `unwrap_bold("****X****")` → strip one `**` from each
side → `"**X**"`, then `to_text` re-wraps → `format!("**{}**", "**X**")` =
`****X****` — the exact broken form the backlog reported. The doc comment says the
result is "always exactly one layer" and the schema says "an already-bold header is
unwrapped to a single layer"; both are false for this input.

Why non-blocking: the model would have to invent quadruple asterisks in a map header
(the realistic copy-the-example input is single-wrapped, which is handled), and even if
it did, the new tolerant extractor (`extract_bold_header_impl`) and `stepBody` regex
both parse `****X****` correctly at display time — so the user-visible bug stays fixed.
It is purely a documented-behavior gap.

Suggested fix (either): loop to a fixpoint (unwrap repeatedly while the inner is
non-empty and still `**`-bounded), or soften the three doc/schema wordings to "an
already-bold header has one layer of `**` markers removed". A loop is 3 lines and makes
the docs true; recommend it, but a wording fix is also acceptable.

---

## Verified correct (no findings)

### `extract_bold_header_impl` — `src/workflow/plan_file.rs:407-464`
- **Byte-index safety (multibyte UTF-8):** the leading-run length is computed in bytes
  over ASCII `*` (`plan_file.rs:415`), so `&trimmed[lead..]` is always a char boundary.
  In the scan loop, `b'*'` (0x2A) can never be a UTF-8 continuation byte (0x80–0xBF),
  so `close_start`/`close_end` are always boundaries; `rest[..close_start]` and
  `rest[close_end..]` cannot panic. Traced `**日本語** — body` → `Some("日本語")`.
- **Runs of 5+:** `******X** — body` → lead 6, closer after X → `Some("X")`. `*****`
  alone → rest empty → `None`. `**src/*.rs**` → single interior `*` is not a closer →
  `Some("src/*.rs")`. Headers with separators inside (`**A — B** — body`) →
  `Some("A — B")`.
- **Existing semantics preserved:** mid-sentence bold ("Use **bold** for emphasis") →
  `None` (lead 0); empty bold (`**** — body`, `** ** — body`) → `None` via the empty-
  header check; closer followed by a plain word (`**H** word`) → `None` (leading-token
  check, same as before). The only behavior changes are the intended tolerances:
  `***X***` old `None`/`Some("*X")` → new `Some("X")`, and consuming the ENTIRE closing
  run makes `**X***— body` extract where the old code saw a stray leading `*`. Both are
  deliberate and covered by the new tests.
- **Both header producers fixed end-to-end:** parse path (`plan_file.rs:185`), freeform
  `## Step N` path (`plan_file.rs:169`), and `Workflow::update_plan` replaced steps
  (`src/workflow/mod.rs:486`) all go through the fixed extractor, so StatusBar.tsx:658
  and PlanProgress.tsx:149 (`step.header ?? step.text`) — intentionally unedited — now
  display legacy double-wrapped steps correctly, as claimed.

### `unwrap_bold` — `src/tool/workflow/plan.rs:115-125`
Single-wrapped, whitespace-padded (`" **X** "` → `"X"`), `*italics*`, mid-text `**`,
and all-marker (`"**"`) inputs all behave as documented and tested. update_plan shares
`StepInput::to_text` (`plan.rs:154`, `steps_to_text`), so the unwrap applies to both
tools.

### Frontend `stepBody` — `frontend/src/lib/planSteps.ts:24-26`
- Non-greedy `[\s\S]*?` + greedy `\*{2,}` closer: `****Verify builds/tests** — run
  them` → `run them` with no stray `**` (asserted by test); `**H** — use **bold**
  here` → `use **bold** here` (mid-body bold preserved); `**src/*.rs**` works; only one
  separator consumed (`**H** — -flag handling` → `-flag handling`, tested).
- **Divergence from the Rust extractor is unreachable in practice:** `stepBody` lacks
  Rust's leading-token check (it would strip `**H**word` → `word` where Rust says
  `None`), but it is only invoked under `step.header ?` (PlanProgress.tsx:257), and
  `step.header` is only ever `Some` when the stricter Rust check already passed. No
  path feeds `stepBody` a text Rust rejected.
- Freeform `## Step N — Title` plans: text has no leading `**`, regex no-matches, body =
  full text under the parsed header — identical to the old inline regex's behavior
  (pre-existing, not introduced here).

### Test quality — regression tests genuinely fail without their fixes
- `step_input_to_text_unwraps_already_bold_header` — without `unwrap_bold`, to_text
  emits `****…****` → assertion fails. ✓
- `create_plan_normalizes_already_bold_map_header` — the `steps[0].text` assertion
  fails without the unwrap even if the extractor is fixed, so each layer is caught
  independently. ✓
- `bold_header_tolerates_double_wrapped_runs` / `parse_double_wrapped_checklist_line_
  extracts_header` — old extractor returns `None` (empty header) for the exact legacy
  line → both fail. ✓
- `planSteps.test.ts` "no stray asterisks" — old inline regex leaves
  `Verify builds/tests** — run them` → fails. ✓
- `markdownRendering.test.ts` renders through the real `MarkdownImpl`
  (react-markdown + remark-gfm + rehype-highlight) via `renderToStaticMarkup` — a
  legitimate executable contract answering "what other formatting is failing?" (nothing
  in the renderer; 8/8 pass).

### Security — no injection surface
`frontend/src/components/chat/MarkdownImpl.tsx:9-11` uses only remark-gfm +
rehype-highlight; no `rehype-raw` anywhere (raw HTML is escaped by react-markdown
default). New display paths render `{step.header}` / `{stepBody(step.text)}` as React
text children — no `dangerouslySetInnerHTML`. The autolink in the test is GFM
autolink-literal with an renderer-built `href`.

### Constitution compliance
Doc comments present on `unwrap_bold`, updated on `extract_bold_header*`, `to_text`,
`Map.header`, `stepBody`, and both changed schemas; no `#[allow(...)]` anywhere in the
diff; a regression test exists for each of the three defect layers; submitter verified
`cargo test` 1131/0/1 + frontend 360/360 + builds green and warning-free under
`#![deny(warnings)]` (consistent with the diff — no dead code or unused imports).
`.coding/` churn (backlog compaction, stack.json, new plan md) is app-generated
bookkeeping and matches the expected flow.

### Design decisions — concur
Plain-text + `font-semibold` headers (not markdown-rendered) is the right call given
the 2s-poll re-render; not rewriting existing plan files is safe because parse-time
extraction fixes display while `step.text` stays the verbatim source of truth
(explicitly asserted in the parse regression test); leaving StatusBar unedited is
correct — it consumes the now-fixed `header` field.

(Fittingly, the new plan file `.coding/plans/5b1abb5e-….md` itself contains
`****…****` step lines — string steps passed verbatim — which now display correctly
through the parse-time fix, live proof the legacy path works.)
