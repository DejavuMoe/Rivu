// 自动捕获（规范 §4.6）：文档级委托监听 `click`、`auxclick`（仅中键）与 `submit`（捕获阶段）。
//
// - 优先级：tagged > 外链 > 下载；一次点击只产生一个事件（外部站点上的 `.pdf` 记为外链）。
// - tagged：元素本身或向上最多 3 层父元素，**就近优先**；表单内的点击不算（只算 `submit`）；
//   命中的元素是链接则自动补 `url` 属性。
// - 外链：`link.host` 非空且不等于 `location.host`（含端口，子域也算外链），属性 `url` 为 `link.href` 原样。
// - 下载：`pathname` 的扩展名（小写）在表里，属性 `url` 去掉 query 与 hash。
// - 表单：有 `novalidate` 或 `checkValidity()` 为真；被 tagged 的表单发 tagged 事件，不重复发。
// - 不拦截导航、不 `preventDefault`、不延迟跳转（`keepalive` 保证送达）。

import type { Config } from '../config.ts';
import { event, type Opts } from './api.ts';
import { DOWNLOADS, extOf, parseTag } from './config.ts';

let cfg: Config;

/** 从 `el` 起向上最多 3 层父元素里最近的 tagged 元素，转成事件。 */
const tagged = (el: Element | null): [string, Opts, Element] | undefined => {
  for (let i = 0; i < 4 && el; i++, el = el.parentElement) {
    const [name, props] = parseTag(el.classList);
    if (name) {
      const href = (el as HTMLAnchorElement).href;
      const o: Opts = { props: el.tagName == 'A' && href ? { ...props, url: href } : props };
      return [name, o, el];
    }
  }
};

const onClick = (e: MouseEvent) => {
  try {
    if (e.type == 'auxclick' && e.button != 1) return;
    const target = e.target as Element;
    if (cfg.tagged) {
      const t = tagged(target);
      if (t) {
        // 表单本身或表单内的元素：只在 submit 时算
        if (!t[2].closest('form')) event(t[0], t[1]);
        return;
      }
    }
    const a = target.closest && (target.closest('a[href]') as HTMLAnchorElement | null);
    if (!a) return;
    if (cfg.outbound && a.host && a.host != location.host) {
      event('Outbound Link: Click', { props: { url: a.href } });
      return;
    }
    const dl = cfg.downloads as string[] | undefined;
    if (dl && dl.indexOf(extOf(a.pathname)) >= 0) {
      event('File Download', { props: { url: a.origin + a.pathname } });
    }
  } catch {}
};

const onSubmit = (e: Event) => {
  try {
    const f = e.target as HTMLFormElement;
    const t = cfg.tagged && tagged(f);
    if (t) event(t[0], t[1]);
    else if (cfg.forms && (f.noValidate || f.checkValidity())) event('Form: Submission');
  } catch {}
};

export const captureInit = (c: Config): void => {
  cfg = c;
  if (c.downloads === true) c.downloads = DOWNLOADS;
  const d = document;
  d.addEventListener('click', onClick, true);
  d.addEventListener('auxclick', onClick, true);
  d.addEventListener('submit', onSubmit, true);
};
