// 站点的可变状态：当前站点列表，以及删除后仍在 7 天恢复期内的站点。
// 删除、恢复会改变这两个数组，侧栏、站点列表等都跟着更新。

import { NOW, SEED_SITES, type Site } from "./fixtures";

export const RESTORE_DAYS = 7;

export const SITES = $state<Site[]>([...SEED_SITES]);

function ago(days: number, hour: number): Date {
  const d = new Date(NOW);
  d.setDate(d.getDate() - days);
  d.setHours(hour, 0, 0, 0);
  return d;
}

export const DELETED_SITES = $state<Site[]>([
  { id: 7, org: "o1", name: "Hackathon 2026", domain: "hack.northwind.dev", siteKey: "h8kq4vz3npw2", timezone: "Asia/Shanghai", isPublic: false, scale: 0.2, live: 0, createdDaysAgo: 150, deletedAt: ago(2, 10) },
  { id: 8, org: "o1", name: "旧官网", domain: "old.northwind.dev", siteKey: "o9ld5wb7xtm1", timezone: "Asia/Shanghai", isPublic: false, scale: 0.4, live: 0, createdDaysAgo: 500, deletedAt: ago(5, 9) },
  { id: 9, org: "o1", name: "Beta Feedback", domain: "beta.northwind.dev", siteKey: "b3ta8fk2qzr6", timezone: "UTC", isPublic: false, scale: 0.1, live: 0, createdDaysAgo: 80, deletedAt: ago(7, 18) },
]);

/** 距离永久删除还有几天（按日历日算）；0 表示今天。 */
export function daysUntilPurge(s: Site): number {
  const purge = new Date(s.deletedAt ?? NOW);
  purge.setDate(purge.getDate() + RESTORE_DAYS);
  const day = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  return Math.max(0, Math.round((day(purge) - day(NOW)) / 864e5));
}

/** 删除站点：移出列表，进入恢复期。 */
export function deleteSite(id: number) {
  const i = SITES.findIndex((s) => s.id === id);
  if (i < 0) return;
  const [s] = SITES.splice(i, 1);
  DELETED_SITES.unshift({ ...s, live: 0, deletedAt: new Date(NOW) });
}

/** 恢复站点：回到列表。 */
export function restoreSite(id: number) {
  const i = DELETED_SITES.findIndex((s) => s.id === id);
  if (i < 0) return;
  const [s] = DELETED_SITES.splice(i, 1);
  const restored: Site = { ...s };
  delete restored.deletedAt;
  SITES.push(restored);
}
