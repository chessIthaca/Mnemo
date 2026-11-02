/**
 * Environment detection shared by every transport adapter.
 *
 * Kept in its own module so a test that mocks one transport (for example
 * `./leadEngine`) cannot accidentally undefine the runtime check that the
 * project and team transports depend on.
 */

import { invoke } from "@tauri-apps/api/core";

/** Whether the page is running inside a Tauri WebView. */
export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

/**
 * Whether this page is running inside the **Mnemo2** host specifically.
 *
 * `isTauriRuntime` is not enough: Mnemo1's Browser tab is a Tauri WebView too,
 * so Mnemo2's dev UI viewed there would send its commands to Mnemo1's IPC,
 * which never registered them — Tauri answers `project_pick_folder not
 * allowed by ACL`. Only the real Mnemo2 host answers the handshake, so a
 * rejected handshake means "no native shell" (browser-only mode) rather than
 * a broken native session.
 */
export async function probeMnemo2Host(
  invoker: typeof invoke = invoke,
): Promise<boolean> {
  if (!isTauriRuntime()) return false;
  try {
    return (await invoker<string>("mnemo2_host_ping")) === "pong";
  } catch {
    return false;
  }
}
