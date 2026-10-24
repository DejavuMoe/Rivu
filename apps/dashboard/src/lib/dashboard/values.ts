// 维度取值的显示名。服务端返回原始取值（国家是 ISO 代码，渠道是英文名，空值是固定文案），
// 筛选与 URL 里一律用原始取值，只在显示时翻译。

import type { Lang } from "../state.svelte";

/** 服务端的空值文案（system-design.md §6.2）。 */
const EMPTY: Record<string, [string, string]> = {
  "(not set)": ["（未知）", "(not set)"],
  "(none)": ["（无）", "(none)"],
  "Direct / None": ["直接访问", "Direct / None"],
};

const CHANNELS: Record<string, string> = {
  Direct: "直接访问",
  "Cross-network": "跨网络广告",
  "Paid Search": "付费搜索",
  "Paid Social": "付费社交",
  "Paid Video": "付费视频",
  "Paid Shopping": "付费购物",
  "Paid Other": "其他付费",
  Display: "展示广告",
  "Organic Search": "自然搜索",
  "Organic Social": "自然社交",
  "Organic Video": "自然视频",
  "Organic Shopping": "自然购物",
  Email: "邮件",
  Affiliates: "联盟推广",
  Audio: "音频",
  SMS: "短信",
  Referral: "引荐",
  Unassigned: "未归类",
};

const DEVICES: Record<string, [string, string]> = {
  desktop: ["桌面", "Desktop"],
  mobile: ["手机", "Mobile"],
  tablet: ["平板", "Tablet"],
};

const regionNames = new Map<Lang, Intl.DisplayNames | null>();
function regions(lang: Lang): Intl.DisplayNames | null {
  if (!regionNames.has(lang)) {
    try {
      regionNames.set(lang, new Intl.DisplayNames([lang === "zh" ? "zh-CN" : "en"], { type: "region" }));
    } catch {
      regionNames.set(lang, null);
    }
  }
  return regionNames.get(lang)!;
}

export function displayValue(dim: string, value: string, lang: Lang): string {
  const i = lang === "zh" ? 0 : 1;
  const empty = EMPTY[value];
  if (empty) return empty[i];
  if (dim === "country" && /^[A-Z]{2}$/.test(value)) return regions(lang)?.of(value) ?? value;
  if (dim === "channel" && lang === "zh") return CHANNELS[value] ?? value;
  if (dim === "device") return DEVICES[value]?.[i] ?? value;
  return value;
}

/** 行首的代码徽标（国家、地区）：两个字母。 */
export function valueCode(dim: string, value: string): string | undefined {
  if ((dim === "country" || dim === "region") && /^[A-Z]{2}(-|$)/.test(value)) return value.slice(0, 2);
  return undefined;
}
