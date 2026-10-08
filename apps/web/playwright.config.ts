import { defineConfig } from '@playwright/test';

const port = process.env.ANCHOR_WEB_TEST_PORT || '5173';
const launchOptions = process.env.ANCHOR_BROWSER_BINARY
  ? { executablePath: process.env.ANCHOR_BROWSER_BINARY }
  : undefined;

export default defineConfig({
  testDir: './e2e',
  use: {
    baseURL: `http://127.0.0.1:${port}`,
    viewport: { width: 1440, height: 1000 },
    launchOptions,
  },
  webServer: { command: `npm run dev -- --port ${port}`, url: `http://127.0.0.1:${port}`, reuseExistingServer: !process.env.CI },
});
