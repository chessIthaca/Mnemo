/**
 * Environment detection shared by every transport adapter.
 *
 * Kept in its own module so a test that mocks one transport (for example
 * `./leadEngine`) cannot accidentally undefine the runtime check that the
 * project and team transports depend on.
 */

/** Whether the page is running inside a Tauri WebView. */
export function isTauriRuntime(): boolean {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}
