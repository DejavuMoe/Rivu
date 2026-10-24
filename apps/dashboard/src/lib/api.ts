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
