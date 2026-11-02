import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  activeTraceChips,
  actorLabel,
  actorSubLabel,
  callStatus,
  capText,
  cacheReuseLabel,
  createTracePageTransport,
  DEFAULT_TRACE_FILTERS,
  finishLabel,
  formatDuration,
  formatTokens,
  groupTraceRows,
  loadTracePrefs,
  messageBlocks,
  messageText,
  phaseTotalMs,
  prefixReuse,
  previousComparableRow,
  prettyJson,
  purposeLabel,
  requestView,
  responseText,
  responseView,
  saveTracePrefs,
  seedTracePagePreview,
  shortTurnId,
  traceAgentOptions,
  traceAge,
  traceClock,
  traceClockMs,
  traceCostLabel,
  traceDayKey,
  traceDayLabel,
  traceEntryFilters,
  traceListQuery,
  traceModelOptions,
  tracePhases,
  traceStatus,
  traceTimeRange,
  type TraceAttribution,
  type TraceDetail,
  type TracePageTransport,
  type TraceRow,
} from "./trace";

vi.mock("./runtime", () => ({ isTauriRuntime: vi.fn(() => false) }));

const row = (overrides: Partial<TraceRow> = {}): TraceRow => ({
  id: 1,
  ts_ms: 0,
  model: "lead-model",
  base_url: "https://lead.invalid/v1",
  provider: "lead-endpoint",
  http_status: null,
  usage: null,
  finish_reason: null,
  is_complete: false,
  version: 1,
  ttft_ms: null,
  generation_ms: null,
  reasoning_ms: null,
  connect_ms: null,
  tools_ms: null,
  prep_ms: null,
  compact_ms: null,
  backoff_ms: null,
  stall_ms: null,
  error: null,
  cancelled: false,
  response_truncated: false,
  response_evicted: false,
  guard_cut: null,
  run_id: "run-1",
  attribution: null,
  cost_micros: null,
  priced: false,
  retained: false,
  ...overrides,
});

const detail = (overrides: Partial<TraceDetail> = {}): TraceDetail => ({
  ...row(),
  request_json: {},
  response_raw: null,
  request_truncated: false,
  raw_tool_calls: null,
  ...overrides,
});

describe("traceClock", () => {
  it("renders the request's local wall-clock time", () => {
    const at = new Date(2026, 10, 2, 9, 5, 7).getTime();
    expect(traceClock(at)).toBe("09:05:07");
  });
});

describe("formatTokens", () => {
  it("keeps small counts exact and shortens the rest", () => {
    expect(formatTokens(0)).toBe("0");
    expect(formatTokens(999)).toBe("999");
    expect(formatTokens(1_234)).toBe("1.2k");
    expect(formatTokens(12_345)).toBe("12k");
    expect(formatTokens(2_500_000)).toBe("2.5M");
  });
});

describe("formatDuration", () => {
  it("shows milliseconds, then seconds, and a dash when absent", () => {
    expect(formatDuration(null)).toBe("—");
    expect(formatDuration(undefined)).toBe("—");
    expect(formatDuration(840)).toBe("840 ms");
    expect(formatDuration(2_400)).toBe("2.4 s");
    expect(formatDuration(12_000)).toBe("12 s");
  });
});

describe("traceStatus", () => {
  it("reads an unfinished row as streaming", () => {
    expect(traceStatus(row())).toEqual({ label: "streaming", tone: "live" });
  });

  it("shows a clean turn's finish reason", () => {
    expect(
      traceStatus(row({ is_complete: true, http_status: 200, finish_reason: "tool_calls" })),
    ).toEqual({ label: "tool_calls", tone: "ok" });
  });

  it("keeps a refusal's HTTP status and marks it an error", () => {
    expect(
      traceStatus(
        row({
          is_complete: true,
          http_status: 400,
          error: "unexpected keyword 'strict' in tool schema",
        }),
      ),
    ).toEqual({ label: "400", tone: "error" });
  });

  it("separates a cancelled turn from a provider failure", () => {
    expect(traceStatus(row({ is_complete: true, cancelled: true }))).toEqual({
      label: "cancelled",
      tone: "stopped",
    });
  });
});

describe("capText", () => {
  it("leaves a short payload whole", () => {
    expect(capText("hello", 10)).toEqual({ text: "hello", capped: false, total: 5 });
  });

  it("clamps a long payload and remembers the whole length", () => {
    const capped = capText("x".repeat(25), 10);
    expect(capped).toEqual({ text: "x".repeat(10), capped: true, total: 25 });
  });

  it("never splits a surrogate pair at the cut", () => {
    const capped = capText(`${"a".repeat(9)}😀tail`, 10);
    expect(capped.text).toBe("a".repeat(9));
    expect(capped.capped).toBe(true);
    expect(capped.total).toBe(15);
  });
});

describe("messageText", () => {
  it("flattens content parts and lists delivered tool calls", () => {
    expect(
      messageText({
        role: "user",
        content: [{ type: "texta", text: "hello" }, { type: "image_url" }],
      }),
    ).toBe("hello\n[image]");
    expect(
      messageText({
        role: "assistant",
        content: "checking",
        tool_calls: [{ function: { name: "read_file", arguments: "{\"path\":\"a\"}" } }],
      }),
    ).toBe("checking\n→ read_file({\"path\":\"a\"})");
  });
});

describe("requestView", () => {
  it("splits the body into system, conversation, tools and parameters", () => {
    const view = requestView(
      detail({
        request_json: {
          model: "worker-model",
          temperature: 0,
          stream: true,
          messages: [
            { role: "system", content: "You are the lead." },
            { role: "user", content: "fix the failing test" },
          ],
          tools: [
            { type: "function", function: { name: "read_file" } },
            { type: "function", function: { name: "apply_patch" } },
          ],
        },
      }),
    );

    expect(view.system).toEqual(["You are the lead."]);
    expect(view.messages).toEqual([{ role: "user", text: "fix the failing test" }]);
    expect(view.tools?.names).toEqual(["read_file", "apply_patch"]);
    expect(view.tools?.json).toContain("\"name\": \"apply_patch\"");
    expect(view.params).toContain("\"model\": \"worker-model\"");
    expect(view.params).toContain("\"stream\": true");
    expect(view.raw).toContain("\"messages\"");
  });

  it("falls back to the raw body when it is not a JSON object", () => {
    const view = requestView(detail({ request_json: "<html>proxy error</html>" }));
    expect(view.messages).toEqual([]);
    expect(view.system).toEqual([]);
    expect(view.tools).toBeNull();
    expect(view.params).toBe("");
    expect(view.raw).toBe("<html>proxy error</html>");
  });
});

describe("responseText", () => {
  it("returns the raw response, and nothing for an evicted payload", () => {
    expect(responseText(detail({ response_raw: "data: [DONE]\n" }))).toBe("data: [DONE]\n");
    expect(
      responseText(detail({ response_raw: null, response_evicted: true })),
    ).toBe("");
  });
});

describe("prettyJson", () => {
  it("pretty-prints values and passes strings through", () => {
    expect(prettyJson({ a: 1 })).toBe("{\n  \"a\": 1\n}");
    expect(prettyJson("plain")).toBe("plain");
    expect(prettyJson(undefined)).toBe("");
  });
});

// ---------------------------------------------------------------------------
// The Trace route's contract mirror and helpers.
// ---------------------------------------------------------------------------


describe("trace page contract helpers", () => {
  const attr = (overrides: Partial<TraceAttribution> = {}): TraceAttribution => ({
    actor_kind: "worker",
    agent_id: "agent-tracePag",
    session_id: "session-1",
    agent_name: "design-trace",
    worker_id: "design-trace",
    plan_id: "c3ba2b58",
    plan_step: 0,
    turn_id: "178d91f2-42",
    purpose: "turn",
    ...overrides,
  });

  describe("callStatus", () => {
    it("classifies terminal and live calls the way the filter values read", () => {
      expect(callStatus(row({ error: "boom" }))).toEqual({ key: "failed", label: "Failed", tone: "error" });
      expect(callStatus(row({ cancelled: true }))).toEqual({ key: "cancelled", label: "Cancelled", tone: "stopped" });
      expect(callStatus(row({ guard_cut: { boundary: "length", byte_idx: 0, delta_len: 1, chars_before: 0, prefix_empty: true } }))).toEqual({
        key: "guard_cut",
        label: "Guard cut",
        tone: "guard",
      });
      expect(callStatus(row({}))).toEqual({ key: "in_progress", label: "Waiting", tone: "live" });
      expect(callStatus(row({ ttft_ms: 300 }))).toEqual({ key: "in_progress", label: "Streaming", tone: "live" });
      expect(callStatus(row({ is_complete: true }))).toEqual({ key: "complete", label: "Complete", tone: "ok" });
    });
  });

  describe("attribution labels", () => {
    it("names the lead, workers by captured name, and unattributed rows", () => {
      expect(actorLabel(null)).toBe("Unattributed");
      expect(actorLabel(attr({ actor_kind: "lead" }))).toBe("Lead");
      expect(actorLabel(attr())).toBe("design-trace");
      expect(actorLabel(attr({ actor_kind: "subagent", agent_name: "" }))).toBe("Subagent");
    });

    it("adds the worker's plan and step, one-based", () => {
      expect(actorSubLabel(attr())).toBe("Plan c3ba2b58 · Step 1");
      expect(actorSubLabel(attr({ plan_step: null }))).toBe("Plan c3ba2b58");
      expect(actorSubLabel(attr({ actor_kind: "lead" }))).toBe("");
      expect(actorSubLabel(null)).toBe("");
    });

    it("translates purposes and shortens turn ids", () => {
      expect(purposeLabel("turn")).toBe("Conversation turn");
      expect(purposeLabel("memory_consolidation")).toBe("Memory consolidation");
      expect(purposeLabel("other")).toBe("Other");
      expect(shortTurnId("178d91f2-42")).toBe("178d…2-42");
      expect(shortTurnId("short")).toBe("short");
    });
  });

  describe("token, cost and timing helpers", () => {
    it("separates 'not reported' from a real cache share", () => {
      expect(cacheReuseLabel(null)).toBe("cache n/a");
      expect(cacheReuseLabel({ prompt: 1000, completion: 10, reasoning: 0, cached: 0 })).toBe("0% cached");
      expect(cacheReuseLabel({ prompt: 1000, completion: 10, reasoning: 0, cached: 920 })).toBe("92% cached");
    });

    it("keeps an unpriced model a dash, never zero dollars", () => {
      expect(traceCostLabel(null)).toBe("—");
      expect(traceCostLabel(80_000)).toContain("$");
      expect(traceCostLabel(80_000)).not.toBe("$0.00");
    });

    it("nests compaction inside setup and stalls inside generation", () => {
      const phases = tracePhases(row({ prep_ms: 500, compact_ms: 200, connect_ms: 300, ttft_ms: 900, generation_ms: 4000, stall_ms: 1200, backoff_ms: 100 }));
      expect(phases.map((phase) => phase.key)).toEqual(["setup", "connect", "first_token", "generation", "retry_wait"]);
      expect(phases[0].inside?.[0]).toEqual({ key: "compaction", label: "Compaction", ms: 200 });
      expect(phases[3].inside?.[0]).toEqual({ key: "stalls", label: "Stalls", ms: 1200 });
      expect(phaseTotalMs(row({ prep_ms: 500, compact_ms: 200, connect_ms: 300, ttft_ms: 900, generation_ms: 4000, backoff_ms: 100 }))).toBe(5800);
      expect(phaseTotalMs(row())).toBeNull();
    });
  });

  describe("time formatting", () => {
    it("shows local clock with milliseconds and relative ages", () => {
      const now = Date.UTC(2026, 10, 2, 12, 30, 0);
      expect(traceClockMs(now)).toMatch(/^\d{2}:\d{2}:\d{2}\.\d{3}$/);
      expect(traceAge(now, now + 4_000)).toBe("just now");
      expect(traceAge(now, now + 90_000)).toBe("2 minutes ago");
      expect(traceAge(now, now + 5 * 3_600_000)).toBe("5 hours ago");
      expect(traceAge(now, now + 3 * 86_400_000)).toBe("3 days ago");
      expect(traceDayLabel(traceDayKey(now), now)).toBe("Today");
      expect(traceDayLabel(traceDayKey(now - 86_400_000), now)).toBe("Yesterday");
    });

    it("translates known finish reasons and passes unknown ones through", () => {
      expect(finishLabel(null)).toBeNull();
      expect(finishLabel("stop")).toBe("Completed");
      expect(finishLabel("tool_calls")).toBe("Tool calls");
      expect(finishLabel("length")).toBe("Length limit");
      expect(finishLabel("weird_new_one")).toBe("weird_new_one");
    });
  });

  describe("responseView", () => {
    it("reassembles SSE reasoning and answer chunks into readable text", () => {
      const sse = [
        'data: {"choices":[{"delta":{"reasoning_content":"thinking hard "}}]}',
        'data: {"choices":[{"delta":{"reasoning_content":"about spans"}}]}',
        'data: {"choices":[{"delta":{"content":"The answer"}}]}',
        'data: {"choices":[{"delta":{"content":" is 42."}}],"usage":null}',
        "data: [DONE]",
      ].join("\n");
      const view = responseView({ ...row({ is_complete: true }), request_json: null, response_raw: sse, request_truncated: false, raw_tool_calls: null });
      expect(view.absent).toBe(false);
      expect(view.streamed).toBe(true);
      expect(view.reasoning).toBe("thinking hard about spans");
      expect(view.answer).toBe("The answer is 42.");
    });

    it("reads a JSON error envelope as a provider error, not an answer", () => {
      const body = JSON.stringify({ error: { message: "rate limited", type: "requests" } });
      const view = responseView({ ...row({ is_complete: true, error: "rate limited" }), request_json: null, response_raw: body, request_truncated: false, raw_tool_calls: null });
      expect(view.errorText).toBe("rate limited");
      expect(view.answer).toBe("");
      expect(view.absent).toBe(false);
    });

    it("passes a plain text body through and says when nothing was kept", () => {
      const plain = responseView({ ...row(), request_json: null, response_raw: "just text", request_truncated: false, raw_tool_calls: null });
      expect(plain.answer).toBe("just text");
      expect(plain.streamed).toBe(false);
      const evicted = responseView({ ...row({ response_evicted: true }), request_json: null, response_raw: null, request_truncated: false, raw_tool_calls: null });
      expect(evicted.absent).toBe(true);
    });
  });

  describe("messageBlocks", () => {
    it("splits content arrays into text and image blocks", () => {
      const blocks = messageBlocks({ role: "user", content: [{ type: "text", text: "hello" }, { type: "image_url", image_url: { url: "x" } }] });
      expect(blocks).toEqual([
        { kind: "text", text: "hello" },
        { kind: "image", text: "[image]" },
      ]);
    });

    it("renders requested tools as readable call lines", () => {
      const blocks = messageBlocks({ role: "assistant", content: null, tool_calls: [{ id: "c1", type: "function", function: { name: "read_file", arguments: "{\"path\":\"a.rs\"}" } }] });
      expect(blocks).toEqual([{ kind: "tool", text: '→ read_file({"path":"a.rs"})' }]);
    });

    it("falls back to pretty JSON for anything else", () => {
      const blocks = messageBlocks(42);
      expect(blocks).toEqual([{ kind: "text", text: "42" }]);
    });
  });

  describe("groupTraceRows", () => {
    const now = Date.UTC(2026, 10, 2, 12, 0, 0);
    const leadCall = row({ id: 5, ts_ms: now - 60_000, attribution: attr({ actor_kind: "lead", agent_id: "lead", worker_id: null, plan_id: null, plan_step: null, turn_id: null }), run_id: "run-1" });
    const workerCall1 = row({ id: 3, ts_ms: now - 30_000, attribution: attr({ turn_id: "t-100" }), run_id: "run-1" });
    const workerCall2 = row({ id: 4, ts_ms: now - 20_000, attribution: attr({ turn_id: "t-100", purpose: "compaction" }), run_id: "run-1" });
    const rows = [leadCall, workerCall2, workerCall1].sort((a, b) => b.ts_ms - a.ts_ms);

    it("groups by day, newest first, in Time mode", () => {
      const groups = groupTraceRows(rows, "time", now);
      expect(groups).toHaveLength(1);
      expect(groups[0].title).toBe("Today");
      expect(groups[0].rows.map((entry) => entry.id)).toEqual([4, 3, 5]);
      expect(groups[0].meta).toContain("3 calls");
    });

    it("groups by actor with per-turn sections, chronological inside", () => {
      const groups = groupTraceRows(rows, "agent", now);
      const actors = groups.map((group) => group.title);
      expect(actors).toEqual(["design-trace", "Lead"]);
      const worker = groups[0];
      expect(worker.rows.map((entry) => entry.id)).toEqual([3, 4]);
      expect(worker.turns?.map((turn) => turn.title)).toEqual(["Turn t-100"]);
    });

    it("groups every agent's calls by turn in Turn mode", () => {
      const groups = groupTraceRows([...rows, leadCall], "turn", now);
      const titles = groups.map((group) => group.title);
      expect(titles.some((title) => title.startsWith("Turn t-100"))).toBe(true);
      expect(titles.some((title) => title.includes("Standalone calls"))).toBe(true);
      const turn = groups.find((group) => group.key === "turn:t-100");
      expect(turn?.rows.map((entry) => entry.id)).toEqual([3, 4]);
      expect(turn?.title).toContain("design-trace");
    });
  });

  describe("prefix reuse", () => {
    const detailOf = (id: number, messages: unknown[] | null): TraceDetail => ({
      ...row({ id, is_complete: true }),
      request_json: { messages },
      response_raw: "ok",
      request_truncated: false,
      raw_tool_calls: null,
    });

    it("measures the identical beginning against the previous comparable call", () => {
      const current = detailOf(9, [
        { role: "system", content: "You are the lead." },
        { role: "user", content: "Fix the failing test." },
      ]);
      const previous = detailOf(8, [
        { role: "system", content: "You are the lead." },
        { role: "user", content: "Review the failing test." },
      ]);
      const reuse = prefixReuse(current, previous);
      expect(reuse).not.toBeNull();
      expect(reuse?.totalChars).toBe(
        JSON.stringify([
          { role: "system", content: "You are the lead." },
          { role: "user", content: "Fix the failing test." },
        ]).length,
      );
      // The serialized bodies diverge exactly where "Fix" replaces "Review".
      expect(reuse?.addedChars).toBe('Fix the failing test."}]'.length);
      expect(reuse?.sameChars).toBeGreaterThan(30);
      expect(reuse?.pct).toBeGreaterThan(0.5);
    });

    it("picks the previous call of the same agent, purpose and model", () => {
      const current = row({ id: 9, ts_ms: 1_000, attribution: attr({ turn_id: "t-9" }) });
      const rows = [
        row({ id: 8, ts_ms: 900, attribution: attr({ turn_id: "t-8" }) }),
        row({ id: 7, ts_ms: 800, attribution: attr({ turn_id: "t-7", purpose: "compaction" }) }),
        row({ id: 6, ts_ms: 700, model: "other-model", attribution: attr({ turn_id: "t-6" }) }),
      ];
      expect(previousComparableRow(rows, current)?.id).toBe(8);
      expect(previousComparableRow([], current)).toBeNull();
    });

    it("refuses the comparison when a body was truncated or not retained", () => {
      const current = detailOf(9, [{ role: "user", content: "hi" }]);
      expect(prefixReuse(current, null)).toBeNull();
      const truncated = { ...detailOf(8, [{ role: "user", content: "hi" }]), request_truncated: true };
      expect(prefixReuse(current, truncated)).toBeNull();
      const noMessages = detailOf(8, null);
      expect(prefixReuse(current, noMessages)).toBeNull();
    });
  });

  describe("filters and query building", () => {
    it("translates the toolbar into the frozen wire query, omitting the all-values", () => {
      const query = traceListQuery(
        { ...DEFAULT_TRACE_FILTERS, agent: "design-trace", model: "kimi-k3.1-pro", status: "failed", search: "read_file", scope: "tools" },
        { sessionStartMs: 0, nowMs: 100, limit: 50 },
      );
      expect(query).toEqual({
        agent: "design-trace",
        model: "kimi-k3.1-pro",
        status: "failed",
        from_ms: 0,
        search: "read_file",
        search_scope: "tools",
        limit: 50,
      });
    });

    it("maps the time presets and the custom range", () => {
      const base = { sessionStartMs: 500, nowMs: 10_000 };
      expect(traceTimeRange({ ...DEFAULT_TRACE_FILTERS, time: "session" }, 500, 10_000)).toEqual({ from_ms: 500 });
      expect(traceTimeRange({ ...DEFAULT_TRACE_FILTERS, time: "hour" }, 500, 10_000)).toEqual({ from_ms: 10_000 - 3_600_000 });
      expect(traceTimeRange({ ...DEFAULT_TRACE_FILTERS, time: "all" }, 500, 10_000)).toEqual({});
      expect(traceTimeRange({ ...DEFAULT_TRACE_FILTERS, time: "custom", from_ms: 2, to_ms: 9 }, 500, 10_000)).toEqual({ from_ms: 2, to_ms: 9 });
      expect(traceListQuery({ ...DEFAULT_TRACE_FILTERS, time: "custom", from_ms: 2, to_ms: 9 }, base).to_ms).toBe(9);
    });

    it("labels the removable chips with the friendly names", () => {
      const chips = activeTraceChips({ ...DEFAULT_TRACE_FILTERS, status: "failed", purpose: "turn", turn_id: "178d91f2-42" });
      expect(chips.find((chip) => chip.key === "status")?.label).toBe("Status: Errors only");
      expect(chips.find((chip) => chip.key === "purpose")?.label).toBe("Purpose: Conversation turn");
      expect(chips.find((chip) => chip.key === "turn_id")?.label).toBe("Turn: 178d…2-42");
      expect(activeTraceChips({ ...DEFAULT_TRACE_FILTERS })).toEqual([]);
    });
  });

  describe("agent and model options", () => {
    it("keeps live workers, observed actors and deleted workers' names", () => {
      const options = traceAgentOptions(
        [
          row({ attribution: attr() }),
          row({ attribution: attr({ agent_id: "agent-gone", agent_name: "retired-worker", worker_id: null }) }),
        ],
        [{ id: "design-trace", name: "design-trace" }],
      );
      expect(options.map((option) => option.value)).toEqual(["all", "lead", "design-trace", "agent-gone", "unattributed"]);
    });

    it("disambiguates models by provider when a name serves two", () => {
      const options = traceModelOptions([
        row({ model: "kimi-k3.1-pro", provider: "kimi" }),
        row({ model: "kimi-k3.1-pro", provider: "openrouter" }),
        row({ model: "claude-opus-4.5", provider: "anthropic" }),
      ]);
      expect(options.find((option) => option.value === "kimi-k3.1-pro")?.label).toBe("kimi-k3.1-pro (kimi, openrouter)");
      expect(options.find((option) => option.value === "claude-opus-4.5")?.label).toBe("claude-opus-4.5");
    });
  });

  describe("per-project preferences", () => {
    beforeEach(() => {
      window.localStorage.clear();
    });

    it("round-trips filter state per project and tolerates junk", () => {
      expect(loadTracePrefs(null)).toBeNull();
      expect(loadTracePrefs("proj")).toBeNull();
      saveTracePrefs("proj", { agent: "design-trace", grouping: "turn", insightsOpen: true, listWidth: 500 });
      expect(loadTracePrefs("proj")).toEqual({ agent: "design-trace", grouping: "turn", insightsOpen: true, listWidth: 500 });
      window.localStorage.setItem("mnemo2.trace.proj", "{not json");
      expect(loadTracePrefs("proj")).toBeNull();
    });
  });

  describe("entry hand-over", () => {
    it("translates an openTrace hand-over into filter state", () => {
      expect(traceEntryFilters(null)).toBeNull();
      expect(traceEntryFilters({})).toBeNull();
      const patch = traceEntryFilters({ agent: "design-trace", turn_id: "t-9", from_ms: 5 });
      expect(patch).toEqual({ agent: "design-trace", turn_id: "t-9", time: "custom", from_ms: 5, to_ms: null });
      expect(traceEntryFilters({ search: "read_file", search_scope: "response" })).toEqual({ search: "read_file", scope: "response" });
      // The compact tab hands its open call over so the route opens the same one.
      expect(traceEntryFilters({ selected: "run-1:7" })).toEqual({ selected: "run-1:7" });
    });
  });

  describe("trace page transport", () => {
    it("stays null outside the desktop host", () => {
      expect(createTracePageTransport()).toBeNull();
    });

    it("returns the preview seed instead of the host transport", () => {
      // A stub whose only job is identity: the page must hand back exactly the seed.
      const seeded = { list: () => Promise.resolve({ rows: [], total: 0, unpriced: 0, has_more: false, next_cursor: null, capped: false }) } as unknown as TracePageTransport;
      seedTracePagePreview(seeded);
      expect(createTracePageTransport()).toBe(seeded);
      seedTracePagePreview(null);
      expect(createTracePageTransport()).toBeNull();
    });
  });
});