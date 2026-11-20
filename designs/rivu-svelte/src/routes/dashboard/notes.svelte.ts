// 图表批注：钉在某一天上的一句话（如「发布 v2.0」），整个站点共用。
// 一条批注 = 站点本地日期 + 最多 255 个字符；每个站点最多 500 条。
// editor 起（含站点访客 editor）能添加、修改、删除；查看者（含站点访客 viewer）只读；共享链接与公开看板不显示。

import { NOW, type Point } from "../../lib/data/fixtures";
import { app } from "../../lib/state.svelte";
import { ymd } from "../settings/store.svelte";
import type { Gran } from "./query";

export interface Note {
  id: number;
  site: number;
  /** 站点本地日期 YYYY-MM-DD */
  date: string;
  text: string;
}

export const NOTE_TEXT_MAX = 255;
export const NOTES_PER_SITE = 500;

// 日期对上 fixtures 里流量的两个尖峰（10 月 5 日的发布、9 月 2 日的转载）
const SEED: Note[] = [
  { id: 1, site: 1, date: "2026-10-05", text: "发布 v2.0" },
  { id: 2, site: 1, date: "2026-10-05", text: "定价页改版上线" },
  { id: 3, site: 1, date: "2026-09-02", text: "被 Hacker News 首页转载" },
  { id: 4, site: 1, date: "2026-10-20", text: "切换到新的 CDN" },
  { id: 5, site: 2, date: "2026-10-05", text: "文档站改版" },
];

export const NOTES = $state<Note[]>(structuredClone(SEED));
let seq = 100;

/** 站点的批注，按日期排序。评审面板「图表批注」选「无」时一条都没有。 */
export function notesOf(siteId: number): Note[] {
  if (app.review.notes === "none") return [];
  return NOTES.filter((n) => n.site === siteId).sort((a, b) => a.date.localeCompare(b.date) || a.id - b.id);
}

/** 站点已有的批注条数；评审面板选「已满」时按 500 条算。 */
export function noteCount(siteId: number): number {
  return app.review.notes === "full" ? NOTES_PER_SITE : notesOf(siteId).length;
}

export function addNote(siteId: number, date: string, text: string): Note {
  const n: Note = { id: ++seq, site: siteId, date, text: text.trim() };
  NOTES.push(n);
  return n;
}

export function updateNote(id: number, text: string) {
  const n = NOTES.find((x) => x.id === id);
  if (n) n.text = text.trim();
}

export function deleteNote(id: number) {
  const i = NOTES.findIndex((x) => x.id === id);
  if (i >= 0) NOTES.splice(i, 1);
}

export const today = () => ymd(NOW);

/** 批注落在第几个时间桶：按天、按周、按月取包含这一天的桶，按小时取这一天的第一个小时；不在范围里为 -1。 */
export function bucketIndex(points: Point[], gran: Gran, date: string): number {
  if (gran === "minute" || !points.length) return -1;
  const d = new Date(`${date}T00:00:00`);
  const same = (a: Date) => a.getFullYear() === d.getFullYear() && a.getMonth() === d.getMonth() && a.getDate() === d.getDate();
  if (gran === "hour" || gran === "day") return points.findIndex((p) => same(p.t));
  let i = -1;
  for (let k = 0; k < points.length; k++) if (points[k].t.getTime() <= d.getTime()) i = k;
  if (i < 0) return -1;
  const start = points[i].t;
  const end = gran === "week" ? new Date(start.getFullYear(), start.getMonth(), start.getDate() + 7) : new Date(start.getFullYear(), start.getMonth() + 1, 1);
  return d.getTime() < end.getTime() ? i : -1;
}
