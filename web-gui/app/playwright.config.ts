import net from "node:net";

import { defineConfig, devices } from "@playwright/test";

async function reservePort(): Promise<number> {
  return await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        server.close();
        reject(new Error("failed to allocate an E2E server port"));
        return;
      }
      server.close((error) => error ? reject(error) : resolve(address.port));
    });
  });
}

const inheritedPort = Number(process.env.HOLON_E2E_PORT);
const port = Number.isInteger(inheritedPort) && inheritedPort > 0
  ? inheritedPort
  : await reservePort();
process.env.HOLON_E2E_PORT = String(port);
const baseURL = `http://127.0.0.1:${port}`;

export default defineConfig({
  testDir: "./e2e",
  testIgnore: ["real-daemon/**"],
  outputDir: "test-results",
  fullyParallel: false,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 2 : 0,
  reporter: process.env.CI ? [["line"], ["html", { open: "never" }]] : "line",
  use: {
    baseURL,
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
    video: "retain-on-failure",
  },
  webServer: {
    command: `node e2e/fixture-server.mjs --port ${port}`,
    gracefulShutdown: { signal: "SIGTERM", timeout: 5_000 },
    url: `${baseURL}/__e2e__/health`,
    reuseExistingServer: false,
    stdout: "pipe",
    stderr: "pipe",
    timeout: 30_000,
  },
  projects: [
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],
});
