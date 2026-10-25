import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauriRuntime } from "./runtime";

export const BROWSER_EVENT_CHANNEL = "mnemo2://browser-event";
export const BROWSER_OPEN_CHANNEL = "mnemo2://browser-open-request";

export interface BrowserSnapshot {
  supported: boolean;
  url: string;
  title: string;
  loading: boolean;
}

export interface BrowserRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface BrowserTransportBindings {
  invoke: typeof invoke;
  listen: typeof listen;
}

export interface BrowserTransport {
  supported(): Promise<boolean>;
  snapshot(): Promise<BrowserSnapshot>;
  ensure(rect: BrowserRect, url: string): Promise<BrowserSnapshot>;
  navigate(url: string): Promise<BrowserSnapshot>;
  history(direction: "back" | "forward"): Promise<void>;
  reload(): Promise<void>;
  setRect(rect: BrowserRect): Promise<void>;
  setVisible(visible: boolean): Promise<void>;
  overlay(enter: boolean): Promise<void>;
  close(): Promise<void>;
  openExternal(url: string): Promise<void>;
  onEvent(handler: (snapshot: BrowserSnapshot) => void): Promise<UnlistenFn>;
  onOpenRequest(handler: (snapshot: BrowserSnapshot) => void): Promise<UnlistenFn>;
}

export function createTauriBrowserTransport(bindings: BrowserTransportBindings): BrowserTransport {
  return {
    supported: () => bindings.invoke<boolean>("browser_supported"),
    snapshot: () => bindings.invoke<BrowserSnapshot>("browser_snapshot"),
    ensure: (rect, url) => bindings.invoke<BrowserSnapshot>("browser_ensure", { ...rect, url }),
    navigate: (url) => bindings.invoke<BrowserSnapshot>("browser_navigate", { url }),
    history: (direction) => bindings.invoke<void>("browser_history", { direction }),
    reload: () => bindings.invoke<void>("browser_reload"),
    setRect: (rect) => bindings.invoke<void>("browser_set_rect", { ...rect }),
    setVisible: (visible) => bindings.invoke<void>("browser_set_visible", { visible }),
    overlay: (enter) => bindings.invoke<void>("browser_overlay", { enter }),
    close: () => bindings.invoke<void>("browser_close"),
    openExternal: (url) => bindings.invoke<void>("browser_open_external", { url }),
    onEvent: (handler) => bindings.listen<BrowserSnapshot>(BROWSER_EVENT_CHANNEL, (event) => handler(event.payload)),
    onOpenRequest: (handler) => bindings.listen<BrowserSnapshot>(BROWSER_OPEN_CHANNEL, (event) => handler(event.payload)),
  };
}

export function createBrowserTransport(bindings?: BrowserTransportBindings): BrowserTransport | null {
  if (!bindings && !isTauriRuntime()) return null;
  return createTauriBrowserTransport(bindings ?? { invoke, listen });
}
