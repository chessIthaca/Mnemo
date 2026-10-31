import { describe, expect, it, vi } from "vitest";
import {
  applyExternalLeadEvent,
  createExternalLeadTransport,
  createTauriExternalLeadTransport,
  EXTERNAL_LEAD_CHANNEL,
  EXTERNAL_OPEN_CHANNEL,
  externalLeadFromStatus,
  noExternalLead,
  normalizeProjectPath,
  sameProjectPath,
} from "./externalLeadEngine";

describe("project path comparison", () => {
  it("ignores the Windows verbatim prefix, separators, trailing slashes and case", () => {
    expect(sameProjectPath("\\\\?\\C:\\Mnemo\\", "c:/mnemo")).toBe(true);
    expect(sameProjectPath("C:\\Users\\Me\\Proj", "c:/users/me/proj/")).toBe(true);
    expect(sameProjectPath("\\\\?\\UNC\\Server\\Share\\Proj", "\\\\server\\share\\proj")).toBe(true);
  });

  it("keeps case on POSIX paths and tells different projects apart", () => {
    expect(sameProjectPath("/home/me/Proj/", "/home/me/Proj")).toBe(true);
    expect(sameProjectPath("/home/me/Proj", "/home/me/proj")).toBe(false);
    expect(sameProjectPath("C:/Mnemo", "C:/Mnemo2")).toBe(false);
  });

  it("normalizes a bare root to a stable form", () => {
    expect(normalizeProjectPath("/")).toBe("/");
    expect(normalizeProjectPath("C:\\")).toBe("c:");
  });
});

describe("external lead state", () => {
  const attached = { attached: true, root: "C:/Mnemo", sinceMs: 5 };

  it("reads the native status for the open project only", () => {
    const status = { attached: true, root: "\\\\?\\C:\\Mnemo", since_ms: 42 };
    expect(externalLeadFromStatus(status, "c:/mnemo")).toEqual({
      attached: true,
      root: "\\\\?\\C:\\Mnemo",
      sinceMs: 42,
    });
    expect(externalLeadFromStatus(status, "C:/Other")).toEqual(noExternalLead());
    expect(externalLeadFromStatus({ attached: false, root: null, since_ms: null }, "C:/Mnemo")).toEqual(
      noExternalLead(),
    );
  });

  it("attaches and detaches on events for the open project", () => {
    const next = applyExternalLeadEvent(noExternalLead(), { attached: true, root: "c:\\mnemo\\" }, "C:/Mnemo", 77);
    expect(next).toEqual({ attached: true, root: "c:\\mnemo\\", sinceMs: 77 });
    expect(applyExternalLeadEvent(next, { attached: false, root: "C:/Mnemo" }, "C:/Mnemo")).toEqual(
      noExternalLead(),
    );
  });

  it("keeps the original attach time on a repeated attach", () => {
    expect(applyExternalLeadEvent(attached, { attached: true, root: "C:/Mnemo" }, "C:/Mnemo", 99)).toBe(attached);
  });

  it("ignores events for another root or while no project is open", () => {
    expect(applyExternalLeadEvent(attached, { attached: false, root: "C:/Other" }, "C:/Mnemo")).toBe(attached);
    expect(
      applyExternalLeadEvent(noExternalLead(), { attached: true, root: "C:/Other" }, "C:/Mnemo").attached,
    ).toBe(false);
    expect(applyExternalLeadEvent(attached, { attached: false, root: "C:/Mnemo" }, null)).toBe(attached);
  });

  it("treats a rootless detach as clearing and a rootless attach as unplaceable", () => {
    expect(applyExternalLeadEvent(attached, { attached: false, root: null }, "C:/Mnemo")).toEqual(
      noExternalLead(),
    );
    expect(applyExternalLeadEvent(noExternalLead(), { attached: true, root: null }, "C:/Mnemo").attached).toBe(false);
  });
});

describe("external lead transport", () => {
  it("calls the native commands and listens on the native channels", async () => {
    const invoke = vi.fn(async (command: string) =>
      command === "mnemo2_startup_project"
        ? "C:/Mnemo"
        : { attached: true, root: "C:/Mnemo", since_ms: 1 },
    );
    const unlisten = vi.fn();
    const listen = vi.fn(async (_channel: string, _handler: (event: { payload: unknown }) => void) => unlisten);
    const transport = createTauriExternalLeadTransport({
      invoke: invoke as never,
      listen: listen as never,
    });

    expect(await transport.startupProject()).toBe("C:/Mnemo");
    expect(invoke).toHaveBeenCalledWith("mnemo2_startup_project");
    expect(await transport.status()).toEqual({ attached: true, root: "C:/Mnemo", since_ms: 1 });
    expect(invoke).toHaveBeenCalledWith("external_lead_status");

    const opened = vi.fn();
    const changed = vi.fn();
    await transport.onOpenRequest(opened);
    await transport.onChanged(changed);
    expect(listen.mock.calls.map((call) => call[0])).toEqual([EXTERNAL_OPEN_CHANNEL, EXTERNAL_LEAD_CHANNEL]);
    listen.mock.calls[0][1]({ payload: { root: "C:/Mnemo" } });
    listen.mock.calls[1][1]({ payload: { attached: true, root: "C:/Mnemo" } });
    expect(opened).toHaveBeenCalledWith({ root: "C:/Mnemo" });
    expect(changed).toHaveBeenCalledWith({ attached: true, root: "C:/Mnemo" });
  });

  it("has no native transport in a browser-only session", () => {
    expect(createExternalLeadTransport()).toBeNull();
  });
});
