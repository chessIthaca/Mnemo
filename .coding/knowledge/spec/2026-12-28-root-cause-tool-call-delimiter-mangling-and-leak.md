+++
title = "Root-cause tool-call delimiter mangling is upstream model/tokenizer failure"
created = "2026-12-28"
+++

Root cause: model/tokenizer generation failure upstream (NOT a harness bug).
1. Delimiters (e.g. `<tool_call>`, `</tool_call>`) originate upstream from the model's generation and chat template formatting.
2. In the harness transport pipeline (`src/provider/openai.rs` and `src/provider/stream.rs`), SSE streaming parses deltas verbatim: `delta.content` is emitted directly as `LlmEvent::TextDelta` and `delta.tool_calls` as `LlmEvent::ToolCallArgumentDelta`. There is no substring slicing or regex stripping on content deltas (only exact-match stop sequence cutoffs via `find_boundary_cutoff`).
3. Verdict: Tag-prefix loss occurs during model generation or upstream tokenizer/template rendering. When mangled upstream, the provider returns the tokens in `delta.content` rather than structured `tool_calls`, causing the harness to pass it through faithfully as regular text rather than executing a tool call.
