+++
title = "evidence marks a record; it barely affects its rank"
created = "2026-11-08"
evidence = "mnemo2_memory_eval run on one store snapshot at 0, 0.05 and 0.005; an_unconfirmed_row_ranks_below_an_equal_confirmed_one"
+++

memory_write, memory_supersede and memory_update accept optional `evidence` (a test name, a file and line, a run, the user said so). A semantic or procedural claim without evidence or how_known shows `[unconfirmed]` in memory_search; plan and review digests never do. Nothing is refused for lacking it.

The confirmation bonus in recall scoring is 0.005, a tiebreaker. A penalty on unconfirmed rows was rejected because nearly every record lacks evidence today, which would push durable records below digests. A 0.05 bonus was measured and rejected: paraphrased hit@5 fell 0.400 to 0.325, because most BUG records count as confirmed by mentioning a regression test, so it acted as a class weight. Do not raise it without rerunning the eval. Old records are confirmed by passing `evidence` to memory_update; a row with no knowledge file cannot take it that way, and the tool says so.
