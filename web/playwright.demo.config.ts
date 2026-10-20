import { defineConfig } from "@playwright/test";

// The static online demo (scripts/build_demo_site.sh -> site/), served as plain files the
// way GitHub Pages serves it. DEMO_URL=https://… tests a published demo instead.
const published = process.env.DEMO_URL;

export default defineConfig({
  testDir: "e2e-demo",
  timeout: 90_000,
  retries: 0,
  use: {
    baseURL: published ?? "http://127.0.0.1:18766",
    viewport: { width: 1280, height: 800 },
    launchOptions: { args: ["--use-angle=swiftshader", "--enable-unsafe-swiftshader"] },
  },
  webServer: published
    ? undefined
    : {
        command: "python3 -m http.server 18766 --bind 127.0.0.1 --directory ../site",
        url: "http://127.0.0.1:18766/index.html",
        reuseExistingServer: false,
      },
  reporter: [["list"]],
});
