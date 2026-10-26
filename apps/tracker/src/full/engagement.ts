// engagement 状态机（规范 §4.5）。
//
// 状态：当前页的 `url`、`props`（取自该页 pageview）、`maxPx`（最大滚动像素，初始即视口底部）、
// `sentPx`（初始 −1，所以首个 flush 必发）、`acc`（累计可见毫秒）、`run`（计时起点，0 表示暂停）。
// 时钟用单调的 `performance.now()`。只在页面可见**且有焦点**时计时。
//
// flush：只有 `maxPx > sentPx` 或 `acc ≥ 3000` 才发送，否则保留 `acc` 留待下次（短暂失焦不丢时长）；
// 发送后 `sentPx = maxPx`、`acc = 0`。`e` 是自上次发送以来的增量毫秒。beacon 优先。

import type { Config } from '../config.ts';
import { buildPayload } from '../payload.ts';
import { beacon } from './beacon.ts';

let cfg: Config;
/** 当前页有 engagement 状态：pageview 已发且未被忽略、开关打开。 */
let on: boolean | undefined;
let url: string;
let props: Record<string, string> | undefined;
let maxPx = 0;
let sentPx = -1;
let docH = 1;
let acc = 0;
let run = 0;

const now = () => performance.now();

/** 文档高度：body 与 documentElement 的 scrollHeight / offsetHeight / clientHeight 最大值。 */
const h = (x: HTMLElement | null) => (x ? Math.max(x.scrollHeight, x.offsetHeight, x.clientHeight) : 1);
const measure = () => {
  docH = Math.max(h(document.body), h(document.documentElement));
  // 视口底部；视口高 ≥ 文档高时等于（或超过）文档高，sd 计为 100
  maxPx = Math.max(maxPx, scrollY + innerHeight);
};

const active = () => document.visibilityState == 'visible' && document.hasFocus();

const pause = () => {
  if (run) acc += now() - run;
  run = 0;
};

const resume = () => {
  if (on && !run && active()) run = now();
};

const flush = () => {
  if (!on) return;
  pause();
  if (maxPx > sentPx || acc >= 3000) {
    const p = buildPayload(cfg.site!, __SV__, 'engagement', url, '', cfg.hash);
    p.sd = Math.min(100, Math.round((100 * maxPx) / docH));
    p.e = Math.round(acc);
    if (props) p.p = props;
    beacon(cfg.endpoint!, p);
    sentPx = maxPx;
    acc = 0;
  }
};

/** 新页面（pageview 已发出）：重置状态并按当前可见性与焦点决定是否开始计时。 */
export const engBegin = (u: string, p?: Record<string, string>): void => {
  on = cfg.engagement;
  url = u;
  props = p;
  maxPx = 0;
  sentPx = -1;
  acc = 0;
  run = 0;
  measure();
  resume();
};

/** 离开当前页（下一个 pageview 之前，或该页被忽略）：累计、flush，然后停用，直到下一个 `engBegin`。 */
export const engEnd = (): void => {
  flush();
  on = false;
};

export const engInit = (c: Config): void => {
  cfg = c;
  if (!c.engagement) return;
  const hide = () => {
    pause();
    flush();
  };
  document.addEventListener('visibilitychange', () => (document.visibilityState == 'visible' ? resume() : hide()));
  addEventListener('blur', hide);
  addEventListener('focus', resume);
  addEventListener('scroll', measure, { passive: true });
  // 文档高度变化（懒加载、展开）：重新量高度。ResizeObserver 不可用时只靠 scroll。
  if (window.ResizeObserver) new ResizeObserver(measure).observe(document.documentElement);
};
