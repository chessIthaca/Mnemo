import { fireEvent, render, screen } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { actions, app } from "../lib/store";
import { fixtureState } from "../test/fixtures";
import BrowserTab from "./BrowserTab.svelte";
import Workbench from "./Workbench.svelte";
import type { BrowserSnapshot } from "../lib/browserEngine";

const { transport } = vi.hoisted(() => ({
  transport: {
    supported: vi.fn(),
    snapshot: vi.fn(),
    ensure: vi.fn(),
    navigate: vi.fn(),
    history: vi.fn(),
    reload: vi.fn(),
    setRect: vi.fn(),
    setVisible: vi.fn(),
    overlay: vi.fn(),
    close: vi.fn(),
    openExternal: vi.fn(),
    onEvent: vi.fn(),
    onOpenRequest: vi.fn(),
  },
}));
vi.mock("../lib/browserEngine", () => ({ createBrowserTransport: () => transport }));
vi.mock("../lib/runtime", async (original) => ({
  ...(await original()),
  probeMnemo2Host: async () => true,
}));

const page: BrowserSnapshot = {
  supported: true,
  url: "http://localhost:3000/",
  title: "Real local page",
  loading: false,
};

describe("native Browser tab", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    transport.supported.mockResolvedValue(true);
    transport.snapshot.mockResolvedValue(page);
    transport.ensure.mockResolvedValue(page);
    transport.navigate.mockResolvedValue(page);
    transport.onEvent.mockResolvedValue(() => {});
    transport.onOpenRequest.mockResolvedValue(() => {});
    transport.setVisible.mockResolvedValue(undefined);
    transport.setRect.mockResolvedValue(undefined);
    transport.history.mockResolvedValue(undefined);
    transport.reload.mockResolvedValue(undefined);
    const state = fixtureState();
    state.workbenchOpen = true;
    state.workbenchTab = "browser";
    state.browserUrl = page.url;
    state.browserSupported = null;
    state.browserImage = null;
    app.set(state);
    vi.spyOn(HTMLDivElement.prototype, "getBoundingClientRect").mockReturnValue({
      x: 10, y: 20, width: 320, height: 200,
      top: 20, right: 330, bottom: 220, left: 10,
      toJSON: () => ({}),
    } as DOMRect);
    Object.defineProperty(window, "devicePixelRatio", { value: 2, configurable: true });
  });

  it("uses real page state and physical native-pane bounds", async () => {
    render(BrowserTab);
    await vi.waitFor(() => expect(transport.ensure).toHaveBeenCalled());
    expect(transport.ensure).toHaveBeenCalledWith({
      x: 20, y: 40, width: 640, height: 400,
    }, page.url);
    expect(await screen.findByText("Real local page")).toBeTruthy();
    expect(transport.setVisible).toHaveBeenCalledWith(true, expect.any(Number));
    const ensureCalls = transport.ensure.mock.calls.length;
    app.update((state) => ({ ...state, browserTitle: "Unrelated title update" }));
    await Promise.resolve();
    expect(transport.ensure).toHaveBeenCalledTimes(ensureCalls);
    await fireEvent.click(screen.getByRole("button", { name: "Back" }));
    expect(transport.history).toHaveBeenCalledWith("back");
    await fireEvent.click(screen.getByRole("button", { name: "Reload" }));
    expect(transport.reload).toHaveBeenCalled();
  });

  /** The newest visibility request (the native pane applies the highest seq). */
  function lastVisibility(): { visible: boolean; seq: number } {
    const calls = transport.setVisible.mock.calls as [boolean, number][];
    const [visible, seq] = calls.reduce((latest, call) => (call[1] > latest[1] ? call : latest));
    return { visible, seq };
  }

  it("hides the native pane when another workbench tab is chosen", async () => {
    app.update((state) => ({ ...state, route: "lead", browserSupported: true }));
    render(Workbench);
    await vi.waitFor(() => expect(transport.ensure).toHaveBeenCalled());
    expect(lastVisibility().visible).toBe(true);
    const shown = lastVisibility().seq;
    await fireEvent.click(screen.getByRole("button", { name: /Plan/ }));
    await vi.waitFor(() => expect(lastVisibility().seq).toBeGreaterThan(shown));
    expect(lastVisibility().visible).toBe(false);
  });

  it("hides the pane under the team drawer and shows it again when the drawer closes", async () => {
    app.update((state) => ({ ...state, route: "lead", browserSupported: true }));
    render(Workbench);
    await vi.waitFor(() => expect(transport.ensure).toHaveBeenCalled());
    expect(lastVisibility().visible).toBe(true);
    actions.toggleTeamPeek();
    await vi.waitFor(() => expect(lastVisibility().visible).toBe(false));
    actions.toggleTeamPeek();
    await vi.waitFor(() => expect(lastVisibility().visible).toBe(true));
  });

  it("keeps the pane up across a main-tab switch and hides it on the start screen", async () => {
    app.update((state) => ({ ...state, route: "lead", browserSupported: true }));
    render(Workbench);
    await vi.waitFor(() => expect(transport.ensure).toHaveBeenCalled());
    expect(lastVisibility().visible).toBe(true);

    app.update((state) => ({ ...state, route: "team" }));
    await vi.waitFor(() => expect(lastVisibility().seq).toBeGreaterThan(0));
    expect(lastVisibility().visible).toBe(true);

    app.update((state) => ({ ...state, route: "start" }));
    await vi.waitFor(() => expect(lastVisibility().visible).toBe(false));
  });
});
