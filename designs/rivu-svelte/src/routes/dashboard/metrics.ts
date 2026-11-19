// 指标的显示规则：格式、「越低越好」、紧凑与完整写法。

import { compact, duration, full, percent } from "../../lib/format";
import type { Lang } from "../../lib/state.svelte";
import type { MetricKey } from "./query";

export function fmtMetric(key: MetricKey, v: number, lang: Lang, mode: "compact" | "full" = "compact"): string {
  switch (key) {
    case "bounce_rate":
      return percent(v, 0);
    case "visit_duration":
      return duration(v, lang);
    case "views_per_visit":
      return v.toFixed(2);
    default:
      return mode === "compact" ? compact(v, lang) : full(v, lang);
  }
}

/** 纵轴刻度用：比 fmtMetric 更短 */
export function fmtAxis(key: MetricKey, v: number, lang: Lang): string {
  if (key === "bounce_rate") return percent(v, 0);
  if (key === "visit_duration") return v >= 60 ? `${Math.round(v / 60)}m` : `${Math.round(v)}s`;
  if (key === "views_per_visit") return v.toFixed(1);
  return compact(v, lang);
}

export const lowerIsBetter = (key: MetricKey) => key === "bounce_rate";
