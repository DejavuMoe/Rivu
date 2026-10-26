// 成员与 API 密钥页的纯逻辑：谁能改谁、密钥范围与有效期的归类、示例请求（node --test）。
// 服务端按同样的规则判定（platform-and-ops.md §4.1），这里只决定界面上显示哪些操作。

import type { Role } from "../api.ts";

const MS_DAY = 864e5;

export const ASSIGNABLE_BY_OWNER: Role[] = ["owner", "admin", "editor", "viewer"];
export const ASSIGNABLE_BY_ADMIN: Role[] = ["admin", "editor", "viewer"];

/** 当前登录者能指派的角色；不能管理成员时为空。 */
export function assignableRoles(actor: Role): Role[] {
  if (actor === "owner") return ASSIGNABLE_BY_OWNER;
  if (actor === "admin") return ASSIGNABLE_BY_ADMIN;
  return [];
}

/** 邀请时可选的角色：永远不能邀请所有者。 */
export function invitableRoles(actor: Role): Exclude<Role, "owner">[] {
  return assignableRoles(actor).filter((r): r is Exclude<Role, "owner"> => r !== "owner");
}

export interface EditCtx {
  /** 接口返回的 can_manage */
  canManage: boolean;
  actorRole: Role;
  ownerCount: number;
}

/** 这一行能否被改角色或移除：不能动自己；管理员不能动所有者；不能动最后一位所有者。 */
export function canEditMember(m: { me: boolean; role: Role }, ctx: EditCtx): boolean {
  if (!ctx.canManage || m.me) return false;
  if (m.role === "owner") return ctx.actorRole === "owner" && ctx.ownerCount > 1;
  return true;
}

export type KeyScope = { kind: "all" } | { kind: "site"; name: string } | { kind: "sites"; n: number };

/** 密钥范围：全部站点、单个站点名，或站点数。 */
export function keyScope(k: { scopes: string[]; sites: { name: string }[] }): KeyScope {
  if (k.scopes.includes("stats:read:*")) return { kind: "all" };
  if (k.scopes.length === 1 && k.sites.length === 1) return { kind: "site", name: k.sites[0].name };
  return { kind: "sites", n: k.scopes.length };
}

export type KeyExpiry = { kind: "never" } | { kind: "expired" } | { kind: "soon"; days: number } | { kind: "date"; at: Date };

/** 有效期归类：已过期、7 天内（含）到期、之后、永不。 */
export function keyExpiry(expiresAt: string | null, now: number): KeyExpiry {
  if (!expiresAt) return { kind: "never" };
  const at = Date.parse(expiresAt);
  if (Number.isNaN(at)) return { kind: "never" };
  if (at <= now) return { kind: "expired" };
  const days = Math.ceil((at - now) / MS_DAY);
  return days <= 7 ? { kind: "soon", days } : { kind: "date", at: new Date(at) };
}

/** 距今分钟数；时间无法解析时为 null，未来时间按 0。 */
export function minutesSince(iso: string | null, now: number): number | null {
  if (!iso) return null;
  const at = Date.parse(iso);
  if (Number.isNaN(at)) return null;
  return Math.max(0, Math.floor((now - at) / 60000));
}

/** 邀请还剩几天（向上取整，至少 1）。 */
export function daysLeft(expiresAt: string, now: number): number {
  const at = Date.parse(expiresAt);
  if (Number.isNaN(at)) return 0;
  return Math.max(1, Math.ceil((at - now) / MS_DAY));
}

/** 创建密钥后展示的 curl 示例。 */
export function curlExample(origin: string, key: string, siteKey: string): string {
  return `curl -X POST ${origin}/api/v1/query \\
  -H "Authorization: Bearer ${key}" \\
  -H "Content-Type: application/json" \\
  -d '{"site":"${siteKey}","metrics":["visitors"],"date_range":"7d"}'`;
}
