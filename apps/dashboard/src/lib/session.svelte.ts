// 登录用户、所属组织与可见站点。应用外壳（(app)/+layout）加载一次；登出、切换组织时更新。
// 权限判断只用于界面上显示与否，服务端仍按自己的规则判定（无权访问一律 404）。

import { api, ApiError, type Me, type Org, type Role, type Site } from "./api";

const ORG_KEY = "rivu.org";

export const session = $state({
  me: null as Me | null,
  sites: [] as Site[],
  /** 当前组织（侧栏切换）；站点页会切到站点所属的组织 */
  orgId: null as number | null,
});

function rememberedOrg(): number | null {
  try {
    const v = Number(localStorage.getItem(ORG_KEY));
    return Number.isFinite(v) && v > 0 ? v : null;
  } catch {
    return null;
  }
}

/** 加载会话。没有登录时抛出 401 的 ApiError。 */
export async function loadSession(): Promise<void> {
  const [me, { sites }] = await Promise.all([api.me(), api.sites()]);
  session.me = me;
  session.sites = sites;
  const known = new Set([...me.organizations.map((o) => o.id), ...sites.map((s) => s.organization.id)]);
  const want = session.orgId ?? rememberedOrg();
  session.orgId = want !== null && known.has(want) ? want : (me.organizations[0]?.id ?? sites[0]?.organization.id ?? null);
}

export async function refreshSites(): Promise<void> {
  session.sites = (await api.sites()).sites;
}

export function setOrg(id: number) {
  session.orgId = id;
  try {
    localStorage.setItem(ORG_KEY, String(id));
  } catch {
    /* 忽略 */
  }
}

export function clearSession() {
  session.me = null;
  session.sites = [];
}

export function isUnauthorized(e: unknown): boolean {
  return e instanceof ApiError && e.status === 401;
}

/** 侧栏列出的组织：用户是成员的组织，加上只以站点访客身份可见的组织。 */
export function orgs(): { id: number; name: string; role: Role | null }[] {
  const out: { id: number; name: string; role: Role | null }[] = (session.me?.organizations ?? []).map((o: Org) => ({
    id: o.id,
    name: o.name,
    role: o.role,
  }));
  for (const s of session.sites) {
    if (!out.some((o) => o.id === s.organization.id)) out.push({ id: s.organization.id, name: s.organization.name, role: null });
  }
  return out;
}

export function currentOrg() {
  return orgs().find((o) => o.id === session.orgId) ?? null;
}

export function orgSites(orgId: number | null = session.orgId): Site[] {
  return session.sites.filter((s) => s.organization.id === orgId);
}

export function findSite(id: number): Site | undefined {
  return session.sites.find((s) => s.id === id);
}

/** 能否写站点内容（目标、分群）与管理共享链接：editor 及以上。 */
export function canEdit(site: Site | undefined): boolean {
  return !!site?.role && site.role !== "viewer";
}

/** 能否改站点设置：owner / admin。 */
export function canManage(site: Site | undefined): boolean {
  return site?.role === "owner" || site?.role === "admin";
}

export function displayName(): string {
  const u = session.me?.user;
  return u?.name || u?.email || "";
}
