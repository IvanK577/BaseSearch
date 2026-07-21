import { defineConfig, devices } from "@playwright/test";
import { fileURLToPath } from "node:url";

// Smoke tests run against a running Base Search browser workspace. Point them
// at an existing server with BASE_URL, or set SMOKE_DB to have Playwright launch
// the CLI server itself against a seeded database.
const baseURL = process.env.BASE_URL || "http://127.0.0.1:7842";
const smokeDb = process.env.SMOKE_DB;
const cliName = process.platform === "win32" ? "base-search-cli.exe" : "base-search-cli";
const cliPath = fileURLToPath(new URL(`../target/debug/${cliName}`, import.meta.url));

function shellQuote(value: string): string {
  if (process.platform === "win32") {
    return `"${value.replaceAll('"', '""')}"`;
  }
  return `'${value.replaceAll("'", "'\\''")}'`;
}

export default defineConfig({
  testDir: "./tests",
  timeout: 30_000,
  fullyParallel: false,
  reporter: [["list"]],
  use: {
    baseURL,
    headless: true,
    trace: "off",
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  ...(smokeDb && !process.env.BASE_URL
    ? {
        webServer: {
          command: `${shellQuote(cliPath)} browser ${shellQuote(smokeDb)} --port 7842 --no-open`,
          url: baseURL,
          reuseExistingServer: true,
          timeout: 30_000,
        },
      }
    : {}),
});
