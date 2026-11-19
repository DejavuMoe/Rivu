// 看板的「查询」：把 URL 里的看板状态（范围、对比、粒度、筛选）换算成模拟结果。
// 正式实现里这一层换成 POST /api/internal/sites/{id}/query（QuerySpec），组件不变。

import { breakdown, daily, hourly, monthly, NOW, type Point, type Row, type Site } from "../../lib/data/fixtures";
import { dateShort, hourLabel, dateLong } from "../../lib/format";
import type { Lang } from "../../lib/state.svelte";

export type RangeKey =
  | "realtime"
  | "day"
  | "24h"
  | "7d"
  | "30d"
  | "month"
  | "6mo"
  | "12mo"
  | "year"
  | "all";
export type CompareKey = "off" | "previous_period" | "year_over_year";
export type Gran = "minute" | "hour" | "day" | "week" | "month";
export type MetricKey = "visitors" | "visits" | "pageviews" | "views_per_visit" | "bounce_rate" | "visit_duration";
export type Op = "is" | "is_not" | "contains" | "contains_not";

export interface Filter {
  dim: string;
  op: Op;
  values: string[];
}

export interface DashState {
  range: RangeKey;
  compare: CompareKey;
  gran: Gran;
  metric: MetricKey;
  filters: Filter[];
}

export const RANGES: RangeKey[][] = [
  ["realtime"],
  ["day", "24h"],
  ["7d", "30d"],
  ["month", "6mo", "12mo", "year"],
  ["all"],
];

export const METRICS: MetricKey[] = ["visitors", "visits", "pageviews", "views_per_visit", "bounce_rate", "visit_duration"];

/** 每个范围允许的粒度，第一个是默认值。 */
export function grans(range: RangeKey): Gran[] {
  switch (range) {
    case "realtime":
      return ["minute"];
    case "day":
    case "24h":
      return ["hour"];
    case "7d":
      return ["day", "hour"];
    case "30d":
    case "month":
      return ["day", "week"];
    case "6mo":
    case "12mo":
    case "year":
      return ["month", "week", "day"];
    case "all":
      return ["month", "week"];
  }
}

// ---------- URL 编解码 ----------

export function decodeFilters(s: string | null): Filter[] {
  if (!s) return [];
  return s
    .split(";")
    .map((p) => {
      const [dim, op, v] = p.split("~");
      return { dim, op: op as Op, values: (v ?? "").split("|").filter(Boolean).map(decodeURIComponent) };
    })
    .filter((f) => f.dim && f.op && f.values.length);
}

export function encodeFilters(fs: Filter[]): string {
  return fs.map((f) => [f.dim, f.op, f.values.map(encodeURIComponent).join("|")].join("~")).join(";");
}

export function readState(q: URLSearchParams): DashState {
  const range = (q.get("range") as RangeKey) || "30d";
  const allowed = grans(range);
  const gran = allowed.includes(q.get("gran") as Gran) ? (q.get("gran") as Gran) : allowed[0];
  let metric = (q.get("metric") as MetricKey) || "visitors";
  // 实时只有访客与浏览量两条曲线
  if (range === "realtime" && metric !== "pageviews") metric = "visitors";
  return {
    range,
    compare: (q.get("cmp") as CompareKey) || "previous_period",
    gran,
    metric,
    filters: decodeFilters(q.get("f")),
  };
}

// ---------- 结果 ----------

export interface Kpi {
  /** current：实时在线（只在 realtime 范围出现，不能作为主图指标） */
  key: MetricKey | "current";
  value: number;
  prev: number | null;
  spark: number[];
}

export interface DashResult {
  points: Point[];
  prevPoints: Point[] | null;
  xLabels: string[];
  tipLabels: string[];
  prevTipLabels: string[];
  partialLast: boolean;
  kpis: Kpi[];
  totalVisitors: number;
  factor: number;
  breakdown: (dim: string) => Row[];
  /** 选中范围的文字描述（用于对比说明等） */
}

/** 筛选把流量按份额缩小：is 取所选值的份额之和，is_not 取剩余，contains 估 30%。 */
function filterFactor(site: Site, filters: Filter[]): number {
  let f = 1;
  for (const flt of filters) {
    const rows = breakdown(site, flt.dim, 10000);
    const picked = rows.filter((r) => flt.values.includes(r.label)).reduce((a, r) => a + r.visitors, 0) / 10000;
    const share = Math.min(1, picked || 0.05);
    if (flt.op === "is") f *= Math.max(0.02, share);
    else if (flt.op === "is_not") f *= Math.max(0.05, 1 - share);
    else if (flt.op === "contains") f *= 0.3;
    else f *= 0.7;
  }
  return f;
}

function minuteSeries(site: Site): Point[] {
  // 实时：过去 30 分钟，每分钟一个点
  const out: Point[] = [];
  let seed = site.id * 17;
  const rnd = () => ((seed = (seed * 9301 + 49297) % 233280) / 233280);
  for (let i = 29; i >= 0; i--) {
    const t = new Date(NOW.getTime() - i * 60_000);
    const v = Math.max(0, site.live * (0.55 + 0.15 * Math.sin(i / 4) + rnd() * 0.2));
    out.push({
      t,
      visitors: Math.round(v),
      visits: Math.round(v * 1.05),
      pageviews: Math.round(v * 1.9),
      bounce_rate: 0.4,
      visit_duration: 120,
      views_per_visit: 1.9,
    });
  }
  return out;
}

function weeks(points: Point[]): Point[] {
  const out: Point[] = [];
  for (let i = 0; i < points.length; i += 7) {
    const chunk = points.slice(i, i + 7);
    const visits = chunk.reduce((a, p) => a + p.visits, 0);
    const pv = chunk.reduce((a, p) => a + p.pageviews, 0);
    out.push({
      t: chunk[0].t,
      visitors: Math.round(chunk.reduce((a, p) => a + p.visitors, 0) * 0.86),
      visits,
      pageviews: pv,
      bounce_rate: chunk.reduce((a, p) => a + p.bounce_rate * p.visits, 0) / (visits || 1),
      visit_duration: chunk.reduce((a, p) => a + p.visit_duration * p.visits, 0) / (visits || 1),
      views_per_visit: pv / (visits || 1),
    });
  }
  return out;
}

function series(site: Site, s: DashState, factor: number, prev: boolean): Point[] {
  const yoy = s.compare === "year_over_year";
  const off = prev ? 1 : 0;
  const k = prev ? (yoy ? 0.71 : 0.93) * factor : factor;
  switch (s.range) {
    case "realtime":
      return minuteSeries(site);
    case "day":
      return hourly(site, NOW.getHours() + 1, off * 24, k);
    case "24h":
      return hourly(site, 24, off * 24, k);
    case "7d":
    case "30d": {
      // 过去 N 天包含今天
      const n = { "7d": 7, "30d": 30 }[s.range];
      if (s.gran === "hour") return hourly(site, n * 24, off * n * 24, k);
      const d = daily(site, n, off * n, k);
      return s.gran === "week" ? weeks(d) : d;
    }
    case "month": {
      const d = daily(site, NOW.getDate(), off * 31, k);
      return s.gran === "week" ? weeks(d) : d;
    }
    case "6mo":
    case "12mo":
    case "year":
    case "all": {
      const months = { "6mo": 6, "12mo": 12, year: NOW.getMonth() + 1, all: 18 }[s.range];
      if (s.gran === "day") return daily(site, months * 30, off * months * 30, k);
      if (s.gran === "week") return weeks(daily(site, months * 30, off * months * 30, k));
      return monthly(site, months, k, off);
    }
  }
}

function labels(points: Point[], gran: Gran, lang: Lang) {
  const x = points.map((p) => {
    if (gran === "minute") {
      const m = Math.round((NOW.getTime() - p.t.getTime()) / 60_000);
      return m === 0 ? (lang === "zh" ? "现在" : "now") : lang === "zh" ? `${m} 分钟前` : `-${m}m`;
    }
    if (gran === "hour") return hourLabel(p.t);
    if (gran === "month") return lang === "zh" ? `${p.t.getMonth() + 1}月` : p.t.toLocaleDateString("en-US", { month: "short" });
    return dateShort(p.t, lang);
  });
  const tip = points.map((p) => {
    if (gran === "minute") return `${String(p.t.getHours()).padStart(2, "0")}:${String(p.t.getMinutes()).padStart(2, "0")}`;
    if (gran === "hour") return `${dateShort(p.t, lang)} ${hourLabel(p.t)}`;
    if (gran === "week") return lang === "zh" ? `${dateShort(p.t, lang)} 起的一周` : `Week of ${dateShort(p.t, lang)}`;
    if (gran === "month")
      return lang === "zh" ? `${p.t.getFullYear()}年${p.t.getMonth() + 1}月` : p.t.toLocaleDateString("en-US", { month: "long", year: "numeric" });
    return dateLong(p.t, lang);
  });
  return { x, tip };
}

function total(points: Point[], key: MetricKey): number {
  const visits = points.reduce((a, p) => a + p.visits, 0) || 1;
  switch (key) {
    case "visitors":
      // 多天访客近似为各天之和乘一个重复访问折扣（访客 ID 按天轮换，见 system-design §6.6）
      return Math.round(points.reduce((a, p) => a + p.visitors, 0) * 0.94);
    case "visits":
      return visits;
    case "pageviews":
      return points.reduce((a, p) => a + p.pageviews, 0);
    case "views_per_visit":
      return points.reduce((a, p) => a + p.pageviews, 0) / visits;
    case "bounce_rate":
      return points.reduce((a, p) => a + p.bounce_rate * p.visits, 0) / visits;
    case "visit_duration":
      return points.reduce((a, p) => a + p.visit_duration * p.visits, 0) / visits;
  }
}

export function runQuery(site: Site, s: DashState, lang: Lang): DashResult {
  const factor = filterFactor(site, s.filters);
  const points = series(site, s, factor, false);
  const cmpOn = s.compare !== "off" && s.range !== "realtime";
  const prevPoints = cmpOn ? series(site, s, factor, true).slice(-points.length) : null;
  const { x, tip } = labels(points, s.gran, lang);
  const prevTip = prevPoints ? labels(prevPoints, s.gran, lang).tip : [];
  // 每个范围都包含「现在」（过去 N 天含今天，6mo、12mo 含本月），最后一个桶都不完整
  const partialLast = true;
  // 每个范围都包含「现在」，最后一个桶都不完整（6mo、12mo 包含本月）
  let kpis: Kpi[] = METRICS.map((key) => ({
    key,
    value: total(points, key),
    prev: prevPoints ? total(prevPoints, key) : null,
    spark: points.map((p) => p[key] as number),
  }));
  if (s.range === "realtime") {
    // 实时：当前在线（过去 5 分钟）+ 过去 30 分钟的独立访客与浏览量；每分钟访客不能相加
    const uniq = Math.round(points.reduce((a, p) => a + p.visitors, 0) * 0.31);
    kpis = [
      { key: "current", value: site.live, prev: null, spark: [] },
      { key: "visitors", value: uniq, prev: null, spark: [] },
      { key: "pageviews", value: total(points, "pageviews"), prev: null, spark: [] },
    ];
  }
  const totalVisitors = kpis.find((k) => k.key === "visitors")!.value;

  return {
    points,
    prevPoints,
    xLabels: x,
    tipLabels: tip,
    prevTipLabels: prevTip,
    partialLast,
    kpis,
    totalVisitors,
    factor,
    breakdown: (dim) => {
      const rows = breakdown(site, dim, totalVisitors);
      const own = s.filters.find((f) => f.dim === dim && f.op === "is");
      return own ? rows.filter((r) => own.values.includes(r.label)) : rows;
    },
  };
}
