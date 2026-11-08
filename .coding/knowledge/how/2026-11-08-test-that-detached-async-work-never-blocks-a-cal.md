+++
title = "test that detached async work never blocks a call: gate the stub, don't time the call"
created = "2026-11-08"
+++

To prove a call returns without waiting on background work (e.g. a team message vs. Laya's drift classification), hold the background dependency behind a gate the test controls instead of making it slow and bounding the call's elapsed time. Pattern (src-tauri/src/mnemo2/team/tests.rs: StubDispatchClassifier::gated + drift_never_blocks_the_message): a tokio::sync::watch::channel(false) gate; the stub counts each call first, then calls numbered >= first_gated_call wait on `wait_for(|open| *open)`. Wait for earlier calls to finish (a sync point such as the Route row), make the call under a generous hang guard (20 s), assert the background effect has not happened yet, open the gate, then wait for the effect with eventually(). A blocking implementation either hangs or times out behind the closed gate, so the test still fails for it (verified with a mutation that awaited the spawned task). Wall-clock bounds like `elapsed < 1 s` measure machine load and flake when several suites run at once.
