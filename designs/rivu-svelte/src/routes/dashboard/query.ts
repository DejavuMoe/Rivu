// 看板的「查询」：把 URL 里的看板状态（范围、对比、粒度、筛选）换算成模拟结果。
// 正式实现里这一层换成 POST /api/internal/sites/{id}/query（QuerySpec），组件不变。

import { breakdown, daily, hourly, monthly, NOW, type Point, type Row as FixtureRow, type Site } from "../../lib/data/fixtures";
import { dateShort, hourLabel, dateLong } from "../../lib/format";
import { app, type Lang } from "../../lib/state.svelte";
import { cfgs, geoCapability, INSTANCE_RETENTION } from "../settings/store.svelte";
import { capped, comparePeriod, dayCount, readRange, resolve, type Period, type RangeKey, type RangeParam } from "./period";

export type { RangeKey } from "./period";
export type CompareKey = "off" | "previous_period" | "year_over_year";
export type Gran = "minute" | "hour" | "day" | "week" | "month";
export type MetricKey = "visitors" | "visits" | "pageviews" | "views_per_visit" | "bounce_rate" | "visit_duration";
export type Op = "is" | "is_not" | "contains" | "contains_not";

export interface Filter {
  dim: string;
  op: Op;
  values: string[];
}

/** 范围（range，自定义时还有 from、to）+ 对比、粒度、指标、筛选。 */
export interface DashState extends RangeParam {
  compare: CompareKey;
  gran: Gran;
  metric: MetricKey;
  filters: Filter[];
}

/** 范围菜单里的预设，按组分隔；自定义范围在菜单最后单独一项。 */
export const RANGES: RangeKey[][] = [
  ["realtime"],
  ["day", "yesterday", "24h"],
  ["7d", "30d"],
  ["month", "last_month", "6mo", "12mo", "year"],
  ["all"],
];

export const METRICS: MetricKey[] = ["visitors", "visits", "pageviews", "views_per_visit", "bounce_rate", "visit_duration"];

/** 自定义属性的界面维度名：`prop:<键>`。 */
export const PROP_PREFIX = "prop:";

export function isPropDim(dim: string): boolean {
  return dim.startsWith(PROP_PREFIX) && dim.length > PROP_PREFIX.length;
}

/** 可以按它筛选的维度，顺序即筛选对话框里的顺序。 */
export const DIMS = [
  "goal",
  "page",
  "entry_page",
  "exit_page",
  "hostname",
  "source",
  "channel",
  "referrer",
  "utm_source",
  "utm_medium",
  "utm_campaign",
  "country",
  "region",
  "city",
  "device",
  "browser",
  "os",
];

/** 只有国家级地理库时没有意义的维度。 */
export const CITY_LEVEL_DIMS = new Set(["region", "city"]);

/** 只能按等于 / 不等于筛选的维度：目标按名字，城市按 GeoNames id。 */
const EXACT_ONLY_DIMS = new Set(["goal", "city"]);

/** 维度允许的筛选条件。 */
export function opsFor(dim: string): Op[] {
  return EXACT_ONLY_DIMS.has(dim) ? ["is", "is_not"] : ["is", "is_not", "contains", "contains_not"];
}

/** 每个范围允许的粒度，第一个是默认值。自定义范围按天数定（与预设的长度对应）。 */
export function grans(r: RangeParam): Gran[] {
  switch (r.range) {
    case "realtime":
      return ["minute"];
    case "day":
    case "yesterday":
    case "24h":
      return ["hour"];
    case "7d":
      return ["day", "hour"];
    case "30d":
    case "month":
    case "last_month":
      return ["day", "week"];
    case "6mo":
    case "12mo":
    case "year":
      return ["month", "week", "day"];
    case "all":
      return ["month", "week"];
    case "custom": {
      if (r.from?.includes("T")) return ["hour"];
      const n = dayCount(resolve(r, NOW, NOW));
      if (n <= 1) return ["hour"];
      if (n <= 7) return ["day", "hour"];
      if (n <= 92) return ["day", "week"];
      if (n <= 731) return ["month", "week", "day"];
      return ["month", "week"];
    }
  }
}

// ---------- URL 编解码 ----------

const OPS = new Set<Op>(["is", "is_not", "contains", "contains_not"]);

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
    if (!dim || !(DIMS.includes(dim) || isPropDim(dim)) || !OPS.has(op as Op)) continue;
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

export function readState(q: URLSearchParams): DashState {
  const r = readRange(q, NOW);
  const range = r.range;
  const allowed = grans(r);
  const gran = allowed.includes(q.get("gran") as Gran) ? (q.get("gran") as Gran) : allowed[0];
  const rawMetric = q.get("metric") as MetricKey;
  let metric: MetricKey = METRICS.includes(rawMetric) ? rawMetric : "visitors";
  // 实时只有访客与浏览量两条曲线
  if (range === "realtime" && metric !== "pageviews") metric = "visitors";
  const rawCmp = q.get("cmp") as CompareKey;
  return {
    ...r,
    compare: rawCmp === "off" || rawCmp === "year_over_year" ? rawCmp : "previous_period",
    gran,
    metric,
    filters: decodeFilters(q.get("f")),
  };
}

// ---------- 结果 ----------

export interface Kpi {
  /** current：当前在线（只在实时视图出现，不能作为主图指标） */
  key: MetricKey | "current";
  value: number;
  prev: number | null;
}

/** 分解行：维度取值（服务端的原始取值，显示名见 values.ts）与访客数。 */
export interface Row {
  label: string;
  visitors: number;
  /** 城市行所在的国家（ISO 代码），界面上和城市名一起显示 */
  country?: string;
}

export interface GoalRow {
  /** 目标的名字 */
  label: string;
  visitors: number;
  /** 完成次数 */
  events: number;
  /** 0–1 */
  cr: number;
}

export interface DashResult {
  /** 主区间与对比期（站点本地日；图例、导出文件名用） */
  period: Period;
  comparePeriod: Period | null;
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
  /** 每个有转化的目标一行（没有时整张目标卡片不显示） */
  goals: GoalRow[];
  /** 范围伸到了原始数据保留期之前、而日汇总层答不了这次查询（meta.warnings: range_beyond_raw_retention） */
  beyondRetention: boolean;
  /** 导入的历史数据没有算进这次查询（meta.warnings: imported_data_excluded） */
  importedExcluded: boolean;
}

// ---------- 分解的原始取值 ----------

/** 夹具里没有的维度：按份额生成。 */
const EXTRA_DIMS: Record<string, [string, number][]> = {
  utm_source: [
    ["newsletter", 0.05],
    ["twitter", 0.03],
    ["hackernews", 0.02],
  ],
  utm_medium: [
    ["email", 0.05],
    ["social", 0.035],
    ["cpc", 0.012],
  ],
};

/** 夹具里的地区代码 → ISO 3166-2（服务端存 `<国家>-<第一级子区划代码>`，界面原样显示代码）。 */
const REGION_ISO: Record<string, string> = { "CN-SH": "CN-31", "CN-BJ": "CN-11", "CN-GD": "CN-44", "CN-ZJ": "CN-33" };

/** 服务端返回的是原始取值（国家是 ISO 代码，地区是 ISO 3166-2），中文名、国家名在界面按语言翻译。 */
function rawLabel(dim: string, r: FixtureRow): string {
  if (dim === "country") return r.code ?? r.label;
  if (dim === "region") return r.code ? (REGION_ISO[r.code] ?? r.code) : r.label;
  return r.label;
}

function rowsFor(site: Site, dim: string, total: number): Row[] {
  if (dim === "hostname") {
    return [
      { label: site.domain, visitors: Math.max(1, Math.round(total * 0.86)) },
      { label: `www.${site.domain}`, visitors: Math.max(1, Math.round(total * 0.14)) },
    ];
  }
  const extra = EXTRA_DIMS[dim];
  if (extra) return extra.map(([label, share]) => ({ label, visitors: Math.max(1, Math.round(total * share)) }));
  return breakdown(site, dim, total).map((r) => ({
    label: rawLabel(dim, r),
    visitors: r.visitors,
    ...(dim === "city" && r.code ? { country: r.code } : {}),
  }));
}

/** 筛选对话框里的候选值：该维度的排行。 */
export function suggestValues(site: Site, dim: string): string[] {
  return site.scale === 0 ? [] : rowsFor(site, dim, 10000).map((r) => r.label);
}

/** 这个维度自己的筛选作用在排行上：等于只留所选值，不等于去掉所选值，包含 / 不包含按子串匹配。 */
function applyOwn(rows: Row[], own: Filter | undefined): Row[] {
  if (!own) return rows;
  const hit = (label: string) =>
    own.op === "is" || own.op === "is_not"
      ? own.values.includes(label)
      : own.values.some((v) => label.toLowerCase().includes(v.toLowerCase()));
  const keep = own.op === "is" || own.op === "contains";
  return rows.filter((r) => hit(r.label) === keep);
}

/** 筛选把流量按份额缩小：is 取所选值的份额之和，is_not 取剩余，contains 估 30%，contains_not 估 70%。 */
function filterFactor(site: Site, filters: Filter[]): number {
  let f = 1;
  for (const flt of filters) {
    const rows = rowsFor(site, flt.dim, 10000);
    const picked = rows.filter((r) => flt.values.includes(r.label)).reduce((a, r) => a + r.visitors, 0) / 10000;
    const share = Math.min(1, picked || 0.05);
    if (flt.op === "is") f *= Math.max(0.02, share);
    else if (flt.op === "is_not") f *= Math.max(0.05, 1 - share);
    else if (flt.op === "contains") f *= 0.3;
    else f *= 0.7;
  }
  return f;
}

// ---------- 模拟的请求 ----------

export function isAbort(e: unknown): boolean {
  return e instanceof DOMException && e.name === "AbortError";
}

/** 模拟一次请求：隔一小会儿返回；中途取消就中止；评审开关选了出错、排队、超时时失败。 */
export function simulateFetch<T>(value: () => T, signal: AbortSignal, ms = 300): Promise<T> {
  return new Promise((resolve, reject) => {
    const id = setTimeout(() => {
      const ds = app.review.dataState;
      if (ds === "error" || ds === "busy" || ds === "timeout") reject(new Error(ds));
      else resolve(value());
    }, ms);
    signal.addEventListener(
      "abort",
      () => {
        clearTimeout(id);
        reject(new DOMException("Aborted", "AbortError"));
      },
      { once: true },
    );
  });
}

// ---------- 页面的滚动深度与停留时间 ----------

/** 页面的平均滚动深度（0–100）与平均停留时间（秒）；站点没开「页面停留时长」采集时没有（正式实现：这个范围里没有 engagement 事件）。 */
export interface PageEngagement {
  scroll_depth: number;
  time_on_page: number;
}

export function pageEngagement(site: Site, page: string): PageEngagement | null {
  if (!cfgs[site.id]?.script.engagement) return null;
  // 按页面路径固定生成：首页滚得浅、文档和博客读得久
  let h = site.id * 131;
  for (const c of page) h = (h * 31 + c.charCodeAt(0)) % 100_003;
  const kind = page === "/" ? 0 : page.startsWith("/blog") ? 1 : page.startsWith("/docs") ? 2 : 3;
  const depth = [44, 68, 61, 52][kind] + (h % 17) - 8;
  const secs = [38, 164, 131, 57][kind] + (h % 41) - 20;
  return { scroll_depth: Math.max(5, Math.min(100, depth)), time_on_page: Math.max(4, secs) };
}

// ---------- 地理库 ----------

/** 地理库的署名：服务端随含位置维度的查询一起返回（meta.geo_attribution），文字固定是英文。 */
export function geoAttribution(site: Site): { text: string; url: string } {
  return geoCapability(site) === "city"
    ? { text: "This product includes GeoLite Data created by MaxMind", url: "https://www.maxmind.com" }
    : { text: "IP Geolocation by DB-IP", url: "https://db-ip.com" };
}

/** 地理库只有国家级（meta.geo_capability = country，默认的 DB-IP Lite）：位置卡片只有国家，并说明原因。 */
export function geoCountryOnly(site: Site): boolean {
  return geoCapability(site) === "country";
}

/**
 * 位置卡片的级别：地理库的能力和站点的地理精度（geo_level）取低的那个。城市级库下可以到地区、城市；
 * 站点自己选了「国家」时也只有国家（这是站点的选择，不用解释）。
 */
export function geoLevels(site: Site): ("country" | "region" | "city")[] {
  if (geoCapability(site) !== "city") return ["country"];
  const level = cfgs[site.id]?.geo ?? "city";
  return level === "country" ? ["country"] : level === "region" ? ["country", "region"] : ["country", "region", "city"];
}

// ---------- 提示：保留期与导入 ----------

const DAY = 864e5;

interface Span {
  start: Date;
  end: Date;
}

/** 站点已完成的导入覆盖的日期范围。 */
function importedSpan(site: Site): Span | null {
  const done = (cfgs[site.id]?.imports ?? []).filter((i) => i.status === "completed" && i.from && i.to);
  if (!done.length) return null;
  const from = Math.min(...done.map((i) => Date.parse(`${i.from}T00:00:00`)));
  const to = Math.max(...done.map((i) => Date.parse(`${i.to}T23:59:59`)));
  return { start: new Date(from), end: new Date(to) };
}

/** 站点最早有数据的本地日：「全部时间」的起点。 */
export function firstDay(site: Site): Date {
  const imported = importedSpan(site)?.start;
  const created = new Date(NOW.getFullYear(), NOW.getMonth(), NOW.getDate() - site.createdDaysAgo);
  return imported && imported < created ? imported : created;
}

/** 范围的区间：起点与终点（不含），终点封顶到现在。 */
function spanOf(p: Period): Span {
  if (p.kind === "instant") return { start: p.from, end: p.to < NOW ? p.to : NOW };
  const end = new Date(p.to.getFullYear(), p.to.getMonth(), p.to.getDate() + 1);
  return { start: p.from, end: end < NOW ? end : NOW };
}

function periodsOf(site: Site, s: DashState): { cur: Period; cmp: Period | null } {
  const cur = resolve(s, NOW, firstDay(site));
  const cmp = s.compare === "off" || s.range === "realtime" ? null : comparePeriod(cur, s.compare, NOW);
  return { cur, cmp };
}

/**
 * 日汇总层答不了的查询：原始层里保留期之前的天没有数据，导入的天（只在汇总层）算不进来。
 * 汇总层能答的查询（整本地日的范围、天以上的粒度、没有筛选）没有这两个提示；规则见 system-design §6.4。
 */
function warningsOf(site: Site, s: DashState): { beyondRetention: boolean; importedExcluded: boolean } {
  const compatible = !s.filters.length && s.range !== "realtime" && s.range !== "24h" && !s.from?.includes("T") && s.gran !== "minute" && s.gran !== "hour";
  if (compatible) return { beyondRetention: false, importedExcluded: false };
  const { cur, cmp } = periodsOf(site, s);
  const spans = cmp ? [spanOf(cur), spanOf(cmp)] : [spanOf(cur)];
  const keep = Number(cfgs[site.id]?.retention) || INSTANCE_RETENTION;
  const since = NOW.getTime() - keep * DAY;
  const imported = importedSpan(site);
  return {
    beyondRetention: spans.some((x) => x.start.getTime() < since),
    importedExcluded: !!imported && spans.some((x) => x.start.getTime() <= imported.end.getTime() && x.end.getTime() >= imported.start.getTime()),
  };
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

/** 按月合并按天的点（自定义的长范围按月看）。 */
function monthsOf(points: Point[]): Point[] {
  const out: Point[][] = [];
  for (const p of points) {
    const last = out.at(-1);
    if (last && last[0].t.getMonth() === p.t.getMonth() && last[0].t.getFullYear() === p.t.getFullYear()) last.push(p);
    else out.push([p]);
  }
  return out.map((chunk) => {
    const visits = chunk.reduce((a, p) => a + p.visits, 0);
    const pv = chunk.reduce((a, p) => a + p.pageviews, 0);
    return {
      t: new Date(chunk[0].t.getFullYear(), chunk[0].t.getMonth(), 1),
      visitors: Math.round(chunk.reduce((a, p) => a + p.visitors, 0) * 0.8),
      visits,
      pageviews: pv,
      bounce_rate: chunk.reduce((a, p) => a + p.bounce_rate * p.visits, 0) / (visits || 1),
      visit_duration: chunk.reduce((a, p) => a + p.visit_duration * p.visits, 0) / (visits || 1),
      views_per_visit: pv / (visits || 1),
    };
  });
}

const HOUR = 36e5;
const floorHour = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate(), d.getHours());

/**
 * 任意区间的序列（昨天、上个月、自定义范围，以及它们的对比期）：未来的部分不出点。
 * 预设走下面的 series()，保持原来的样子。
 */
function periodSeries(site: Site, p: Period, gran: Gran, factor: number): Point[] {
  const today = new Date(NOW.getFullYear(), NOW.getMonth(), NOW.getDate());
  if (gran === "hour" || gran === "minute") {
    const end = p.kind === "instant" ? new Date(p.to.getTime() - 1) : new Date(p.to.getFullYear(), p.to.getMonth(), p.to.getDate(), 23);
    const last = floorHour(end < NOW ? end : NOW);
    const first = floorHour(p.from);
    const hours = Math.round((last.getTime() - first.getTime()) / HOUR) + 1;
    if (hours <= 0) return [];
    return hourly(site, hours, Math.round((floorHour(NOW).getTime() - last.getTime()) / HOUR), factor);
  }
  const last = p.to > today ? today : p.to;
  const days = Math.round((last.getTime() - p.from.getTime()) / DAY) + 1;
  if (days <= 0) return [];
  const d = daily(site, days, Math.round((today.getTime() - last.getTime()) / DAY), factor);
  return gran === "week" ? weeks(d) : gran === "month" ? monthsOf(d) : d;
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
    default: {
      // 昨天、上个月、自定义范围：按区间生成；对比期就是对比区间里的数据
      const { cur, cmp } = periodsOf(site, s);
      return periodSeries(site, prev && cmp ? cmp : cur, s.gran, factor);
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

/** blank：范围里一条数据也没有（实时视图不显示空状态，所有数字都是 0）。 */
export function runQuery(site: Site, s: DashState, lang: Lang, blank = false): DashResult {
  const factor = filterFactor(site, s.filters);
  const zeroed = (ps: Point[]): Point[] =>
    ps.map((p) => ({ ...p, visitors: 0, visits: 0, pageviews: 0, bounce_rate: 0, visit_duration: 0, views_per_visit: 0 }));
  const points = blank ? zeroed(series(site, s, factor, false)) : series(site, s, factor, false);
  const cmpOn = s.compare !== "off" && s.range !== "realtime";
  const prevPoints = cmpOn ? series(site, s, factor, true).slice(-points.length) : null;
  const { cur, cmp } = periodsOf(site, s);
  const { x, tip } = labels(points, s.gran, lang);
  const prevTip = prevPoints ? labels(prevPoints, s.gran, lang).tip : [];
  // 包含「现在」的范围（过去 N 天含今天，6mo、12mo 含本月）最后一个桶不完整；昨天、上个月这类过去的范围是完整的
  const partialLast = spanOf(cur).end >= NOW;
  let kpis: Kpi[] = METRICS.map((key) => ({
    key,
    value: total(points, key),
    prev: prevPoints ? total(prevPoints, key) : null,
  }));
  if (s.range === "realtime") {
    // 实时：当前在线（过去 5 分钟）+ 过去 30 分钟的独立访客与浏览量；每分钟访客不能相加
    const uniq = Math.round(points.reduce((a, p) => a + p.visitors, 0) * 0.31);
    kpis = [
      { key: "current", value: blank ? 0 : site.live, prev: null },
      { key: "visitors", value: uniq, prev: null },
      { key: "pageviews", value: total(points, "pageviews"), prev: null },
    ];
  }
  const totalVisitors = kpis.find((k) => k.key === "visitors")!.value;
  const ownOf = (dim: string) => s.filters.find((f) => f.dim === dim);

  // 每个目标一行（只有有转化的目标）：完成次数按每个访客的平均次数估，转化率是转化访客占全部访客的份额
  const goals: GoalRow[] = blank
    ? []
    : applyOwn(rowsFor(site, "goal", totalVisitors), ownOf("goal")).map((r, i) => ({
        label: r.label,
        visitors: r.visitors,
        events: Math.round(r.visitors * [1.3, 2.4, 1.1, 1.6][i % 4]),
        cr: totalVisitors ? Math.min(1, r.visitors / totalVisitors) : 0,
      }));

  return {
    period: capped(cur, NOW),
    comparePeriod: cmp,
    points,
    prevPoints,
    xLabels: x,
    tipLabels: tip,
    prevTipLabels: prevTip,
    partialLast,
    kpis,
    totalVisitors,
    factor,
    breakdown: (dim) => (blank ? [] : applyOwn(rowsFor(site, dim, totalVisitors), ownOf(dim))),
    goals,
    ...warningsOf(site, s),
  };
}
