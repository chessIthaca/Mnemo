import { get } from "svelte/store";
import { createBrowserTransport, type BrowserTransport } from "../browserEngine";
import { probeMnemo2Host } from "../runtime";
import type { AppState } from "../types";
import { app, leadError } from "./core";
import { schedulePreferenceSave } from "./preferences";

let browserTransport: BrowserTransport | null = null;
let browserUnlisten: (() => void)[] = [];
let browserToken = 0;
/** Orders native visibility requests (see `browser_set_visible`). */
let browserVisibilitySeq = 0;
/** Whether the mounted Browser tab wants its native pane on screen. */
let browserTabWantsPane = false;
/** The visibility last sent to the native pane. */
let browserPaneShown = false;

/**
 * Whether the layout leaves room for the native Browser pane. The pane is an
 * OS window above the whole UI, so it may only show on the open Browser tab
 * of the Lead view with no drawer, sheet, or preview over it.
 */
function browserPaneAllowed(state: AppState): boolean {
  return (
    state.route === "lead" &&
    state.workbenchOpen &&
    state.workbenchTab === "browser" &&
    !state.teamPeekOpen &&
    !state.browserImage
  );
}

/** Send the pane's desired visibility as a new, ordered request. */
function pushBrowserVisibility(): Promise<void> {
  const wanted = browserTabWantsPane && browserPaneAllowed(get(app));
  browserPaneShown = wanted;
  const seq = ++browserVisibilitySeq;
  return browserCommand((transport) => transport.setVisible(wanted, seq));
}

// The layout, not a component's mount/unmount, has the final say: any change
// that covers the Browser tab (another tab, the team drawer, the Team view,
// a closed workbench) hides the pane, and uncovering it shows it again.
app.subscribe((state) => {
  if (!browserTransport) return;
  if ((browserTabWantsPane && browserPaneAllowed(state)) !== browserPaneShown) {
    void pushBrowserVisibility();
  }
});

function approximateBrowserUrl(value: string): string {
  if (/^https?:\/\//i.test(value)) return value;
  if (/^(?:localhost|127\.0\.0\.1|\[::1\])(?::|\/|$)/i.test(value)) return `http://${value}`;
  return `https://${value}`;
}

function applyBrowserSnapshot(snapshot: { supported: boolean; url: string; title: string; loading: boolean }): void {
  app.update((state) => ({
    ...state,
    browserSupported: snapshot.supported,
    browserUrl: snapshot.url,
    browserTitle: snapshot.title,
    browserLoading: snapshot.loading,
  }));
  schedulePreferenceSave();
}

export async function connectBrowser(): Promise<BrowserTransport | null> {
  if (browserTransport) return browserTransport;
  const transport = createBrowserTransport();
  if (!transport) return null;
  const token = ++browserToken;
  if (!(await probeMnemo2Host()) || token !== browserToken) return null;
  browserTransport = transport;
  try {
    const [supported, snapshot, unlisten, openUnlisten] = await Promise.all([
      transport.supported(),
      transport.snapshot(),
      transport.onEvent((event) => {
        if (token === browserToken) applyBrowserSnapshot(event);
      }),
      transport.onOpenRequest((event) => {
        if (token === browserToken) void navigateBrowser(event.url);
      }),
    ]);
    if (token !== browserToken) {
      unlisten();
      openUnlisten();
      return null;
    }
    browserUnlisten = [unlisten, openUnlisten];
    app.update((state) => ({
      ...state,
      browserSupported: supported,
      browserUrl: snapshot.url === "about:blank" ? state.browserUrl : snapshot.url,
      browserTitle: snapshot.title,
      browserLoading: snapshot.loading,
      browserError: null,
    }));
    return transport;
  } catch (error) {
    if (token === browserToken) {
      browserTransport = null;
      app.update((state) => ({ ...state, browserError: leadError(error) }));
    }
    return null;
  }
}

async function browserCommand(operation: (transport: BrowserTransport) => Promise<unknown>): Promise<void> {
  const transport = await connectBrowser();
  if (!transport) return;
  try {
    await operation(transport);
  } catch (error) {
    app.update((state) => ({ ...state, browserError: leadError(error) }));
  }
}

export function ensureBrowser(rect: Parameters<BrowserTransport["ensure"]>[0], url: string): Promise<void> {
  return browserCommand(async (transport) => {
    await transport.ensure(rect, url);
  });
}

export function setBrowserRect(rect: Parameters<BrowserTransport["setRect"]>[0]): Promise<void> {
  return browserCommand((transport) => transport.setRect(rect));
}

export function setBrowserVisible(visible: boolean): Promise<void> {
  browserTabWantsPane = visible;
  return pushBrowserVisibility();
}

export function browserOverlay(enter: boolean): void {
  void browserCommand((transport) => transport.overlay(enter));
}

export function browserBack(): void {
  void browserCommand((transport) => transport.history("back"));
}

export function browserForward(): void {
  void browserCommand((transport) => transport.history("forward"));
}

export function browserReload(): void {
  void browserCommand((transport) => transport.reload());
}

export function openExternalBrowser(): void {
  const url = get(app).browserUrl;
  if (url === "about:blank") return;
  if (!createBrowserTransport()) {
    window.open(url, "_blank", "noopener,noreferrer");
    return;
  }
  void browserCommand((transport) => transport.openExternal(url));
}

export function openBrowser(url: string): void {
  void navigateBrowser(url);
}

export async function navigateBrowser(url: string): Promise<void> {
  const value = url.trim();
  if (!value) return;
  app.update((state) => ({
    ...state,
    route: state.projectId ? "lead" : state.route,
    browserUrl: approximateBrowserUrl(value),
    browserTitle: "",
    browserImage: null,
    browserError: null,
    workbenchOpen: true,
    workbenchTab: "browser",
  }));
  schedulePreferenceSave();
  const transport = await connectBrowser();
  if (!transport) return;
  try {
    await transport.navigate(value);
  } catch (error) {
    app.update((state) => ({ ...state, browserError: leadError(error) }));
  }
}

/** Tear the native Browser pane down: stop its listeners, forget it, close it. */
export async function disconnectBrowser(): Promise<void> {
  browserToken += 1;
  for (const unlisten of browserUnlisten) unlisten();
  browserUnlisten = [];
  const browser = browserTransport;
  browserTransport = null;
  browserTabWantsPane = false;
  browserPaneShown = false;
  if (browser) await browser.close().catch(() => {});
}
