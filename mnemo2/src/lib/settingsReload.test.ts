import { describe, expect, it, vi } from "vitest";
import {
  createSettingsReloadTransport,
  createTauriSettingsReloadTransport,
  SETTINGS_RELOAD_CHANNEL,
  type SettingsReloadNotice,
} from "./settingsReload";

describe("settingsReload", () => {
  it("listens on the settings-reload channel and forwards the notice payload", async () => {
    const notice: SettingsReloadNotice = {
      ok: true,
      message: "Settings changed in another window or by hand; reloaded.",
    };
    const listen = vi.fn(async (_channel: string, handler: (event: { payload: unknown }) => void) => {
      handler({ payload: notice });
      return () => {};
    });
    const transport = createTauriSettingsReloadTransport({ listen: listen as never });
    const handler = vi.fn();

    await transport.onNotice(handler);

    expect(listen.mock.calls[0][0]).toBe(SETTINGS_RELOAD_CHANNEL);
    expect(handler).toHaveBeenCalledWith(notice);
  });

  it("has no transport outside the native shell", () => {
    expect(createSettingsReloadTransport()).toBeNull();
  });
});
