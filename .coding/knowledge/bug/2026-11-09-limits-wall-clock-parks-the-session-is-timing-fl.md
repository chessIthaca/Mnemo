+++
title = "limits_wall_clock_parks_the_session was timing-flaky, 200 ms budget against a first turn of about 200 ms"
created = "2026-11-09"
+++

Symptom: tests/integration/headless_kernel.rs limits_wall_clock_parks_the_session failed intermittently at its `first.final_text == "one"` assert: left was "" because the wall-clock budget ran out before turn 1's first request. Root cause: wall_clock_ms was 200 and counts from spawn, while a passing first turn takes about 200 ms, so the assert raced the budget. Load made it worse: 2 of 5 single cargo runs, 2 of 3 group runs, and 320 of 320 with 64 copies at once on 32 cores; 0 of 30 running the binary alone on an idle machine. A v2.0.0 build failed 2 of 30 the same way. It could fail a release build, because CI does not retry it.

Fix, tests only: no test needs a turn to finish inside a real time budget any more. limits_wall_clock_parks_the_session spends a 1 ms budget before the first prompt (spawn starts the epoch before it returns), then checks LimitReached, no provider request and session_parked, which the old test never checked despite its name. limits_wall_clock_leaves_a_session_under_budget_alone uses a one-hour budget. wall_clock_budget_admits_requests_until_it_elapses in src/agent/loop_impl.rs back-dates the limiter's epoch to cover the switch within one session without waiting. Under the same load each integration test passed 640 of 640. Making the budget never run out, or always run out, in SessionLimiter::begin_request fails the unit test and one integration test each.
