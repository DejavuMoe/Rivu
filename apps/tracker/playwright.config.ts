import { defineConfig, devices } from '@playwright/test';

// 三个内核各跑一遍同一套用例。浏览器由 `pnpm run browsers`（playwright install）安装。
// 每个 worker 自己起一套夹具服务器（见 test/e2e/fixtures.ts），用例之间互不干扰。
export default defineConfig({
  testDir: 'test/e2e',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  timeout: 30_000,
  expect: { timeout: 5_000 },
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        // Playwright 默认给 Chromium 传 `--disable-back-forward-cache`；去掉它才有机会测到真正的 bfcache 恢复。
        launchOptions: { ignoreDefaultArgs: ['--disable-back-forward-cache'] },
      },
    },
    { name: 'firefox', use: { ...devices['Desktop Firefox'] } },
    { name: 'webkit', use: { ...devices['Desktop Safari'] } },
  ],
});
