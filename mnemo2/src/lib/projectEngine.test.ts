import { describe, expect, it, vi } from "vitest";
import { createTauriProjectTransport } from "./projectEngine";

describe("projectEngine", () => {
  it("lists the real registry through project_list", async () => {
    const invoke = vi.fn(async () => [{ name: "demo", path: "C:/demo" }]);
    const transport = createTauriProjectTransport({ invoke: invoke as never });

    await expect(transport.list("C:/cfg")).resolves.toEqual([
      { name: "demo", path: "C:/demo" },
    ]);
    expect(invoke).toHaveBeenCalledWith("project_list", { configDir: "C:/cfg" });
  });

  it("registers a real folder through project_add", async () => {
    const invoke = vi.fn(async () => ({ name: "demo", path: "C:/demo" }));
    const transport = createTauriProjectTransport({ invoke: invoke as never });

    await expect(transport.add("C:/demo", null)).resolves.toEqual({
      name: "demo",
      path: "C:/demo",
    });
    expect(invoke).toHaveBeenCalledWith("project_add", {
      path: "C:/demo",
      configDir: null,
    });
  });

  it("opens the native folder picker through project_pick_folder", async () => {
    const invoke = vi.fn(async () => "C:/picked");
    const transport = createTauriProjectTransport({ invoke: invoke as never });

    await expect(transport.pickFolder()).resolves.toBe("C:/picked");
    expect(invoke).toHaveBeenCalledWith("project_pick_folder");
  });
});
