// 组织页（成员、API 密钥）的模拟数据。固定值，时间相对 fixtures 的 NOW。

import { NOW } from "../../lib/data/fixtures";
import type { Role } from "../../lib/state.svelte";

export interface Member {
  id: string;
  org: string;
  name: string;
  email: string;
  role: Role;
  joinedDaysAgo: number;
  /** 最近活跃（分钟前） */
  activeMinutesAgo: number;
  /** 当前登录的人：角色跟随评审面板的角色开关 */
  me?: boolean;
}

export interface Invitation {
  id: string;
  org: string;
  email: string;
  role: Exclude<Role, "owner">;
  expiresInDays: number;
}

export interface Guest {
  id: string;
  org: string;
  email: string;
  siteId: number;
  role: "editor" | "viewer";
}

export const MEMBERS: Member[] = [
  { id: "m1", org: "o1", name: "林夏", email: "lin.xia@example.com", role: "owner", joinedDaysAgo: 420, activeMinutesAgo: 0, me: true },
  { id: "m2", org: "o1", name: "周启明", email: "zhou.qiming@northwind.dev", role: "owner", joinedDaysAgo: 420, activeMinutesAgo: 95 },
  { id: "m3", org: "o1", name: "陈薇", email: "chen.wei@northwind.dev", role: "admin", joinedDaysAgo: 310, activeMinutesAgo: 24 * 60 + 40 },
  { id: "m4", org: "o1", name: "Marcus Weber", email: "marcus@northwind.dev", role: "editor", joinedDaysAgo: 180, activeMinutesAgo: 14 },
  { id: "m5", org: "o1", name: "苏晓", email: "su.xiao@northwind.dev", role: "editor", joinedDaysAgo: 97, activeMinutesAgo: 3 * 24 * 60 },
  { id: "m6", org: "o1", name: "Priya Nair", email: "priya.nair@northwind.dev", role: "viewer", joinedDaysAgo: 41, activeMinutesAgo: 6 * 60 },
  { id: "m7", org: "o1", name: "赵一鸣", email: "zhao.yiming@northwind.dev", role: "viewer", joinedDaysAgo: 12, activeMinutesAgo: 12 * 24 * 60 },
  { id: "m8", org: "o2", name: "孙雅", email: "sun.ya@oss-docs.cn", role: "owner", joinedDaysAgo: 600, activeMinutesAgo: 52 },
  { id: "m9", org: "o2", name: "林夏", email: "lin.xia@example.com", role: "editor", joinedDaysAgo: 240, activeMinutesAgo: 0, me: true },
  { id: "m10", org: "o2", name: "郑远", email: "zheng.yuan@oss-docs.cn", role: "admin", joinedDaysAgo: 410, activeMinutesAgo: 5 * 60 },
  { id: "m11", org: "o2", name: "Tom Becker", email: "tom@oss-docs.cn", role: "viewer", joinedDaysAgo: 33, activeMinutesAgo: 8 * 24 * 60 },
];

export const INVITATIONS: Invitation[] = [
  { id: "i1", org: "o1", email: "dev@acme.io", role: "viewer", expiresInDays: 5 },
  { id: "i2", org: "o1", email: "han.xue@northwind.dev", role: "editor", expiresInDays: 2 },
  { id: "i3", org: "o1", email: "ops@northwind.dev", role: "admin", expiresInDays: 6 },
  { id: "i4", org: "o2", email: "volunteer@oss-docs.cn", role: "viewer", expiresInDays: 4 },
];

export const GUESTS: Guest[] = [
  { id: "g1", org: "o1", email: "editor@partner.io", siteId: 6, role: "editor" },
  { id: "g2", org: "o1", email: "analyst@partner.io", siteId: 6, role: "viewer" },
  { id: "g3", org: "o1", email: "agency@studio.example", siteId: 2, role: "viewer" },
];

export interface ApiKey {
  id: string;
  org: string;
  name: string;
  /** rivu_ 开头的前 12 个字符 */
  prefix: string;
  /** stats:read:* 或 stats:read:site/<id> */
  scopes: string[];
  createdDaysAgo: number;
  lastUsedMinutesAgo: number | null;
  /** 距今天数；null 永不过期 */
  expiresInDays: number | null;
}

export const API_KEYS: ApiKey[] = [
  { id: "k1", org: "o1", name: "BI 日报导出", prefix: "rivu_k3Vx9Qa", scopes: ["stats:read:*"], createdDaysAgo: 142, lastUsedMinutesAgo: 22, expiresInDays: 223 },
  { id: "k2", org: "o1", name: "Grafana northwind.dev", prefix: "rivu_Zp7mT2d", scopes: ["stats:read:site/1"], createdDaysAgo: 61, lastUsedMinutesAgo: 3 * 60, expiresInDays: null },
  { id: "k3", org: "o1", name: "一次性数据迁移", prefix: "rivu_w9Bn4Lc", scopes: ["stats:read:site/1", "stats:read:site/2"], createdDaysAgo: 28, lastUsedMinutesAgo: null, expiresInDays: 2 },
  { id: "k4", org: "o2", name: "文档站看板嵌入", prefix: "rivu_Hs5qRe8", scopes: ["stats:read:site/5"], createdDaysAgo: 75, lastUsedMinutesAgo: 9 * 60, expiresInDays: 15 },
];

export function daysFromNow(d: number): Date {
  const x = new Date(NOW);
  x.setHours(0, 0, 0, 0);
  x.setDate(x.getDate() + d);
  return x;
}

/** 2026-10-24 这样的日期，中英文一致，便于表格对齐。 */
export function isoDate(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

