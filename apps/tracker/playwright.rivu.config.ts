import { defineConfig, devices } from '@playwright/test';

// 真实浏览器 -> 真实运行的 rivu -> ClickHouse -> `rivu query` 的端到端测试（test/e2e-rivu）。
// 不能单独运行：需要 `rivu serve` 已在监听，并且为每个内核建好了站点，见 scripts/e2e-rivu.sh。
// 三个内核各一个站点（RIVU_E2E_SITE_CHROMIUM / _FIREFOX / _WEBKIT），所以每个项目的计数互不干扰。
export default defineConfig({
  testDir: 'test/e2e-rivu',
  // 串行：每个用例都会等采集链路落库再调用 `rivu query`，并行没有收益，日志也更好读。
  fullyParallel: false,
  workers: 1,
  forbidOnly: !!process.env.CI,
  retries: 0,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never', outputFolder: 'playwright-report-rivu' }]] : 'list',
  timeout: 60_000,
  expect: { timeout: 5_000 },
  // Firefox 与 WebKit 不用设备预设里的 userAgent：Playwright 改写 UA 只覆盖页面发出的 fetch，它们的 sendBeacon
  // 仍带浏览器自己的 UA，同一个人的 pageview 与 engagement 就成了两个访客（engagement 全部 no_session）。
  // 真实浏览器没有这个问题。Chromium 也一样（卸载页面时发的 beacon 不跟随改写），但无头 Chromium 自带的 UA 含
  // HeadlessChrome、会被判为机器人，所以改用启动参数 --user-agent 在整个浏览器层面设置 UA。
  projects: [
    {
      name: 'chromium',
      use: {
        ...devices['Desktop Chrome'],
        userAgent: undefined,
        // 去掉 Playwright 默认的 `--disable-back-forward-cache`，才有机会测到真正的 bfcache 恢复。
        launchOptions: {
          ignoreDefaultArgs: ['--disable-back-forward-cache'],
          args: [`--user-agent=${devices['Desktop Chrome'].userAgent}`],
        },
      },
    },
    { name: 'firefox', use: { ...devices['Desktop Firefox'], userAgent: undefined } },
    { name: 'webkit', use: { ...devices['Desktop Safari'], userAgent: undefined } },
  ],
});
