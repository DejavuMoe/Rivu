// 已知答案探针（docs/brief-p1.md 验收：「定时制造已知访问的无头浏览器，连续 7 天全部通过」）。
//
// 每一轮：一个全新的无头 Chromium 上下文（UA 每轮不同，所以是一个新访客、一个新会话），在本机起的测试页上做一组已知动作，
// 脚本从被测的 rivu 加载（GET <base>/js/<site_key>.js），事件发往被测的 rivu；然后用公开查询 API
// （POST <base>/api/v1/query，API key 只要 stats:read）按本轮独有的路径前缀过滤，直到数字出现，并与已知答案逐项比较：
//
//   动作：打开 /p/<rid>/a -> SPA pushState 到 /p/<rid>/b -> 点链接整页跳转到 /p/<rid>/c -> 关闭页面（flush engagement）
//   答案：pageviews = 3，visits = 1，visitors = 1，bounce_rate = 0，events = 3（events 不含 engagement）
//
// 每轮写一行 JSON 到 --out（追加）：时间、轮次 id、是否通过、各指标、可见延迟（最后一个动作到数字完全出现）、失败原因。
// 站点的 hostname 白名单里要有 127.0.0.1（测试页在本机），tracker 默认排除 127.x，所以脚本标签带 data-local="true"。
//
// 用法（apps/tracker 目录，需要 pnpm install 与 playwright 的 chromium）：
//   RIVU_PROBE_API_KEY=... node probe/probe.mjs --base https://stats.example.com --site <site_key> \
//     --out probe.jsonl --interval 600 --rounds 0          # 每 10 分钟一轮，一直跑
//   node probe/summary.mjs probe.jsonl                      # 汇总：通过率、最长连续通过、延迟分位数
// 退出码：--rounds 有限时，全部通过为 0，否则为 1。

import http from 'node:http';
import { appendFileSync } from 'node:fs';
import { parseArgs } from 'node:util';
import { chromium } from '@playwright/test';

const { values: opt } = parseArgs({
  options: {
    base: { type: 'string' },
    site: { type: 'string' },
    out: { type: 'string', default: 'probe.jsonl' },
    interval: { type: 'string', default: '600' },
    rounds: { type: 'string', default: '1' },
    timeout: { type: 'string', default: '60' },
  },
});
const BASE = (opt.base ?? process.env.RIVU_PROBE_BASE ?? '').replace(/\/$/, '');
const SITE = opt.site ?? process.env.RIVU_PROBE_SITE;
const KEY = process.env.RIVU_PROBE_API_KEY;
if (!BASE || !SITE || !KEY) {
  console.error('需要 --base、--site 与环境变量 RIVU_PROBE_API_KEY');
  process.exit(2);
}
const INTERVAL_MS = Number(opt.interval) * 1000;
const ROUNDS = Number(opt.rounds);
const TIMEOUT_MS = Number(opt.timeout) * 1000;
const EXPECT = { visitors: 1, visits: 1, pageviews: 3, bounce_rate: 0, events: 3 };
const METRICS = Object.keys(EXPECT);

/** 测试页：任何 /p/<rid>/* 都是同一页，a 页有一个跳到 c 的链接。 */
function startPages() {
  const server = http.createServer((req, res) => {
    const p = new URL(req.url ?? '/', 'http://x').pathname;
    const m = /^\/p\/([a-z0-9]+)\/([a-z])$/.exec(p);
    if (!m) {
      res.writeHead(404);
      res.end();
      return;
    }
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    res.end(
      `<!doctype html><html><head><meta charset="utf-8"><title>probe ${m[2]}</title>` +
        `<script defer src="${BASE}/js/${SITE}.js" data-local="true"></script></head>` +
        `<body><h1>${m[2]}</h1><a id="next" href="/p/${m[1]}/c">next</a></body></html>`,
    );
  });
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve({ server, origin: `http://127.0.0.1:${server.address().port}` }));
  });
}

async function query(rid) {
  const r = await fetch(`${BASE}/api/v1/query`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${KEY}`, 'Content-Type': 'application/json' },
    body: JSON.stringify({
      site: SITE,
      metrics: METRICS,
      date_range: '7d',
      filters: [['contains', 'event:page', [`/p/${rid}/`]]],
    }),
  });
  if (!r.ok) throw new Error(`查询 API 返回 ${r.status}：${(await r.text()).slice(0, 200)}`);
  const body = await r.json();
  const row = body.results && body.results[0];
  const values = row ? row.metrics : METRICS.map(() => 0);
  return Object.fromEntries(METRICS.map((k, i) => [k, values[i] ?? 0]));
}

async function round(browser, origin) {
  const rid = Date.now().toString(36) + Math.random().toString(36).slice(2, 6);
  const started = new Date().toISOString();
  // UA 每轮不同（版本号里带轮次），所以每轮是一个新访客；不能用无头 Chromium 自带的 UA（含 HeadlessChrome，会被判为机器人）
  const n = parseInt(rid.slice(-6), 36);
  const ua = `Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.${n % 9000}.${n % 97} Safari/537.36`;
  const context = await browser.newContext({ userAgent: ua });
  await context.addInitScript(() => {
    window.__rivu = true; // 放行自动化检测（规范 §4.7）
  });
  const page = await context.newPage();
  const sent = [];
  page.on('request', (r) => {
    if (r.method() === 'POST' && r.url() === `${BASE}/api/event`) sent.push(r.postData() ?? '');
  });
  const waitPageviews = async (k) => {
    const deadline = Date.now() + 10_000;
    while (sent.filter((s) => s.includes('"n":"pageview"')).length < k) {
      if (Date.now() > deadline) throw new Error(`浏览器没有发出第 ${k} 个 pageview`);
      await page.waitForTimeout(50);
    }
  };
  const result = { at: started, rid, ok: false, expect: EXPECT };
  try {
    await page.goto(`${origin}/p/${rid}/a`);
    await waitPageviews(1);
    await page.evaluate((r) => history.pushState(null, '', `/p/${r}/b`), rid);
    await waitPageviews(2);
    await page.click('#next');
    await page.waitForURL(/\/c$/);
    await waitPageviews(3);
    await context.close(); // 关闭页面：flush engagement
    const done = Date.now();
    let got;
    for (;;) {
      got = await query(rid);
      if (METRICS.every((k) => got[k] === EXPECT[k])) break;
      if (Date.now() - done > TIMEOUT_MS) break;
      await new Promise((r) => setTimeout(r, 250));
    }
    result.got = got;
    result.visible_ms = Date.now() - done;
    result.ok = METRICS.every((k) => got[k] === EXPECT[k]);
    if (!result.ok) result.error = `${TIMEOUT_MS / 1000} 秒内没有等到已知答案`;
    // 等一会儿再查一次：数字不应继续变化（没有重复计数）
    if (result.ok) {
      await new Promise((r) => setTimeout(r, 3000));
      const again = await query(rid);
      if (!METRICS.every((k) => again[k] === EXPECT[k])) {
        result.ok = false;
        result.error = `数字随后又变了：${JSON.stringify(again)}`;
      }
    }
  } catch (e) {
    result.error = String(e && e.message ? e.message : e);
    await context.close().catch(() => {});
  }
  result.requests = sent.length;
  return result;
}

const { server, origin } = await startPages();
const browser = await chromium.launch();
let failures = 0;
try {
  for (let i = 0; ROUNDS === 0 || i < ROUNDS; i++) {
    const t0 = Date.now();
    const r = await round(browser, origin);
    appendFileSync(opt.out, JSON.stringify(r) + '\n');
    console.log(`${r.at} ${r.rid} ${r.ok ? 'PASS' : 'FAIL'} ${r.visible_ms ?? '-'} ms ${r.error ?? ''}`);
    if (!r.ok) failures++;
    if (ROUNDS !== 0 && i + 1 >= ROUNDS) break;
    await new Promise((res) => setTimeout(res, Math.max(0, INTERVAL_MS - (Date.now() - t0))));
  }
} finally {
  await browser.close();
  server.close();
}
process.exit(failures === 0 ? 0 : 1);
