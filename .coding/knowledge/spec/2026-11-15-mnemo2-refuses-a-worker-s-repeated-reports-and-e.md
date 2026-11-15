+++
title = "Mnemo2 refuses a worker's repeated reports and ends the turn on the third"
created = "2026-11-15"
evidence = "the five tests in src-tauri/src/mnemo2/team/tests.rs; full mnemo2 suite 773 passed"
+++

src-tauri/src/mnemo2/team/pump.rs pump_worker keeps a per-turn report streak: consecutive message_parent calls with no other tool call between them (memory_* calls do not break the streak; any other tool, plan_read included, does; a new turn resets it). The 1st report is forwarded unchanged (TeamMessage, lead "reports:" notice). The 2nd and later get REPEATED_REPORT_REFUSED ("not sent to the lead… End your turn now, with no further tool calls") and their HostToolCall TeamEvent is never sent, so runtime.rs builds no notice. On the 3rd the pump sends SessionCommand::Interrupt; the turn ends through the normal Finished path ("finished its turn", worker idle for review). Why: small local models kept re-reporting "Task complete" instead of ending the turn. Tests: first_report_is_unchanged, second_report_in_a_row_is_not_forwarded, other_tool_call_resets_the_report_streak, memory_writes_do_not_reset_the_report_streak, third_report_in_a_row_ends_the_turn (team/tests.rs).
