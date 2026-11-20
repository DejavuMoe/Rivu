// 分群：保存下来的一组筛选条件，整个站点共用（segments 表，site 级，没有 owner）。
// 能读这个站点的人都能应用；editor 起（含站点访客 editor）能保存、改名、删除。
// 看板 URL 用 seg=<id> 引用分群，分群的条件与 f= 里的筛选同时生效（隐式 AND）。
// 共享链接可以绑定一个分群；被绑定的分群不能删除，先在共享链接里撤销。

import { app } from "../../lib/state.svelte";
import { cfgs } from "../settings/store.svelte";
import type { Filter } from "./query";

export interface Segment {
  id: number;
  site: number;
  name: string;
  filters: Filter[];
}

/** 名称上限（与 segments.name 的约束一致，按字符计）。 */
export const SEGMENT_NAME_MAX = 100;

const SEED: Segment[] = [
  {
    id: 101,
    site: 1,
    name: "移动端搜索访客",
    filters: [
      { dim: "device", op: "is", values: ["mobile"] },
      { dim: "channel", op: "is", values: ["Organic Search"] },
    ],
  },
  { id: 102, site: 1, name: "中国访客", filters: [{ dim: "country", op: "is", values: ["CN"] }] },
  { id: 103, site: 1, name: "博客读者", filters: [{ dim: "page", op: "contains", values: ["/blog/"] }] },
  { id: 201, site: 2, name: "入门文档", filters: [{ dim: "page", op: "contains", values: ["/docs/getting-started"] }] },
];

export const SEGMENTS = $state<Segment[]>(structuredClone(SEED));

let seq = 900;

/** 站点的分群，按名称排序。评审面板「分群」选「无」时一个都没有。 */
export function segmentsOf(siteId: number): Segment[] {
  if (app.review.segments === "none") return [];
  return SEGMENTS.filter((s) => s.site === siteId).sort((a, b) => a.name.localeCompare(b.name, "zh-CN"));
}

export function findSegment(siteId: number, id: number | null): Segment | undefined {
  return id === null ? undefined : segmentsOf(siteId).find((s) => s.id === id);
}

/** 同一站点里名称不能重复（不区分大小写，去掉首尾空白后比较）。 */
export function nameTaken(siteId: number, name: string, except?: number): boolean {
  const k = name.trim().toLowerCase();
  return segmentsOf(siteId).some((s) => s.id !== except && s.name.trim().toLowerCase() === k);
}

export function addSegment(siteId: number, name: string, filters: Filter[]): Segment {
  const s: Segment = { id: ++seq, site: siteId, name: name.trim(), filters: structuredClone(filters) };
  SEGMENTS.push(s);
  return s;
}

export function renameSegment(id: number, name: string) {
  const s = SEGMENTS.find((x) => x.id === id);
  if (s) s.name = name.trim();
}

export function deleteSegment(id: number) {
  const i = SEGMENTS.findIndex((x) => x.id === id);
  if (i >= 0) SEGMENTS.splice(i, 1);
}

/** 绑定了这个分群的共享链接（包括已过期但还没撤销的）。 */
export function linksUsing(siteId: number, id: number) {
  return (cfgs[siteId]?.links ?? []).filter((l) => l.segment === id);
}
