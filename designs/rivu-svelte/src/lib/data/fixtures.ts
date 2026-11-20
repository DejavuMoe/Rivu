// 原型的模拟数据。用固定种子生成，刷新页面结果不变，截图可复现。
// 形状尽量贴近 QuerySpec 的结果（results/meta），但这里直接给组件需要的结构。

export interface Org {
  id: string;
  name: string;
  /** 当前用户在该组织里的角色；null 表示不是成员，只以站点访客身份看到其中被邀请的站点 */
  role: "owner" | "admin" | "editor" | "viewer" | null;
}

export interface Site {
  id: number;
  org: string;
  name: string;
  domain: string;
  siteKey: string;
  timezone: string;
  isPublic: boolean;
  /** 访客权限：通过站点邀请获得，不是组织成员（只出现在 role 为 null 的组织里） */
  guest?: boolean;
  /** 访客在这个站点上的角色（组织成员的角色由评审面板的「角色」决定） */
  guestRole?: "editor" | "viewer";
  /** 规模系数：决定模拟流量的大小；0 表示刚创建、尚无数据 */
  scale: number;
  live: number;
  createdDaysAgo: number;
  /** 删除时间：只出现在已删除（仍在 7 天恢复期内）的站点上 */
  deletedAt?: Date;
}

export const USER = { name: "林夏", email: "lin.xia@example.com", initials: "林" };

export const ORGS: Org[] = [
  { id: "o1", name: "Northwind Labs", role: "owner" },
  { id: "o2", name: "开源文档社区", role: "editor" },
  { id: "o3", name: "Fabrikam Studio", role: null },
];

export const SEED_SITES: Site[] = [
  { id: 1, org: "o1", name: "northwind.dev", domain: "northwind.dev", siteKey: "k7qm2xv9atbr", timezone: "Asia/Shanghai", isPublic: false, scale: 1, live: 37, createdDaysAgo: 420 },
  { id: 2, org: "o1", name: "Northwind Docs", domain: "docs.northwind.dev", siteKey: "d4pl6nq2wzce", timezone: "Asia/Shanghai", isPublic: true, scale: 0.62, live: 21, createdDaysAgo: 300 },
  { id: 3, org: "o1", name: "Status", domain: "status.northwind.dev", siteKey: "s2ty7hr5mvka", timezone: "UTC", isPublic: false, scale: 0.08, live: 2, createdDaysAgo: 200 },
  { id: 4, org: "o1", name: "Launch Week", domain: "launch.northwind.dev", siteKey: "l6wk3bf7xq2p", timezone: "Asia/Shanghai", isPublic: false, scale: 0, live: 0, createdDaysAgo: 0 },
  { id: 5, org: "o2", name: "rustdoc-zh", domain: "rustdoc.cn", siteKey: "r5zh2kd6mnpq", timezone: "Asia/Shanghai", isPublic: true, scale: 0.85, live: 14, createdDaysAgo: 600 },
  { id: 6, org: "o1", name: "合作方博客", domain: "blog.partner.io", siteKey: "b3pt7nw2rlx4", timezone: "Europe/Berlin", isPublic: false, scale: 0.3, live: 4, createdDaysAgo: 90 },
  // o3 里当前用户不是成员，只是被邀请进两个站点的访客：没有成员页、不能添加站点，看不到设置里的大部分分区
  { id: 10, org: "o3", name: "Fabrikam 品牌站", domain: "brand.fabrikam.example", siteKey: "f7bk3qz8wnd5", timezone: "Europe/London", isPublic: false, guest: true, guestRole: "viewer", scale: 0.2, live: 3, createdDaysAgo: 120 },
  { id: 11, org: "o3", name: "Fabrikam 活动页", domain: "events.fabrikam.example", siteKey: "f2ev6mx9ktc4", timezone: "Europe/London", isPublic: false, guest: true, guestRole: "editor", scale: 0.12, live: 1, createdDaysAgo: 45 },
];

// ---------- 伪随机 ----------

function mulberry32(seed: number) {
  return () => {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

/** 「今天」固定在一个时间点，避免截图随日期变化。 */
export const NOW = new Date(2026, 9, 24, 15, 42);

// ---------- 时间序列 ----------

export interface Point {
  t: Date;
  visitors: number;
  visits: number;
  pageviews: number;
  bounce_rate: number;
  visit_duration: number;
  views_per_visit: number;
}

function dayValue(rand: () => number, d: Date, base: number) {
  const dow = d.getDay();
  const weekly = dow === 0 || dow === 6 ? 0.62 : dow === 1 ? 1.06 : 1;
  // 缓慢增长 + 两个流量尖峰（一次发布、一次被转载）
  const days = (d.getTime() - NOW.getTime()) / 864e5;
  const trend = 1 + days * 0.0012;
  const spike = Math.exp(-(((days + 19) / 1.4) ** 2)) * 1.6 + Math.exp(-(((days + 52) / 2) ** 2)) * 0.9;
  return base * weekly * trend * (1 + spike) * (0.9 + rand() * 0.2);
}

function makePoint(rand: () => number, t: Date, visitors: number): Point {
  const visits = visitors * (1.18 + rand() * 0.08);
  const vpv = 2.3 + rand() * 0.5;
  return {
    t,
    visitors: Math.round(visitors),
    visits: Math.round(visits),
    pageviews: Math.round(visits * vpv),
    bounce_rate: 0.38 + rand() * 0.1,
    visit_duration: 150 + rand() * 70,
    views_per_visit: vpv,
  };
}

/** 按天的序列：从 offsetDays 天前开始共 days 天（含今天时最后一天是不完整的）。 */
export function daily(site: Site, days: number, endOffset = 0, factor = 1): Point[] {
  const rand = mulberry32(site.id * 7919 + days * 31 + endOffset);
  const out: Point[] = [];
  for (let i = days - 1; i >= 0; i--) {
    const d = new Date(NOW);
    d.setHours(0, 0, 0, 0);
    d.setDate(d.getDate() - i - endOffset);
    let v = dayValue(rand, d, 3200 * site.scale * factor);
    if (i === 0 && endOffset === 0) v *= NOW.getHours() / 24 + 0.05;
    out.push(makePoint(rand, d, v));
  }
  return out;
}

/** 按小时的序列（今天 / 24 小时）。 */
export function hourly(site: Site, hours: number, endOffsetHours = 0, factor = 1): Point[] {
  const rand = mulberry32(site.id * 104729 + hours + endOffsetHours);
  const out: Point[] = [];
  for (let i = hours - 1; i >= 0; i--) {
    const d = new Date(NOW);
    d.setMinutes(0, 0, 0);
    d.setHours(d.getHours() - i - endOffsetHours);
    const h = d.getHours();
    const diurnal = 0.25 + 0.75 * Math.max(0, Math.sin(((h - 6) / 24) * Math.PI * 2 * 0.5 + 0.2)) ** 1.4;
    let v = (3200 / 24) * 1.6 * site.scale * factor * diurnal * (0.85 + rand() * 0.3);
    if (i === 0 && endOffsetHours === 0) v *= NOW.getMinutes() / 60;
    out.push(makePoint(rand, d, v));
  }
  return out;
}

/** 按月的序列。 */
export function monthly(site: Site, months: number, factor = 1, seed = 0): Point[] {
  const rand = mulberry32(site.id * 15485863 + months + seed * 7);
  const out: Point[] = [];
  for (let i = months - 1; i >= 0; i--) {
    const d = new Date(NOW.getFullYear(), NOW.getMonth() - i, 1);
    const growth = 1 - i * 0.045;
    let v = 3200 * 30 * 0.88 * site.scale * factor * growth * (0.92 + rand() * 0.16);
    if (i === 0) v *= NOW.getDate() / 31;
    out.push(makePoint(rand, d, v));
  }
  return out;
}

// ---------- 分解（breakdown） ----------

export interface Row {
  label: string;
  /** 可选的副标签（如来源的域名、页面的标题） */
  sub?: string;
  /** 国家代码、浏览器等用于图标 */
  code?: string;
  visitors: number;
}

const BREAKDOWNS: Record<string, [string, number, string?][]> = {
  channel: [
    ["Organic Search", 0.41],
    ["Direct", 0.27],
    ["Referral", 0.13],
    ["Organic Social", 0.09],
    ["Email", 0.04],
    ["Paid Search", 0.03],
    ["Organic Video", 0.015],
    ["Unassigned", 0.01],
  ],
  source: [
    ["Google", 0.31, "google.com"],
    ["Direct / None", 0.27],
    ["Bing", 0.06, "bing.com"],
    ["GitHub", 0.058, "github.com"],
    ["Hacker News", 0.047, "news.ycombinator.com"],
    ["Baidu", 0.04, "baidu.com"],
    ["V2EX", 0.032, "v2ex.com"],
    ["X", 0.028, "x.com"],
    ["DuckDuckGo", 0.021, "duckduckgo.com"],
    ["Reddit", 0.017, "reddit.com"],
    ["Newsletter", 0.012],
    ["LinkedIn", 0.009, "linkedin.com"],
  ],
  referrer: [
    ["news.ycombinator.com/item?id=41827133", 0.04],
    ["github.com/northwind/rivu", 0.033],
    ["v2ex.com/t/1086271", 0.026],
    ["www.reddit.com/r/selfhosted", 0.014],
    ["lobste.rs/s/fqk2ma", 0.008],
  ],
  utm_campaign: [
    ["launch-week-2026", 0.05],
    ["newsletter-oct", 0.02],
    ["docs-footer", 0.008],
  ],
  page: [
    ["/", 0.52, "Northwind — Self-hosted analytics"],
    ["/pricing", 0.18],
    ["/docs/getting-started", 0.16],
    ["/blog/why-clickhouse", 0.12],
    ["/docs/tracker", 0.09],
    ["/changelog", 0.07],
    ["/blog/privacy-by-default", 0.06],
    ["/about", 0.035],
    ["/docs/api/query", 0.03],
    ["/careers", 0.012],
  ],
  entry_page: [
    ["/", 0.44],
    ["/blog/why-clickhouse", 0.11],
    ["/docs/getting-started", 0.1],
    ["/pricing", 0.07],
    ["/blog/privacy-by-default", 0.05],
  ],
  exit_page: [
    ["/", 0.29],
    ["/pricing", 0.15],
    ["/docs/getting-started", 0.12],
    ["/blog/why-clickhouse", 0.09],
    ["/changelog", 0.05],
  ],
  country: [
    ["中国", 0.38, "CN"],
    ["美国", 0.19, "US"],
    ["德国", 0.07, "DE"],
    ["日本", 0.055, "JP"],
    ["英国", 0.045, "GB"],
    ["新加坡", 0.035, "SG"],
    ["法国", 0.03, "FR"],
    ["加拿大", 0.027, "CA"],
    ["印度", 0.024, "IN"],
    ["荷兰", 0.02, "NL"],
  ],
  region: [
    ["上海", 0.11, "CN-SH"],
    ["北京", 0.09, "CN-BJ"],
    ["广东", 0.08, "CN-GD"],
    ["California", 0.06, "US-CA"],
    ["浙江", 0.04, "CN-ZJ"],
    ["Berlin", 0.025, "DE-BE"],
  ],
  // 城市名来自 GeoNames（英文名），第三项是所在国家
  city: [
    ["Shanghai", 0.11, "CN"],
    ["Beijing", 0.09, "CN"],
    ["Shenzhen", 0.05, "CN"],
    ["San Francisco", 0.035, "US"],
    ["Hangzhou", 0.03, "CN"],
    ["Berlin", 0.022, "DE"],
    ["Tokyo", 0.02, "JP"],
    ["London", 0.018, "GB"],
    ["Singapore", 0.016, "SG"],
    ["Seattle", 0.012, "US"],
    ["Paris", 0.011, "FR"],
    ["Toronto", 0.009, "CA"],
  ],
  device: [
    ["desktop", 0.64],
    ["mobile", 0.33],
    ["tablet", 0.03],
  ],
  browser: [
    ["Chrome", 0.58],
    ["Safari", 0.19],
    ["Edge", 0.09],
    ["Firefox", 0.08],
    ["Samsung Internet", 0.02],
    ["Opera", 0.01],
  ],
  os: [
    ["Windows", 0.36],
    ["macOS", 0.27],
    ["iOS", 0.17],
    ["Android", 0.14],
    ["Linux", 0.05],
  ],
  goal: [
    ["Signup", 0.034],
    ["Visit /pricing", 0.18],
    ["Download docs PDF", 0.012],
    ["Outbound link: github.com", 0.041],
  ],
  // 自定义属性（事件的 p 字段）：键 = `prop:<键>`
  "prop:url": [
    ["https://github.com/dejavu-moe/rivu", 0.031],
    ["https://docs.example.com/install", 0.014],
    ["https://twitter.com/rivu", 0.006],
  ],
  "prop:plan": [
    ["free", 0.024],
    ["pro", 0.009],
    ["team", 0.003],
  ],
  "prop:method": [
    ["email", 0.019],
    ["github", 0.012],
    ["google", 0.004],
  ],
};

/** 分解结果：按总访客数缩放，并加一点随机，避免看起来太整齐。 */
export function breakdown(site: Site, dim: string, totalVisitors: number): Row[] {
  const rand = mulberry32(site.id * 31 + dim.length * 977);
  return (BREAKDOWNS[dim] ?? []).map(([label, share, extra]) => ({
    label,
    visitors: Math.max(1, Math.round(totalVisitors * share * (0.92 + rand() * 0.16))),
    ...(extra ? (/^[A-Z]{2}(-|$)/.test(extra) ? { code: extra } : { sub: extra }) : {}),
  }));
}

/** 范围里出现过的属性键（事件数多的在前）；空站点没有。 */
export function propertyKeys(site: Site): string[] {
  return site.scale === 0 ? [] : ["url", "plan", "method"];
}

/** 某个维度可选的值（筛选对话框的建议列表）。 */
export function dimensionValues(dim: string): string[] {
  return (BREAKDOWNS[dim] ?? []).map(([label]) => label);
}
