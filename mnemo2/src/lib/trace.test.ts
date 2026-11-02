import { afterEach, describe, expect, it, vi } from "vitest";
import {
  capText,
  createTraceTransport,
  formatDuration,
  formatTokens,
  messageText,
  newestId,
  prettyJson,
  requestView,
  responseText,
  seedTracePreview,
  traceClock,
  traceStatus,
  type TraceDetail,
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

describe("newestId", () => {
  it("is the last row's id, or null for an empty log", () => {
    expect(newestId({ requests: [], capacity: 32 })).toBeNull();
    expect(
      newestId({ requests: [row({ id: 3 }), row({ id: 4 })], capacity: 32 }),
    ).toBe(4);
  });
});

describe("createTraceTransport", () => {
  afterEach(() => {
    seedTracePreview(null);
  });

  it("is null outside the desktop app, so the tab can say so", () => {
    expect(createTraceTransport()).toBeNull();
  });

  it("uses the preview seed when one is installed", async () => {
    const detailRow = detail({ id: 7, request_json: { model: "m" }, response_raw: "ok" });
    seedTracePreview({
      list: async () => ({ requests: [row({ id: 7 })], capacity: 32 }),
      detail: async () => detailRow,
      clear: async () => undefined,
    });
    const transport = createTraceTransport();
    expect(transport).not.toBeNull();
    expect(await transport?.list()).toEqual({ requests: [row({ id: 7 })], capacity: 32 });
    expect((await transport?.detail(7))?.response_raw).toBe("ok");
    await expect(transport?.clear()).resolves.toBeUndefined();
  });
});
