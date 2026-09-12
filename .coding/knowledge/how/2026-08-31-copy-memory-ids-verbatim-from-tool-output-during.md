+++
title = "copy memory ids verbatim from tool output during sweeps"
created = "2026-08-31"
+++

HOW to avoid fabricated-memory-id errors during hygiene sweeps:
FAILURE MODE seen twice in one session (ad1e51f4 delete attempt, then paired supersedes for tcp_nodelay/comprehensive-diff both bounced with "no memory with id"): reconstructing UUID suffix characters from partial/truncated recall displays instead of copying exact strings from tool output produces invalid ids and wasted round-trips.
RULES:
1) NEVER type any memory/backlog id from recollection or pattern-completion; supersedes/deletes/updates may use ONLY ids read verbatim from memory_search/memory_recall/backlog_list output in the CURRENT conversation context.
2) If display truncates an id mid-string, run one targeted query first and copy from fresh full output before mutating.
3) Batch pattern for sweeps: gather all target ids in read-only search pass -> execute mutations parallel -> handle any bounce by re-querying that row individually, never re-sending identical bad args.
Related discipline records: lookup-taxonomy HOW (symbol vs literal), act-on-nudge-immediately HOW.

