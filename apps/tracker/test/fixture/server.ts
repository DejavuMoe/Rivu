// 浏览器测试夹具：两个本地 HTTP 服务器，不依赖 Rust 服务端，也不连数据库。
//
//  - site 服务器：托管测试页面（普通页、SPA、hash 路由、预渲染、bfcache ……），模拟「站点所有者的网站」。
//    它同时有一个 `POST /api/event`，模拟站点第一方反代的采集路径（测试 `credentials: 'omit'` 用）。
//  - rivu 服务器：模拟 Rivu 自己（另一个 origin，所以脚本发出的是跨域请求）：
//      GET  /js/<site_key>.js   站点专属脚本 = 配置前缀（规范 §2.3）+ dist/core.js
//      GET  /t.js               通用脚本（无前缀，配置来自 data-*，规范 §2.2）
//      POST /api/event          采集端：记录方法、Content-Type、Cookie 头与请求体，回 202（规范 §6.1）
//      OPTIONS /api/event       也记录（测试断言「没有预检」），回 204
//
// 两个服务器都监听 127.0.0.1 的随机端口。所有被记录的请求放在同一个 `events` 数组里。

import { readFile } from 'node:fs/promises';
import http from 'node:http';
import type { AddressInfo } from 'node:net';

export const SITE_KEY = 'k7qm2xv5atbr';

/** 采集端记录的一个请求。 */
export interface Recorded {
  /** 由哪个服务器收到：`rivu` 是标准的跨域采集端，`site` 是站点第一方反代路径。 */
  server: 'rivu' | 'site';
  method: string;
  path: string;
  contentType: string | undefined;
  /** 原样记录；没有 Cookie 头时为 undefined。 */
  cookie: string | undefined;
  origin: string | undefined;
  referer: string | undefined;
  secFetchSite: string | undefined;
  secFetchDest: string | undefined;
  /** 请求体字节数。 */
  bytes: number;
  /** 请求体原文（UTF-8）。 */
  text: string;
  /** 请求体解析为 JSON 的结果；不是合法 JSON 时为 undefined。 */
  json: Record<string, unknown> | undefined;
}

/** 测试页面的选项，经 `?t=<JSON>` 传给 site 服务器（跳转时原样带到下一页）。 */
export interface PageOpts {
  /** 站点 key，默认 {@link SITE_KEY}。 */
  site?: string;
  /** 合入配置的键，默认 `{local:true}`（夹具在 127.0.0.1，必须允许 localhost 采集）。 */
  cfg?: Record<string, unknown>;
  /**
   * 脚本的加载与配置方式：
   *  - `prefix`（默认）：`/js/<site>.js`，配置在前缀里
   *  - `data`：`/t.js` + `data-site/api/hash/local`
   *  - `data-default`：`/t.js` + 仅 `data-site`/`data-local`，端点取脚本 src 的 origin
   *  - `none`：不加载脚本（用于对比 window 全局变量）
   */
  mode?: 'prefix' | 'data' | 'data-default' | 'none';
  /** 采集端点：`rivu`（默认，跨域）、`site`（第一方反代路径）、`dead`（连不上的端口）。 */
  endpoint?: 'rivu' | 'site' | 'dead';
  /** 在脚本之前放队列桩与几条命令（规范 §2.1）。 */
  stub?: boolean;
  /** 同一个脚本引入两次（重复加载）。 */
  twice?: boolean;
  /** 在脚本之前内联 `rivu.c = {...}`（桩 + 配置前缀），用来测 `rivu.c` 与 `data-*` 的优先级。 */
  inlineC?: Record<string, unknown>;
  /** 页面自己的代码在脚本之前 `history.replaceState` 到该 URL（测 `u` 取发送瞬间的 `location.href`）。 */
  replaceUrl?: string;
  /** 页面自己的代码在脚本之前写 `localStorage.rivu_ignore = "true"`（站点所有者手动设置的模拟）。 */
  ignore?: boolean;
}

export interface Fixture {
  siteOrigin: string;
  rivuOrigin: string;
  events: Recorded[];
  /** 只看 POST 且体为 JSON 对象的事件。 */
  posts(): Recorded[];
  reset(): void;
  /** 生成测试页面的 URL。`origin` 默认是 site 服务器；传 `http://localhost:<port>` 之类可换主机名。 */
  url(path: string, opts?: PageOpts, origin?: string): string;
  close(): Promise<void>;
}

const distUrl = (f: string) => new URL(`../../dist/${f}`, import.meta.url);

/** 规范 §2.3：JSON 转义并额外转义 `<`、U+2028/2029。 */
function jsString(v: unknown): string {
  return JSON.stringify(v)
    .replace(/</g, '\\u003c')
    .split(String.fromCharCode(0x2028))
    .join('\\u2028')
    .split(String.fromCharCode(0x2029))
    .join('\\u2029');
}

/** 站点脚本的配置前缀，形态同规范 §2.3。 */
export function buildPrefix(cfg: Record<string, unknown>): string {
  return `window.rivu=window.rivu||function(){(rivu.q=rivu.q||[]).push(arguments)};rivu.c=${jsString(cfg)};`;
}

/** 采集端的响应头（规范 §6.1「所有响应」）。 */
const CORS_HEADERS = {
  'Access-Control-Allow-Origin': '*',
  'Access-Control-Expose-Headers': 'x-rivu-dropped, Retry-After',
  'Cache-Control': 'no-store',
  'Cross-Origin-Resource-Policy': 'cross-origin',
};

function listen(server: http.Server): Promise<number> {
  return new Promise((resolve) => {
    server.listen(0, '127.0.0.1', () => resolve((server.address() as AddressInfo).port));
  });
}

function readBody(req: http.IncomingMessage): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on('data', (c: Buffer) => chunks.push(c));
    req.on('end', () => resolve(Buffer.concat(chunks)));
    req.on('error', reject);
  });
}

const header = (req: http.IncomingMessage, name: string): string | undefined => {
  const v = req.headers[name];
  return Array.isArray(v) ? v.join(', ') : v;
};

export async function startFixture(): Promise<Fixture> {
  const coreJs = await readFile(distUrl('core.js'), 'utf8');
  const events: Recorded[] = [];
  let siteOrigin = '';
  let rivuOrigin = '';

  async function collect(server: Recorded['server'], req: http.IncomingMessage, res: http.ServerResponse) {
    const body = await readBody(req);
    const text = body.toString('utf8');
    let json: Recorded['json'];
    try {
      const v: unknown = JSON.parse(text);
      if (v && typeof v === 'object' && !Array.isArray(v)) json = v as Record<string, unknown>;
    } catch {
      /* 非 JSON：json 保持 undefined */
    }
    events.push({
      server,
      method: req.method ?? '',
      path: req.url ?? '',
      contentType: header(req, 'content-type'),
      cookie: header(req, 'cookie'),
      origin: header(req, 'origin'),
      referer: header(req, 'referer'),
      secFetchSite: header(req, 'sec-fetch-site'),
      secFetchDest: header(req, 'sec-fetch-dest'),
      bytes: body.length,
      text,
      json,
    });
    if (req.method === 'OPTIONS') {
      res.writeHead(204, {
        ...CORS_HEADERS,
        'Access-Control-Allow-Methods': 'POST, OPTIONS',
        'Access-Control-Allow-Headers': 'Content-Type',
        'Access-Control-Max-Age': '86400',
      });
    } else {
      // 规范 §6.2：事件被接受时 202 空体。
      res.writeHead(202, { ...CORS_HEADERS, 'Content-Length': '0' });
    }
    res.end();
  }

  // ---- rivu 服务器 ----
  const rivu = http.createServer((req, res) => {
    void (async () => {
      const u = new URL(req.url ?? '/', 'http://x');
      if (u.pathname === '/api/event' && (req.method === 'POST' || req.method === 'OPTIONS')) {
        await collect('rivu', req, res);
        return;
      }
      const script = { ...CORS_HEADERS, 'Content-Type': 'application/javascript; charset=utf-8', 'Cache-Control': 'no-store' };
      const m = /^\/js\/([A-Za-z0-9_-]+)\.js$/.exec(u.pathname);
      if (m && req.method === 'GET') {
        const cfg = JSON.parse(u.searchParams.get('c') ?? '{}') as Record<string, unknown>;
        // 站点脚本的配置前缀：`site` 来自路径，`endpoint` 默认指向本服务器。
        const prefix = buildPrefix({ site: m[1], endpoint: `${rivuOrigin}/api/event`, ...cfg });
        res.writeHead(200, script);
        res.end(prefix + coreJs);
        return;
      }
      if (u.pathname === '/t.js' && req.method === 'GET') {
        res.writeHead(200, script);
        res.end(coreJs);
        return;
      }
      res.writeHead(404, { 'Cache-Control': 'no-store' });
      res.end();
    })().catch(() => {
      res.destroy();
    });
  });

  // ---- site 服务器 ----
  const site = http.createServer((req, res) => {
    void (async () => {
      const u = new URL(req.url ?? '/', 'http://x');
      if (u.pathname === '/api/event' && (req.method === 'POST' || req.method === 'OPTIONS')) {
        await collect('site', req, res);
        return;
      }
      if (req.method !== 'GET') {
        res.writeHead(405);
        res.end();
        return;
      }
      if (u.pathname === '/favicon.ico') {
        // 避免浏览器的 favicon 404 在控制台留下噪音（用例要断言 core 不输出任何控制台消息）。
        res.writeHead(204);
        res.end();
        return;
      }
      const raw = u.searchParams.get('t');
      const opts = (raw ? JSON.parse(raw) : {}) as PageOpts;
      const html = renderPage(u.pathname, opts);
      if (html === undefined) {
        res.writeHead(404);
        res.end();
        return;
      }
      // 不设 `Cache-Control: no-store`：它会让页面失去 bfcache 资格（Chromium）。
      res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
      res.end(html);
    })().catch(() => {
      res.destroy();
    });
  });

  function trackerTags(opts: PageOpts): string {
    const mode = opts.mode ?? 'prefix';
    if (mode === 'none') return '';
    const site = opts.site ?? SITE_KEY;
    const cfg: Record<string, unknown> = { local: true, ...opts.cfg };
    const endpoint =
      opts.endpoint === 'site'
        ? `${siteOrigin}/api/event`
        : opts.endpoint === 'dead'
          ? 'http://127.0.0.1:1/api/event'
          : `${rivuOrigin}/api/event`;
    let tag: string;
    if (mode === 'prefix') {
      const c = JSON.stringify({ ...cfg, endpoint });
      tag = `<script defer src="${rivuOrigin}/js/${site}.js?c=${encodeURIComponent(c)}"></script>`;
    } else {
      const attrs = [`data-site="${site}"`];
      if (mode === 'data') attrs.push(`data-api="${endpoint}"`);
      if (cfg.local) attrs.push('data-local="true"');
      if (cfg.hash) attrs.push('data-hash="true"');
      tag = `<script defer src="${rivuOrigin}/t.js" ${attrs.join(' ')}></script>`;
    }
    return opts.twice ? tag + tag : tag;
  }

  function preScripts(opts: PageOpts): string {
    let s = '';
    // 站点所有者在自己浏览器里手动设置的开关；这里是页面代码在写，不是追踪脚本。
    if (opts.ignore) s += `<script>localStorage.setItem('rivu_ignore','true')</script>`;
    if (opts.replaceUrl) s += `<script>history.replaceState(null,'',${jsString(opts.replaceUrl)})</script>`;
    if (opts.inlineC) s += `<script>${buildPrefix(opts.inlineC)}</script>`;
    if (opts.stub) {
      // 队列桩（规范 §2.1）+ 加载前入队的命令。core 不回放这些命令；callback 属于 full 的手动 API。
      s +=
        `<script>window.rivu=window.rivu||function(){(rivu.q=rivu.q||[]).push(arguments)};` +
        `window.__cb=0;` +
        `rivu('event','Signup',{props:{plan:'pro'},callback:function(){window.__cb++}});` +
        `rivu('set',{props:{a:'1'}});rivu('init',{hash:true});</script>`;
    }
    return s;
  }

  function renderPage(pathname: string, opts: PageOpts): string | undefined {
    const t = encodeURIComponent(JSON.stringify(opts));
    const link = (id: string, href: string, label: string) =>
      `<a id="${id}" href="${href}${href.includes('?') ? '&' : '?'}t=${t}">${label}</a>`;
    let body: string;
    let head = '';
    if (pathname === '/plain') {
      body = `<h1>plain</h1>${link('to-other', '/other', 'other')} ${link('to-spa', '/spa/a', 'spa')}`;
    } else if (pathname === '/other') {
      body = `<h1>other</h1>${link('to-plain', '/plain', 'plain')}`;
    } else if (pathname === '/spa' || pathname.startsWith('/spa/')) {
      // History 模式的单页应用：任何 /spa/* 都返回同一页，导航由测试用 history.pushState 触发。
      body = `<h1>spa</h1><div id="view"></div>`;
    } else if (pathname === '/hash') {
      body = `<h1>hash</h1>`;
    } else if (pathname === '/prerender-a') {
      // Speculation Rules：预渲染 /prerender-b。
      const rules = { prerender: [{ source: 'list', urls: [`/prerender-b?t=${t}`] }] };
      head = `<script type="speculationrules">${JSON.stringify(rules)}</script>`;
      body = `<h1>prerender-a</h1>${link('go', '/prerender-b', 'b')}`;
    } else if (pathname === '/prerender-b') {
      body = `<h1>prerender-b</h1><script>window.__prerenderingAtLoad=document.prerendering;window.__loadedAt=Date.now()</script>`;
    } else if (pathname === '/bfcache-a') {
      // 页面 JS 状态只有在 bfcache 恢复时才会保留，测试据此判断是否真的从 bfcache 恢复。
      body = `<h1>bfcache-a</h1><script>window.__marker='alive'</script>${link('away', '/bfcache-b', 'away')}`;
    } else if (pathname === '/bfcache-b') {
      body = `<h1>bfcache-b</h1>`;
    } else {
      return undefined;
    }
    return `<!doctype html><html><head><meta charset="utf-8"><title>${pathname}</title>${head}${preScripts(opts)}${trackerTags(opts)}</head><body>${body}</body></html>`;
  }

  const [sitePort, rivuPort] = await Promise.all([listen(site), listen(rivu)]);
  siteOrigin = `http://127.0.0.1:${sitePort}`;
  rivuOrigin = `http://127.0.0.1:${rivuPort}`;

  return {
    siteOrigin,
    rivuOrigin,
    events,
    posts: () => events.filter((e) => e.method === 'POST' && e.json !== undefined),
    reset: () => {
      events.length = 0;
    },
    url: (path, opts, origin = siteOrigin) =>
      `${origin}${path}${opts ? `?t=${encodeURIComponent(JSON.stringify(opts))}` : ''}`,
    close: () =>
      new Promise<void>((resolve) => {
        site.closeAllConnections();
        rivu.closeAllConnections();
        site.close(() => rivu.close(() => resolve()));
      }),
  };
}
