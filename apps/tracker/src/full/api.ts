// 手动 API 与事件发送（规范 §4.8）。
//
// - `rivu('event', name, opts?)`：自定义事件；`opts` = `props`、`revenue`（`{amount, currency}`）、
//   `interactive`（`false` 时发 `i:false`）、`url`（覆盖 `u`）、`callback`。
// - `rivu('pageview', opts?)` 等价 `rivu('event', 'pageview', opts)`：走 pageview 流程（先 flush engagement）。
// - `rivu('set', {props})`：全局属性，替换上一次的 `set`；与单次 `props` 合并，单次优先；对所有事件（含 engagement）生效。
// - `rivu('init', opts)` 只在脚本加载前入队才有效（回放在 index.ts）；`engagement` 不能经手动 API 发送（忽略）。
// - `callback` 恰好调用一次：fetch 成功 `{status}`、网络错误 `{error}`、beacon `{}`、被忽略时无参。
// - 所有命令内部吞异常，不抛给页面。

import type { Config } from '../config.ts';
import { ignored } from '../guard.ts';
import { buildPayload } from '../payload.ts';
import { send, type Callback } from '../send.ts';
import { beacon } from './beacon.ts';
import { cleanProps } from './config.ts';
import { engBegin } from './engagement.ts';

export interface Opts {
  props?: unknown;
  revenue?: { amount?: unknown; currency?: unknown };
  interactive?: boolean;
  url?: string;
  callback?: Callback;
}

let cfg: Config;
let global: Record<string, string> | undefined;

export const apiInit = (c: Config): void => {
  cfg = c;
};

export const setProps = (o?: { props?: unknown }): void => {
  global = cleanProps(o && o.props);
};

/** 被忽略：`debug` 时输出原因；调用一次无参的 callback。 */
export const skipped = (why: unknown, o?: Opts): void => {
  if (cfg.debug) console.warn('[rivu] ignored: ' + why);
  if (o && o.callback) o.callback();
};

/**
 * 组装并发送一个事件（调用方已做过排除判定）。pageview 之后开始该页的 engagement。
 * @param ref 只有文档的第一个 pageview 带 `document.referrer`
 */
export const emit = (name: string, o?: Opts, ref?: string): void => {
  o = o || {};
  const u = o.url || location.href;
  const p = buildPayload(cfg.site!, __SV__, name, u, ref, cfg.hash);
  // 单次属性优先；合并后没有任何属性就不发 p
  const props = cleanProps({ ...global, ...(o.props as object) });
  if (props) p.p = props;
  if (name != 'pageview') {
    if (o.interactive === false) p.i = false;
    const r = o.revenue;
    if (r) p.$ = { a: String(r.amount), c: r.currency };
  }
  const cb = o.callback;
  // 页面已隐藏、又不需要结果：beacon 更可能送达
  if (!cb && document.visibilityState == 'hidden') beacon(cfg.endpoint!, p);
  else send(cfg.endpoint!, p, cb);
  if (name == 'pageview') engBegin(u, props);
};

/** 自定义事件（自动捕获与手动 API 共用）。 */
export const event = (name: unknown, o?: Opts): void => {
  if (typeof name != 'string' || name == 'engagement') return;
  const why = ignored(cfg);
  if (why) skipped(why, o);
  else emit(name, o);
};
