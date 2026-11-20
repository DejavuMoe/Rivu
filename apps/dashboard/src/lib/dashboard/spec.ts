// 看板状态（URL 查询串）与 QuerySpec 之间的换算，以及把查询结果整理成组件要的形状。
// 纯函数，不依赖 Svelte 与浏览器，test/spec.test.ts 直接用 node --test 跑。
// QuerySpec 的契约见 docs/architecture/system-design.md §6.2、§6.7。

import type { QueryResult } from "../api.ts";

export type RangeKey = "realtime" | "day" | "24h" | "7d" | "30d" | "month" | "6mo" | "12mo" | "year" | "all";
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

export const RANGES: RangeKey[][] = [["realtime"], ["day", "24h"], ["7d", "30d"], ["month", "6mo", "12mo", "year"], ["all"]];
const RANGE_SET = new Set(RANGES.flat());

export const METRICS: MetricKey[] = ["visitors", "visits", "pageviews", "views_per_visit", "bounce_rate", "visit_duration"];

/** 实时视图的时间窗：过去 30 分钟，按分钟。 */
export const REALTIME_MINUTES = 30;

/** 界面上的维度名 → QuerySpec 的维度。 */
export const DIMS: Record<string, string> = {
  goal: "event:goal",
  page: "event:page",
  entry_page: "visit:entry_page",
  exit_page: "visit:exit_page",
  hostname: "event:hostname",
  source: "visit:source",
  channel: "visit:channel",
  referrer: "visit:referrer",
  utm_source: "visit:utm_source",
  utm_medium: "visit:utm_medium",
  utm_campaign: "visit:utm_campaign",
  country: "visit:country",
  region: "visit:region",
  city: "visit:city",
  device: "visit:device",
  browser: "visit:browser",
  os: "visit:os",
};

/** 自定义属性的界面维度名：`prop:<键>`。 */
export const PROP_PREFIX = "prop:";

export function isPropDim(dim: string): boolean {
  if (!dim.startsWith(PROP_PREFIX)) return false;
  const key = dim.slice(PROP_PREFIX.length);
  return key.length > 0 && (new TextEncoder().encode(key).length <= 300 || [...key].length <= 200);
}

/** 界面维度名 → QuerySpec 维度（属性是 `event:props:<键>`）。 */
export function specDim(dim: string): string {
  return isPropDim(dim) ? `event:props:${dim.slice(PROP_PREFIX.length)}` : DIMS[dim];
}

/** 只有国家级地理库时没有意义的维度。 */
export const CITY_LEVEL_DIMS = new Set(["region", "city"]);

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

const OPS = new Set<Op>(["is", "is_not", "contains", "contains_not"]);

/** 只能按等于 / 不等于筛选的维度：目标按名字，城市按 GeoNames id（服务端只支持 is、is_not）。 */
const EXACT_ONLY_DIMS = new Set(["goal", "city"]);

/** 维度允许的筛选条件。 */
export function opsFor(dim: string): Op[] {
  return EXACT_ONLY_DIMS.has(dim) ? ["is", "is_not"] : ["is", "is_not", "contains", "contains_not"];
}

export function decodeFilters(s: string | null): Filter[] {
  if (!s) return [];
  const out: Filter[] = [];
  for (const p of s.split(";")) {
    const [rawDim, op, v] = p.split("~");
    let dim = rawDim;
    try {
      dim = decodeURIComponent(rawDim ?? "");
    } catch {
      continue;
    }
    if (!dim || !(dim in DIMS || isPropDim(dim)) || !OPS.has(op as Op)) continue;
    if (!opsFor(dim).includes(op as Op)) continue;
    const values = (v ?? "")
      .split("|")
      .filter(Boolean)
      .map((x) => {
        try {
          return decodeURIComponent(x);
        } catch {
          return x;
        }
      });
    if (!values.length) continue;
    // 一个维度只有一条筛选（后出现的覆盖前面的）
    const f = { dim, op: op as Op, values };
    const same = out.findIndex((x) => x.dim === dim);
    if (same >= 0) out[same] = f;
    else out.push(f);
  }
  return out;
}

export function encodeFilters(fs: Filter[]): string {
  // encodeURIComponent 不编码 ~，而 ~ 是这里的字段分隔符
  const enc = (v: string) => encodeURIComponent(v).replaceAll("~", "%7E");
  // 维度名也编码：属性键里可能有分隔符
  return fs.map((f) => [enc(f.dim), f.op, f.values.map(enc).join("|")].join("~")).join(";");
}

export function readState(q: { get(key: string): string | null }): DashState {
  const rawRange = q.get("range") as RangeKey;
  const range: RangeKey = RANGE_SET.has(rawRange) ? rawRange : "30d";
  const allowed = grans(range);
  const gran = allowed.includes(q.get("gran") as Gran) ? (q.get("gran") as Gran) : allowed[0];
  const rawMetric = q.get("metric") as MetricKey;
  let metric: MetricKey = METRICS.includes(rawMetric) ? rawMetric : "visitors";
  // 实时只有访客与浏览量两条曲线
  if (range === "realtime" && metric !== "pageviews") metric = "visitors";
  const rawCmp = q.get("cmp") as CompareKey;
  return {
    range,
    compare: rawCmp === "off" || rawCmp === "year_over_year" ? rawCmp : "previous_period",
    gran,
    metric,
    filters: decodeFilters(q.get("f")),
  };
}

// ---------- QuerySpec ----------

export type Spec = Record<string, unknown>;

export function filtersSpec(fs: Filter[]): unknown[][] {
  return fs.map((f) => [f.op, specDim(f.dim), f.values]);
}

/** 实时窗口的时刻区间（含起点、不含终点），终点取下一个整分钟，让最后一个桶是当前分钟。 */
export function realtimeRange(now: Date): [string, string] {
  const end = new Date(now);
  end.setUTCSeconds(0, 0);
  end.setUTCMinutes(end.getUTCMinutes() + 1);
  const start = new Date(end.getTime() - REALTIME_MINUTES * 60_000);
  const iso = (d: Date) => d.toISOString().replace(/\.\d{3}Z$/, "+00:00");
  return [iso(start), iso(end)];
}

function dateRange(s: DashState, now: Date): unknown {
  return s.range === "realtime" ? realtimeRange(now) : s.range;
}

function withCompare(spec: Spec, s: DashState): Spec {
  if (s.range === "realtime" || s.compare === "off") return spec;
  return { ...spec, include: { ...(spec.include as object), compare: s.compare } };
}

/** 指标条：整个范围的合计（带对比）。实时只要访客与浏览量。 */
export function totalsSpec(s: DashState, now: Date): Spec {
  const metrics = s.range === "realtime" ? ["visitors", "pageviews"] : METRICS;
  const spec: Spec = { metrics, date_range: dateRange(s, now) };
  if (s.filters.length) spec.filters = filtersSpec(s.filters);
  return withCompare(spec, s);
}

/** 主图：按粒度的时间序列，含空桶的完整标签（带对比时也要对比期的标签）。 */
export function seriesSpec(s: DashState, now: Date): Spec {
  const metrics = s.range === "realtime" ? ["visitors", "pageviews"] : METRICS;
  const spec: Spec = {
    metrics,
    date_range: dateRange(s, now),
    dimensions: [`time:${s.gran}`],
    include: { time_labels: true },
  };
  if (s.filters.length) spec.filters = filtersSpec(s.filters);
  return withCompare(spec, s);
}

/**
 * 分解卡片：按一个维度的访客数排行。属性只列带这个键的事件（去掉 `(none)`，否则所有浏览都落在那一行），
 * 除非已经按这个键筛选了。
 */
export function breakdownSpec(s: DashState, dim: string, limit: number, now: Date): Spec {
  const spec: Spec = {
    metrics: ["visitors"],
    date_range: dateRange(s, now),
    dimensions: [specDim(dim)],
    pagination: { limit },
  };
  const filters = filtersSpec(s.filters);
  if (isPropDim(dim) && !s.filters.some((f) => f.dim === dim)) filters.push(["is_not", specDim(dim), ["(none)"]]);
  if (filters.length) spec.filters = filters;
  return spec;
}

/** 目标卡片：每个目标一行（只有有转化的目标），访客、完成次数、转化率。 */
export const GOAL_LIMIT = 100;

/** CSV 导出的请求体：当前的时间范围、筛选与趋势粒度（实时不能导出）。 */
export function exportBody(s: DashState): { date_range: unknown; filters: unknown[][]; interval: Gran } {
  return { date_range: s.range, filters: filtersSpec(s.filters), interval: s.gran };
}

export function goalsSpec(s: DashState, now: Date): Spec {
  const spec: Spec = {
    metrics: ["visitors", "events", "conversion_rate"],
    date_range: dateRange(s, now),
    dimensions: ["event:goal"],
    pagination: { limit: GOAL_LIMIT },
  };
  if (s.filters.length) spec.filters = filtersSpec(s.filters);
  return spec;
}

/** 属性卡片的键列表：时间范围与筛选同看板（服务端只用这两项）。 */
export function propertyKeysSpec(s: DashState, now: Date): Spec {
  const spec: Spec = { metrics: ["events"], date_range: dateRange(s, now) };
  if (s.filters.length) spec.filters = filtersSpec(s.filters);
  return spec;
}

// ---------- 结果 ----------

export interface GoalRow {
  /** 目标的名字（显示名，否则事件名或页面路径） */
  label: string;
  visitors: number;
  /** 完成次数 */
  events: number;
  /** 0–1 */
  cr: number;
}

export function readGoals(res: QueryResult): GoalRow[] {
  return res.results.map((r) => ({
    label: r.dimensions[0],
    visitors: r.metrics[0] ?? 0,
    events: r.metrics[1] ?? 0,
    cr: r.metrics[2] ?? 0,
  }));
}

export interface Kpi {
  /** current：当前在线（只在实时视图出现，不能作为主图指标） */
  key: MetricKey | "current";
  value: number;
  prev: number | null;
}

export interface Totals {
  values: Record<string, number>;
  prev: Record<string, number> | null;
}

export function readTotals(res: QueryResult, metrics: string[]): Totals {
  const row = res.results[0];
  const values: Record<string, number> = {};
  metrics.forEach((m, i) => (values[m] = row?.metrics[i] ?? 0));
  // 主区间没有数据时没有行，也就没有对比
  const cmp = row?.comparison;
  if (!cmp) return { values, prev: null };
  const prev: Record<string, number> = {};
  metrics.forEach((m, i) => (prev[m] = cmp.metrics[i] ?? 0));
  return { values, prev };
}

export interface Series {
  /** 桶的标签（服务端原样） */
  labels: string[];
  /** 每个指标一列，与 labels 对齐，空桶补 0 */
  values: Record<string, number[]>;
  prevLabels: string[] | null;
  prevValues: Record<string, number[]> | null;
}

function fill(labels: string[], rows: { dimensions: string[]; metrics: number[] }[], metrics: string[]) {
  const at = new Map(rows.map((r) => [r.dimensions[0], r.metrics]));
  const out: Record<string, number[]> = {};
  metrics.forEach((m, i) => (out[m] = labels.map((l) => at.get(l)?.[i] ?? 0)));
  return out;
}

export function readSeries(res: QueryResult, metrics: string[]): Series {
  const labels = res.meta.time_labels ?? res.results.map((r) => r.dimensions[0]);
  const prevLabels = res.comparison_results ? (res.meta.comparison_time_labels ?? null) : null;
  return {
    labels,
    values: fill(labels, res.results, metrics),
    prevLabels,
    prevValues: prevLabels && res.comparison_results ? fill(prevLabels, res.comparison_results, metrics) : null,
  };
}

/**
 * 把桶标签换成「墙上时间」的 Date：只取标签里的年月日时分，不做时区换算。
 * 服务端的标签已经是站点本地时间（日期桶 YYYY-MM-DD，小时与分钟桶带偏移），界面按站点时间显示。
 */
export function labelDate(label: string): Date {
  const m = /^(\d{4})-(\d{2})-(\d{2})(?:T(\d{2}):(\d{2}))?/.exec(label);
  if (!m) return new Date(NaN);
  return new Date(+m[1], +m[2] - 1, +m[3], m[4] ? +m[4] : 0, m[5] ? +m[5] : 0);
}

/** 分解行：维度取值与访客数。 */
export interface Row {
  label: string;
  visitors: number;
}

export function readRows(res: QueryResult): Row[] {
  return res.results.map((r) => ({ label: r.dimensions[0], visitors: r.metrics[0] ?? 0 }));
}
