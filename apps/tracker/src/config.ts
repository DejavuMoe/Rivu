// 配置解析（规范 §2.2、§4.1 步 2）。纯函数，便于单元测试。

/** core 用到的配置键。full 的其余开关（engagement、outbound ……）以后在这里追加。 */
export interface Config {
  /** `site_key`；缺省则脚本什么也不做。 */
  site?: string;
  /** 采集端点；缺省取脚本 `src` 的 origin + `/api/event`。 */
  endpoint?: string;
  /** hash 路由模式。 */
  hash?: boolean;
  /** 允许 localhost / 127.x / file: 采集。 */
  local?: boolean;
}

/**
 * 优先级：`rivu.c`（站点脚本前缀）> `data-*`（`t.js` 的 `document.currentScript`）> 默认值。
 * 布尔型的 `data-*` 只有字面量 `"true"` 才为真，和 `data-downloads="true"` 的写法一致。
 *
 * @param c   `window.rivu.c`
 * @param d   当前脚本的 `dataset`
 * @param src 当前脚本的 `src`，仅在没有显式端点时用来推导默认端点
 */
export const resolveConfig = (
  c?: Config,
  d?: DOMStringMap | null,
  src?: string | null,
): Config => {
  d = d || {};
  // 源码不使用 `?.`、`??`：esbuild 会为 es2018 降级它们，白白增加体积（规范 §1.3）。
  const o: Config = {
    site: d.site,
    endpoint: d.api,
    hash: d.hash == 'true',
    local: d.local == 'true',
    ...c,
  };
  if (!o.endpoint && src) o.endpoint = new URL(src).origin + '/api/event';
  return o;
};
