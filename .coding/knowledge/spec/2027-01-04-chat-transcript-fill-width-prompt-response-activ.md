+++
title = "chat transcript — fill width + prompt/response/activity indent ladder"
created = "2027-01-04"
status = "superseded"
+++

Chat transcript layout (plan 6d51d819, commit 8dc7022, branch wt/agenticcoding):

1. **Fill width**: the transcript column (Conversation.tsx) has NO max-w cap — it fills the scroll container (which keeps px-4). All former `max-w-3xl` message-content caps are removed; prose fills the width (its `max-w-none` stands).

2. **Indent ladder** (em-based — scales with the app font-size setting; documented above MessageImpl's switch in Message.tsx): level 0 = user prompts (user + steer) flush LEFT, left-aligned (`items-start`/`justify-start`, bubble corner cue `rounded-bl-sm`); level 1 = agent prose (assistant streaming + final, error, qa) + Conversation.tsx's ApprovalPrompt/QuestionPrompt at `pl-[1em]`; level 2 = agent tool-work (tool/memory/vision/skill) at `pl-[2em]`. Level rule = who is speaking: user prompts (0), agent prose (1), agent tool-work (2).

3. **Turn grouping**: prompts carry `pt-[0.75em]` (padding, NOT margin — Tailwind space-y's margin-top would fight an mt) so each turn (prompt → response → tools) stands apart.

4. **Tests**: Conversation.layout.test.ts (source contracts over Conversation.tsx?raw + Message.tsx?raw) pins width fill, alignment, corner cue, turn grouping, and the ladder. Absence assertions are SCOPED to the old prompt wrapper classes (`flex flex-col items-end`, `flex justify-end`) — NOT bare `items-end`/`justify-end` — because Message.tsx also hosts ToolCard/CallDetail/memory/vision cards where those classes could legitimately appear (review F1). The whole-file absences (`rounded-br-sm`, `max-w-3xl`, `max-w-4xl`) are intentional (bubble/width-specific).

Future readability suggestions offered to the user (not implemented): vertical thread line on the activity channel; ~100ch prose cap with full-width code blocks; alternating turn tint; hover timestamps.
