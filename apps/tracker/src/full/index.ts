// full 变体独有的功能（规范 §3.2：beacon、engagement、links、forms、tagged、api、debug）。
//
// 目前 full 尚未实现：本文件只是挂载点。core 构建里 `if (__FULL__) fullInit(...)` 被整体删除，
// 因此本模块不会进入 core 产物（scripts/size.mjs 的「模块泄漏」检查会断言这一点）。
// full 实现时：
//   - 在 `fullInit` 里回放 `init`/`set` 队列、注册 click/auxclick/submit 委托监听；
//   - 在 pageview.ts 的 `track()` 里插入 engagement flush/重置，并把 send.ts 换成 beacon 优先；
//   - 把最终的 `window.rivu` 换成真实命令实现（api 模块），而不是 core 的空函数。

import type { Config } from '../config.ts';

export const fullInit = (_cfg: Config, _rivu: RivuGlobal): void => {};
