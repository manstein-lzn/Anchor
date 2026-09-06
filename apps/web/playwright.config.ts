import { defineConfig } from '@playwright/test';

export default defineConfig({
  testDir: './e2e',
  fullyParallel: false,
  workers: 1,
  use: { baseURL: 'http://127.0.0.1:5181', viewport: { width: 1440, height: 960 }, trace: 'retain-on-failure' },
  webServer: [
    { command: '../../.venv/bin/python ../../scripts/web_test_api.py', url: 'http://127.0.0.1:8091/health/live', reuseExistingServer: false },
    { command: 'npm run dev -- --port 5181', url: 'http://127.0.0.1:5181', env: { ANCHOR_WEB_API_URL: 'http://127.0.0.1:8091' }, reuseExistingServer: false },
  ],
});
