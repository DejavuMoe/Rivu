// 时间桶标签的显示：横轴用短写法，提示用长写法。标签按站点时间显示（labelDate 只取墙上时间）。

import { dateLong, dateShort, hourLabel } from "../format.ts";
import type { Lang } from "../state.svelte";
import { labelDate, type Gran } from "./spec.ts";

const pad = (n: number) => String(n).padStart(2, "0");

export function axisLabels(labels: string[], gran: Gran, lang: Lang): string[] {
  const ds = labels.map(labelDate);
  const last = ds[ds.length - 1];
  return ds.map((d) => {
    if (gran === "minute") {
      const m = Math.round((last.getTime() - d.getTime()) / 60_000);
      return m === 0 ? (lang === "zh" ? "现在" : "now") : lang === "zh" ? `${m} 分钟前` : `-${m}m`;
    }
    if (gran === "hour") return hourLabel(d);
    if (gran === "month") return lang === "zh" ? `${d.getMonth() + 1}月` : d.toLocaleDateString("en-US", { month: "short" });
    return dateShort(d, lang);
  });
}

export function tipLabels(labels: string[], gran: Gran, lang: Lang): string[] {
  return labels.map((l) => {
    const d = labelDate(l);
    if (gran === "minute") return `${pad(d.getHours())}:${pad(d.getMinutes())}`;
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
