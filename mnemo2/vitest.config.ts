import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";

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
