+++
title = "workers copy a large file byte-for-byte with chunked file_write, then verify with git diff --no-index"
created = "2026-11-11"
evidence = "The 2026-11-11 copy of 8 files (~139 KB) from frontend/ into mnemo2/src/lib cost about $4.7 on glm-5.3. Every hazard was caught by git diff --no-index."
+++

Workers cannot shell-copy files (the file-mutation policy forbids Copy-Item), so they copy files with the file tools:

1. Read the source in ranges of about 400 lines.
2. Write the target with file_write mode "overwrite" for the first chunk (at most about 4000 chars), then mode "append". Split only at line boundaries, and end each chunk with "\n".
3. Verify with `git diff --no-index <src> <dst>`. No output means the copy is byte-identical.

Known hazards:
- Lines at chunk boundaries get dropped or duplicated.
- Familiar tokens leak in by transcription (for example "MIT OR Apache-2.0" written where the source said "MIT").
- The tools cannot write a file without a trailing newline, so the copy can differ by one byte. Document that in the commit.
- To fix mangled unicode, use a from_read line cite.

Expect such copies to be slow and cost several dollars on a medium model. For big copies, a lead may prefer to do a plain `git mv`/cp itself when the work is pure relocation.
