// 数字、时长、百分比、日期的格式化。语言相关的写法都在这里，组件不自己拼。

import type { Lang } from "./state.svelte";

const locale = (lang: Lang) => (lang === "zh" ? "zh-CN" : "en-US");

/** 紧凑数字：中文用「万」，英文用 K/M。小于 1 万（1K）时给完整千分位。 */
export function compact(n: number, lang: Lang): string {
  if (!Number.isFinite(n)) return "—";
  const abs = Math.abs(n);
  if (lang === "zh") {
    if (abs >= 1e8) return trim(n / 1e8) + " 亿";
    if (abs >= 1e4) return trim(n / 1e4) + " 万";
    return Math.round(n).toLocaleString("zh-CN");
  }
  if (abs >= 1e6) return trim(n / 1e6) + "M";
  if (abs >= 1e4) return trim(n / 1e3) + "K";
  return Math.round(n).toLocaleString("en-US");
}

function trim(x: number) {
  if (x >= 100) return x.toFixed(0);
  return x.toFixed(x >= 10 ? 1 : 2).replace(/\.?0+$/, "");
}

export function full(n: number, lang: Lang): string {
  return Math.round(n).toLocaleString(locale(lang));
}

export function percent(ratio: number, digits = 1): string {
  if (!Number.isFinite(ratio)) return "—";
  return (ratio * 100).toFixed(digits).replace(/\.0$/, "") + "%";
}

/** 时长：秒 → 1m 23s / 1 分 23 秒（紧凑写法用于指标卡） */
export function duration(sec: number, lang: Lang): string {
  if (!Number.isFinite(sec)) return "—";
  const s = Math.round(sec);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  if (lang === "zh") {
    if (h) return `${h} 时 ${m} 分`;
    if (m) return `${m} 分 ${r} 秒`;
    return `${r} 秒`;
  }
  if (h) return `${h}h ${m}m`;
  if (m) return `${m}m ${r}s`;
  return `${r}s`;
}

/** 环比变化：返回带符号的百分比文字与方向。 */
export function delta(cur: number, prev: number): { text: string; dir: "up" | "down" | "flat" } {
  if (!prev) return { text: "—", dir: "flat" };
  const d = (cur - prev) / prev;
  if (Math.abs(d) < 0.0005) return { text: "0%", dir: "flat" };
  return { text: (d > 0 ? "+" : "−") + percent(Math.abs(d)), dir: d > 0 ? "up" : "down" };
}

export function dateShort(d: Date, lang: Lang): string {
  return lang === "zh"
    ? `${d.getMonth() + 1}月${d.getDate()}日`
    : d.toLocaleDateString("en-US", { month: "short", day: "numeric" });
}

export function dateLong(d: Date, lang: Lang): string {
  return lang === "zh"
    ? `${d.getFullYear()}年${d.getMonth() + 1}月${d.getDate()}日 ${weekday(d, lang)}`
    : d.toLocaleDateString("en-US", { weekday: "short", year: "numeric", month: "short", day: "numeric" });
}

export function weekday(d: Date, lang: Lang): string {
  return d.toLocaleDateString(locale(lang), { weekday: "short" });
}

export function hourLabel(d: Date): string {
  return `${String(d.getHours()).padStart(2, "0")}:00`;
}

export function relativeTime(minutesAgo: number, lang: Lang): string {
  if (minutesAgo < 1) return lang === "zh" ? "刚刚" : "just now";
  if (minutesAgo < 60) return lang === "zh" ? `${minutesAgo} 分钟前` : `${minutesAgo}m ago`;
  const h = Math.floor(minutesAgo / 60);
  if (h < 24) return lang === "zh" ? `${h} 小时前` : `${h}h ago`;
  const d = Math.floor(h / 24);
  return lang === "zh" ? `${d} 天前` : `${d}d ago`;
}
