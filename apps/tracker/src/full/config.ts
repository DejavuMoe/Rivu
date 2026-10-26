// full 专属配置（规范 §2.2）与几个纯函数，便于单元测试。
//
// 优先级与 core 相同：`rivu.c`（站点脚本前缀）> `data-*`（`t.js`）> 默认值。
// `engagement` 与 `tagged` 默认开；其余默认关。`data-downloads="true"` 用默认扩展名表，
// 其它值是逗号分隔的扩展名列表（整体替换默认表）；前缀里是 `true` / `false` 或数组。

import type { Config } from '../config.ts';

/** 默认下载扩展名（28 项，规范 §4.6）。 */
export const DOWNLOADS = /* @__PURE__ */ (
  'pdf xlsx xls docx doc txt rtf csv exe key pps ppt pptx 7z pkg rar gz zip avi mov mp4 mpeg wmv midi mp3 wav wma dmg'.split(' ')
);

/** 布尔开关；前两个默认开。 */
const SWITCHES = /* @__PURE__ */ ('engagement tagged outbound forms manual debug'.split(' ') as (keyof Config)[]);

export const resolveFull = (c?: Config, d?: DOMStringMap | null): Config => {
  const o: Record<string, unknown> = {};
  c = c || {};
  d = d || {};
  // 前缀里的值优先；data-* 只认字面量 "true" / "false"，缺省用默认值
  SWITCHES.forEach((k, i) => (o[k] = c![k] != null ? !!c![k] : d![k] ? d![k] == 'true' : i < 2));
  const dl = c.downloads != null ? c.downloads : d.downloads;
  o.downloads =
    dl === true || dl == 'true' ? DOWNLOADS : typeof dl == 'string' ? dl != 'false' && dl.toLowerCase().split(/ *, */) : dl;
  return o as Config;
};

/** 属性值只留 string / number / boolean（客户端转成字符串），其余类型跳过（规范 §4.8）。没有属性时返回 undefined。 */
export const cleanProps = (o: unknown): Record<string, string> | undefined => {
  let out: Record<string, string> | undefined;
  for (const k in o as Record<string, unknown>) {
    const v = (o as Record<string, unknown>)[k];
    if (/^[snb]/.test(typeof v)) (out = out || {})[k] = String(v);
  }
  return out;
};

/** 链接的下载扩展名：`pathname` 最后一段里最后一个 `.` 之后，转小写（不受 query 与 hash 影响）。 */
export const extOf = (pathname: string): string => {
  const m = /\.([^./]+)$/.exec(pathname);
  return m ? m[1]!.toLowerCase() : '';
};

/** tagged 标记解析的结果：事件名、属性、收入金额、收入币种。 */
export type Tag = [name?: string, props?: Record<string, string>, amount?: string, currency?: string];

/**
 * 一个元素的 class 列表里的 tagged 标记（规范 §4.6）：`rivu-event-name=Signup`（或 `--`）定义事件名，
 * `rivu-event-<k>=<v>` 定义属性（值里的 `+` 换成空格），`rivu-revenue-amount` / `-currency` 定义收入。
 */
export const parseTag = (classes: Iterable<string>): Tag => {
  const t: Tag = [];
  for (const c of classes) {
    const m = /^rivu-(event|revenue)-(.+?)(?:=|--)(.+)$/.exec(c);
    if (m) {
      const k = m[2]!;
      const v = m[3]!.replace(/\+/g, ' ');
      if (m[1] == 'revenue') {
        if (k == 'amount') t[2] = v;
        else if (k == 'currency') t[3] = v;
      } else if (k == 'name') t[0] = v;
      else (t[1] = t[1] || {})[k] = v;
    }
  }
  return t;
};
