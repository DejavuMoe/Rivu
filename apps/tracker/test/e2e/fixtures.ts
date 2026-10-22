// Playwright 夹具：每个 worker 一套服务器（site + rivu），每个用例开始前清空记录。

import { test as base, expect, type BrowserContext, type Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { startFixture, type Fixture, type PageOpts, type Recorded } from '../fixture/server.ts';
import { CORE_KEYS, validateWire } from '../wire.ts';

export { expect };
export type { PageOpts, Recorded };

const meta = JSON.parse(readFileSync(new URL('../../dist/meta.json', import.meta.url), 'utf8')) as { sv: number };
/** 构建时注入的构建号，事件里的 `sv` 必须与之相等。 */
export const BUILD_SV = meta.sv;

interface TestFixtures {
  /** 夹具：服务器地址与已记录的请求。用例开始前已清空。 */
  fx: Fixture;
  /** 测试页面设 `window.__rivu = true`（规范 §11.2），放行自动化检测。`test.use({ bypassAutomation: false })` 关闭。 */
  bypassAutomation: boolean;
  /** 自动夹具：用例结束后对所有记录的请求做线协议与请求形态的通用检查。 */
  wireCheck: void;
}

interface WorkerFixtures {
  fixture: Fixture;
}

export const test = base.extend<TestFixtures, WorkerFixtures>({
  fixture: [
    async ({}, use) => {
      const f = await startFixture();
      await use(f);
      await f.close();
    },
    { scope: 'worker' },
  ],
  fx: async ({ fixture }, use) => {
    fixture.reset();
    await use(fixture);
  },
  bypassAutomation: [true, { option: true }],
  context: async ({ context, bypassAutomation }, use) => {
    if (bypassAutomation) {
      await context.addInitScript(() => {
        (window as unknown as { __rivu: boolean }).__rivu = true;
      });
    }
    await use(context);
  },
  wireCheck: [
    async ({ fx, page }, use) => {
      // 脚本不得向宿主页面抛异常：任何用例里出现未捕获异常或未处理的 Promise 拒绝都算失败。
      const pageErrors: string[] = [];
      page.on('pageerror', (e) => pageErrors.push(String(e)));
      await use();
      expect(pageErrors, '页面不应出现未捕获异常').toEqual([]);
      // 无论用例测什么，core 发出的每个请求都必须符合线协议与请求形态规范。
      for (const e of fx.events) {
        const where = `${e.method} ${e.server}${e.path}`;
        if (e.method === 'OPTIONS') continue; // 是否出现预检由具体用例断言
        expect(e.method, where).toBe('POST');
        expect(e.contentType ?? '', `${where} 的 Content-Type`).toMatch(/^text\/plain\b/i);
        expect(e.cookie, `${where} 不得带 Cookie`).toBeUndefined();
        expect(e.bytes, `${where} 体积`).toBeLessThan(8192);
        expect(e.json, `${where} 的体必须是 JSON 对象：${e.text}`).toBeDefined();
        expect(validateWire(e.json, e.bytes), `${where} 违反线协议：${e.text}`).toEqual([]);
        // core 只发 pageview，且只用这些键。
        expect(e.json!.n, where).toBe('pageview');
        for (const k of Object.keys(e.json!)) expect(CORE_KEYS, `${where} 多发了字段 ${k}`).toContain(k);
        expect(e.json!.sv, `${where} 的 sv`).toBe(BUILD_SV);
      }
    },
    { auto: true },
  ],
});

/** 采集端当前记录的 POST 体。 */
export const bodies = (fx: Fixture) => fx.posts().map((e) => e.json!);

/** 等到至少收到 n 个事件（后续再用 {@link settle} 确认「恰好」）。 */
export async function waitForEvents(fx: Fixture, n: number): Promise<void> {
  await expect.poll(() => fx.posts().length, { message: `等待 ${n} 个事件` }).toBeGreaterThanOrEqual(n);
}

/**
 * 给「不应该有请求」的断言留出沉淀时间：在页面里等一小段，再等网络空闲。
 * 事件是同步发出的（`fetch` 在调用时就入队），所以这里的时间只用来等请求到达采集端。
 */
export async function settle(page: Page, ms = 300): Promise<void> {
  await page.evaluate((t) => new Promise<void>((r) => setTimeout(r, t)), ms);
  await page.waitForLoadState('networkidle').catch(() => {});
}

/**
 * 在页面脚本运行之前把 `document.visibilityState` / `document.hidden` 固定成给定值，
 * 并提供 `window.__setVisibility(state)` 切换并派发 `visibilitychange`。
 *
 * 为什么用模拟：无头模式下所有页面都报告 `visible`，没有跨三个内核通用的「真正切到后台」手段；
 * 覆盖实例属性并派发事件，覆盖的正是脚本读到的那两个接口。
 */
export async function emulateVisibility(context: BrowserContext, initial: 'hidden' | 'visible' | 'prerender'): Promise<void> {
  await context.addInitScript((init) => {
    let state: string = init;
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => state });
    Object.defineProperty(document, 'hidden', { configurable: true, get: () => state !== 'visible' });
    (window as unknown as Record<string, unknown>).__setVisibility = (s: string) => {
      state = s;
      document.dispatchEvent(new Event('visibilitychange'));
    };
  }, initial);
}
