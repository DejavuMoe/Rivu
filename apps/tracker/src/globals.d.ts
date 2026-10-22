// 构建期常量由 esbuild 的 `define` 注入，见 scripts/lib.mjs。除这两个之外不得新增编译期开关（规范 §3.1）。

/** `true` 为 full 构建，`false` 为 core 构建。core 构建里所有 `if (__FULL__)` 分支都会被整体删除。 */
declare const __FULL__: boolean;
/** 构建号：单调递增的整数，随事件以 `sv` 上报，只作计数标签（规范 §8）。 */
declare const __SV__: number;

/** `window.rivu`：队列桩、配置前缀与探测字段的载体（规范 §2.3、§2.5）。 */
interface RivuGlobal {
  (...args: unknown[]): void;
  /** 已初始化标志；先于任何监听注册置位，保证重复加载只初始化一次。 */
  l?: 1;
  /** 队列桩压入的命令。core 不回放。 */
  q?: unknown[];
  /** 站点脚本的配置前缀（`GET /js/<site_key>.js` 的响应头部）。 */
  c?: import('./config.ts').Config;
  /** 探测字段：协议版本、构建号、构建名。 */
  v?: number;
  sv?: number;
  b?: 'core' | 'full';
}

interface Window {
  rivu?: RivuGlobal;
  /** 测试环境放行自动化检测（规范 §4.7）。 */
  __rivu?: unknown;
  Cypress?: unknown;
}
