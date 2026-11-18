import { defineConfig, devices } from '@playwright/test';

// No dev server is started. Smoke tests serve the production bundle by route interception.
export default defineConfig({
  testDir: './tests', testMatch: ['browser.spec.ts', 'qualityChecks.browser.spec.ts'], fullyParallel: true,
  reporter: 'list', use: { baseURL: 'http://localhost:48991', headless: true, trace: 'retain-on-failure' },
  projects: [
    { name: 'desktop', use: { ...devices['Desktop Chrome'] } },
    { name: 'mobile', use: { ...devices['iPhone 13'], defaultBrowserType: 'chromium' } },
  ],
});
