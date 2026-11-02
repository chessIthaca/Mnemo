import "@fontsource-variable/inter";
import "@fontsource-variable/source-serif-4";
import "./app.css";

import { mount } from "svelte";
import App from "./App.svelte";
import { actions } from "./lib/store";
import { applyCachedTheme } from "./lib/theme";

// The engine's CSP (`script-src 'self'`) forbids an inline pre-paint script, so
// the very first module statement paints the mirrored theme — a dark-theme
// user never sees the light palette — and `initTheme` then reads the
// authoritative `[ui] theme` and corrects the mirror.
applyCachedTheme();

const app = mount(App, {
  target: document.getElementById("app")!,
});

void actions.initTheme();

export default app;
