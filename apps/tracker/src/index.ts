// 入口（规范 §4.1 初始化流程，幂等）。
//
// 变体：同一份源码，esbuild 以 `__FULL__` 产出 core / full 两个 IIFE。core 构建里
// 所有 `if (__FULL__)` 分支与只被它引用的模块都会被删除；CI 用 metafile 断言 core 不含 `src/full/`。

import { resolveConfig } from './config.ts';
import { fullConfig, fullStart, rivuApi } from './full/index.ts';
import { start } from './pageview.ts';
import { route } from './router.ts';

const boot = (): void => {
  try {
    // 步 1：已初始化则返回，否则立刻置位（先于任何监听注册）。
    const w = window;
    const R: RivuGlobal = (w.rivu = w.rivu || function () {});
    if (R.l) return;
    R.l = 1;

    // 步 2：解析配置。`document.currentScript` 只在脚本同步执行期间有效，必须此刻读。
    const s = document.currentScript as HTMLScriptElement | null;
    const d = s && s.dataset;
    const cfg = resolveConfig(R.c, d, s && s.src);
    // 步 3（full）：合并 full 开关，回放 init / set（可能影响首个 pageview）。
    const later = __FULL__ ? fullConfig(cfg, R, d) : [];
    if (!cfg.site || !cfg.endpoint) return; // 缺 site：什么也不做
    if (__FULL__) fullStart(cfg);

    // 步 4：首个 pageview 与路由监听（full 的 manual 模式两者都不做）。
    start(cfg);
    if (!(__FULL__ && cfg.manual)) route(cfg.hash);

    // 步 5：full 回放其余命令，window.rivu 换成真实实现；core 不含手动 API，队列不回放，
    // window.rivu 换成空函数（避免队列无限增长）。两者都写探测字段。
    if (__FULL__) {
      for (const a of later) rivuApi(a[0], a[1], a[2]);
      w.rivu = Object.assign(rivuApi, { l: 1 as const, v: 1, sv: __SV__, b: 'full' as const });
    } else w.rivu = Object.assign(function () {}, { l: 1 as const, v: 1, sv: __SV__, b: 'core' as const });
  } catch {}
};

boot();
