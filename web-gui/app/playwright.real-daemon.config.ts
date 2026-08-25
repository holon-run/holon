import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e/real-daemon",
  outputDir: "test-results/real-daemon",
  fullyParallel: false,
  workers: 1,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? [["line"], ["html", { open: "never" }]] : "line",
  use: {
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium-real-daemon",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
});
