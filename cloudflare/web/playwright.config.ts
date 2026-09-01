import { defineConfig } from "@playwright/test";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const artifactRoot = fileURLToPath(new URL("./.artifacts/admin-website/", import.meta.url));

export default defineConfig({
  testDir: "./e2e",
  testMatch: "admin-*.spec.ts",
  fullyParallel: false,
  workers: 1,
  reporter: "line",
  outputDir: join(artifactRoot, "test-results"),
  use: {
    baseURL: "http://127.0.0.1:4179",
    browserName: "chromium",
    headless: true,
    locale: "zh-CN",
    trace: "retain-on-failure",
  },
  webServer: {
    command: "node e2e/serve-admin.mjs",
    url: "http://127.0.0.1:4179/__health",
    reuseExistingServer: false,
    timeout: 15_000,
  },
});
