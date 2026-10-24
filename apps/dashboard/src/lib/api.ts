// /api/internal/* 的客户端。契约见 bin/rivu/src/dash_api.rs 开头的端点表。
// 会话在 HttpOnly cookie 里，脚本拿不到；会改变状态的请求一律带 Content-Type: application/json（CSRF 检查要求）。

export type Role = "owner" | "admin" | "editor" | "viewer";

export interface User {
  id: number;
  email: string;
  name: string | null;
  is_instance_admin: boolean;
}

export interface Org {
  id: number;
  name: string;
  slug: string;
  role: Role;
}

export interface Me {
  user: User;
  organizations: Org[];
}

export interface Site {
  id: number;
  site_key: string;
  name: string;
  timezone: string;
  is_public: boolean;
  organization: { id: number; name: string; slug: string };
  role: Role | null;
  access: "member" | "guest";
}

/** 共享链接与公开看板拿到的站点信息（没有组织与角色）。 */
export interface PublicSite {
  id: number;
  site_key: string;
  name: string;
  timezone: string;
}

export interface SessionInfo {
  id: number;
  device_label: string | null;
  created_at: string;
  last_used_at: string;
  expires_at: string;
  current: boolean;
}

export interface QueryRow {
  dimensions: string[];
  metrics: number[];
  comparison?: { dimensions: string[]; metrics: number[]; change: (number | null)[] };
}

export interface QueryResult {
  results: QueryRow[];
  comparison_results?: QueryRow[];
  meta: {
    time_labels?: string[];
    comparison_time_labels?: string[];
    warnings?: string[];
    geo_capability?: "country" | "city";
    /** 地理库的署名（结果含位置维度时必带，许可要求显示） */
    geo_attribution?: { source: string; text: string; url: string; license: string } | null;
    total_rows?: number;
  };
  query: Record<string, unknown>;
}

export interface SiteSettings {
  week_starts_on: number;
  collect_enabled: boolean;
  tracker_hash: boolean;
  /** null：用实例默认值 */
  ingest_rate_limit_per_min: number | null;
  geo_level: "country" | "region" | "city";
}

export interface Hostname {
  pattern: string;
  created_at: string;
}

export interface SettingsBody {
  site: { id: number; site_key: string; name: string; timezone: string; is_public: boolean; created_at: string };
  settings: SiteSettings;
  hostnames: Hostname[];
  /** 默认上限为 null：全局没有限流 */
  limits: { ingest_rate_limit_default_per_min: number | null; shield_max_rules: number; ingest_rate_limit_max_per_min: number };
  /** 当前加载的地理库能力；没有库为 null */
  geo_capability: "country" | "city" | null;
}

export interface SitePatch {
  name?: string;
  timezone?: string;
  week_starts_on?: number;
  is_public?: boolean;
  collect_enabled?: boolean;
  tracker_hash?: boolean;
  ingest_rate_limit_per_min?: number | null;
  geo_level?: "country" | "region" | "city";
}

export type ShieldType = "ip" | "cidr" | "country" | "hostname" | "page";

export interface ShieldRule {
  id: string;
  type: ShieldType;
  action: "deny" | "allow";
  value: string;
  description: string | null;
  created_at: string;
}

export interface SharedLink {
  id: number;
  name: string | null;
  slug: string;
  has_password: boolean;
  expires_at: string | null;
  created_at: string;
  created_by: { id: number; name: string | null; email: string } | null;
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly retryAfter: number | null = null,
  ) {
    super(message);
  }
}

interface Options {
  body?: unknown;
  /** 共享链接的 slug：查询与实时请求带上它，服务端按链接授权 */
  share?: string;
  signal?: AbortSignal;
}

export async function request<T>(method: string, path: string, opts: Options = {}): Promise<T> {
  const headers: Record<string, string> = { Accept: "application/json" };
  const unsafe = method !== "GET" && method !== "HEAD";
  if (unsafe) headers["Content-Type"] = "application/json";
  if (opts.share) headers["X-Rivu-Share"] = opts.share;
  let res: Response;
  try {
    res = await fetch(path, {
      method,
      headers,
      credentials: "same-origin",
      body: unsafe ? JSON.stringify(opts.body ?? {}) : undefined,
      signal: opts.signal,
    });
  } catch (e) {
    if (e instanceof DOMException && e.name === "AbortError") throw e;
    throw new ApiError(0, "network", "network error");
  }
  if (res.status === 204) return undefined as T;
  const text = await res.text();
  let data: unknown = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    /* 非 JSON（代理错误页等），按状态码处理 */
  }
  if (!res.ok) {
    const d = (data ?? {}) as { error?: string; message?: string; retry_after?: number };
    const header = Number(res.headers.get("Retry-After"));
    throw new ApiError(
      res.status,
      d.error ?? `http_${res.status}`,
      d.message ?? res.statusText,
      d.retry_after ?? (Number.isFinite(header) && header > 0 ? header : null),
    );
  }
  return data as T;
}

const base = "/api/internal";

export const api = {
  setup: (body: { token?: string; email: string; password: string; org_name: string; name?: string }) =>
    request<{ user: User; organization: Org }>("POST", `${base}/setup`, { body }),
  login: (body: { email: string; password: string; device_label?: string }) =>
    request<{ user: User }>("POST", `${base}/login`, { body }),
  logout: () => request<void>("POST", `${base}/logout`),
  logoutAll: () => request<void>("POST", `${base}/logout-all`),
  me: () => request<Me>("GET", `${base}/me`),
  sites: () => request<{ sites: Site[] }>("GET", `${base}/sites`),
  createSite: (org: number, body: { domain: string; name?: string; timezone: string }) =>
    request<{ site: Site }>("POST", `${base}/orgs/${org}/sites`, { body }),
  siteSettings: (site: number | string) => request<SettingsBody>("GET", `${base}/sites/${site}/settings`),
  patchSite: (site: number | string, body: SitePatch) => request<SettingsBody>("PATCH", `${base}/sites/${site}`, { body }),
  deleteSite: (site: number | string, confirm: string) => request<void>("DELETE", `${base}/sites/${site}`, { body: { confirm } }),
  addHostname: (site: number | string, pattern: string) =>
    request<{ hostname: Hostname }>("POST", `${base}/sites/${site}/hostnames`, { body: { pattern } }),
  removeHostname: (site: number | string, pattern: string) =>
    request<void>("DELETE", `${base}/sites/${site}/hostnames/${encodeURIComponent(pattern)}`),
  shield: (site: number | string) => request<{ rules: ShieldRule[]; max_rules: number }>("GET", `${base}/sites/${site}/shield`),
  addShieldRule: (site: number | string, body: { type: ShieldType; action: "deny" | "allow"; value: string; description?: string }) =>
    request<{ rule: ShieldRule }>("POST", `${base}/sites/${site}/shield`, { body }),
  removeShieldRule: (site: number | string, id: string) =>
    request<void>("DELETE", `${base}/sites/${site}/shield/${encodeURIComponent(id)}`),
  sharedLinks: (site: number | string) => request<{ links: SharedLink[] }>("GET", `${base}/sites/${site}/shared-links`),
  createSharedLink: (site: number | string, body: { name?: string; password?: string; expires_at?: string }) =>
    request<{ link: SharedLink }>("POST", `${base}/sites/${site}/shared-links`, { body }),
  revokeSharedLink: (site: number | string, id: number) =>
    request<void>("DELETE", `${base}/sites/${site}/shared-links/${id}`),
  sessions: () => request<{ sessions: SessionInfo[] }>("GET", `${base}/sessions`),
  revokeSession: (id: number) => request<void>("DELETE", `${base}/sessions/${id}`),
  query: (site: number | string, spec: unknown, opts: { share?: string; signal?: AbortSignal } = {}) =>
    request<QueryResult>("POST", `${base}/sites/${site}/query`, { ...opts, body: spec }),
  realtime: (site: number | string, opts: { share?: string; signal?: AbortSignal } = {}) =>
    request<QueryResult>("GET", `${base}/sites/${site}/realtime`, opts),
  shared: (slug: string) =>
    request<{ password_required: boolean; authorized: boolean; site?: PublicSite }>(
      "GET",
      `${base}/shared/${encodeURIComponent(slug)}`,
    ),
  sharedAuth: (slug: string, password: string) =>
    request<{ authorized: true; site: PublicSite }>("POST", `${base}/shared/${encodeURIComponent(slug)}/auth`, {
      body: { password },
    }),
  publicSite: (key: string) =>
    request<{ site: PublicSite }>("GET", `${base}/public/${encodeURIComponent(key)}`),
};
