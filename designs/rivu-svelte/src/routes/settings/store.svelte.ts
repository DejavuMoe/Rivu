// 站点设置的模拟数据与会话内存储：保存后在本次会话里保持，刷新回到初始值。
// 固定数据，不随机；真实实现里这些来自 site_settings、hostnames、shield_rules、goals、shared_links 表。

import { NOW, type Site } from "../../lib/data/fixtures";
import { DELETED_SITES, SITES } from "../../lib/data/sites.svelte";

/** 实例对外地址，只用于拼接分享链接的展示。 */
export const INSTANCE = "https://rivu.northwind.dev";
export const GLOBAL_RATE = 300000;
export const INSTANCE_RETENTION = 731;
export const MAX_RULES = 500;

export type RuleType = "ip" | "cidr" | "country" | "hostname" | "page";
export type GoalKind = "event" | "page" | "scroll";

export interface Hostname {
  pattern: string;
  addedAt: Date;
}
export interface Rule {
  id: number;
  type: RuleType;
  action: "deny" | "allow";
  value: string;
  note: string;
  createdAt: Date;
}
export interface Goal {
  id: number;
  kind: GoalKind;
  /** 事件名、页面路径（可带通配）或滚动深度百分比 */
  target: string;
  /** 滚动深度目标可限定页面 */
  path: string;
  display: string;
  props: { key: string; value: string }[];
}
export interface ShareLink {
  id: number;
  name: string;
  slug: string;
  password: boolean;
  segment: string;
  expires: Date | null;
  by: string;
  createdAt: Date;
}
export interface DeletionJob {
  id: number;
  from: string;
  to: string;
  status: "pending" | "running" | "done";
  createdAt: Date;
}

/** 外部数据导入（site_imports）。日期是站点本地日，导入完成前为空。 */
export interface Import {
  id: number;
  file: string;
  status: "importing" | "completed" | "failed";
  from: string;
  to: string;
  error: string;
  by: string;
  createdAt: Date;
}

export interface ScriptFeatures {
  outbound: boolean;
  downloads: boolean;
  forms: boolean;
  hash: boolean;
  tagged: boolean;
  engagement: boolean;
}

export interface Cfg {
  name: string;
  timezone: string;
  weekStart: string;
  isPublic: boolean;
  hostnames: Hostname[];
  collectEnabled: boolean;
  stripQuery: boolean;
  script: ScriptFeatures;
  rateLimit: string;
  eventProps: string[];
  geo: "country" | "region" | "city";
  retention: string;
  rules: Rule[];
  goals: Goal[];
  links: ShareLink[];
  jobs: DeletionJob[];
  imports: Import[];
}

export const SEGMENTS = ["中国大陆访客", "移动端访客", "来自搜索引擎"];

const day = (n: number) => new Date(NOW.getTime() - n * 864e5);
const ahead = (n: number) => new Date(NOW.getTime() + n * 864e5);

export function ymd(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

function hosts(site: Site): Hostname[] {
  const created = site.createdDaysAgo;
  switch (site.id) {
    case 1:
      return [
        { pattern: "northwind.dev", addedAt: day(created) },
        { pattern: "*.northwind.dev", addedAt: day(380) },
        { pattern: "northwind-preview.pages.dev", addedAt: day(95) },
      ];
    case 3:
      return []; // 演示 fail-closed：白名单为空
    default:
      return [{ pattern: site.domain, addedAt: day(created) }];
  }
}

const RULES1: [RuleType, "deny" | "allow", string, string, number][] = [
  ["ip", "deny", "203.0.113.7", "办公室测试机", 210],
  ["ip", "deny", "2001:db8:85a3::8a2e:370:7334", "CI 机房出口", 188],
  ["cidr", "deny", "198.51.100.0/24", "合作方内网", 160],
  ["cidr", "allow", "192.0.2.0/28", "监控探针（放行）", 150],
  ["country", "deny", "KP", "", 140],
  ["country", "deny", "RU", "垃圾流量集中", 96],
  ["hostname", "deny", "staging.northwind.dev", "预发环境", 90],
  ["hostname", "deny", "localhost.northwind.dev", "", 71],
  ["page", "deny", "/admin/*", "后台页面", 64],
  ["page", "deny", "/healthz", "健康检查", 64],
  ["page", "deny", "/preview/*", "草稿预览", 33],
  ["page", "allow", "/admin/public-report", "例外：对外报告页", 12],
];

function rules(site: Site): Rule[] {
  const list: [RuleType, "deny" | "allow", string, string, number][] =
    site.id === 1 ? RULES1 : site.id === 2 ? RULES1.slice(6, 9) : [];
  return list.map(([type, action, value, note, ago], i) => ({
    id: i + 1,
    type,
    action,
    value,
    note,
    createdAt: day(ago),
  }));
}

function goals(site: Site): Goal[] {
  if (site.scale === 0) return [];
  return [
    { id: 1, kind: "event", target: "Signup", path: "", display: "注册", props: [{ key: "plan", value: "pro" }] },
    { id: 2, kind: "page", target: "/pricing", path: "", display: "查看定价", props: [] },
    { id: 3, kind: "event", target: "Download", path: "", display: "下载文档 PDF", props: [{ key: "type", value: "pdf" }] },
    { id: 4, kind: "scroll", target: "75", path: "/blog/*", display: "读完博客", props: [] },
    { id: 5, kind: "page", target: "/blog/*", path: "", display: "", props: [] },
  ];
}

function links(site: Site): ShareLink[] {
  if (site.scale === 0 || site.id === 3) return [];
  return [
    {
      id: 1,
      name: "季度复盘（只读）",
      slug: "9f3k2m7q8w1x4z6c5v0b2n8m",
      password: false,
      segment: "",
      expires: null,
      by: "林夏",
      createdAt: day(41),
    },
    {
      id: 2,
      name: "合作方增长看板",
      slug: "t5r8y2u6i0o3p7a1s4d9f2g6",
      password: true,
      segment: "移动端访客",
      expires: ahead(7),
      by: "林夏",
      createdAt: day(6),
    },
    {
      id: 3,
      name: "发布周对外同步",
      slug: "expiredq2w3e4r5t6y7u8i9o0p1",
      password: false,
      segment: "",
      expires: day(9),
      by: "周远",
      createdAt: day(40),
    },
  ];
}

/** 站点自己的数据从哪天开始（创建当天）；导入只写这天之前的日子。 */
export const firstDay = (site: Site) => ymd(day(site.createdDaysAgo));

function imports(site: Site): Import[] {
  const start = day(site.createdDaysAgo);
  const before = (n: number) => ymd(new Date(start.getTime() - n * 864e5));
  if (site.id === 1)
    return [
      {
        id: 1,
        file: "northwind_dev_20250310.zip",
        status: "completed",
        from: before(912),
        to: before(1),
        error: "",
        by: "林夏",
        createdAt: day(site.createdDaysAgo - 2),
      },
    ];
  if (site.id === 2)
    return [
      {
        id: 2,
        file: "docs_export.zip",
        status: "failed",
        from: "",
        to: "",
        error: "压缩包里没有 imported_visitors 开头的 CSV，确认它是 Plausible 导出的原始 zip",
        by: "周远",
        createdAt: day(12),
      },
    ];
  return [];
}

function make(site: Site): Cfg {
  return {
    name: site.name,
    timezone: site.timezone,
    weekStart: "1",
    isPublic: site.isPublic,
    hostnames: hosts(site),
    collectEnabled: true,
    stripQuery: true,
    script: { outbound: false, downloads: false, forms: false, hash: false, tagged: true, engagement: true },
    rateLimit: "",
    eventProps: site.id === 1 ? ["plan", "type", "variant"] : [],
    geo: "city",
    retention: "",
    rules: rules(site),
    goals: goals(site),
    links: links(site),
    jobs: [],
    imports: imports(site),
  };
}

export const cfgs = $state<Record<number, Cfg>>(Object.fromEntries([...SITES, ...DELETED_SITES].map((s) => [s.id, make(s)])));

let seq = 1000;
export const nextId = () => ++seq;

// ---------- 主机名匹配（与收集端一致：见 tracker-and-protocol.md） ----------

const HOST_RE = /^(\*\.)?([a-z0-9-]+\.)*[a-z0-9-]+$/;

/** 匹配前的规范化：小写、去端口、去末尾点、去一个前导 www.。 */
export function normalizeHost(raw: string): string {
  let h = raw.trim().toLowerCase();
  h = h.replace(/:\d+$/, "").replace(/\.$/, "");
  if (h.startsWith("www.")) h = h.slice(4);
  return h;
}

export function hostMatches(pattern: string, rawHost: string): boolean {
  const host = normalizeHost(rawHost);
  if (pattern.startsWith("*.")) {
    const base = pattern.slice(2);
    return host.length > base.length + 1 && host.endsWith("." + base);
  }
  return host === pattern;
}

export type HostError = "empty" | "scheme" | "wildcard" | "chars" | "www" | "dup" | null;

export function checkPattern(pattern: string, existing: string[]): HostError {
  if (!pattern) return "empty";
  if (/[/:?#@\s]/.test(pattern)) return "scheme";
  if (pattern.includes("*") && !/^\*\.[^*]+$/.test(pattern)) return "wildcard";
  if (!HOST_RE.test(pattern)) return "chars";
  if (pattern.startsWith("www.") || pattern.startsWith("*.www.")) return "www";
  if (existing.includes(pattern)) return "dup";
  return null;
}

/** 给添加对话框展示的示例主机：每个示例标出会不会匹配。 */
export function hostExamples(pattern: string): { host: string; match: boolean }[] {
  const wild = pattern.startsWith("*.");
  const base = wild ? pattern.slice(2) : pattern;
  const hosts = wild
    ? [`blog.${base}`, `a.b.${base}`, base, `www.${base}`]
    : [base, `www.${base}`, `${base}:8443`, `shop.${base}`];
  return hosts.map((host) => ({ host, match: hostMatches(pattern, host) }));
}
