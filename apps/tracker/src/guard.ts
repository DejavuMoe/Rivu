// 排除逻辑（规范 §4.7），每次发送前判定。被忽略时 core 静默；full 在 `debug` 打开时输出原因，
// 所以 full 构建返回原因字符串，core 构建返回 1（`__FULL__ ? '原因' : 1` 在 core 里被折叠掉，不占体积）。

import type { Config } from './config.ts';

/** `localhost`、`127.x.x.x`、`[::1]`（`location.hostname` 对 IPv6 带方括号），或 `file:` 协议。 */
export const isLocalHost = (hostname: string, protocol: string): boolean =>
  /^(localhost|127(\.\d+){3}|\[::1\])$/.test(hostname) || protocol == 'file:';

/** 是否应当忽略本次事件：真值是忽略的原因。`site` 缺失在启动时已经终止，不在这里判断。 */
export const ignored = (cfg: Config): string | number | false => {
  const w = window;
  if (!cfg.local && isLocalHost(location.hostname, location.protocol)) return __FULL__ ? 'localhost' : 1;
  // 自动化环境（Playwright、Cypress 等）默认不采集；测试页设 `window.__rivu = true` 放行。
  if ((navigator.webdriver || w.Cypress) && !w.__rivu) return __FULL__ ? 'automation' : 1;
  // 自助排除：站点所有者在自己的浏览器控制台手动设置。脚本只读该键，
  // 从不写入、不删除、不用它标识访客；读取抛异常（隐私模式、存储被禁用）视为未设置。
  try {
    return localStorage.rivu_ignore === 'true' && (__FULL__ ? 'rivu_ignore' : 1);
  } catch {
    return false;
  }
};
