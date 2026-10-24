// 站点设置的分区与可见性（只决定界面上显示与否，服务端仍按自己的规则判定）。
import type { Role } from "../api.ts";

export type SectionKey = "general" | "hostnames" | "collection" | "privacy" | "shield" | "sharing" | "danger";

export interface Access {
  role: Role | null;
  access: "member" | "guest";
}

/** 分区顺序即导航顺序。 */
export const SECTIONS: SectionKey[] = ["general", "hostnames", "collection", "privacy", "shield", "sharing", "danger"];

const isMember = (s: Access) => s.access === "member";
const isManager = (s: Access) => isMember(s) && (s.role === "owner" || s.role === "admin");
const isEditor = (s: Access) => !!s.role && s.role !== "viewer";

/** 能进入这个分区：设置类与危险操作需要组织的 owner / admin；屏蔽规则 editor 起可看；共享链接要组织角色 editor 起（站点访客不行）。 */
export function sectionVisible(key: SectionKey, s: Access): boolean {
  switch (key) {
    case "shield":
      return isEditor(s);
    case "sharing":
      return isMember(s) && isEditor(s);
    default:
      return isManager(s);
  }
}

/** 进入后只读：屏蔽规则只有 owner / admin 能写。 */
export function sectionReadOnly(key: SectionKey, s: Access): boolean {
  return key === "shield" && !isManager(s);
}

export function visibleSections(s: Access): SectionKey[] {
  return SECTIONS.filter((k) => sectionVisible(k, s));
}

export function isSectionKey(v: string | undefined): v is SectionKey {
  return SECTIONS.includes(v as SectionKey);
}

/** 能管理共享链接（看板页头的「分享」入口）。 */
export function canShareLinks(s: Access): boolean {
  return sectionVisible("sharing", s);
}
