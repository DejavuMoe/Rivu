// 站点设置的模拟数据与会话内存储：保存后在本次会话里保持，刷新回到初始值。
// 固定数据，不随机；真实实现里这些来自 site_settings、hostnames、shield_rules、goals、shared_links 表。

import { NOW, type Site } from "../../lib/data/fixtures";
import { DELETED_SITES, SITES } from "../../lib/data/sites.svelte";

/** 实例对外地址，只用于拼接分享链接的展示。 */
export const INSTANCE = "https://rivu.northwind.dev";
/** 实例默认的每分钟上限（站点留空时用它）与站点能设的上限。 */
export const GLOBAL_RATE = 300000;
export const RATE_MAX = 1_000_000;
/** 原始数据保留天数的实例默认值，也是站点能设的上限；站点最少 30 天。 */
export const INSTANCE_RETENTION = 731;
export const MIN_RETENTION = 30;
export const MAX_RULES = 500;
/** 流量突增提醒人数的上限。 */
export const SPIKE_MAX = 1_000_000;
export const MAX_GOALS = 100;

export type RuleType = "ip" | "cidr" | "country" | "hostname" | "page";
export type GoalKind = "event" | "page" | "scroll";

/** 地理库能力：国家级库只能选国家，城市级库可选国家、地区、城市，没有库不记录位置。真实里是实例级，原型按站点放几种以便评审。 */
export function geoCapability(site: Site): "country" | "city" | null {
  if (site.id === 3) return null;
  if (site.id === 5) return "country";
  return "city";
}

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
  /** 名称可选，留空的链接在列表里显示「未命名」 */
  name: string;
  slug: string;
  password: boolean;
  expires: Date | null;
  by: string;
  createdAt: Date;
}
export interface DeletionJob {
  id: number;
  from: string;
  to: string;
  status: "pending" | "running" | "done" | "failed" | "cancelled";
  createdAt: Date;
}

/** 外部数据导入（site_imports）。日期是站点本地日，导入完成前为空。 */
export interface Import {
  id: number;
  file: string;
  status: "importing" | "completed" | "failed";
  from: string;
  to: string;
  /** 失败原因的代码（invalid_zip、not_plausible、invalid_csv、no_days、overlap、interrupted、internal），文案按语言在界面里取 */
  errorCode: string;
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

/** 站点的名称、时区、公开状态存在站点本身（Site）上，改了侧栏、站点列表等都跟着变；这里只放设置页自己的内容。 */
export interface Cfg {
  weekStart: string;
  hostnames: Hostname[];
  collectEnabled: boolean;
  script: ScriptFeatures;
  rateLimit: string;
  eventProps: string[];
  geo: "country" | "region" | "city";
  retention: string;
  /** 流量突增的提醒人数：当前访客超过它时发邮件；空串 = 不提醒（默认） */
  spikeThreshold: string;
  rules: Rule[];
  goals: Goal[];
  links: ShareLink[];
  jobs: DeletionJob[];
  imports: Import[];
}

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
      expires: null,
      by: "林夏",
      createdAt: day(41),
    },
    {
      id: 2,
      name: "合作方增长看板",
      slug: "t5r8y2u6i0o3p7a1s4d9f2g6",
      password: true,
      expires: ahead(7),
      by: "林夏",
      createdAt: day(6),
    },
    {
      id: 3,
      name: "发布周对外同步",
      slug: "expiredq2w3e4r5t6y7u8i9o0p1",
      password: false,
      expires: day(9),
      by: "周远",
      createdAt: day(40),
    },
    {
      id: 4,
      name: "",
      slug: "m8c2x6v1b5n9q3w7e4r0t2y6",
      password: false,
      expires: null,
      by: "周远",
      createdAt: day(3),
    },
  ];
}

/** 删除统计数据的任务：站点 1 放几条已结束的，演示各种状态。新提交的排在最前。 */
function jobs(site: Site): DeletionJob[] {
  if (site.id !== 1) return [];
  return [
    { id: 1, from: ymd(day(120)), to: ymd(day(110)), status: "done", createdAt: day(60) },
    { id: 2, from: ymd(day(300)), to: ymd(day(280)), status: "failed", createdAt: day(35) },
    { id: 3, from: ymd(day(30)), to: ymd(day(30)), status: "cancelled", createdAt: day(29) },
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
        errorCode: "",
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
        errorCode: "not_plausible",
        by: "周远",
        createdAt: day(12),
      },
    ];
  return [];
}

/** 设了提醒人数的站点；其余默认不提醒。 */
const SPIKES: Record<number, string> = { 1: "200", 2: "80", 5: "60" };

function make(site: Site): Cfg {
  return {
    weekStart: "1",
    hostnames: hosts(site),
    collectEnabled: true,
    script: { outbound: false, downloads: false, forms: false, hash: false, tagged: true, engagement: true },
    rateLimit: "",
    eventProps: site.id === 1 ? ["plan", "type", "variant"] : [],
    geo: geoCapability(site) === "country" ? "country" : "city",
    retention: "",
    spikeThreshold: SPIKES[site.id] ?? "",
    rules: rules(site),
    goals: goals(site),
    links: links(site),
    jobs: jobs(site),
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

// ---------- 目标表单的初筛（与生产的 lib/admin/goals.ts 一致） ----------

export const EVENT_NAME_MAX = 120;
export const PAGE_PATH_MAX_BYTES = 250;

/** 页面路径的规范化：trim，连续的 * 合并成一个。 */
export function normalizePagePath(raw: string): string {
  return raw.trim().replace(/\*{2,}/g, "*");
}

export function eventNameError(raw: string): string | null {
  const s = raw.trim();
  if (!s) return null;
  if ([...s].length > EVENT_NAME_MAX) return "set.goals.err.eventLen";
  if (s === "engagement") return "set.goals.err.engagement";
  return null;
}

export function pagePathError(raw: string): string | null {
  const s = normalizePagePath(raw);
  if (!s) return null;
  if (!s.startsWith("/") || /[\s\p{Cc}]/u.test(s)) return "set.goals.err.page";
  if (new TextEncoder().encode(s).length > PAGE_PATH_MAX_BYTES) return "set.goals.err.page";
  return null;
}

/** 目标值：事件名、页面路径，滚动深度写成「≥ 75%」。 */
export function goalTarget(g: Goal): string {
  return g.kind === "scroll" ? `≥ ${g.target}%` : g.target;
}

/** 看板按名字区分目标：显示名，否则目标值。 */
export const goalLabel = (g: Goal) => g.display || goalTarget(g);

// ---------- 共享链接有效期（与生产的 lib/admin/expiry.ts 一致；「现在」用原型固定的 NOW） ----------

export type ExpiryKind = "never" | "7" | "30" | "custom";

/** 某个本地日期（YYYY-MM-DD）当天 23:59:59 的时刻；格式不对或不存在的日期返回 null。 */
export function endOfLocalDay(value: string): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!m) return null;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  const date = new Date(y, mo - 1, d, 23, 59, 59);
  // 排除 2 月 31 日这类被 Date 自动进位的输入
  return date.getFullYear() === y && date.getMonth() === mo - 1 && date.getDate() === d ? date : null;
}

export interface Expiry {
  /** null：永久有效（或输入无效，此时 error 非空） */
  at: Date | null;
  error: "invalid" | "past" | null;
}

export function computeExpiry(kind: ExpiryKind, custom: string, now: Date): Expiry {
  if (kind === "never") return { at: null, error: null };
  let end: Date | null;
  if (kind === "custom") end = endOfLocalDay(custom);
  else end = endOfLocalDay(ymd(new Date(now.getFullYear(), now.getMonth(), now.getDate() + Number(kind))));
  if (!end) return { at: null, error: "invalid" };
  if (end.getTime() <= now.getTime()) return { at: null, error: "past" };
  return { at: end, error: null };
}

// ---------- 删除统计数据的日期范围（与生产的 lib/admin/deletion.ts 一致） ----------

/** 某个时刻在 IANA 时区里的日期 YYYY-MM-DD；时区无效时按 UTC。 */
export function ymdInZone(at: Date, timeZone: string): string {
  const fmt = (tz: string) => new Intl.DateTimeFormat("en-CA", { timeZone: tz, year: "numeric", month: "2-digit", day: "2-digit" }).format(at);
  try {
    return fmt(timeZone);
  } catch {
    return fmt("UTC");
  }
}

/** 一次最多删除的天数（含两端）。 */
export const MAX_RANGE_DAYS = 731;

/** 严格的 YYYY-MM-DD 且是真实存在的日期，返回 UTC 零点的毫秒数；否则 null。 */
function dayMs(value: string): number | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!m) return null;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  const ms = Date.UTC(y, mo - 1, d);
  const back = new Date(ms);
  return back.getUTCFullYear() === y && back.getUTCMonth() === mo - 1 && back.getUTCDate() === d ? ms : null;
}

export type RangeProblem = "invalid" | "order" | "future" | "tooLong";

/** 校验日期范围（站点本地日期）；任一端为空时不报错（还没填完）。`today` 是站点时区的今天。 */
export function rangeError(from: string, to: string, today: string): RangeProblem | null {
  if (!from || !to) return null;
  const a = dayMs(from);
  const b = dayMs(to);
  if (a === null || b === null) return "invalid";
  if (a > b) return "order";
  if (to > today) return "future";
  if ((b - a) / 86_400_000 + 1 > MAX_RANGE_DAYS) return "tooLong";
  return null;
}
