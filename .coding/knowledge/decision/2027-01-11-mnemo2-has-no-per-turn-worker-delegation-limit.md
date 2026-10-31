+++
title = "Mnemo2 has no per-turn worker delegation limit"
created = "2027-01-11"
+++

The user explicitly rejected Mnemo2's four-worker per-lead-turn delegation cap. There must be no per-turn dispatch limit; only the separate per-project concurrent live-worker slot limit remains. begin_lead_turn should manage busy state, not a delegation budget.
