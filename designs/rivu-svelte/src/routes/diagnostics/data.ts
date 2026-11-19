// 接入诊断的模拟数据：形状贴近 ClickHouse ingest_counters（minute, metric, script_version, value）。
// 固定种子，刷新不变。真实实现里这里换成按范围聚合 ingest_counters 的查询。

import { NOW, type Site } from "../../lib/data/fixtures";

export type RangeId = "1h" | "24h" | "7d";

export type FixSection = "hostnames" | "collection" | "shield";

/** 丢弃原因的界面信息（与生产的 reasons.ts 一致）。key 是 metric 去掉 dropped_ 前缀后的标识。 */
export interface ReasonDef {
  /** 处理入口：站点设置的分区 */
  fix?: FixSection;
  /** 站点管理员处理不了的实例问题 */
  admin?: boolean;
  /** 名称看不出含义，需要一行说明 */
  why?: boolean;
}

// dropped_unknown_site 记在 site_id = 0，不会出现在站点的诊断里。
export const REASONS: Record<string, ReasonDef> = {
  site_disabled: { fix: "collection" },
  hostname_not_allowed: { fix: "hostnames", why: true },
  rate_limited: { fix: "collection", why: true },
  spam_referrer: { why: true },
  ip_blocked: { fix: "shield" },
  page_blocked: { fix: "shield" },
  country_blocked: { fix: "shield" },
  bot: {},
  datacenter_ip: { why: true }, // 仅用于旧版本写入的计数。
  no_session: { why: true },
  salt_missing: { admin: true },
  overloaded: { admin: true },
};

/** 各原因在丢弃里的近似权重（只用于生成模拟数据）。 */
const WEIGHTS: [string, number][] = [
  ["bot", 0.44],
  ["spam_referrer", 0.21],
  ["hostname_not_allowed", 0.15],
  ["datacenter_ip", 0.06],
  ["ip_blocked", 0.045],
  ["rate_limited", 0.03],
  ["no_session", 0.028],
  ["country_blocked", 0.022],
  ["page_blocked", 0.012],
  ["overloaded", 0.003],
];

export interface ScriptVersion {
  /** 脚本版本号（事件里的 sv）；0 表示没有上报 */
  version: number;
  share: number;
  /** 最后出现距今的分钟数 */
  lastSeenMin: number;
}

const VERSIONS: ScriptVersion[] = [
  { version: 3, share: 0.89, lastSeenMin: 0 },
  { version: 2, share: 0.085, lastSeenMin: 2 },
  { version: 1, share: 0.015, lastSeenMin: 41 },
  { version: 0, share: 0.01, lastSeenMin: 300 },
];

function rng(seed: number) {
  return () => {
    seed |= 0;
    seed = (seed + 0x6d2b79f5) | 0;
    let x = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    x = (x + Math.imul(x ^ (x >>> 7), 61 | x)) ^ x;
    return ((x ^ (x >>> 14)) >>> 0) / 4294967296;
  };
}

export interface Bucket {
  t: Date;
  accepted: number;
  dropped: number;
}

export interface Diag {
  buckets: Bucket[];
  accepted: number;
  dropped: number;
  reasons: { id: string; count: number }[];
  versions: (ScriptVersion & { events: number })[];
  /** 最后收到事件距今的秒数 */
  lastEventSec: number | null;
  partialLast: boolean;
}

const SPEC: Record<RangeId, { n: number; stepMin: number }> = {
  "1h": { n: 60, stepMin: 1 },
  "24h": { n: 24, stepMin: 60 },
  "7d": { n: 28, stepMin: 360 },
};

/** 每分钟平均接收数（北风站点 scale=1 时约 18 条/分钟，按昼夜起伏）。 */
function perMinute(scale: number, t: Date) {
  const h = t.getHours() + t.getMinutes() / 60;
  const diurnal = 0.35 + 0.65 * Math.max(0, Math.sin(((h - 6) / 24) * Math.PI * 2 * 0.5 + 0.2)) ** 1.3;
  return 18 * scale * diurnal * (t.getDay() === 0 || t.getDay() === 6 ? 0.7 : 1);
}

export function diagnose(site: Site, range: RangeId, offset = 0): Diag {
  const { n, stepMin } = SPEC[range];
  const rand = rng(site.id * 7727 + n * 13 + offset * 101);
  const bucketStart = new Date(NOW);
  // 10 秒一桶、只写已关闭的桶：最后一个时间桶不完整
  if (stepMin >= 60) bucketStart.setMinutes(0, 0, 0);
  else bucketStart.setSeconds(0, 0);
  const dropShare = 0.17 + (site.id % 3) * 0.04;
  const buckets: Bucket[] = [];
  for (let i = n - 1; i >= 0; i--) {
    const t = new Date(bucketStart.getTime() - (i + offset * n) * stepMin * 60_000);
    let acc = perMinute(site.scale, t) * stepMin * (0.82 + rand() * 0.36);
    let drop = acc * (dropShare / (1 - dropShare)) * (0.7 + rand() * 0.6);
    // 一次短暂的机器人扫描尖峰
    if (range === "24h" && i === 9 && offset === 0) drop *= 3.4;
    if (range === "7d" && i === 14 && offset === 0) drop *= 2.6;
    if (range === "1h" && i === 22 && offset === 0) drop *= 4;
    if (i === 0 && offset === 0) {
      const frac = stepMin >= 60 ? NOW.getMinutes() / 60 || 0.05 : 0.8;
      acc *= frac;
      drop *= frac;
    }
    buckets.push({ t, accepted: Math.round(acc), dropped: Math.round(drop) });
  }
  const accepted = buckets.reduce((a, b) => a + b.accepted, 0);
  const dropped = buckets.reduce((a, b) => a + b.dropped, 0);

  // 北风站点：bot 最多，其次垃圾引荐、主机名不在白名单，少量限流
  const total = WEIGHTS.reduce((a, [, w]) => a + w, 0);
  const reasons = WEIGHTS.map(([id, w]) => ({ id, count: Math.round((dropped * w) / total) }));
  const diff = dropped - reasons.reduce((a, r) => a + r.count, 0);
  if (reasons.length) reasons[0].count += diff;

  const events = accepted + dropped;
  const versions = VERSIONS.map((v) => ({ ...v, events: Math.round(events * v.share) })).filter((v) => v.events > 0);

  return {
    buckets,
    accepted,
    dropped,
    reasons: reasons.filter((r) => r.count > 0),
    versions,
    lastEventSec: site.scale === 0 ? null : site.scale < 0.1 ? 6 * 60 + 20 : 12,
    partialLast: true,
  };
}
