+++
title = "Code-bearing tool-payload emissions degrade under long sessions — root cause = model double-escaping \\\\n (transport verified clean)"
supersedes = "2026-12-20-indented-multi-line-tool-payload-emissions-corru"
created = "2026-12-20"
+++

REFINED root-cause confirmation (plan d0084b1d step-1, 2026-12-20 session):

SYMPTOM: code-bearing tool-call payloads degrade unpredictably — literal `\n` (backslash+n, 0x5C 0x6E) appears where real newlines (0x0A) should be. Affects ALL THREE output channels: file_edit payloads (tool-call args), agent display (text deltas), and reasoning (reasoning_content deltas). Context-depth-dependent — worsens as sessions grow.

ROOT CAUSE = model double-escaping, NOT a transport regression:
- Transport VERIFIED CLEAN: parse_sse_chunk (openai.rs:1630) extracts strings via .as_str() verbatim; DeltaAccumulator (stream.rs:72) just push_strs fragments. No re-encoding, no double-escaping anywhere in the pipeline.
- normalize_line_endings (line_endings.rs:78) only collapses real \r\n/\r to \n — does NOT touch literal backslash-n. Passes it through unchanged.
- Regex crate AMPLIFIES: re.replacen (file_edit.rs:500) treats replacement &str literally except $1/$2 capture refs. Backslash is NOT an escape in regex replacement strings, so literal \n inserts as two chars \+n. This is the 156-site corruption seen in plan 5c94b497.
- The model emits \\n (JSON double-escape) instead of \n (JSON newline escape) in its JSON output. serde_json correctly parses \\n to literal \n (backslash+n). Transport passes it through. Result: literal \n in files/display.

HARDENING (plan d0084b1d): (1) add read-before-edit guidance to file_edit description; (2) add regex-mode newline warning to use_regex field; (3) normalization safety net — convert literal \n to real newlines in new_string before applying edit (catches model double-escaping automatically).

FOUR mechanisms originally tracked: file_edit exact-match, line-range, \u002f-escape, and this \n-in-regex-replacement. All share the same model-double-escaping root cause.
