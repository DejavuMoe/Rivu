// 时间桶提示按站点时间显示，实时横轴按实际经过的分钟数显示。

import { dateLong, dateShort, hourLabel } from "../format.ts";
import type { Lang } from "../state.svelte";
import { labelDate, type Gran } from "./spec.ts";

export function axisLabels(labels: string[], gran: Gran, lang: Lang): string[] {
  if (gran === "minute" && labels.length) {
    const last = Date.parse(labels[labels.length - 1]);
    return labels.map((label) => {
      const m = Math.round((last - Date.parse(label)) / 60_000);
      return m === 0 ? (lang === "zh" ? "现在" : "now") : lang === "zh" ? `${m} 分钟前` : `-${m}m`;
    });
  }
  return labels.map((label) => {
    const d = labelDate(label);
    if (gran === "hour") return hourLabel(d);
    if (gran === "month") return lang === "zh" ? `${d.getMonth() + 1}月` : d.toLocaleDateString("en-US", { month: "short" });
    return dateShort(d, lang);
  });
}

export function tipLabels(labels: string[], gran: Gran, lang: Lang): string[] {
  return labels.map((l) => {
    if (gran === "minute") return l.slice(11, 16);
    const d = labelDate(l);
    if (gran === "hour") return `${dateShort(d, lang)} ${hourLabel(d)}`;
    if (gran === "week") return lang === "zh" ? `${dateShort(d, lang)} 起的一周` : `Week of ${dateShort(d, lang)}`;
    if (gran === "month")
      return lang === "zh" ? `${d.getFullYear()}年${d.getMonth() + 1}月` : d.toLocaleDateString("en-US", { month: "long", year: "numeric" });
    return dateLong(d, lang);
  });
}

/** 对比期按位置对齐到主区间：多出的截掉，不足的补 null（不画）。 */
export function alignTo<T>(values: T[], n: number): (T | null)[] {
  return Array.from({ length: n }, (_, i) => (i < values.length ? values[i] : null));
}
