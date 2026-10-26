// 丢弃原因（`ingest_counters.metric` 去掉 `dropped_` 前缀，与 Rust 的 `DropReason` 一致）：
// fix 是处理入口（站点设置的分区），admin 是站点管理员处理不了的实例问题，why 表示名称看不出含义、需要一行说明。
// dropped_unknown_site 记在 site_id = 0，不会出现在站点的诊断里。

export type FixSection = "hostnames" | "collection" | "shield";

export interface ReasonDef {
  fix?: FixSection;
  admin?: boolean;
  why?: boolean;
}

export const REASONS: Record<string, ReasonDef> = {
  site_disabled: { fix: "collection" },
  hostname_not_allowed: { fix: "hostnames", why: true },
  rate_limited: { fix: "collection", why: true },
  spam_referrer: { why: true },
  ip_blocked: { fix: "shield" },
  page_blocked: { fix: "shield" },
  country_blocked: { fix: "shield" },
  bot: {},
  datacenter_ip: { why: true },
  no_session: { why: true },
  salt_missing: { admin: true },
  overloaded: { admin: true },
};
