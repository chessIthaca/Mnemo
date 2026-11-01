+++
title = "\"step landed\" in mnemo2 means ledger::merged, never bare status Merged (revert keeps the status)"
created = "2027-01-11"
+++

In the Mnemo repo's mnemo2 code, "this worker's plan step landed on the lead branch" is answered by ledger::merged(worker) — status == Merged AND a merge record present AND merge.revert_commit.is_none() — not by a bare status check. Key fact: revert_merge backs out a merge but KEEPS status == Merged on the record (it only sets merge.revert_commit); after_revert reopens the board step using ledger::merged. Any new feature that names "the worker that landed step X" (or suppresses an unmerged-work warning on that basis) must use ledger::merged, or it will claim landed steps for reverted merges.
