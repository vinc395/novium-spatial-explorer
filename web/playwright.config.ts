import { defineConfig } from "@playwright/test";

// Expects a running server (scripts/verify.sh starts one on a synthetic index).
export default defineConfig({
  testDir: "e2e",
  timeout: 60_000,
  retries: 0,
  use: {
    baseURL: process.env.XENIUM_E2E_URL ?? "http://127.0.0.1:18080",
    viewport: { width: 1280, height: 800 },
    launchOptions: { args: ["--use-angle=swiftshader", "--enable-unsafe-swiftshader"] },
  },
  reporter: [["list"]],
});
