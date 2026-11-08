+++
title = "Laya message nudges fire on plain go-ahead replies to read-back workers"
created = "2026-11-08"
status = "superseded"
+++

Symptom: after a worker's read-back, the lead's plain go-ahead through team_message ("proceed as specified", "your defaults stand, implement") draws a "this got harder, consider tier medium" nudge at confidence 0.80 to 1.00, although the message adds no work and Laya's own dispatch-time rating of the step was routine at 0.80 (seen 3 of 3 times on 2026-11-08). Likely cause: the message nudge classifies the message text alone, without the step or the dispatch label, so an imperative about implementing reads as involved. Not diagnosed further or fixed yet. Workaround: treat message-triggered nudges on read-back replies as noise; strike nudges are a separate signal. See src-tauri/src/mnemo2/team/nudge.rs and src/agent/route_question.rs.
