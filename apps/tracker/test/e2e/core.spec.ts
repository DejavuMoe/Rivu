// `core` 变体的浏览器行为测试（chromium / firefox / webkit 各跑一遍）。
// 场景对照 docs/architecture/tracker-and-protocol.md §11.2；方括号里的 § 是该行为的规范出处。
//
// 通用检查（见 fixtures.ts 的 wireCheck，每个用例结束时自动执行）：
//   采集端收到的每个请求都符合线协议、Content-Type 为 text/plain、不带 Cookie、体 < 8 KB、sv 等于构建号，
//   且页面没有任何未捕获异常。

import { SITE_KEY } from '../fixture/server.ts';
import { BUILD_SV, bodies, emulateVisibility, expect, settle, test, waitForEvents } from './fixtures.ts';

// 页面里读写的 window 扩展字段（测试用）。
// （排除 src/globals.d.ts 里给 rivu 等字段声明的严格类型：测试要探测「它可能不存在」的情形。）
type W = Omit<Window, 'rivu' | '__rivu' | 'Cypress'> & Record<string, any>; // eslint-disable-line @typescript-eslint/no-explicit-any

test.describe('前置条件与排除逻辑 [§4.7]', () => {
  test('Playwright 下 navigator.webdriver 为 true', async ({ page, fx }) => {
    await page.goto(fx.url('/plain', { mode: 'none' }));
    expect(await page.evaluate(() => navigator.webdriver)).toBe(true);
  });

  test.describe('默认设置（webdriver 为真，无 __rivu）', () => {
    test.use({ bypassAutomation: false });

    test('0 个请求，core 没有任何控制台输出', async ({ page, fx }) => {
      const logs: string[] = [];
      page.on('console', (m) => logs.push(`${m.type()}: ${m.text()}`));
      await page.goto(fx.url('/plain'));
      await settle(page);
      expect(fx.events).toHaveLength(0);
      expect(logs).toEqual([]);
      // 脚本照常加载并写了探测字段，只是不发请求。
      expect(await page.evaluate(() => (window as W).rivu?.b)).toBe('core');
    });

    test('Cypress 标记同样视为自动化', async ({ page, fx, context }) => {
      await context.addInitScript(() => {
        (window as W).Cypress = {};
      });
      await page.goto(fx.url('/plain'));
      await settle(page);
      expect(fx.events).toHaveLength(0);
    });

    test('navigator.webdriver 被覆盖为 false 后正常发送', async ({ page, fx, context }) => {
      await context.addInitScript(() => {
        Object.defineProperty(navigator, 'webdriver', { get: () => false, configurable: true });
      });
      await page.goto(fx.url('/plain'));
      await waitForEvents(fx, 1);
      await settle(page);
      expect(fx.posts()).toHaveLength(1);
    });
  });

  test('addInitScript 设 window.__rivu = true 后发出 1 个 pageview', async ({ page, fx }) => {
    expect(await (async () => (await page.goto(fx.url('/plain', { mode: 'none' })), page.evaluate(() => (window as W).__rivu)))()).toBe(true);
    await page.goto(fx.url('/plain'));
    await waitForEvents(fx, 1);
    await settle(page);
    expect(fx.posts()).toHaveLength(1);
  });

  test('localStorage.rivu_ignore = "true"：0 个请求，且脚本不改写该键', async ({ page, fx }) => {
    await page.goto(fx.url('/plain', { ignore: true }));
    await settle(page);
    expect(fx.events).toHaveLength(0);
    expect(await page.evaluate(() => localStorage.getItem('rivu_ignore'))).toBe('true');
    // SPA 跳转同样被忽略。
    await page.goto(fx.url('/spa/a', { ignore: true }));
    await page.evaluate(() => history.pushState({}, '', '/spa/b'));
    await settle(page);
    expect(fx.events).toHaveLength(0);
  });

  test('rivu_ignore 为 "true" 以外的值不排除', async ({ page, fx, context }) => {
    await context.addInitScript(() => {
      try {
        localStorage.setItem('rivu_ignore', 'false');
      } catch {
        /* 无存储时忽略 */
      }
    });
    await page.goto(fx.url('/plain'));
    await waitForEvents(fx, 1);
  });

  test('读取 localStorage 抛异常（defineProperty）：视为未设置，照常发送', async ({ page, fx, context }) => {
    await context.addInitScript(() => {
      Object.defineProperty(window, 'localStorage', {
        configurable: true,
        get() {
          throw new DOMException('denied', 'SecurityError');
        },
      });
    });
    await page.goto(fx.url('/plain'));
    await waitForEvents(fx, 1);
    await settle(page);
    expect(fx.posts()).toHaveLength(1);
  });

  test('localhost / 127.x 默认不采集；local:true（脚本前缀或 data-local）才采集', async ({ page, fx }) => {
    const port = new URL(fx.siteOrigin).port;
    // 前缀配置 local:false，主机名 127.0.0.1 与 localhost 都被排除。
    await page.goto(fx.url('/plain', { cfg: { local: false } }));
    await settle(page);
    await page.goto(fx.url('/plain', { cfg: { local: false } }, `http://localhost:${port}`));
    await settle(page);
    // data-* 配置：不写 data-local 同样被排除。
    await page.goto(fx.url('/plain', { mode: 'data', cfg: { local: false } }));
    await settle(page);
    expect(fx.events).toHaveLength(0);

    // 同样的 localhost 主机名，开 local 后采集。
    await page.goto(fx.url('/plain', { cfg: { local: true } }, `http://localhost:${port}`));
    await waitForEvents(fx, 1);
    expect(String(bodies(fx)[0]!.u).startsWith(`http://localhost:${port}/plain`)).toBe(true);
  });

  test('缺 site：脚本什么也不做，不抛异常', async ({ page, fx }) => {
    await page.goto(fx.url('/plain', { cfg: { site: '' } }));
    await settle(page);
    expect(fx.events).toHaveLength(0);
  });
});

test.describe('首个 pageview 与请求体 [§4.2、§5.1]', () => {
  test('直接打开：恰好 1 个 pageview，键集合与键序固定，没有 r', async ({ page, fx }) => {
    await page.goto(fx.url('/plain'));
    await waitForEvents(fx, 1);
    await settle(page);
    expect(fx.posts()).toHaveLength(1);
    const b = bodies(fx)[0]!;
    expect(Object.keys(b)).toEqual(['v', 'sv', 's', 'n', 'u']);
    expect(b).toEqual({ v: 1, sv: BUILD_SV, s: SITE_KEY, n: 'pageview', u: page.url() });
  });

  test('第一个 pageview 带 r（document.referrer）；SPA 跳转不带', async ({ page, fx }) => {
    await page.goto(fx.url('/plain'));
    const plainUrl = page.url();
    await waitForEvents(fx, 1);
    await page.click('#to-spa');
    await page.waitForURL(/\/spa\/a/);
    await waitForEvents(fx, 2);

    // 新文档的第一个 pageview：r 为上一页的完整 URL（同源，默认 Referrer Policy 给全 URL）。
    expect(bodies(fx)[1]).toMatchObject({ n: 'pageview', r: plainUrl });
    expect(bodies(fx)[1]!.u).toBe(page.url());

    await page.evaluate(() => history.pushState({}, '', '/spa/b'));
    await waitForEvents(fx, 3);
    await settle(page);
    expect(fx.posts()).toHaveLength(3);
    expect(Object.keys(bodies(fx)[2]!)).toEqual(['v', 'sv', 's', 'n', 'u']); // 没有 r
    expect(String(bodies(fx)[2]!.u)).toMatch(/\/spa\/b$/);
  });

  test('goto 的 referer 选项：r 以该来源开头；空 referrer 不发 r 键', async ({ page, fx }) => {
    await page.goto(fx.url('/plain'), { referer: 'https://ref.example.org/post/42?id=7' });
    await waitForEvents(fx, 1);
    const r = bodies(fx)[0]!.r;
    // 跨站来源在默认 Referrer Policy 下可能被浏览器裁剪为 origin，所以只断言来源站点。
    expect(typeof r).toBe('string');
    expect(String(r).startsWith('https://ref.example.org/')).toBe(true);
  });

  test('u 取发送瞬间的 location.href（脚本执行前页面已 replaceState）', async ({ page, fx }) => {
    await page.goto(fx.url('/plain', { replaceUrl: '/replaced?x=1#h' }));
    await waitForEvents(fx, 1);
    expect(bodies(fx)[0]!.u).toBe(`${fx.siteOrigin}/replaced?x=1#h`);
  });

  test('配置优先级：rivu.c 压过 data-*；缺省端点取脚本 src 的 origin', async ({ page, fx }) => {
    // data-site 是 SITE_KEY，内联 rivu.c 给了另一个 site；endpoint 两边都没给 → 取 t.js 所在 origin（rivu 服务器）。
    await page.goto(fx.url('/plain', { mode: 'data-default', inlineC: { site: 'zzzz2222aaaa' } }));
    await waitForEvents(fx, 1);
    expect(bodies(fx)[0]!.s).toBe('zzzz2222aaaa');
    expect(fx.posts()[0]!.server).toBe('rivu');
  });

  test('t.js 形态：data-site / data-api / data-local 配置', async ({ page, fx }) => {
    await page.goto(fx.url('/plain', { mode: 'data', endpoint: 'site' }));
    await waitForEvents(fx, 1);
    expect(bodies(fx)[0]!.s).toBe(SITE_KEY);
    expect(fx.posts()[0]!.server).toBe('site'); // data-api 指向站点第一方路径
  });
});

test.describe('可见性与预渲染 [§4.2]', () => {
  test('hidden 页面：切到 visible 前 0 请求，之后恰好 1 个 pageview（带 r，u 为发送瞬间的 URL）', async ({ page, fx, context }) => {
    await emulateVisibility(context, 'hidden');
    await page.goto(fx.url('/plain'), { referer: fx.siteOrigin + '/other' });
    await settle(page);
    expect(fx.events).toHaveLength(0);

    await page.evaluate(() => history.replaceState(null, '', '/plain?later=1')); // replaceState 不算 pageview
    await settle(page, 100);
    expect(fx.events).toHaveLength(0);

    await page.evaluate(() => (window as W).__setVisibility('visible'));
    await waitForEvents(fx, 1);
    await settle(page);
    expect(fx.posts()).toHaveLength(1);
    const b = bodies(fx)[0]!;
    expect(b.u).toBe(`${fx.siteOrigin}/plain?later=1`);
    expect(String(b.r)).toContain(`${fx.siteOrigin}/other`);

    // 再来回切换可见性：已发过 pageview，不再发。
    await page.evaluate(() => (window as W).__setVisibility('hidden'));
    await page.evaluate(() => (window as W).__setVisibility('visible'));
    await settle(page);
    expect(fx.posts()).toHaveLength(1);
  });

  test('visibilityState 为旧式的 "prerender"：同样等到 visible', async ({ page, fx, context }) => {
    await emulateVisibility(context, 'prerender');
    await page.goto(fx.url('/plain'));
    await settle(page);
    expect(fx.events).toHaveLength(0);
    await page.evaluate(() => (window as W).__setVisibility('visible'));
    await waitForEvents(fx, 1);
    await settle(page);
    expect(fx.posts()).toHaveLength(1);
  });

  for (const order of ['仅 prerenderingchange', 'visibilitychange 先于 prerenderingchange', 'prerenderingchange 先于 visibilitychange'] as const) {
    // 模拟 document.prerendering：预渲染期间 visibilityState 为 hidden；激活时 prerendering 变 false。
    // 真实预渲染只在 Chromium 可测（见下一个用例），这里的模拟在三个内核上验证脚本的事件处理。
    test(`prerendering 路径（模拟）：${order}，激活前 0 请求，激活后恰好 1 个`, async ({ page, fx, context }) => {
      await context.addInitScript(() => {
        let prerendering = true;
        let vis = 'hidden';
        Object.defineProperty(document, 'prerendering', { configurable: true, get: () => prerendering });
        Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => vis });
        Object.defineProperty(document, 'hidden', { configurable: true, get: () => vis !== 'visible' });
        (window as W).__activate = (order: string) => {
          prerendering = false;
          vis = 'visible';
          const vc = () => document.dispatchEvent(new Event('visibilitychange'));
          const pc = () => document.dispatchEvent(new Event('prerenderingchange'));
          if (order === 'vc-first') (vc(), pc());
          else if (order === 'pc-first') (pc(), vc());
          else pc();
        };
      });
      await page.goto(fx.url('/plain'));
      await settle(page);
      expect(fx.events).toHaveLength(0);
      // 预渲染期间的 visibilitychange（仍是 hidden / prerendering）也不应触发。
      await page.evaluate(() => document.dispatchEvent(new Event('visibilitychange')));
      await settle(page, 100);
      expect(fx.events).toHaveLength(0);

      const arg = order === '仅 prerenderingchange' ? 'pc-only' : order.startsWith('visibilitychange') ? 'vc-first' : 'pc-first';
      await page.evaluate((a) => (window as W).__activate(a), arg);
      await waitForEvents(fx, 1);
      await settle(page);
      expect(fx.posts()).toHaveLength(1);
    });
  }

  test('真实预渲染（Speculation Rules）：预渲染期间 0 请求，激活后恰好 1 个 pageview', async ({ page, fx, browserName }) => {
    // 只有 Chromium 实现 Speculation Rules 预渲染；Firefox/WebKit 没有 document.prerendering 的真实触发路径，
    // 它们的事件处理由上面的模拟用例覆盖。
    //
    // 实测（Playwright 1.64、Chromium 156、Debian 13）：无论 chromium-headless-shell 还是 `channel: 'chromium'`
    // 的 new headless，加 `--enable-features=Prerender2` 也一样，被 Playwright/CDP 驱动的页面都不会真正预渲染
    // （导航后 `activationStart` 恒为 0），所以这个用例目前会在运行期被跳过。保留它是为了在能触发预渲染的环境
    // （例如有头 Chrome、或以后的 Playwright）里自动生效，而不是让这条路径长期没有真机覆盖。
    test.skip(browserName !== 'chromium', 'Speculation Rules 预渲染仅 Chromium 支持');
    await page.goto(fx.url('/prerender-a'));
    await waitForEvents(fx, 1);
    await page.waitForTimeout(1500); // 给预渲染留时间
    // 预渲染的 /prerender-b 此刻不得发过任何事件（只有 prerender-a 自己的 pageview）。
    expect(fx.posts().map((e) => String(e.json!.u)).filter((u) => u.includes('/prerender-b'))).toEqual([]);
    const countBefore = fx.posts().length;

    await page.click('#go');
    await page.waitForURL(/\/prerender-b/);
    const activationStart = await page.evaluate(() => (performance.getEntriesByType('navigation')[0] as PerformanceNavigationTiming & { activationStart?: number }).activationStart ?? 0);
    test.skip(activationStart === 0, '本次运行中浏览器没有真正预渲染（可能被资源或策略限制）');

    await waitForEvents(fx, countBefore + 1);
    await settle(page);
    const forB = fx.posts().filter((e) => String(e.json!.u).includes('/prerender-b'));
    expect(forB).toHaveLength(1);
  });
});

test.describe('SPA 路由（history 模式）[§4.3]', () => {
  test('pushState 新 pathname → 1；仅改 query/hash → 0；replaceState → 0；popstate 回到不同 pathname → 1', async ({ page, fx }) => {
    await page.goto(fx.url('/spa/a'));
    await waitForEvents(fx, 1);
    const start = page.url();
    const nav = async (js: () => void, expected: number) => {
      await page.evaluate(js);
      await settle(page, 150);
      expect(fx.posts()).toHaveLength(expected);
    };

    await nav(() => history.pushState({}, '', '/spa/b'), 2); //                    新 pathname → 发
    await nav(() => history.pushState({}, '', '/spa/b?x=1'), 2); //                仅 query → 不发
    await nav(() => history.pushState({}, '', '/spa/b?x=1#h'), 2); //              仅 hash → 不发
    await nav(() => history.replaceState({}, '', '/spa/zzz'), 2); //               replaceState → 不发（也不更新 lastPath）
    await nav(() => history.back(), 2); //   → /spa/b?x=1，pathname 与上一个已发的相同 → 不发
    await nav(() => history.back(), 2); //   → /spa/b → 不发
    await nav(() => history.back(), 3); //   → 起始页（pathname 不同）→ 发
    await nav(() => history.forward(), 4); // → /spa/b → 发

    const us = bodies(fx).map((b) => String(b.u));
    expect(us).toEqual([start, `${fx.siteOrigin}/spa/b`, start, `${fx.siteOrigin}/spa/b`]);
    // 除文档第一个 pageview 外，SPA 导航与 popstate 的 pageview 都没有 r。
    for (const b of bodies(fx)) expect('r' in b).toBe(false);
  });

  test('pushState 包装透明：返回值、this、参数、状态与原函数一致；原函数的异常照常抛给页面', async ({ page, fx }) => {
    await page.goto(fx.url('/spa/a'));
    await waitForEvents(fx, 1);
    const r = await page.evaluate(() => {
      const out: Record<string, unknown> = {};
      out.ret = history.pushState({ n: 1 }, '', '/spa/q');
      out.state = history.state;
      out.path = location.pathname;
      // this 不是 History：原生实现抛 TypeError，包装也应如此（并且不发事件）。
      try {
        history.pushState.call({}, {}, '', '/spa/nope');
      } catch (e) {
        out.illegalThis = (e as Error).name;
      }
      // 跨源 URL：原生抛 SecurityError。
      try {
        history.pushState({}, '', 'https://other.example/');
      } catch (e) {
        out.crossOrigin = (e as Error).name;
      }
      out.replaceStateNative = /\[native code\]/.test(Function.prototype.toString.call(history.replaceState));
      return out;
    });
    expect(r.ret).toBeUndefined();
    expect(r.state).toEqual({ n: 1 });
    expect(r.path).toBe('/spa/q');
    expect(r.illegalThis).toBe('TypeError');
    expect(r.crossOrigin).toBe('SecurityError');
    expect(r.replaceStateNative).toBe(true);
    await waitForEvents(fx, 2); // 起始页 + /spa/q
    await settle(page);
    expect(fx.posts()).toHaveLength(2);
  });
});

test.describe('hash 模式（h=1）[§4.3]', () => {
  test('每次 hashchange 发 1 个 pageview，均带 h:1，不做去重；不包装 pushState', async ({ page, fx }) => {
    await page.goto(fx.url('/hash', { cfg: { hash: true } }));
    await waitForEvents(fx, 1);
    const setHash = async (h: string, expected: number) => {
      await page.evaluate((v) => {
        location.hash = v;
      }, h);
      await waitForEvents(fx, expected);
      await settle(page, 150);
      expect(fx.posts()).toHaveLength(expected);
    };
    await setHash('#/a', 2);
    await setHash('#/b', 3);
    await setHash('#/a', 4); // 回到之前的 hash：不去重

    expect(bodies(fx).map((b) => b.h)).toEqual([1, 1, 1, 1]);
    expect(bodies(fx).map((b) => String(b.u).split('#')[1])).toEqual([undefined, '/a', '/b', '/a']);

    // 不包装 pushState：pathname 变化不产生事件，且函数仍是原生的。
    const native = await page.evaluate(() => {
      history.pushState({}, '', '/hash/other');
      return /\[native code\]/.test(Function.prototype.toString.call(history.pushState));
    });
    expect(native).toBe(true);
    await settle(page);
    expect(fx.posts()).toHaveLength(4);
  });

  test('data-hash="true"（t.js 形态）同样生效', async ({ page, fx }) => {
    await page.goto(fx.url('/hash', { mode: 'data', cfg: { hash: true } }));
    await waitForEvents(fx, 1);
    await page.evaluate(() => {
      location.hash = '#/x';
    });
    await waitForEvents(fx, 2);
    expect(bodies(fx).map((b) => b.h)).toEqual([1, 1]);
  });

  test('History 模式下 hash 变化不发（pathname 相同）', async ({ page, fx }) => {
    await page.goto(fx.url('/spa/a'));
    await waitForEvents(fx, 1);
    await page.evaluate(() => {
      location.hash = '#section';
    });
    await settle(page);
    expect(fx.posts()).toHaveLength(1);
    expect('h' in bodies(fx)[0]!).toBe(false);
  });
});

test.describe('bfcache [§4.4]', () => {
  test('pageshow persisted（模拟）：新 pageview，无 r，不做 pathname 去重；persisted=false 不发', async ({ page, fx }) => {
    await page.goto(fx.url('/plain'), { referer: fx.siteOrigin + '/other' });
    await waitForEvents(fx, 1);
    expect('r' in bodies(fx)[0]!).toBe(true);

    await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: false })));
    await settle(page, 150);
    expect(fx.posts()).toHaveLength(1);

    await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })));
    await waitForEvents(fx, 2);
    await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })));
    await waitForEvents(fx, 3);
    await settle(page);
    expect(fx.posts()).toHaveLength(3);
    expect(bodies(fx)[1]!.u).toBe(bodies(fx)[0]!.u); // 同一 URL 也发
    expect('r' in bodies(fx)[1]!).toBe(false);
    expect('r' in bodies(fx)[2]!).toBe(false);
  });

  test('从未可见的页面被 bfcache 恢复：这个 pageview 也不带 r（r 只属于首次加载）', async ({ page, fx, context }) => {
    await emulateVisibility(context, 'hidden');
    await page.goto(fx.url('/plain'), { referer: fx.siteOrigin + '/other' });
    await settle(page);
    expect(fx.events).toHaveLength(0);
    await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })));
    await waitForEvents(fx, 1);
    await settle(page);
    expect(fx.posts()).toHaveLength(1);
    expect('r' in bodies(fx)[0]!).toBe(false);
  });

  test('真实 bfcache：导航离开再 back，persisted 恢复产生新 pageview 且无 r', async ({ page, fx, browserName }) => {
    await page.goto(fx.url('/bfcache-a'));
    await waitForEvents(fx, 1);
    const aUrl = page.url();
    await page.click('#away');
    await page.waitForURL(/\/bfcache-b/);
    await waitForEvents(fx, 2);
    await page.goBack();
    await page.waitForURL(/\/bfcache-a/);
    // 页面里的 JS 状态还在 = 真的从 bfcache 恢复；否则是重新加载。
    const restored = await page.evaluate(() => (window as W).__marker === 'alive');
    test.skip(
      !restored,
      `${browserName}：本次运行没有发生 bfcache 恢复（Playwright 的 Firefox/WebKit 构建默认关闭 bfcache，或页面不满足资格）；` +
        '由上一个用例（模拟 pageshow persisted）覆盖脚本逻辑',
    );
    await waitForEvents(fx, 3);
    await settle(page);
    expect(fx.posts()).toHaveLength(3);
    expect(bodies(fx)[2]).toMatchObject({ n: 'pageview', u: aUrl });
    expect('r' in bodies(fx)[2]!).toBe(false);
  });

  test('普通 back（不论是否命中 bfcache）：恰好多出 1 个 pageview', async ({ page, fx }) => {
    // 命中 bfcache 走 pageshow(persisted) 路径；没命中则是全新的文档加载，走「首个 pageview」路径。
    await page.goto(fx.url('/plain'));
    await page.click('#to-other');
    await page.waitForURL(/\/other/);
    await waitForEvents(fx, 2);
    await page.goBack();
    await page.waitForURL(/\/plain/);
    await waitForEvents(fx, 3);
    await settle(page);
    // 无论是 bfcache 恢复（无 r）还是重新加载（referrer 取决于浏览器），都恰好多出 1 个 pageview。
    expect(fx.posts()).toHaveLength(3);
  });
});

test.describe('请求形态 [§3.2、§6.1、§11.2]', () => {
  test('POST、text/plain、跨域、无预检、体 < 8 KB、fetch keepalive、不带 Cookie（即使 Cookie 罐里有）', async ({ page, fx, context }) => {
    // 两个 origin 共享 127.0.0.1 这个 cookie 主机（Cookie 不分端口），
    // 若请求带了凭证，采集端一定能收到这些 Cookie。
    await context.addCookies([
      { name: 'site_cookie', value: '1', url: fx.siteOrigin },
      { name: 'rivu_cookie', value: '1', url: fx.rivuOrigin },
    ]);
    await context.addInitScript(() => {
      const w = window as W;
      w.__fetchCalls = [];
      w.__beaconCalls = 0;
      const origFetch = window.fetch;
      window.fetch = function (this: unknown, input: RequestInfo | URL, init?: RequestInit) {
        w.__fetchCalls.push({ url: String(input), method: init?.method, keepalive: init?.keepalive, credentials: init?.credentials });
        return origFetch.apply(window, [input, init] as Parameters<typeof fetch>);
      };
      const origBeacon = navigator.sendBeacon?.bind(navigator);
      if (origBeacon) {
        navigator.sendBeacon = (...a: Parameters<typeof origBeacon>) => {
          w.__beaconCalls++;
          return origBeacon(...a);
        };
      }
    });
    const requests: { method: string; type: string; url: string }[] = [];
    page.on('request', (r) => {
      if (r.url().includes('/api/event')) requests.push({ method: r.method(), type: r.resourceType(), url: r.url() });
    });

    await page.goto(fx.url('/plain'));
    await waitForEvents(fx, 1);
    await settle(page);

    // 罐里确实有 Cookie（否则「没带」没有意义）。
    expect(await page.evaluate(() => document.cookie)).toContain('site_cookie=1');

    expect(fx.events).toHaveLength(1);
    const e = fx.events[0]!;
    expect(e.server).toBe('rivu');
    expect(e.method).toBe('POST');
    expect(e.contentType).toMatch(/^text\/plain/i);
    expect(e.cookie).toBeUndefined();
    expect(e.origin).toBe(fx.siteOrigin); // 跨域请求
    expect(e.bytes).toBeLessThan(8192);
    expect(fx.events.filter((x) => x.method === 'OPTIONS')).toEqual([]); // 简单请求，无预检
    expect(requests).toEqual([{ method: 'POST', type: 'fetch', url: `${fx.rivuOrigin}/api/event` }]);

    const calls = await page.evaluate(() => (window as W).__fetchCalls);
    expect(calls).toEqual([{ url: `${fx.rivuOrigin}/api/event`, method: 'POST', keepalive: true, credentials: 'omit' }]);
    expect(await page.evaluate(() => (window as W).__beaconCalls)).toBe(0); // core 不用 sendBeacon
  });

  test('第一方反代路径（同源端点）：credentials 为 omit，同源 Cookie 也不带', async ({ page, fx, context }) => {
    await context.addCookies([{ name: 'first_party', value: '1', url: fx.siteOrigin }]);
    await page.goto(fx.url('/plain', { endpoint: 'site' }));
    await waitForEvents(fx, 1);
    await settle(page);
    expect(await page.evaluate(() => document.cookie)).toContain('first_party=1');
    expect(fx.events).toHaveLength(1);
    expect(fx.events[0]!.server).toBe('site');
    expect(fx.events[0]!.cookie).toBeUndefined();
  });
});

test.describe('命令队列、重复加载、异常 [§2.5、§4.1]', () => {
  test('队列桩：core 不回放命令（无自定义事件、callback 不触发、init 不生效）；加载后 window.rivu 是空函数', async ({ page, fx }) => {
    await page.goto(fx.url('/plain', { stub: true }));
    await waitForEvents(fx, 1);
    await settle(page);
    // 只有自动 pageview；`rivu('event','Signup')`、`rivu('set')`、`rivu('init',{hash:true})` 都没有效果。
    expect(fx.posts()).toHaveLength(1);
    expect(Object.keys(bodies(fx)[0]!)).toEqual(['v', 'sv', 's', 'n', 'u']);

    const r = await page.evaluate(() => {
      const w = window as W;
      const out: Record<string, unknown> = {
        // 探测字段（规范 §2.5）
        b: w.rivu.b,
        v: w.rivu.v,
        sv: w.rivu.sv,
        l: w.rivu.l,
        queueGone: w.rivu.q === undefined,
        cbBefore: w.__cb,
      };
      let calls = 0;
      try {
        out.ret = [w.rivu('event', 'X', { callback: () => calls++ }), w.rivu('pageview'), w.rivu('set', { props: { a: 1 } }), w.rivu('nonsense')];
      } catch (e) {
        out.threw = String(e);
      }
      out.callbackCalls = calls;
      out.queueAfter = w.rivu.q === undefined;
      return out;
    });
    expect(r).toMatchObject({ b: 'core', v: 1, sv: BUILD_SV, l: 1, queueGone: true, cbBefore: 0, callbackCalls: 0, queueAfter: true });
    expect(r.threw).toBeUndefined();
    await settle(page);
    expect(fx.posts()).toHaveLength(1); // 加载后调用也不产生请求
  });

  test('重复加载（同一脚本引入两次）：只初始化一次', async ({ page, fx }) => {
    await page.goto(fx.url('/spa/a', { twice: true }));
    await waitForEvents(fx, 1);
    await settle(page);
    expect(fx.posts()).toHaveLength(1);
    // 监听器若注册了两遍，一次 pushState 会发两个。
    await page.evaluate(() => history.pushState({}, '', '/spa/b'));
    await waitForEvents(fx, 2);
    await settle(page);
    expect(fx.posts()).toHaveLength(2);
    await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })));
    await waitForEvents(fx, 3);
    await settle(page);
    expect(fx.posts()).toHaveLength(3);
  });

  test('初始化中途失败也算已初始化（先置位）：第二次加载不会重复注册', async ({ page, fx, context }) => {
    // 让 `history.pushState = ...` 在 strict mode 下抛 TypeError，模拟初始化在路由监听处失败。
    await context.addInitScript(() => {
      Object.defineProperty(History.prototype, 'pushState', { configurable: true, writable: false, value: History.prototype.pushState });
    });
    await page.goto(fx.url('/spa/a', { twice: true }));
    await waitForEvents(fx, 1);
    await settle(page);
    expect(fx.posts()).toHaveLength(1); // 首个 pageview 在失败之前已发出；第二份脚本直接返回
  });

  test('fetch 不存在：不发送、不抛异常，页面的 pushState 照常工作', async ({ page, fx, context }) => {
    await context.addInitScript(() => {
      Object.defineProperty(window, 'fetch', { value: undefined, configurable: true, writable: true });
    });
    await page.goto(fx.url('/spa/a'));
    await settle(page);
    const ok = await page.evaluate(() => {
      history.pushState({ ok: 1 }, '', '/spa/b');
      return history.state?.ok === 1 && location.pathname === '/spa/b';
    });
    expect(ok).toBe(true);
    await settle(page, 100);
    expect(fx.events).toHaveLength(0);
  });

  test('fetch 同步抛异常：吞掉，不影响页面', async ({ page, fx, context }) => {
    await context.addInitScript(() => {
      window.fetch = () => {
        throw new Error('boom');
      };
    });
    await page.goto(fx.url('/spa/a'));
    const ok = await page.evaluate(() => {
      history.pushState({ ok: 1 }, '', '/spa/b');
      return location.pathname === '/spa/b';
    });
    expect(ok).toBe(true);
    await settle(page, 100);
    expect(fx.events).toHaveLength(0);
  });

  test('采集端不可达（fetch 被拒绝）：没有未处理的 Promise 拒绝', async ({ page, fx, context }) => {
    // wireCheck 会在用例结束时断言没有 pageerror；这里再用 unhandledrejection 事件兜底（各内核对 pageerror 的覆盖不同）。
    await context.addInitScript(() => {
      (window as W).__unhandled = [];
      window.addEventListener('unhandledrejection', (e) => (window as W).__unhandled.push(String(e.reason)));
    });
    await page.goto(fx.url('/spa/a', { endpoint: 'dead' }));
    await page.evaluate(() => history.pushState({}, '', '/spa/b'));
    await settle(page, 500);
    expect(await page.evaluate(() => (window as W).__unhandled)).toEqual([]);
    expect(fx.events).toHaveLength(0);
  });

  // 以下是 full 才有的行为（规范 §4.8、§4.5、§4.6），core 没有，所以只登记、不实现，避免被误以为漏测。
  test.fixme('full：rivu(event) 的 callback 恰好调用一次（成功 {status}、失败 {error}、beacon {}、被忽略无参）', () => {});
  test.fixme('full：init/set 先于首个 pageview 回放；set 与单次 props 合并；加载后 rivu("init") 无效', () => {});
  test.fixme('full：engagement（<3 s 不发、≥3 s 发且 e 为增量、更深滚动发、SPA 跳转前先 flush、beacon 优先并回退 fetch）', () => {});
  test.fixme('full：外链 / 下载 / 表单 / tagged 自动捕获', () => {});
});

test.describe('无副作用 [§1.1]', () => {
  test('全流程不写 Cookie、localStorage、sessionStorage、IndexedDB、Cache；只读 localStorage.rivu_ignore', async ({ page, fx, context }) => {
    await context.addInitScript(() => {
      const w = window as W;
      w.__writes = [] as string[];
      w.__lsReads = [] as string[];
      const note = (s: string) => w.__writes.push(s);

      // 所有写存储的入口都记一笔。
      for (const m of ['setItem', 'removeItem', 'clear'] as const) {
        const orig = Storage.prototype[m] as (...a: unknown[]) => unknown;
        Storage.prototype[m] = function (this: Storage, ...a: unknown[]) {
          note(`Storage.${m}(${a.join(',')})`);
          return orig.apply(this, a);
        } as never;
      }
      const cookie = Object.getOwnPropertyDescriptor(Document.prototype, 'cookie');
      if (cookie?.set) {
        Object.defineProperty(document, 'cookie', {
          configurable: true,
          get: () => cookie.get!.call(document),
          set: (v: string) => {
            note(`cookie=${v}`);
            cookie.set!.call(document, v);
          },
        });
      }
      for (const m of ['open', 'deleteDatabase'] as const) {
        const orig = IDBFactory.prototype[m] as (...a: unknown[]) => unknown;
        IDBFactory.prototype[m] = function (this: IDBFactory, ...a: unknown[]) {
          note(`indexedDB.${m}`);
          return orig.apply(this, a);
        } as never;
      }
      if ('caches' in window) {
        const orig = CacheStorage.prototype.open;
        CacheStorage.prototype.open = function (this: CacheStorage, n: string) {
          note(`caches.open(${n})`);
          return orig.call(this, n);
        };
      }
      // localStorage 包一层代理，记录被读取的属性名。
      // （脚本只允许读 rivu_ignore；方法名如 getItem 也会被记录，所以这里记的是所有字符串键。）
      const real = window.localStorage;
      const proxy = new Proxy(real, {
        get(t, k) {
          if (typeof k === 'string') w.__lsReads.push(k);
          const v = Reflect.get(t, k, t);
          return typeof v === 'function' ? v.bind(t) : v;
        },
      });
      Object.defineProperty(window, 'localStorage', { configurable: true, get: () => proxy });
    });

    // 跑一遍：首个 pageview、SPA、可见性切换、bfcache 恢复。
    await page.goto(fx.url('/spa/a'));
    await waitForEvents(fx, 1);
    await page.evaluate(() => history.pushState({}, '', '/spa/b'));
    await page.evaluate(() => history.back());
    await page.evaluate(() => window.dispatchEvent(new PageTransitionEvent('pageshow', { persisted: true })));
    await page.evaluate(() => (window as W).__setVisibility?.('hidden'));
    await waitForEvents(fx, 4);
    await settle(page);

    const r = await page.evaluate(async () => {
      const w = window as W;
      const dbs = typeof indexedDB.databases === 'function' ? await indexedDB.databases() : [];
      const cacheKeys = 'caches' in window ? await caches.keys() : [];
      return {
        cookie: document.cookie,
        writes: w.__writes as string[],
        lsReads: [...new Set<string>(w.__lsReads)],
        sessionLength: sessionStorage.length,
        dbs: dbs.length,
        cacheKeys,
        windowName: window.name,
      };
    });
    expect(r.writes).toEqual([]); // 没有任何写存储 / Cookie / IndexedDB / Cache 的调用
    expect(r.cookie).toBe('');
    expect(r.sessionLength).toBe(0);
    expect(r.dbs).toBe(0);
    expect(r.cacheKeys).toEqual([]);
    expect(r.windowName).toBe('');
    // localStorage 只读了 rivu_ignore 这一个属性（并且确实读了）。
    expect(r.lsReads).toEqual(['rivu_ignore']);

    // Playwright 视角的存储状态：没有 Cookie，没有 localStorage 条目，没有 IndexedDB。
    const state = await context.storageState({ indexedDB: true });
    expect(state.cookies).toEqual([]);
    expect(state.origins).toEqual([]);
  });

  test('IIFE 不泄漏全局变量：与不加载脚本的页面相比，window 只多一个 rivu', async ({ page, fx }) => {
    const globals = async (mode: 'none' | 'prefix') => {
      await page.goto(fx.url('/plain', { mode }));
      await settle(page, 100);
      return page.evaluate(() => Object.getOwnPropertyNames(window));
    };
    const without = new Set(await globals('none'));
    const withTracker = await globals('prefix');
    expect(withTracker.filter((k) => !without.has(k))).toEqual(['rivu']);
  });
});
