// pageview 的发出时机（规范 §4.2、§4.4）。router 与 full 的手动 API 也经由 `track()` 发 pageview，
// 所以「发送前判定、`r` 只给文档第一个、更新 lastPath、先 flush 上一页的 engagement」都集中在这里。

import type { Config } from './config.ts';
import { emit, skipped, type Opts } from './full/api.ts';
import { engEnd } from './full/engagement.ts';
import { ignored } from './guard.ts';
import { buildPayload } from './payload.ts';
import { send } from './send.ts';

let cfg: Config;
/** 文档的第一个 pageview 还没发出去：只有它带 `r`。 */
let first = true;
/** 是否已发出过 pageview；用来保证「hidden/prerender 页面变可见」只补发一次。 */
let sent = false;
/** 上一个已发 pageview 的 `location.pathname`，供 router 去重（规范 §4.3）。 */
export let lastPath: string | undefined;

/**
 * 发一个 pageview。整个函数包在 try/catch 里：脚本任何异常都不得影响宿主页面（规范 §4.1）。
 * `u` 取发送瞬间的 `location.href`，不是加载时的（full 的手动 pageview 可以用 `url` 覆盖）。
 *
 * @param cached bfcache 恢复：不带 `r`
 * @param o      full 手动 API 的选项（`props`、`url`、`callback`）
 */
export const track = (cached?: boolean, o?: Opts): void => {
  try {
    // full：先对上一页累计并 flush engagement，再重置（规范 §4.3「次序」）；被忽略的页面不再有 engagement
    if (__FULL__) engEnd();
    const why = ignored(cfg);
    if (why) {
      if (__FULL__) skipped(why, o);
      return;
    }
    const ref = first && !cached ? document.referrer : '';
    first = false;
    sent = true;
    lastPath = location.pathname;
    if (__FULL__) emit('pageview', o, ref);
    else send(cfg.endpoint!, buildPayload(cfg.site!, __SV__, 'pageview', location.href, ref, cfg.hash));
  } catch {}
};

/** 首个 pageview 与 bfcache（规范 §4.2、§4.4）。full 的 `manual` 关闭自动 pageview（只保留配置供手动 API 使用）。 */
export const start = (c: Config): void => {
  cfg = c;
  if (__FULL__ && c.manual) return;
  const d = document;
  // `document.prerendering` 还不在 lib.dom 的类型里。
  const ready = () => d.visibilityState == 'visible' && !(d as { prerendering?: boolean }).prerendering;

  if (ready()) track();
  else {
    // hidden 或 prerender：首次变为可见才发。
    // `prerenderingchange` 是 Speculation Rules 预渲染的激活事件；预渲染期间 `visibilityState`
    // 为 hidden，仅靠 `visibilitychange` 可能漏报（规范 §4.2，Rivu 新增）。
    // 两个事件可能先后到达，`sent` 保证只发一次。
    const onReady = () => ready() && !sent && track();
    d.addEventListener('visibilitychange', onReady);
    d.addEventListener('prerenderingchange', onReady);
  }

  // bfcache 恢复视为新 pageview：不做 pathname 去重、不带 `r`，并重置 engagement。
  // 不使用 unload/beforeunload/pagehide（会使页面失去 bfcache 资格）。
  addEventListener('pageshow', (e) => e.persisted && track(true));
};
