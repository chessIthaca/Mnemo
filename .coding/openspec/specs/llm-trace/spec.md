# llm-trace

## Purpose
What the Trace tab records for every model call the engine makes, the lead's and each worker's alike: attribution, phase timings, live status and agent scoping, so calls can be inspected, filtered and exported.

## Requirements

### Requirement: Every model call is traced with attribution
The Trace tab SHALL record every model request and response the engine makes — the lead's and every worker's alike — with full attribution (actor kind, agent name, worker id, plan and step), phase timings, token usage and cost, and SHALL keep them queryable by agent, model, status, purpose and time, and exportable.

#### Scenario: A worker's call appears with its attribution
- **WHEN** a worker makes an LLM call
- **THEN** the trace list shows the call with the worker's name, plan and step

#### Scenario: Calls are filterable
- **WHEN** the user filters by agent, model, status, purpose or time
- **THEN** only matching calls remain in the list and the charts

### Requirement: Non-overlapping phase segments
The phase bar SHALL draw non-overlapping segments — Setup (containing Compaction), Upload, Server wait, Generation (containing Reasoning and Stall), Retry wait and Tools — and SHALL render Connect and First token as metric chips rather than bar segments. For SSE providers, response headers arrive with the first chunk, so Server wait carries the server's queue and prefill.

#### Scenario: The bar decomposes a call
- **WHEN** a call has prep, upload, server and generation windows
- **THEN** the bar shows those segments in order with no overlap and no double-counted window

#### Scenario: Legacy rows collapse absent segments
- **WHEN** a row predates the upload and server stamps
- **THEN** those segments collapse and the bar renders without NaN

### Requirement: Finer live statuses
An in-flight call SHALL derive its live status from its stamped fields — preparing, uploading, waiting for first token, streaming, or stalled — and terminal calls SHALL classify as complete, failed, cancelled or guard-cut.

#### Scenario: The ladder walks a live call
- **WHEN** a polled row gains its stamps
- **THEN** the status walks uploading, then waiting for first token, then streaming

#### Scenario: A stalled stream is visible
- **WHEN** a streaming call goes silent past the stall threshold
- **THEN** its status shows stalled

### Requirement: Agent scoping with Lead default
The Trace tab SHALL offer an agent dropdown listing Lead first, every observed worker, and All, SHALL default it to Lead, and SHALL scope both the call list and the top Insights charts to the selected agent's calls.

#### Scenario: The tab opens on the Lead
- **WHEN** the trace opens with no saved preference
- **THEN** the dropdown shows Lead selected and the charts show only the Lead's calls

#### Scenario: A worker is selectable
- **WHEN** the user picks a worker from the dropdown
- **THEN** the list and charts show only that worker's calls
