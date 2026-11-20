import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { isTauriRuntime } from "./runtime";

/**
 * The channel the native host emits when a hand edit of `~/.mnemo/*.toml` (or
 * another instance's save) was reloaded into this instance — or rejected.
 */
export const SETTINGS_RELOAD_CHANNEL = "mnemo2://settings-reload";

/** The notice the host sends: `ok` is false when the edit could not be loaded. */
export interface SettingsReloadNotice {
  ok: boolean;
  message: string;
}

export interface SettingsReloadBindings {
  listen: typeof listen;
}

export interface SettingsReloadTransport {
  onNotice(handler: (notice: SettingsReloadNotice) => void): Promise<UnlistenFn>;
}

/** The native transport over one `listen` binding. */
export function createTauriSettingsReloadTransport(
  bindings: SettingsReloadBindings,
): SettingsReloadTransport {
  return {
    onNotice: (handler) =>
      bindings.listen<SettingsReloadNotice>(SETTINGS_RELOAD_CHANNEL, (event) =>
        handler(event.payload),
      ),
  };
}

/** The native transport, or `null` in a plain browser (no Tauri host). */
export function createSettingsReloadTransport(
  bindings?: SettingsReloadBindings,
): SettingsReloadTransport | null {
  if (bindings) return createTauriSettingsReloadTransport(bindings);
  if (isTauriRuntime()) return createTauriSettingsReloadTransport({ listen });
  return null;
}
