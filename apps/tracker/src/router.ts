// SPA 路由（规范 §4.3）。

import { lastPath, track, waitingForVisibility } from './pageview.ts';

/**
 * History 模式（默认）：包装 `pushState` 并监听 `popstate`。
 * - 先调用原函数，保留 `this`、参数与返回值；原函数抛的异常照常抛给页面（那是页面自己的行为）。
 * - 不包装 `replaceState`：它不算新 pageview；用它做真实跳转的应用须手动发 pageview。
 * - pathname 去重：与上一个已发 pageview 相同（仅 query 或 hash 变化）则不发。
 *
 * Hash 模式：只监听 `hashchange`，不包装 `pushState`，不去重；`h:1` 由 payload 统一加。
 */
export const route = (hash?: boolean): void => {
  if (hash) {
    addEventListener('hashchange', () => track());
    return;
  }
  const nav = () => {
    if (!waitingForVisibility && location.pathname != lastPath) track();
  };
  const h = history;
  const push = h.pushState;
  h.pushState = function (this: History, ...a: Parameters<History['pushState']>) {
    const r = push.apply(this, a);
    nav();
    return r;
  };
  addEventListener('popstate', nav);
};
