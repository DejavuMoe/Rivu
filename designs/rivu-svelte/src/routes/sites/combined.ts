// 合并看板的「查询」：把组织里当前用户能读的每个站点各查一次，按时间桶相加。
// 正式实现是一次多站点查询，权限检查与单站点相同（只含能读的站点；组织里能读的站点不足两个时整页 404）。
// Rivu 不跨站点识别访客，所以访客是各站点访客数之和，不是去重的人数。

import type { Point, Site } from "../../lib/data/fixtures";
import { SITES } from "../../lib/data/sites.svelte";
import type { Lang } from "../../lib/state.svelte";
import { NOW } from "../../lib/data/fixtures";
import { RANGES, firstDay, grans, runQuery, type Gran, type Kpi, type MetricKey, type RangeKey } from "../dashboard/query";
import { readRange, type Period, type RangeParam } from "../dashboard/period";

/** 组织里当前用户能读的站点（原型里侧栏与站点列表列出的就是能读的）。 */
export function readableSites(orgId: string): Site[] {
  return SITES.filter((s) => s.org === orgId);
}

/** 合并看板只在组织里能读的站点有两个及以上时出现。 */
export function combinedAvailable(orgId: string): boolean {
  return readableSites(orgId).length >= 2;
}

/** 合并看板没有实时（实时是单站点的在线人数）。 */
export const COMBINED_RANGES: RangeKey[][] = RANGES.filter((g) => !g.includes("realtime"));

export const COMBINED_METRICS: MetricKey[] = ["visitors", "visits", "pageviews", "bounce_rate", "visit_duration"];

/** 范围（range，自定义时还有 from、to）+ 粒度、指标。 */
export interface CombinedState extends RangeParam {
  gran: Gran;
  metric: MetricKey;
}

export function readCombined(q: URLSearchParams): CombinedState {
  // 范围的写法与站点看板相同（含自定义的 from、to），只是没有实时
  const read = readRange(q, NOW);
  const r: RangeParam = read.range === "realtime" ? { range: "30d" } : read;
  const allowed = grans(r);
  const gran = allowed.includes(q.get("gran") as Gran) ? (q.get("gran") as Gran) : allowed[0];
  const m = q.get("metric") as MetricKey;
  return { ...r, gran, metric: COMBINED_METRICS.includes(m) ? m : "visitors" };
}

export interface SiteRow {
  site: Site;
  visitors: number;
  visits: number;
  pageviews: number;
  bounce_rate: number;
  visit_duration: number;
  /** 这个站点的访客占各站点访客之和的份额，0–1 */
  share: number;
}

export interface CombinedResult {
  /** 查询区间（终点封顶到今天）：范围按钮文字、前后周期用 */
  period: Period;
  points: Point[];
  xLabels: string[];
  tipLabels: string[];
  partialLast: boolean;
  kpis: Kpi[];
  rows: SiteRow[];
  /** 各站点访客之和 */
  visitorsSum: number;
}

const kpi = (k: Kpi[], key: MetricKey) => k.find((x) => x.key === key)?.value ?? 0;

export function runCombined(sites: Site[], s: CombinedState, lang: Lang, blank = false): CombinedResult {
  const per = sites.map((site) => ({
    site,
    res: runQuery(site, { range: s.range, from: s.from, to: s.to, compare: "off", gran: s.gran, metric: s.metric, filters: [] }, lang, blank),
  }));
  const base = per[0].res;

  // 每个时间桶：访客、访问、浏览量相加；跳出率与访问时长按访问数加权
  const points: Point[] = base.points.map((p, i) => {
    let visitors = 0;
    let visits = 0;
    let pageviews = 0;
    let bounced = 0;
    let dur = 0;
    for (const { res } of per) {
      const q = res.points[i];
      if (!q) continue;
      visitors += q.visitors;
      visits += q.visits;
      pageviews += q.pageviews;
      bounced += q.bounce_rate * q.visits;
      dur += q.visit_duration * q.visits;
    }
    return {
      t: p.t,
      visitors,
      visits,
      pageviews,
      bounce_rate: visits ? bounced / visits : 0,
      visit_duration: visits ? dur / visits : 0,
      views_per_visit: visits ? pageviews / visits : 0,
    };
  });

  // 范围里没有访客的站点整行都是 0（表格里显示「—」）
  const raw = per.map(({ site, res }) => {
    const visitors = kpi(res.kpis, "visitors");
    const on = visitors > 0 ? 1 : 0;
    return {
      site,
      visitors,
      visits: on * kpi(res.kpis, "visits"),
      pageviews: on * kpi(res.kpis, "pageviews"),
      bounce_rate: on * kpi(res.kpis, "bounce_rate"),
      visit_duration: on * kpi(res.kpis, "visit_duration"),
    };
  });
  const visitorsSum = raw.reduce((a, r) => a + r.visitors, 0);
  const visits = raw.reduce((a, r) => a + r.visits, 0);
  const rows: SiteRow[] = raw
    .map((r) => ({ ...r, share: visitorsSum ? r.visitors / visitorsSum : 0 }))
    .sort((a, b) => b.visitors - a.visitors || a.site.name.localeCompare(b.site.name));

  const kpis: Kpi[] = [
    { key: "visitors", value: visitorsSum, prev: null },
    { key: "visits", value: visits, prev: null },
    { key: "pageviews", value: raw.reduce((a, r) => a + r.pageviews, 0), prev: null },
    { key: "bounce_rate", value: visits ? raw.reduce((a, r) => a + r.bounce_rate * r.visits, 0) / visits : 0, prev: null },
    { key: "visit_duration", value: visits ? raw.reduce((a, r) => a + r.visit_duration * r.visits, 0) / visits : 0, prev: null },
  ];

  return { period: base.period, points, xLabels: base.xLabels, tipLabels: base.tipLabels, partialLast: base.partialLast, kpis, rows, visitorsSum };
}

/** 几个站点里最早有数据的本地日：「全部时间」的起点。 */
export function combinedFirstDay(sites: Site[]): Date {
  return new Date(Math.min(...sites.map((s) => firstDay(s).getTime())));
}
