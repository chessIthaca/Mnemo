import { probeMnemo2Host } from "../runtime";
import { createSettingsReloadTransport, type SettingsReloadNotice } from "../settingsReload";
import { app, withToast } from "./core";

let unlisten: (() => void) | null = null;

/**
 * Show one host notice as the window's toast: the fixed "reloaded" text after
 * a successful live reload (plus the Laya restart sentence when a hand edit
 * touched `[general.laya]`), or the file name and parse error of a rejected
 * one.
 */
export function showSettingsReloadNotice(notice: SettingsReloadNotice): void {
  app.update((state) => withToast(state, notice.message, notice.ok ? "info" : "error"));
}

/**
 * Start listening for live-settings notices. Safe to call again.
 *
 * A non-Mnemo2 WebView (the in-app Browser pane) has a Tauri event bridge but
 * no host behind the notice, so the handshake and a rejected `listen` both
 * leave the window without the toast — the reload itself happened host-side
 * either way.
 */
export async function connectSettingsReload(): Promise<void> {
  if (unlisten) return;
  const transport = createSettingsReloadTransport();
  if (!transport) return;
  try {
    if (!(await probeMnemo2Host())) return;
    unlisten = await transport.onNotice(showSettingsReloadNotice);
  } catch {
    // Without the event the settings still reloaded; only the toast is lost.
  }
}

/** Stop listening (a project close or switch). */
export function disconnectSettingsReload(): void {
  unlisten?.();
  unlisten = null;
}
