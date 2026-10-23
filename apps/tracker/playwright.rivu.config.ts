import { defineConfig, devices } from '@playwright/test';

// 真实浏览器 -> 真实运行的 rivu -> ClickHouse -> `rivu stats` 的端到端测试（test/e2e-rivu）。
// 不能单独运行：需要 `rivu all` 已在监听，并且为每个内核建好了站点，见 scripts/e2e-rivu.sh。
// 三个内核各一个站点（RIVU_E2E_SITE_CHROMIUM / _FIREFOX / _WEBKIT），所以每个项目的计数互不干扰。
export default defineConfig({
  testDir: 'test/e2e-rivu',
  // 串行：每个用例都会等采集链路落库再调用 `rivu stats`，并行没有收益，日志也更好读。
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never', outputFolder: 'playwright-report-rivu' }]] : 'list',
  timeout: 60_000,
  expect: { timeout: 5_000 },
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        // 去掉 Playwright 默认的 `--disable-back-forward-cache`，才有机会测到真正的 bfcache 恢复。
        launchOptions: { ignoreDefaultArgs: ['--disable-back-forward-cache'] },
      },
    },
    { name: 'firefox', use: { ...devices['Desktop Firefox'] } },
    { name: 'webkit', use: { ...devices['Desktop Safari'] } },
  ],
});
