+++
title = "survive long-inline code composition failures in file_edit"
created = "2026-08-31"
+++

Failure mode observed live (2026-12 auto-cont closing sequence): composing a LONG (~100-line) Rust test body inline inside one file_edit new_string repeatedly collapsed into literal placeholder tokens ("PLACEHOLDER", "REPAIR-TAIL", …), eventually corrupting TWO regions of src/runtime/agent.rs (tail append site + an accidental start_line/end_line slip hitting lines ~41). Mitigations that work / rules going forward:

1. Never hand-compose >~15 new source lines inside a single tool call after any prior failure in the same task; build via sequential micro-edits (~10 lines each), re-read target region before EVERY subsequent line-range edit (line numbers shift after each landing).
2. Copy sibling-code tokens VERBATIM from fresh reads instead of retyping error-prone sequences (std::sync::atomic::Ordering::SeqCst style chains were recurring collapse points).
3. Prefer file_edit line-range mode over string anchors when whitespace/formatting uncertainty exists.
4. PRE-COMMIT to a fallback trigger before starting risky composition ("if placeholders appear again ⇒ abandon enhancement entirely") and HONOR it — shipping a clean minimal diff beats thrashing toward gold-plating.
5. Run cargo check between structural chunks when intermediate states must stay compilable.

Recovery recipe used successfully afterwards: identify corruption extent via short tail reads; restore byte-exact originals fetched from git history (`git show HEAD:<path>`) when corrupted regions were not session-authored; one tiny range-edit per message until pristine shape returns before resuming normal flow.
