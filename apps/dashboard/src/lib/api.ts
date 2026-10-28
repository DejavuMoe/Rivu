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
    warnings?: { code: string; message: string }[];
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
  /** 站点脚本开关（tracker-and-protocol.md §2.2）；开了任一 hash 以外的项就发 full 脚本 */
  tracker: TrackerSwitches;
  /** null：用实例默认值 */
  ingest_rate_limit_per_min: number | null;
  geo_level: "country" | "region" | "city";
  /** 站点的原始数据保留天数；null = 实例默认（`limits.retention_days_max`）。 */
  retention_days: number | null;
}

export type TrackerKey = "hash" | "engagement" | "outbound" | "downloads" | "forms" | "tagged";
export type TrackerSwitches = Record<TrackerKey, boolean>;

export interface Hostname {
  pattern: string;
  created_at: string;
}

export interface SettingsBody {
  site: { id: number; site_key: string; name: string; timezone: string; is_public: boolean; created_at: string };
  settings: SiteSettings;
  hostnames: Hostname[];
  /** 默认上限为 null：全局没有限流 */
  limits: { ingest_rate_limit_default_per_min: number | null; shield_max_rules: number; ingest_rate_limit_max_per_min: number; retention_days_max: number };
  /** 当前加载的地理库能力；没有库为 null */
  geo_capability: "country" | "city" | null;
}

export interface SitePatch {
  name?: string;
  timezone?: string;
  week_starts_on?: number;
  is_public?: boolean;
  collect_enabled?: boolean;
  /** 只改给出的键 */
  tracker?: Partial<TrackerSwitches>;
  ingest_rate_limit_per_min?: number | null;
  geo_level?: "country" | "region" | "city";
  retention_days?: number | null;
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

export type GoalKind = "event" | "page";

export interface Goal {
  id: number;
  kind: GoalKind;
  event_name: string | null;
  page_path: string | null;
  display_name: string | null;
  /** 显示名，否则事件名，否则页面路径：看板目标卡片与筛选里用的名字 */
  label: string;
  created_at: string;
}

export type NewGoal = { kind: "event"; event_name: string; display_name?: string } | { kind: "page"; page_path: string; display_name?: string };

export interface SharedLink {
  id: number;
  name: string | null;
  slug: string;
  has_password: boolean;
  expires_at: string | null;
  created_at: string;
  created_by: { id: number; name: string | null; email: string } | null;
}

export interface OrgMember {
  user_id: number;
  name: string | null;
  email: string;
  role: Role;
  joined_at: string;
  last_active_at: string | null;
  me: boolean;
}

export interface SiteRef {
  id: number;
  name: string;
}

export interface OrgInvitation {
  id: number;
  email: string;
  /** 站点访客邀请没有组织角色 */
  role: Exclude<Role, "owner"> | null;
  site: SiteRef | null;
  site_role: "editor" | "viewer" | null;
  expires_at: string;
  invited_by: string | null;
}

export interface SiteGuest {
  id: number;
  user_id: number;
  name: string | null;
  email: string;
  site: SiteRef;
  role: "editor" | "viewer";
}

export interface MembersBody {
  members: OrgMember[];
  invitations: OrgInvitation[];
  guests: SiteGuest[];
  can_manage: boolean;
  my_role: Role;
}

export type InviteBody =
  | { email: string; role: Exclude<Role, "owner"> }
  | { email: string; site_id: number; site_role: "editor" | "viewer" };

export interface ApiKeyItem {
  id: number;
  name: string;
  key_prefix: string;
  scopes: string[];
  sites: { site_key: string; name: string }[];
  created_at: string;
  last_used_at: string | null;
  expires_at: string | null;
}

export interface ApiKeysBody {
  keys: ApiKeyItem[];
  limits: { hourly: number; burst: number; concurrency: number };
}

export interface InvitationInfo {
  email: string;
  organization: { id: number; name: string };
  role: Exclude<Role, "owner"> | null;
  site: SiteRef | null;
  site_role: "editor" | "viewer" | null;
  inviter: string | null;
  expires_at: string;
  account_exists: boolean;
  signed_in_as: string | null;
}

export type DiagnosticsRange = "1h" | "24h" | "7d";

/** 接入诊断（GET /sites/{site}/diagnostics）：时间为 UTC ISO，桶按范围对齐，没有数据的桶补 0。 */
export interface Diagnostics {
  range: DiagnosticsRange;
  step_seconds: number;
  buckets: { t: string; accepted: number; dropped: number }[];
  totals: { accepted: number; dropped: number };
  /** 指标去掉 dropped_ 前缀，次数从多到少 */
  reasons: { reason: string; count: number }[];
  /** 脚本版本（数字；0 表示请求没带版本），新版本在前 */
  versions: { version: number; events: number; last_seen: string }[];
  /** 最后一个有 accepted 的分钟（精确到分钟）；从没有过为 null */
  last_event_at: string | null;
  /** 最后一个桶包含「现在」，数据未完整 */
  partial_last: boolean;
}

/** 恢复窗口内的已删除站点（GET /orgs/{org}/deleted-sites）；purge_at 之后数据被清除、不能再恢复。 */
export interface DeletedSite {
  id: number;
  site_key: string;
  name: string;
  domain: string | null;
  deleted_at: string;
  purge_at: string;
}

/** 按日期范围删除统计数据的一条请求；日期是站点本地日期 YYYY-MM-DD。 */
export interface StatsDeletion {
  id: number;
  from: string;
  to: string;
  status: "pending" | "running" | "done" | "failed" | "cancelled";
  created_at: string;
  completed_at: string | null;
}

/** 一次数据导入（Plausible 导出）；日期是站点本地日期，导入完成前为 null。 */
export interface SiteImport {
  id: number;
  source: "plausible_csv";
  status: "importing" | "completed" | "failed";
  file_name: string;
  start_date: string | null;
  end_date: string | null;
  rows_imported: number;
  /** 失败原因的代码，界面据此给出说明 */
  error_code: "invalid_zip" | "not_plausible" | "invalid_csv" | "no_days" | "overlap" | "interrupted" | "internal" | null;
  imported_by: string | null;
  created_at: string;
  completed_at: string | null;
}

/** 账户的两步验证状态（`available`：服务器配置了 RIVU_SECRET_KEY）。 */
export interface TotpStatus {
  available: boolean;
  enabled: boolean;
  recovery_remaining: number | null;
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

/** CSV 导出（看板当前视图）：返回 zip 与服务端给的文件名。 */
async function exportCsv(site: number | string, body: unknown): Promise<{ blob: Blob; name: string }> {
  let res: Response;
  try {
    res = await fetch(`${base}/sites/${site}/export`, {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/zip, application/json" },
      credentials: "same-origin",
      body: JSON.stringify(body),
    });
  } catch {
    throw new ApiError(0, "network", "network error");
  }
  if (!res.ok) {
    let d: { error?: string; message?: string } = {};
    try {
      d = await res.json();
    } catch {
      /* 非 JSON（代理错误页等），按状态码处理 */
    }
    throw new ApiError(res.status, d.error ?? `http_${res.status}`, d.message ?? res.statusText);
  }
  const name = /filename="([^"]+)"/.exec(res.headers.get("Content-Disposition") ?? "")?.[1] ?? "rivu-export.zip";
  return { blob: await res.blob(), name };
}

/**
 * 上传 Plausible 导出的 zip（请求体是原样字节，`Content-Type: application/zip`）。用 XHR 是为了上传进度；
 * 服务端读完请求体后返回 202，导入在后台跑。
 */
function uploadImport(site: number | string, file: File, onProgress: (fraction: number) => void): Promise<{ import: SiteImport }> {
  return new Promise((resolve, reject) => {
    const xhr = new XMLHttpRequest();
    xhr.open("POST", `${base}/sites/${site}/imports?file_name=${encodeURIComponent(file.name)}`);
    xhr.setRequestHeader("Content-Type", "application/zip");
    xhr.setRequestHeader("Accept", "application/json");
    xhr.withCredentials = true;
    xhr.upload.onprogress = (e) => {
      if (e.lengthComputable) onProgress(e.loaded / e.total);
    };
    xhr.onerror = () => reject(new ApiError(0, "network", "network error"));
    xhr.onload = () => {
      let data: unknown = null;
      try {
        data = xhr.responseText ? JSON.parse(xhr.responseText) : null;
      } catch {
        /* 非 JSON（代理错误页等），按状态码处理 */
      }
      if (xhr.status >= 200 && xhr.status < 300) return resolve(data as { import: SiteImport });
      const d = (data ?? {}) as { error?: string; message?: string };
      reject(new ApiError(xhr.status, d.error ?? `http_${xhr.status}`, d.message ?? xhr.statusText));
    };
    xhr.send(file);
  });
}

export const api = {
  setup: (body: { token?: string; email: string; password: string; org_name: string; name?: string }) =>
    request<{ user: User; organization: Org }>("POST", `${base}/setup`, { body }),
  /** 开了两步验证的账号：密码正确只返回 challenge，凭它调 loginTotp。 */
  login: (body: { email: string; password: string; device_label?: string }) =>
    request<{ user?: User; totp_required?: boolean; challenge?: string }>("POST", `${base}/login`, { body }),
  loginTotp: (body: { challenge: string; code: string }) =>
    request<{ user: User; recovery_remaining?: number }>("POST", `${base}/login/totp`, { body }),
  authOptions: () => request<{ password_reset: boolean }>("GET", `${base}/auth-options`),
  requestPasswordReset: (email: string) => request<void>("POST", `${base}/password-reset`, { body: { email } }),
  checkPasswordReset: (token: string) => request<void>("GET", `${base}/password-reset/${encodeURIComponent(token)}`),
  resetPassword: (token: string, password: string) =>
    request<void>("POST", `${base}/password-reset/${encodeURIComponent(token)}`, { body: { password } }),
  totpStatus: () => request<TotpStatus>("GET", `${base}/me/totp`),
  totpSetup: (password: string) =>
    request<{ secret: string; uri: string; qr_svg: string }>("POST", `${base}/me/totp/setup`, { body: { password } }),
  totpEnable: (code: string) => request<{ recovery_codes: string[] }>("POST", `${base}/me/totp/enable`, { body: { code } }),
  totpRecoveryCodes: (password: string) =>
    request<{ recovery_codes: string[] }>("POST", `${base}/me/totp/recovery-codes`, { body: { password } }),
  totpDisable: (password: string) => request<void>("POST", `${base}/me/totp/disable`, { body: { password } }),
  logout: () => request<void>("POST", `${base}/logout`),
  logoutAll: () => request<void>("POST", `${base}/logout-all`),
  me: () => request<Me>("GET", `${base}/me`),
  sites: () => request<{ sites: Site[] }>("GET", `${base}/sites`),
  createSite: (org: number, body: { domain: string; name?: string; timezone: string }) =>
    request<{ site: Site }>("POST", `${base}/orgs/${org}/sites`, { body }),
  siteSettings: (site: number | string) => request<SettingsBody>("GET", `${base}/sites/${site}/settings`),
  patchSite: (site: number | string, body: SitePatch) => request<SettingsBody>("PATCH", `${base}/sites/${site}`, { body }),
  deleteSite: (site: number | string, confirm: string) => request<void>("DELETE", `${base}/sites/${site}`, { body: { confirm } }),
  deletedSites: (org: number) => request<{ sites: DeletedSite[] }>("GET", `${base}/orgs/${org}/deleted-sites`),
  restoreSite: (site: number | string) => request<{ site: Site }>("POST", `${base}/sites/${site}/restore`),
  statsDeletions: (site: number | string) =>
    request<{ deletions: StatsDeletion[] }>("GET", `${base}/sites/${site}/stats-deletions`),
  deleteStats: (site: number | string, body: { from: string; to: string; confirm: string }) =>
    request<{ deletion: StatsDeletion }>("POST", `${base}/sites/${site}/stats-deletions`, { body }),
  addHostname: (site: number | string, pattern: string) =>
    request<{ hostname: Hostname }>("POST", `${base}/sites/${site}/hostnames`, { body: { pattern } }),
  removeHostname: (site: number | string, pattern: string) =>
    request<void>("DELETE", `${base}/sites/${site}/hostnames/${encodeURIComponent(pattern)}`),
  shield: (site: number | string) => request<{ rules: ShieldRule[]; max_rules: number }>("GET", `${base}/sites/${site}/shield`),
  addShieldRule: (site: number | string, body: { type: ShieldType; action: "deny" | "allow"; value: string; description?: string }) =>
    request<{ rule: ShieldRule }>("POST", `${base}/sites/${site}/shield`, { body }),
  removeShieldRule: (site: number | string, id: string) =>
    request<void>("DELETE", `${base}/sites/${site}/shield/${encodeURIComponent(id)}`),
  goals: (site: number | string) => request<{ goals: Goal[]; max_goals: number }>("GET", `${base}/sites/${site}/goals`),
  addGoal: (site: number | string, body: NewGoal) => request<{ goal: Goal }>("POST", `${base}/sites/${site}/goals`, { body }),
  removeGoal: (site: number | string, id: number) => request<void>("DELETE", `${base}/sites/${site}/goals/${id}`),
  imports: (site: number | string) =>
    request<{ imports: SiteImport[]; native_from: string | null }>("GET", `${base}/sites/${site}/imports`),
  uploadImport,
  exportCsv,
  removeImport: (site: number | string, id: number) => request<void>("DELETE", `${base}/sites/${site}/imports/${id}`),
  sharedLinks: (site: number | string) => request<{ links: SharedLink[] }>("GET", `${base}/sites/${site}/shared-links`),
  createSharedLink: (site: number | string, body: { name?: string; password?: string; expires_at?: string }) =>
    request<{ link: SharedLink }>("POST", `${base}/sites/${site}/shared-links`, { body }),
  revokeSharedLink: (site: number | string, id: number) =>
    request<void>("DELETE", `${base}/sites/${site}/shared-links/${id}`),
  /** 成功后服务端撤销全部旧会话并通过 Set-Cookie 签发新会话 */
  changePassword: (body: { current: string; new: string }) => request<Record<string, never>>("POST", `${base}/me/password`, { body }),
  diagnostics: (site: number | string, range: DiagnosticsRange, opts: { signal?: AbortSignal } = {}) =>
    request<Diagnostics>("GET", `${base}/sites/${site}/diagnostics?range=${range}`, opts),
  sessions: () => request<{ sessions: SessionInfo[] }>("GET", `${base}/sessions`),
  revokeSession: (id: number) => request<void>("DELETE", `${base}/sessions/${id}`),
  members: (org: number) => request<MembersBody>("GET", `${base}/orgs/${org}/members`),
  setMemberRole: (org: number, user: number, role: Role) =>
    request<OrgMember>("PATCH", `${base}/orgs/${org}/members/${user}`, { body: { role } }),
  removeMember: (org: number, user: number) => request<void>("DELETE", `${base}/orgs/${org}/members/${user}`),
  invite: (org: number, body: InviteBody) =>
    request<{ invitation: OrgInvitation; url: string; email_sent: boolean }>("POST", `${base}/orgs/${org}/invitations`, { body }),
  revokeInvitation: (org: number, id: number) => request<void>("DELETE", `${base}/orgs/${org}/invitations/${id}`),
  removeGuest: (org: number, id: number) => request<void>("DELETE", `${base}/orgs/${org}/guests/${id}`),
  apiKeys: (org: number) => request<ApiKeysBody>("GET", `${base}/orgs/${org}/api-keys`),
  createApiKey: (org: number, body: { name: string; sites: string[] | null; expires_in_days: number | null }) =>
    request<{ key: string; item: ApiKeyItem }>("POST", `${base}/orgs/${org}/api-keys`, { body }),
  revokeApiKey: (org: number, id: number) => request<void>("DELETE", `${base}/orgs/${org}/api-keys/${id}`),
  invitation: (token: string) => request<InvitationInfo>("GET", `${base}/invitations/${encodeURIComponent(token)}`),
  acceptInvitation: (token: string, body: { name?: string; password?: string } = {}) =>
    request<{ organization_id: number; site_id: number | null }>(
      "POST",
      `${base}/invitations/${encodeURIComponent(token)}/accept`,
      { body },
    ),
  query: (site: number | string, spec: unknown, opts: { share?: string; signal?: AbortSignal } = {}) =>
    request<QueryResult>("POST", `${base}/sites/${site}/query`, { ...opts, body: spec }),
  /** 属性卡片的键列表（时间范围与筛选同看板），事件多的在前 */
  propertyKeys: (site: number | string, spec: unknown, opts: { share?: string; signal?: AbortSignal } = {}) =>
    request<{ keys: { key: string; events: number }[] }>("POST", `${base}/sites/${site}/property-keys`, { ...opts, body: spec }),
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
