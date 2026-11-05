+++
title = "Mnemo2 Memory tab waits visibly while the project engine starts"
created = "2026-11-03"
+++

Mnemo2's memory commands go through the open project's runtime. While a project open is in flight, Mnemo2State's opening flag is set (an RAII guard around lead_open), and memory_view.rs answers the exact error "the project engine is still starting". The Memory tab treats that as a wait: a spinner with "Waiting for the project engine to start…", an ElapsedTime counter, and a retry every 250 ms until it unmounts. Measured on the real 224 MB memory.db: first list page about 2 ms, overview about 140 ms (stored_model_fingerprints dominates), engine start about 38 ms. Opening the store before the engine was rejected: it would save about 10 ms and add a second SQLite writer.
