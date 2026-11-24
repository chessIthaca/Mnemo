import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";

import { testTmpRoot } from "../scripts/test-tmp.mjs";

// The suite's temp folders stay in this checkout's target/tmp (and on its drive),
// not the user's system temp folder: this config runs in the main process before
// the worker pool starts, so every worker and child inherits the redirect.
testTmpRoot();

export default defineConfig({
  plugins: [svelte()],
  resolve: {
    conditions: ["browser"],
  },
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.ts"],
    setupFiles: ["./src/test/setup.ts"],
    css: true,
    // Heavy DOM tests time out at the default 5 s when the machine is busy; a timeout here is load, not a defect.
    testTimeout: 20_000,
    hookTimeout: 20_000,
    maxWorkers: "50%",
  },
});
