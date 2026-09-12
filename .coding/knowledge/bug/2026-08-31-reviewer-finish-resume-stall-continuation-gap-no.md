+++
title = "reviewer-finish resume stall — continuation gap not covered by mitigation ①"
created = "2026-08-31"
status = "superseded"
+++

Live observation (2026-12-04 session): when the reviewer subagent finished, the main agent received the finish/resume notification but produced NO output and stalled — waiting for the user to say "ok" before reading the review report and continuing the closing sequence. This is a real instance of the mid-task stopping behavior, occurring DURING the session that implemented mitigations ① + ④ for it.

GAP EXPOSED: mitigation ① (the chaining bullet, shipped in commit 8ad3d79) frames continuations around TOOL RESULTS — "Tool results are continuations of your turn, not new questions." But the reviewer-finish notification is a RESUME SIGNAL, not a tool result. So ① would NOT have caught this stall even if it had been active in-session. The reviewer-finish resume is a distinct continuation trigger that ① doesn't explicitly cover.

PARALLEL TO FACTOR 4: the APP_RULES reviewer bullet says "the finish notification resumes you; never sleep/poll" — this covers WHAT happens (you get resumed) but not the CADENCE (upon resume, immediately continue — read the report, fix findings, commit, finish). Same gap pattern as the error-retry rule before ④'s cadence clause was added.

PROPOSED FOLLOW-UP (not yet implemented): extend the chaining/cadence guidance to cover resume-from-notification points. Options:
(a) Broaden ①'s wording from "Tool results are continuations" to also name resume notifications, OR
(b) Add an explicit cadence clause to the reviewer-spawn bullet: "when the finish notification resumes you, immediately continue the closing sequence — read the report, don't stall."

This is a THIRD trigger point (resume-from-notification) distinct from the tool-result chaining (①) and error-retry (④) cases already shipped. Recommend (b) as the cheaper, more targeted fix — same stable-head, cache-friendly pattern as ④.

STATUS: observation only. The shipped ① + ④ (commit 8ad3d79) remain correct for their targeted factors; this is an additional gap, not a defect in what shipped.
