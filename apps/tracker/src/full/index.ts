// full 变体独有的功能（规范 §3.2：beacon、engagement、links、forms、tagged、api、debug）。
// core 构建里所有 `if (__FULL__)` 分支被整体删除，本目录不会进入 core 产物（scripts/size.mjs 的「模块泄漏」检查断言这一点）。
//
// 初始化次序（规范 §2.5、§4.1）：合并 full 开关 → 回放 `init`、`set` → 注册监听 → （index.ts）首个 pageview 与路由
// → 回放其余命令 → `window.rivu` 换成真实实现。

import type { Config } from '../config.ts';
import { track } from '../pageview.ts';
import { apiInit, event, setProps, type Opts } from './api.ts';
import { captureInit } from './capture.ts';
import { resolveFull } from './config.ts';
import { engInit } from './engagement.ts';

type Args = ArrayLike<unknown>;

/** 合并 full 开关并回放 `init` / `set`（它们可能影响首个 pageview），返回其余待回放的命令。 */
export const fullConfig = (cfg: Config, R: RivuGlobal, d?: DOMStringMap | null): Args[] => {
  Object.assign(cfg, resolveFull(R.c, d));
  const later: Args[] = [];
  for (const a of (R.q || []) as Args[]) {
    if (a[0] == 'init') {
      // 可以覆盖开关与 endpoint，不能覆盖 site
      const site = cfg.site;
      Object.assign(cfg, a[1]);
      cfg.site = site;
    } else if (a[0] == 'set') setProps(a[1] as { props?: unknown });
    else later.push(a);
  }
  return later;
};

/** 注册自动捕获、engagement 监听，交出模块要用的配置。 */
export const fullStart = (cfg: Config): void => {
  apiInit(cfg);
  engInit(cfg);
  captureInit(cfg);
};

/** `window.rivu` 的真实实现。加载后的 `init` 无效。 */
export const rivuApi = (cmd?: unknown, a?: unknown, b?: unknown): void => {
  try {
    if (cmd == 'pageview' || (cmd == 'event' && a == 'pageview')) track(false, (cmd == 'pageview' ? a : b) as Opts);
    else if (cmd == 'event') event(a, b as Opts);
    else if (cmd == 'set') setProps(a as { props?: unknown });
  } catch {}
};
