// 入口（规范 §4.1 初始化流程，幂等）。
//
// 变体：同一份源码，esbuild 以 `__FULL__` 产出 core / full 两个 IIFE。core 构建里
// 所有 `if (__FULL__)` 分支与只被它引用的模块都会被删除；CI 用 metafile 断言 core 不含 `src/full/`。
// 以后加入 full：实现 `src/full/index.ts` 的 `fullInit`，并在 scripts/lib.mjs 的 VARIANTS 里加上 full。

import { resolveConfig } from './config.ts';
import { fullInit } from './full/index.ts';
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
    const cfg = resolveConfig(R.c, s && s.dataset, s && s.src);
    if (!cfg.site || !cfg.endpoint) return; // 缺 site：什么也不做

    // 步 3：full 在这里回放 init/set 队列、注册自动捕获监听。
    if (__FULL__) fullInit(cfg, R);

    // 步 4：首个 pageview 与路由监听。
    start(cfg);
    route(cfg.hash);

    // 步 5：core 不含手动 API，队列不回放；window.rivu 换成空函数，避免队列无限增长，并写探测字段。
    w.rivu = Object.assign(function () {}, { l: 1 as const, v: 1, sv: __SV__, b: 'core' as const });
  } catch {}
};

boot();
