+++
title = "a full D: drive stops Mnemo itself (team ledger writes fail)"
created = "2026-11-24"
evidence = "team_resume error on 2026-11-24; Get-PSDrive D showed 0 free before and 115.3 GB after deleting the three incremental folders and 221 pool-1 .tmp* leftovers."
+++

When D: reaches 0 bytes, Mnemo can't save its team ledger: team_resume and other state changes fail with "could not write the team ledger: There is not enough space on the disk. (os error 112)", and workers stall. On 2026-11-24 the cause was Rust incremental caches, about 49 GB across D:\Mnemo\target and the pool checkouts (pool-1 alone had 25 GB in target\debug\incremental), plus leaked test folders in target. Fix: confirm no cargo/rustc is running, then delete `target\debug\incremental` in the main checkout and the pools (a pure cache; the next build is just slower) and the `.tmp*` leftovers directly in each target. That freed enough for 115 GB free. Never delete FactoryGame's folders as part of this. Check free space before long full test runs.
