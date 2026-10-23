// 真实浏览器 -> 真实运行的 rivu（`rivu all`，连 dev 的 PostgreSQL / ClickHouse）-> `rivu stats`。
// 「已知答案探针」（docs/brief-p1.md 验收项）的雏形：浏览器做一组已知动作，系统里查到的数字必须与之完全一致。
//
// 与 test/e2e 的区别：这里**没有任何模拟的采集端**。
//   - 脚本来自 rivu 自己：`GET <RIVU>/js/<site_key>.js`（配置前缀 + core.js，规范 §2.3）；
//   - 事件发往 rivu 的 `POST /api/event`；
//   - 断言用 `rivu stats`（reader 账号查 ClickHouse），不看浏览器侧记录的请求。
// 页面由本文件里的小服务器提供（主机名 127.0.0.1 在站点白名单里）；tracker 默认排除 127.x，
// 所以脚本标签上写 `data-local="true"`（规范 §2.2：`data-local` 允许本地采集；`rivu.c` 前缀里没有这个键，所以 data-* 生效）。
//
// 已知动作（每个内核一个站点、一个访客、一个会话）：
//   1. 首屏 /bfcache-a                         -> pageview 1（首个 pageview，document.referrer 为空 -> 来源 Direct）
//   2. SPA pushState 到 /spa/x                 -> pageview 2
//   3. SPA pushState 到 /spa/y                 -> pageview 3
//   4. 点链接整页跳转到 /bfcache-b             -> pageview 4
//   5. history.back() 回到 /spa/y              -> pageview 5（bfcache 恢复走 pageshow，没命中则是重新加载，两条路径都恰好 +1）
// 期望：pageviews = 5，visits = 1，visitors = 1，页面榜 /spa/y 2 次、其余各 1 次。

import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import http from 'node:http';
import type { AddressInfo } from 'node:net';
import path from 'node:path';
import { expect, test } from '@playwright/test';

const need = (name: string): string => {
  const v = process.env[name];
  if (!v) throw new Error(`缺少环境变量 ${name}：这个测试要通过 scripts/e2e-rivu.sh 运行`);
  return v;
};

const RIVU = need('RIVU_E2E_BASE'); // 例如 http://127.0.0.1:18080
const RIVU_BIN = process.env.RIVU_E2E_BIN ?? path.resolve(process.cwd(), '../../target/debug/rivu');
const DIST_META = JSON.parse(readFileSync(new URL('../../dist/meta.json', import.meta.url), 'utf8')) as { sv: number };

interface Row {
  value: string;
  visitors: number;
  visits: number;
  pageviews: number;
}
interface Stats {
  visitors: number;
  visits: number;
  pageviews: number;
  bounce_rate: number;
  breakdown: Record<'page' | 'source' | 'channel' | 'country' | 'device', Row[]>;
  meta: { geo_attribution: unknown; geo_capability: string | null };
}

/** 站点时区是 UTC，范围取今天（UTC）。跨 UTC 午夜的运行会让「今天」变化，调用方会检测。 */
const utcToday = (): string => new Date().toISOString().slice(0, 10);

function stats(siteKey: string, day: string): Stats {
  const out = execFileSync(RIVU_BIN, ['stats', '--site', siteKey, '--from', day, '--to', day], {
    env: process.env,
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  return JSON.parse(out) as Stats;
}

/** 测试页面服务器：任何 /spa/* 返回同一页；/bfcache-a、/bfcache-b 是普通页。不设 no-store（它会让页面失去 bfcache 资格）。 */
function startPages(siteKey: string): Promise<{ origin: string; close: () => Promise<void> }> {
  const server = http.createServer((req, res) => {
    const p = new URL(req.url ?? '/', 'http://x').pathname;
    if (p === '/favicon.ico') {
      res.writeHead(204);
      res.end();
      return;
    }
    let body: string;
    if (p === '/bfcache-a') {
      body = `<h1>a</h1><script>window.__marker='alive'</script><a id="away" href="/bfcache-b">away</a>`;
    } else if (p === '/bfcache-b') {
      body = `<h1>b</h1>`;
    } else if (p.startsWith('/spa/')) {
      body = `<h1>spa</h1>`;
    } else {
      res.writeHead(404);
      res.end();
      return;
    }
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    res.end(
      `<!doctype html><html><head><meta charset="utf-8"><title>${p}</title>` +
        `<script defer src="${RIVU}/js/${siteKey}.js" data-local="true"></script></head><body>${body}</body></html>`,
    );
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => {
      const { port } = server.address() as AddressInfo;
      resolve({
        origin: `http://127.0.0.1:${port}`,
        close: () =>
          new Promise<void>((r) => {
            server.closeAllConnections();
            server.close(() => r());
          }),
      });
    });
  });
}

test('已知动作：5 个 pageview、1 个会话、1 个访客，与 rivu stats 完全一致', async ({ page, context }, testInfo) => {
  const siteKey = need(`RIVU_E2E_SITE_${testInfo.project.name.toUpperCase()}`);
  const day = utcToday();

  // webdriver 为真时 tracker 默认不发送；测试页设 __rivu 放行（规范 §4.7）
  await context.addInitScript(() => {
    (window as unknown as { __rivu: boolean }).__rivu = true;
  });

  // ---- 脚本来自真实的 rivu ----------------------------------------------------------------------
  const scriptRes = await page.request.get(`${RIVU}/js/${siteKey}.js`);
  expect(scriptRes.status()).toBe(200);
  expect(scriptRes.headers()['content-type']).toBe('application/javascript; charset=utf-8');
  expect(scriptRes.headers()['cache-control']).toBe('public, max-age=300');
  expect(scriptRes.headers()['x-content-type-options']).toBe('nosniff');
  expect(scriptRes.headers()['etag']).toMatch(/^"[0-9a-f]{32}"$/);
  const scriptText = await scriptRes.text();
  expect(scriptText.startsWith('window.rivu=window.rivu||function(){(rivu.q=rivu.q||[]).push(arguments)};rivu.c={')).toBe(true);
  expect(scriptText).toContain(`"site":"${siteKey}","endpoint":"${RIVU}/api/event","hash":false}`);
  // 未知站点：404，不可缓存
  const unknown = await page.request.get(`${RIVU}/js/zzzzzzzzzzzz.js`);
  expect(unknown.status()).toBe(404);
  expect(unknown.headers()['cache-control']).toBe('no-store');

  const pages = await startPages(siteKey);
  try {
    // 记录发往 rivu 的采集请求与响应（只用来交叉检查，断言以 rivu stats 为准）
    const posts: string[] = [];
    const responses: { status: number; dropped: string | undefined }[] = [];
    page.on('request', (r) => {
      if (r.method() === 'POST' && r.url() === `${RIVU}/api/event`) posts.push(r.postData() ?? '');
    });
    page.on('response', (r) => {
      if (r.request().method() === 'POST' && r.url() === `${RIVU}/api/event`) {
        responses.push({ status: r.status(), dropped: r.headers()['x-rivu-dropped'] });
      }
    });
    const pageErrors: string[] = [];
    page.on('pageerror', (e) => pageErrors.push(String(e)));
    const sent = (n: number) => expect.poll(() => posts.length, { message: `等待 ${n} 个采集请求` }).toBeGreaterThanOrEqual(n);

    // 1. 首屏
    await page.goto(`${pages.origin}/bfcache-a`);
    await sent(1);
    // 脚本是 rivu 发的 core：构建号与 dist/meta.json 一致
    expect(await page.evaluate(() => (window as unknown as { rivu: { b: string; sv: number } }).rivu.b)).toBe('core');
    expect(await page.evaluate(() => (window as unknown as { rivu: { b: string; sv: number } }).rivu.sv)).toBe(DIST_META.sv);
    // 2、3. SPA 跳转（pathname 变化才算新 pageview）
    await page.evaluate(() => history.pushState(null, '', '/spa/x'));
    await sent(2);
    await page.evaluate(() => history.pushState(null, '', '/spa/y'));
    await sent(3);
    // 4. 整页跳转
    await page.click('#away');
    await page.waitForURL(/\/bfcache-b$/);
    await sent(4);
    // 5. 返回：bfcache 恢复（pageshow persisted）或重新加载，都恰好产生 1 个 pageview
    await page.goBack();
    await page.waitForURL(/\/spa\/y$/);
    await sent(5);
    const restored = await page.evaluate(() => (window as unknown as { __marker?: string }).__marker === 'alive');
    console.log(`[${testInfo.project.name}] 返回时${restored ? '发生了 bfcache 恢复' : '没有命中 bfcache（重新加载）'}`);

    // 让可能多余的请求有机会到达，再确认「恰好」5 个，且都被 rivu 接受（202，没有 x-rivu-dropped）
    await page.waitForTimeout(500);
    expect(posts).toHaveLength(5);
    expect(responses).toEqual(Array.from({ length: 5 }, () => ({ status: 202, dropped: undefined })));
    expect(pageErrors).toEqual([]);

    // ---- 系统里查到的数字必须与已知动作完全一致 --------------------------------------------------
    // 写缓冲每 RIVU_BUFFER_FLUSH_INTERVAL_MS 刷一次；等到数字出现，再多等一会儿确认没有后续变化。
    await expect
      .poll(() => stats(siteKey, day).pageviews, { timeout: 20_000, intervals: [250, 500, 1000], message: '等待 pageview 落库' })
      .toBe(5);
    await page.waitForTimeout(1500);
    const s = stats(siteKey, day);
    if (utcToday() !== day) throw new Error('测试跨过了 UTC 午夜（站点按 UTC 分日），请重跑');

    expect({ visitors: s.visitors, visits: s.visits, pageviews: s.pageviews }).toEqual({ visitors: 1, visits: 1, pageviews: 5 });
    expect(s.bounce_rate).toBe(0); // 5 个 pageview 的会话不是跳出
    // 页面：访客数相同时按 visits、pageviews 降序，再按路径升序
    expect(s.breakdown.page).toEqual([
      { value: '/spa/y', visitors: 1, visits: 1, pageviews: 2 },
      { value: '/bfcache-a', visitors: 1, visits: 1, pageviews: 1 },
      { value: '/bfcache-b', visitors: 1, visits: 1, pageviews: 1 },
      { value: '/spa/x', visitors: 1, visits: 1, pageviews: 1 },
    ]);
    // 会话属性由首事件决定：直接打开页面（referrer 为空）-> Direct；桌面 UA；127.0.0.1 不是公网地址 -> 没有位置
    const one = { visitors: 1, visits: 1, pageviews: 5 };
    expect(s.breakdown.source).toEqual([{ value: 'Direct / None', ...one }]);
    expect(s.breakdown.channel).toEqual([{ value: 'Direct', ...one }]);
    expect(s.breakdown.device).toEqual([{ value: 'desktop', ...one }]);
    expect(s.breakdown.country).toEqual([{ value: '(not set)', ...one }]);
    // 含国家的结果必须带署名与能力（库是手工放置的测试库：署名来自配置，能力取决于库类型）
    expect(s.meta).toHaveProperty('geo_attribution');
    expect(s.meta).toHaveProperty('geo_capability');
  } finally {
    await pages.close();
  }
});
