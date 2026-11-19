// 接入诊断的模拟数据：形状贴近 ClickHouse ingest_counters（minute, metric, script_version, value）。
// 固定种子，刷新不变。真实实现里这里换成按范围聚合 ingest_counters 的查询。

import { NOW, type Site } from "../../lib/data/fixtures";

export type RangeId = "1h" | "24h" | "7d";

export interface ReasonDef {
  /** metric 去掉 dropped_ 前缀后的标识（与 Rust 代码一致） */
  id: string;
  /** 在近似权重里的占比（只用于生成模拟数据） */
  weight: number;
  /** 处理入口：设置分区；没有则只给文字建议 */
  fix?: { section: "hostnames" | "collection" | "shield"; };
  /** 不属于站点管理员能处理的（实例问题） */
  admin?: boolean;
  /** 通常无需处理 */
  benign?: boolean;
}

/** 站点级别的丢弃原因（dropped_unknown_site 不计入站点，只在说明里提）。 */
export const REASONS: ReasonDef[] = [
  { id: "bot", weight: 0.44, benign: true },
  { id: "spam_referrer", weight: 0.21, benign: true },
  { id: "hostname_not_allowed", weight: 0.15, fix: { section: "hostnames" } },
  { id: "datacenter_ip", weight: 0.06, benign: true },
  { id: "ip_blocked", weight: 0.045, fix: { section: "shield" } },
  { id: "rate_limited", weight: 0.03, fix: { section: "collection" } },
  { id: "no_session", weight: 0.028 },
  { id: "country_blocked", weight: 0.022, fix: { section: "shield" } },
  { id: "page_blocked", weight: 0.012, fix: { section: "shield" } },
  { id: "overloaded", weight: 0.003, admin: true },
  { id: "site_disabled", weight: 0, fix: { section: "collection" } },
  { id: "salt_missing", weight: 0, admin: true },
];

export interface ScriptVersion {
  version: string;
  build: "core" | "full";
  share: number;
  /** 最后出现距今的分钟数 */
  lastSeenMin: number;
  outdated: boolean;
}

/** 当前最新版本；低于它的标记为「已过时」。 */
export const LATEST_SCRIPT = "2.4.1";

const VERSIONS: ScriptVersion[] = [
  { version: "2.4.1", build: "full", share: 0.58, lastSeenMin: 0, outdated: false },
  { version: "2.4.1", build: "core", share: 0.31, lastSeenMin: 0, outdated: false },
  { version: "2.3.0", build: "core", share: 0.085, lastSeenMin: 2, outdated: true },
  { version: "1.9.2", build: "full", share: 0.025, lastSeenMin: 41, outdated: true },
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
  const total = REASONS.reduce((a, r) => a + r.weight, 0);
  const reasons = REASONS.filter((r) => r.weight > 0).map((r) => ({
    id: r.id,
    count: Math.round((dropped * r.weight) / total),
  }));
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
