// `full` 变体的浏览器行为测试（chromium / firefox / webkit 各跑一遍）。
// 场景对照 docs/architecture/tracker-and-protocol.md §11.2；方括号里的 § 是该行为的规范出处。
// 通用检查同 core（fixtures.ts 的 wireCheck），只是不限制事件名与字段。

import type { Page } from '@playwright/test';
import type { PageOpts } from './fixtures.ts';
import { bodies, emulateVisibility, expect, settle, test, waitForEvents } from './fixtures.ts';

test.use({ variant: 'full' });

type W = Omit<Window, 'rivu' | '__rivu' | 'Cypress'> & Record<string, any>; // eslint-disable-line @typescript-eslint/no-explicit-any

const full = (o: PageOpts = {}): PageOpts => ({ variant: 'full', ...o, cfg: { local: true, ...o.cfg } });
const named = (fx: Parameters<typeof bodies>[0], n: string) => bodies(fx).filter((b) => b.n === n);
const rivu = (page: Page, ...args: unknown[]) =>
  page.evaluate((a) => (window as W).rivu(...a), args as unknown[]);

test.describe('加载与队列回放 [§2.5、§4.1]', () => {
  test('探测字段是 full；t.js 读 data-* 的 full 开关', async ({ page, fx }) => {
    await page.goto(fx.url('/capture', full({ mode: 'data', cfg: { outbound: true } })));
    await waitForEvents(fx, 1);
    expect(await page.evaluate(() => (window as W).rivu.b)).toBe('full');
    await page.click('#out');
    await expect.poll(() => named(fx, 'Outbound Link: Click').length).toBe(1);
  });

  test('init / set 先于首个 pageview，其余命令在它之后按入队顺序回放；callback 恰好一次', async ({ page, fx }) => {
    await page.goto(fx.url('/plain', full({ stub: true })));
    await waitForEvents(fx, 2);
    await settle(page);
    const pv = named(fx, 'pageview');
    const signup = named(fx, 'Signup');
    expect(pv).toHaveLength(1);
    expect(signup).toHaveLength(1);
    // init 打开了 hash 模式，set 的全局属性带到 pageview；单次属性与全局属性合并
    expect(pv[0]).toMatchObject({ h: 1, p: { a: '1' } });
    expect(signup[0]!.p).toEqual({ a: '1', plan: 'pro' });
    await expect.poll(() => page.evaluate(() => (window as W).__cb)).toBe(1);
    await settle(page);
    expect(await page.evaluate(() => (window as W).__cb)).toBe(1);
  });

  test('加载之后的 init 无效；重复加载只初始化一次', async ({ page, fx }) => {
    await page.goto(fx.url('/plain', full({ twice: true })));
    await waitForEvents(fx, 1);
    await rivu(page, 'init', { hash: true });
    await rivu(page, 'pageview');
    await waitForEvents(fx, 2);
    await settle(page);
    const pv = named(fx, 'pageview');
    expect(pv).toHaveLength(2);
    expect(pv.every((b) => b.h === undefined)).toBe(true);
  });
});

test.describe('手动 API [§4.8]', () => {
  test('自定义事件：属性、interactive:false、url 覆盖和 callback 照常，忽略旧收入选项', async ({ page, fx }) => {
    await page.goto(fx.url('/plain', full()));
    await waitForEvents(fx, 1);
    await page.evaluate(() => {
      const w = window as W;
      w.rivu('set', { props: { tier: 'gold', drop: { nested: 1 } } });
      w.rivu('event', 'Buy', {
        props: { qty: 2, gift: false, tier: 'silver', bad: null },
        revenue: { amount: 29.9, currency: 'EUR' },
        interactive: false,
        url: 'https://shop.example/checkout?x=1',
        callback: (r: unknown) => ((w.__r = r), (w.__n = (w.__n || 0) + 1)),
      });
    });
    await expect.poll(() => page.evaluate(() => (window as W).__r)).toEqual({ status: 202 });
    const buy = named(fx, 'Buy');
    expect(buy).toHaveLength(1);
    expect(buy[0]).toMatchObject({
      u: 'https://shop.example/checkout?x=1',
      i: false,
      p: { tier: 'silver', qty: '2', gift: 'false' },
    });
    expect(buy[0]).not.toHaveProperty('$');
    expect(Object.keys(buy[0]!.p as object).sort()).toEqual(['gift', 'qty', 'tier']);
    expect(await page.evaluate(() => (window as W).__n)).toBe(1);
  });

  test('engagement 不能经手动 API 发送；rivu("pageview") 走 pageview 流程', async ({ page, fx }) => {
    await page.goto(fx.url('/plain', full({ cfg: { engagement: false } })));
    await waitForEvents(fx, 1);
    await rivu(page, 'event', 'engagement', { props: { x: 1 } });
    await rivu(page, 'event', 'pageview', { url: 'https://a.example/manual' });
    await rivu(page, 'pageview', { props: { k: 'v' } });
    await waitForEvents(fx, 3);
    await settle(page);
    expect(named(fx, 'engagement')).toHaveLength(0);
    const pv = named(fx, 'pageview');
    expect(pv.map((b) => b.u)).toContain('https://a.example/manual');
    expect(pv.some((b) => (b.p as Record<string, string> | undefined)?.k === 'v')).toBe(true);
    // pageview 不带 i 与 $
    expect(pv.every((b) => !('i' in b) && !('$' in b))).toBe(true);
  });

  test('manual 模式：不发自动 pageview、不跟路由；手动 pageview 照常', async ({ page, fx }) => {
    await page.goto(fx.url('/spa/a', full({ cfg: { manual: true } })));
    await page.evaluate(() => history.pushState({}, '', '/spa/b'));
    await settle(page);
    expect(fx.posts()).toHaveLength(0);
    await rivu(page, 'pageview');
    await waitForEvents(fx, 1);
    expect(bodies(fx)[0]!.u).toContain('/spa/b');
  });

  test.describe('被忽略时', () => {
    test.use({ bypassAutomation: false });

    test('debug 输出原因，callback 无参调用一次，不发请求', async ({ page, fx }) => {
      const warns: string[] = [];
      page.on('console', (m) => m.type() === 'warning' && warns.push(m.text()));
      await page.goto(fx.url('/plain', full({ cfg: { debug: true } })));
      await page.evaluate(() => {
        const w = window as W;
        w.rivu('event', 'X', { callback: (...a: unknown[]) => ((w.__args = a.length), (w.__n = (w.__n || 0) + 1)) });
      });
      await settle(page);
      expect(fx.events).toHaveLength(0);
      expect(await page.evaluate(() => [(window as W).__n, (window as W).__args])).toEqual([1, 0]);
      expect(warns).toContain('[rivu] ignored: automation');
    });

    test('没有 debug 时静默', async ({ page, fx }) => {
      const logs: string[] = [];
      page.on('console', (m) => logs.push(m.text()));
      await page.goto(fx.url('/plain', full()));
      await settle(page);
      expect(fx.events).toHaveLength(0);
      expect(logs).toEqual([]);
    });
  });
});

test.describe('engagement [§4.5]', () => {
  test('页面隐藏时 flush：首个必发，带该页的 u 与属性；sd 是 0–100 的整数，e 是增量毫秒', async ({ page, fx, context }) => {
    await emulateVisibility(context, 'visible');
    await page.goto(fx.url('/tall', full({ stub: true })));
    await waitForEvents(fx, 2);
    const pv = named(fx, 'pageview')[0]!;
    await page.evaluate(() => (window as W).__setVisibility('hidden'));
    await expect.poll(() => named(fx, 'engagement').length).toBe(1);
    const e = named(fx, 'engagement')[0]!;
    expect(e.u).toBe(pv.u);
    expect(e.p).toEqual(pv.p);
    expect(e.h).toBe(1);
    expect(Number.isInteger(e.sd) && (e.sd as number) >= 0 && (e.sd as number) <= 100).toBe(true);
    expect(Number.isInteger(e.e) && (e.e as number) >= 0).toBe(true);
    expect(e.sd as number).toBeLessThan(100);

    // 不足 3 秒、也没有更深的滚动：再次隐藏不发
    await page.evaluate(() => (window as W).__setVisibility('visible'));
    await page.evaluate(() => (window as W).__setVisibility('hidden'));
    await settle(page);
    expect(named(fx, 'engagement')).toHaveLength(1);

    // 滚到底再隐藏：更深的滚动，发第二个，sd = 100
    await page.evaluate(() => (window as W).__setVisibility('visible'));
    await page.evaluate(() => window.scrollTo(0, document.documentElement.scrollHeight));
    await page.evaluate(() => new Promise((r) => requestAnimationFrame(() => setTimeout(r, 50))));
    await page.evaluate(() => (window as W).__setVisibility('hidden'));
    await expect.poll(() => named(fx, 'engagement').length).toBe(2);
    expect(named(fx, 'engagement')[1]!.sd).toBe(100);
  });

  test('SPA 跳转：先发上一页的 engagement，再发新页的 pageview', async ({ page, fx }) => {
    await page.goto(fx.url('/spa/a', full()));
    await waitForEvents(fx, 1);
    await page.evaluate(() => history.pushState({}, '', '/spa/b'));
    await expect.poll(() => named(fx, 'pageview').length).toBe(2);
    await expect.poll(() => named(fx, 'engagement').length).toBe(1);
    await settle(page);
    const e = named(fx, 'engagement');
    expect(e).toHaveLength(1);
    expect(new URL(e[0]!.u as string).pathname).toBe('/spa/a');
    expect(named(fx, 'pageview').map((b) => new URL(b.u as string).pathname)).toEqual(['/spa/a', '/spa/b']);
  });

  test('engagement:false 不发；被忽略的页面也不发', async ({ page, fx, context }) => {
    await emulateVisibility(context, 'visible');
    await page.goto(fx.url('/plain', full({ cfg: { engagement: false } })));
    await waitForEvents(fx, 1);
    await page.evaluate(() => (window as W).__setVisibility('hidden'));
    await settle(page);
    expect(named(fx, 'engagement')).toHaveLength(0);

    fx.reset();
    await page.goto(fx.url('/plain', full({ ignore: true })));
    await page.evaluate(() => (window as W).__setVisibility('hidden'));
    await settle(page);
    expect(fx.events).toHaveLength(0);
  });
});

test.describe('自动捕获 [§4.6]', () => {
  // engagement 关掉：只数自动捕获的事件
  const opts = full({ cfg: { engagement: false, outbound: true, downloads: true, forms: true } });
  const tagOnly = full({ cfg: { engagement: false } });

  test('外链、下载、同站链接；外链优先于下载；一次点击一个事件', async ({ page, fx }) => {
    await page.goto(fx.url('/capture', opts));
    await waitForEvents(fx, 1);
    await page.click('#out');
    await page.click('#dl');
    await page.click('#same');
    await page.click('#dlout');
    await expect.poll(() => fx.posts().length).toBe(4);
    await settle(page);
    const out = named(fx, 'Outbound Link: Click').map((b) => (b.p as Record<string, string>).url);
    expect(out).toHaveLength(2);
    expect(out[0]).toMatch(/\/away\?q=1#x$/);
    expect(out[1]).toMatch(/\/f\/a\.zip$/);
    const dl = named(fx, 'File Download');
    expect(dl).toHaveLength(1);
    expect((dl[0]!.p as Record<string, string>).url).toMatch(/\/files\/Report\.PDF$/);
    // 页面没有跳走
    expect(new URL(page.url()).pathname).toBe('/capture');
  });

  test('中键点击（auxclick）也算外链', async ({ page, fx, browserName }) => {
    test.skip(browserName !== 'chromium', '无头 Firefox / WebKit 的中键行为不一致（可能打开新标签页）');
    await page.goto(fx.url('/capture', opts));
    await waitForEvents(fx, 1);
    await page.click('#out', { button: 'middle' });
    await expect.poll(() => named(fx, 'Outbound Link: Click').length).toBe(1);
  });

  test('长链接属性保持在 UTF-8 字节上限内，事件仍发出', async ({ page, fx }) => {
    await page.goto(fx.url('/capture', opts));
    await page.locator('#out').evaluate((el) => {
      (el as HTMLAnchorElement).href = 'https://example.org/' + '界'.repeat(800);
    });
    await page.click('#out');
    await expect.poll(() => named(fx, 'Outbound Link: Click').length).toBe(1);
    const url = (named(fx, 'Outbound Link: Click')[0]!.p as Record<string, string>).url!;
    expect(url).toMatch(/^https:\/\/example\.org\//);
    expect(Buffer.byteLength(url)).toBeLessThanOrEqual(2000);
  });

  test('开关关闭时不捕获', async ({ page, fx }) => {
    await page.goto(fx.url('/capture', tagOnly));
    await waitForEvents(fx, 1);
    await page.click('#out');
    await page.click('#dl');
    await page.click('#f1b');
    await settle(page);
    expect(fx.posts()).toHaveLength(1);
  });

  test('tagged：就近优先、属性、链接补 url，忽略旧收入标记；超过 3 层与表单内点击不算', async ({ page, fx }) => {
    await page.goto(fx.url('/capture', tagOnly));
    await waitForEvents(fx, 1);
    await page.click('#tag');
    await page.click('#deep');
    await page.click('#far');
    await page.click('#taglink');
    await page.click('#f3b');
    await expect.poll(() => fx.posts().length).toBe(5);
    await settle(page);
    expect(named(fx, 'Signup')[0]!.p).toEqual({ plan: 'pro plan' });
    expect(named(fx, 'Outer')).toHaveLength(0);
    const wrap = named(fx, 'Wrap');
    expect(wrap).toHaveLength(1);
    expect(wrap[0]!.p).toEqual({ plan: 'pro' });
    expect(wrap[0]).not.toHaveProperty('$');
    expect(named(fx, 'TooFar')).toHaveLength(0);
    expect((named(fx, 'Docs')[0]!.p as Record<string, string>).url).toMatch(/\/docs$/);
    // #f3b 在表单里：点击不算，提交时发表单自己的标记
    expect(named(fx, 'Inner')).toHaveLength(0);
    expect(named(fx, 'Lead')).toHaveLength(1);
  });

  test('表单：有效提交发 Form: Submission，校验失败不发，被标记的表单只发标记事件', async ({ page, fx }) => {
    await page.goto(fx.url('/capture', opts));
    await waitForEvents(fx, 1);
    await page.click('#f1b');
    await page.click('#f3b');
    await expect.poll(() => fx.posts().length).toBe(3);
    // required 为空：浏览器拦下，不触发 submit（放在最后：Firefox 的校验气泡会吞掉紧接着的下一次点击）
    await page.click('#f2b');
    await settle(page);
    expect(fx.posts()).toHaveLength(3);
    expect(named(fx, 'Form: Submission')).toHaveLength(1);
    expect(named(fx, 'Lead')).toHaveLength(1);
  });
});
