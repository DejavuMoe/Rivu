// 站点设置的分区与可见性（对应 apps/dashboard 的 lib/admin/sections.ts）：只决定界面上显示与否。
// 站点页头的「设置」标签链接到第一个可见分区，一个都看不到时没有这个标签；设置页的分区导航也按它过滤。

import { canEdit, siteAccess } from "../state.svelte";
import type { Site } from "./fixtures";

export type SectionKey = "general" | "hostnames" | "collection" | "privacy" | "shield" | "goals" | "sharing" | "imports" | "danger";

/** 分区顺序即导航顺序。 */
export const SECTIONS: SectionKey[] = ["general", "hostnames", "collection", "privacy", "shield", "goals", "sharing", "imports", "danger"];

type Access = ReturnType<typeof siteAccess>;

const isMember = (s: Access) => s.access === "member";
const isManager = (s: Access) => isMember(s) && (s.role === "owner" || s.role === "admin");
const isEditor = (s: Access) => !!s.role && s.role !== "viewer";

/**
 * 能进入这个分区：设置类、导入与危险操作需要组织的 owner / admin；屏蔽规则与目标 editor 起可看（含站点访客 editor）；
 * 共享链接要组织角色 editor 起（站点访客不行）。
 */
export function sectionVisible(key: SectionKey, site: Site): boolean {
  const s = siteAccess(site);
  switch (key) {
    case "shield":
    case "goals":
      return isEditor(s);
    case "sharing":
      return isMember(s) && isEditor(s);
    default:
      return isManager(s);
  }
}

/** 进入后只读：屏蔽规则只有 owner / admin 能写；目标 editor 起都能写。 */
export function sectionReadOnly(key: SectionKey, site: Site): boolean {
  const s = siteAccess(site);
  if (key === "shield") return !isManager(s);
  if (key === "goals") return !isEditor(s);
  return false;
}

export function visibleSections(site: Site): SectionKey[] {
  return SECTIONS.filter((k) => sectionVisible(k, site));
}

export function isSectionKey(v: string | undefined): v is SectionKey {
  return SECTIONS.includes(v as SectionKey);
}

/** 能管理共享链接（看板页头的「分享」入口）。 */
export function canShareLinks(site: Site): boolean {
  return sectionVisible("sharing", site);
}

/** 能导出 CSV（看板页头的「导出」入口）：有效角色 editor 起，含站点访客 editor。 */
export function canExport(site: Site): boolean {
  return canEdit(site);
}
