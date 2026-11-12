+++
title = "tests that edit a watched file must set its mtime explicitly — a write+sleep can keep the old mtime on Windows"
created = "2026-11-12"
evidence = "CI run 43236642429 job 131245650280 log (src/agent/tests.rs:4239 panic, prompt holds PROJECT-RULE-V1); src/project/agent_md.rs source_rereads_after_mtime_change comment documents the same Windows flake."
+++

Code that reloads a file on mtime change (ConstitutionSource::reload_if_changed in src/project/agent_md.rs compares SystemTime by exact equality) misses an edit whose write lands in the same file-timestamp tick as the previous write; Windows updates file times on a coarse clock tick. A test that writes the file and THEN sleeps does not help: the mtime is fixed at the write. agent::tests::constitution_reread_after_agent_md_edit (src/agent/tests.rs) flaked this way on Windows CI (PR #125, turn-2 prompt still held PROJECT-RULE-V1) and now bumps the mtime with File::set_times(now + 60 s), the pattern project::agent_md::tests::source_rereads_after_mtime_change already used. A test that goes from a missing file to a present one is safe (None -> Some).
