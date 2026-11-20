//! 看板的内部端点（`/api/internal/*`，挂在公共端口上）：首次初始化、登录与会话、
//! 看板用的只读端点。**只有后端，没有任何页面**（页面要等 `designs/` 原型审批）。
//!
//! | 方法与路径 | 鉴权 | 说明 |
//! |---|---|---|
//! | `POST /api/internal/setup` | 无（`RIVU_SETUP_TOKEN`） | 首次初始化：创建首个用户（实例管理员）、首个组织与 owner 关系，并直接登录；已有用户后一律 404 |
//! | `POST /api/internal/login` | 无 | 邮箱 + 密码，成功设置会话 cookie |
//! | `POST /api/internal/logout` | 会话（缺失也成功） | 撤销当前会话 |
//! | `POST /api/internal/logout-all` | 会话 | 退出所有设备 |
//! | `GET /api/internal/sessions` | 会话 | 当前用户的有效会话（设备标签、最近使用、是否当前） |
//! | `DELETE /api/internal/sessions/{id}` | 会话 | 撤销自己的某个会话 |
//! | `GET /api/internal/me` | 会话 | 用户与所属组织、角色 |
//! | `POST /api/internal/me/password` | 会话 | 改密码，请求体 `{"current","new"}`；当前密码不对 400 `wrong_password`（总是做哈希校验）、新密码长度不合 400 `invalid_new_password`、与当前相同 400 `same_password`；`(用户, IP)` 5 次 / 60 秒（429）；同一事务换哈希并撤销全部会话，再签发新会话（`Set-Cookie`），响应 `{}` |
//! | `GET /api/internal/sites` | 会话 | 可见的站点（按组织） |
//! | `POST /api/internal/sites/{site}/query` | 会话、共享链接或公开站点 | 看板查询，请求体是 `QuerySpec` |
//! | `POST /api/internal/sites/{site}/property-keys` | 同上 | 属性卡片的键列表：请求体是看板当前的 QuerySpec（只用时间范围与筛选），返回 `{"keys":[{"key","events"}]}`，事件多的在前，至多 100 个；含导入的属性（区间覆盖导入的天、没有筛选时） |
//! | `POST /api/internal/sites/{site}/export` | 会话，有效角色 editor+（`export.create`） | CSV 导出：请求体 `{"date_range","filters"?,"interval"?}`，返回 zip（`Content-Disposition: attachment`），文件与列见 [`crate::export`] |
//! | `GET /api/internal/sites/{site}/realtime` | 同上 | 最近 5 分钟的访客，等价于 `date_range: "realtime"` 的 `visitors` 查询 |
//! | `GET /api/internal/shared/{slug}` | 无（限流） | 共享链接的入口信息：是否要密码、是否已授权；已授权才带站点 |
//! | `POST /api/internal/shared/{slug}/auth` | 无（限流） | 共享链接密码，通过后签发 12 小时的访问凭证 cookie |
//! | `GET /api/internal/public/{site}` | 无（限流） | 公开站点（`is_public`）的入口信息 |
//! | `POST /api/internal/orgs/{org_id}/sites` | 会话，组织 admin+ | 建站（域名规范化、白名单 = [域名]） |
//! | `GET /api/internal/sites/{site}/settings` | 会话，有效角色 editor+ | 站点设置（含流量激增提醒阈值 `spike_threshold`）、白名单、限额、地理库能力与 `mail_available` |
//! | `GET /api/internal/sites/{site}/diagnostics?range=1h\|24h\|7d` | 会话，有效角色 editor+ | 接入诊断：按 `ingest_counters` 聚合的时间桶（已接收 / 已丢弃）、合计、丢弃原因、脚本版本、最后收到事件的时间；实现在 [`crate::diagnostics`] |
//! | `PATCH /api/internal/sites/{site}` | 会话，组织 admin+ | 改名称、时区、周起始日、公开、采集、hash 路由、限速、地理精度、流量激增提醒阈值 |
//! | `DELETE /api/internal/sites/{site}` | 会话，组织 admin+ | 软删除（请求体 `confirm` 须等于站点名），同一事务写入站点删除意图，`RIVU_SITE_RESTORE_DAYS`（默认 7）天内可恢复，之后 `ch_delete_batch` 清除统计数据 |
//! | `GET /api/internal/orgs/{org_id}/deleted-sites` | 会话，组织 admin+ | 恢复窗口内的已删除站点 `{"sites":[{id,site_key,name,domain,deleted_at,purge_at}]}` |
//! | `POST /api/internal/sites/{site}/restore` | 会话，站点所属组织 admin+（非成员 404） | 恢复已删除的站点，响应同站点列表的一项；窗口已过或清除已开始 409 `conflict` |
//! | `GET /api/internal/sites/{site}/stats-deletions`、`POST` | 会话，组织 admin+ | 按日期范围删除统计数据：列表（最近 50 条，新的在前）与请求（`{"from","to","confirm"}`，站点本地日期，`to` 不晚于今天，最多 731 天，`confirm` 须等于站点名） |
//! | `POST /api/internal/sites/{site}/hostnames`、`DELETE …/hostnames/{pattern}` | 会话，组织 admin+ | 白名单增删 |
//! | `GET /api/internal/sites/{site}/shield`（editor+）、`POST`（admin+）、`DELETE …/shield/{id}`（admin+） | 会话 | 屏蔽规则 |
//! | `GET /api/internal/sites/{site}/shared-links`、`POST`、`DELETE …/shared-links/{id}` | 会话，组织 editor+ | 共享链接的列表、创建、吊销 |
//! | `GET /api/internal/sites/{site}/goals`、`POST`、`DELETE …/goals/{id}` | 会话，有效角色 editor+（含站点访客 editor，`site.content.write`） | 目标的列表、添加（`{"kind":"event","event_name"}` 或 `{"kind":"page","page_path"}`，可带 `display_name`；同类同值或同名 409 `conflict`，每站 100 个 409 `limit_reached`）、删除；实现在 [`crate::goals_admin`] |
//! | `GET /api/internal/sites/{site}/imports`、`POST …/imports?file_name=`、`DELETE …/imports/{id}` | 会话，组织 admin+ | Plausible 导出的导入：列表（带 `native_from`，站点自己的数据开始的那天）、上传 zip（请求体是原样字节，最大 128 MiB；202，后台导入；进行中时 409 `import_running`）、删除（进行中 409）；实现在 [`crate::imports_admin`] |
//! | `/api/internal/auth-options`、`/password-reset`、`/login/totp`、`/me/totp/*` | 见 [`crate::account`] | 找回密码、登录的第二步、账户的两步验证 |
//! | `GET /api/internal/orgs/{org_id}/consolidated?range=\|from=&to=&interval=&compare=` | 会话；组织里至少一个能读统计的站点（成员或站点访客），否则 404 | 多站点汇总：调用者能读的全部站点的访客（各站之和）、访问、浏览量、跳出率与访问时长（按访问加权）、趋势与每个站点的数字，最多 100 个站点；实现在 [`crate::consolidated`] |
//!
//! | `GET /api/internal/orgs/{org_id}/members` | 会话，任一组织成员 | 成员、待处理邀请、站点访客，带 `can_manage` 与 `my_role` |
//! | `PATCH /api/internal/orgs/{org_id}/members/{user_id}`、`DELETE` | 会话，组织 admin+ | 改角色、移除成员（移除同时吊销其在该组织的 API key）；admin 不能碰 owner，不能改自己，最后一名 owner 409 |
//! | `POST /api/internal/orgs/{org_id}/invitations`、`DELETE …/invitations/{id}`、`DELETE …/guests/{id}` | 会话，组织 admin+ | 邀请（组织成员或站点访客，返回一次性链接，不发邮件）、撤销邀请、移除站点访客 |
//! | `GET /api/internal/invitations/{token}` | 无（限流） | 邀请页信息；无效、过期、已接受、已撤销一律 404 |
//! | `POST /api/internal/invitations/{token}/accept` | 无（限流）；已有账号须是该邮箱的会话 | 接受邀请；没有账号则建账号并登录 |
//! | `GET /api/internal/orgs/{org_id}/api-keys`、`POST`、`DELETE …/api-keys/{id}` | 会话，任一组织成员 | 只管理自己的 API key（明文仅创建时返回一次） |
//!
//! 站点管理端点的实现在 [`crate::site_admin`]：站点对调用者不可见一律 404，可见但角色不够 403 `forbidden`；
//! 成员、邀请与 API key 端点的实现在 [`crate::org_admin`]（组织不存在或调用者不是成员 404，角色不够 403）。
//!
//! 没有匹配的 `/api/internal/*` 是 404（`public.rs` 的通配）。`{site}` 是站点的数字 id（`sites.id`，文档里的 `{site_id}`）
//! 或 12 位的 `site_key`；匿名的公开看板只知道 `site_key`，所以两种都收。
//!
//! # 站点访问判定（`platform-and-ops.md` §4.2）
//!
//! 「无权限」一律 404，且与「站点不存在」的响应相同。顺序遇到第一个明确结论就返回：
//!
//! ```text
//! 1 站点不存在、已删除、组织已删除                                   → 404
//! 3 请求带了共享链接（头 `X-Rivu-Share: <slug>`）
//!     slug 无效、过期、吊销、不属于这个站点                           → 404（带了链接就不再往下找别的主体）
//!     链接有密码而没有有效的访问凭证 cookie                           → 401 password_required
//!     否则放行：只读统计，叠加链接绑定的 segment 过滤；公开档超时与 (IP, 链接) 限流
//! 4 有效的登录会话，且是该站点所属组织的成员或站点访客              → 放行（看板档超时）
//! 6 站点 is_public                                                  → 放行：匿名只读，公开档超时与 (IP, 站点) 限流
//! 7 其他                                                            → 404
//! ```
//!
//! 文档表里共享链接（步骤 3）排在登录用户（步骤 4）之前：带了链接的请求就是「通过链接看」，要受链接的 segment 约束，
//! 即使调用者同时登录了。第 5 步（API key）属于公开 API，不在这里；实例管理员没有隐式的站点访问权。
//!
//! # 会话与 cookie
//!
//! 见 [`rivu_auth::session`]：32 字节随机令牌，库里只存 SHA-256；cookie `__Host-rivu_session`
//! （`HttpOnly; Secure; SameSite=Lax; Path=/`，`Max-Age` 14 天），明文 HTTP 的开发模式（`RIVU_COOKIE_SECURE` 关或
//! `RIVU_BASE_URL` 是 http）下改用 `rivu_session` 且不带 `Secure`。距上次续期超过 10 分钟的请求才写库续期，
//! 并在响应里刷新 cookie。读取路径同一条查询校验会话未撤销、未过期、用户未禁用也未删除。
//!
//! # CSRF
//!
//! 所有会改变状态的方法（含查询用的 `POST`）：`Content-Type: application/json`（上传导出用 `application/zip`）且 `Origin` 与 `RIVU_BASE_URL` 同源，
//! 见 [`rivu_auth::csrf`]；拒绝时 `403 csrf_rejected`。没有 CORS。
//!
//! # 限流（`platform-and-ops.md` §4.4，`rivu-ratelimit::PolicyLimiter`，进程内）
//!
//! - 登录阻断两个桶，超限 `429` 统一提示（带 `Retry-After`）：每客户端 IP 20 次 / 60 秒（防同一 IP 对多个账号喷洒）；
//!   每 `(账号, 客户端 IP)` 5 次 / 60 秒（账号是规范化邮箱，不论是否存在；防同一来源爆破一个账号）。
//!   **没有单纯按账号的阻断桶**：否则不停用受害者的邮箱登录就能让真用户一直 429（锁号）；
//! - 登录失败告警（只告警不阻断，只计失败、被限流的请求不算）：同一账号 1 小时内失败达到 50 次、全实例每分钟失败达到 300 次，
//!   各打一条 `warn!` 并累加 `rivu_auth_login_alerts_total{kind=account|global}`，同一窗口同一对象只告警一次；
//!   账号表有上限（10 万），满员先清过期窗口，仍满则新账号本窗口不计数；
//! - 首次初始化：每 IP 5 次 / 60 秒（挡住猜令牌）；
//! - 共享链接与公开看板：`(客户端 IP, 链接)` / `(客户端 IP, 站点)`，默认 300 次 / 分钟、突发 60
//!   （`RIVU_PUBLIC_VIEW_RATE_PER_MIN`、`RIVU_PUBLIC_VIEW_BURST`；一个看板页加载并发约 10 个查询再加实时轮询）；
//!   共享链接密码尝试 5 次 / 分钟；
//! - IPv6 客户端按 /64 归并（一个终端用户通常拥有整个 /64）。
//!
//! 看板会话用户的查询暂不限流（文档 §4.4 写明「会话用户的内部 API 尚未接入」）。
//!
//! # 日志与指标
//!
//! 日志里没有密码、令牌、cookie、请求体；登录失败只记（存在时的）用户 id 与结果，不记邮箱与 IP；告警日志同样。
//! 指标：`rivu_login_total{result=success|failure|rate_limited}`、`rivu_auth_login_alerts_total{kind=account|global}`、
//! `rivu_auth_active_sessions`（每分钟刷新）、
//! `rivu_csrf_rejected_total{reason}`、`rivu_api_requests_total{route="internal.*"}`、
//! `rivu_query_seconds{route=internal.query|internal.realtime|internal.consolidated|shared.query|shared.realtime|public.query|public.realtime}`。

use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{MethodRouter, delete, get, post};
use jiff::Timestamp;
use rivu_auth::csrf::{Csrf, CsrfError};
use rivu_auth::identity::{device_label, normalize_email, normalize_org_name, slugify};
use rivu_auth::password::{MAX_PASSWORD_CHARS, PasswordService, validate_password};
use rivu_auth::scope::{Action, OrgRole, allowed};
use rivu_auth::session::{
    self, CookieConfig, RENEW_AFTER, SESSION_TTL, SHARE_GRANT_TTL, cookie_value,
};
use rivu_common::config::{
    Cidr, Config, DEFAULT_PUBLIC_VIEW_BURST, DEFAULT_PUBLIC_VIEW_RATE_PER_MIN,
};
use rivu_geo::{GeoDb, NameLang};
use rivu_query::spec::Filter;
use rivu_query::{
    ExecOptions, QueryCache, QueryError, QueryResult, QuerySpec, SiteContext, TenantScope,
};
use rivu_ratelimit::PolicyLimiter;
use rivu_storage_pg::access::{self, SiteRef, UserSiteRoles};
use rivu_storage_pg::sessions::ActiveSession;
use rivu_storage_pg::shared_links::{self, LinkInfo};
use rivu_storage_pg::sites::SiteOwner;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::query_api::{self, Fail, no_store};

/// 请求头：带上共享链接的 slug（共享链接页面发给 `query` 与 `realtime`）。
pub const SHARE_HEADER: &str = "x-rivu-share";

/// 认证类端点的请求体上限。
pub(crate) const AUTH_BODY_MAX: usize = 16 * 1024;
/// 查询请求体的上限（与公开 API 相同）。
const QUERY_BODY_MAX: usize = query_api::MAX_BODY_BYTES;
/// 实时查询的结果缓存时间（`system-design.md` §6.9：实时 3 s）。
const REALTIME_CACHE_TTL: Duration = Duration::from_secs(3);

/// 各类限流的参数。默认值是文档的值，测试里可以放宽。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DashLimits {
    /// 登录的 `(账号, 客户端 IP)` 组合桶与首次初始化的每 IP 桶：每个窗口允许的次数。
    pub login_attempts: u32,
    /// 登录的每 IP 桶（防同一 IP 对多个账号喷洒）：每个窗口允许的次数，比 `login_attempts` 宽。
    pub login_ip_attempts: u32,
    pub login_window: Duration,
    /// 共享链接与公开看板：每分钟次数与突发容量（来自 `RIVU_PUBLIC_VIEW_*`）。
    pub view_per_min: u32,
    pub view_burst: u32,
    /// 共享链接密码：每分钟尝试次数。
    pub password_per_min: u32,
    /// 登录失败告警（只告警不阻断）：同一账号 1 小时内失败达到这个次数时告警一次。
    pub account_fail_alert: u32,
    /// 登录失败告警：全实例每分钟失败达到这个次数时告警一次。
    pub global_fail_alert: u32,
}

impl Default for DashLimits {
    fn default() -> Self {
        Self::with_view(DEFAULT_PUBLIC_VIEW_RATE_PER_MIN, DEFAULT_PUBLIC_VIEW_BURST)
    }
}

impl DashLimits {
    /// 公开看板的次数与突发取配置（突发容量不超过每分钟次数，与 `RIVU_API_BURST` 的约定一致），其余用默认值。
    pub fn from_config(config: &Config) -> Self {
        Self::with_view(config.public_view_rate_per_min, config.public_view_burst)
    }

    fn with_view(per_min: u32, burst: u32) -> Self {
        DashLimits {
            view_per_min: per_min,
            view_burst: burst.min(per_min),
            login_attempts: 5,
            login_ip_attempts: 20,
            login_window: Duration::from_secs(60),
            password_per_min: 5,
            account_fail_alert: 50,
            global_fail_alert: 300,
        }
    }
}

/// 账号失败计数窗口（固定窗口，从该账号窗口内第一次失败起算）。
const ACCOUNT_FAIL_WINDOW: Duration = Duration::from_secs(3600);
/// 全局失败计数窗口。
const GLOBAL_FAIL_WINDOW: Duration = Duration::from_secs(60);
/// 账号失败表的上限：满员时先清掉过期的窗口，仍然满员则新账号本窗口不计数（内存有界，
/// 宁可漏报也不让告警结构成为被海量不同账号冲击的内存入口）。
const MAX_TRACKED_ACCOUNTS: usize = 100_000;

/// 一个固定窗口内的失败计数。
struct FailWindow {
    start: Instant,
    count: u32,
    /// 本窗口已经告警过：同一窗口只告警一次。
    alerted: bool,
}

impl FailWindow {
    /// 记一次失败；本窗口内第一次达到阈值时返回 `true`。
    fn hit(&mut self, now: Instant, window: Duration, threshold: u32) -> bool {
        if now.saturating_duration_since(self.start) >= window {
            *self = FailWindow {
                start: now,
                count: 0,
                alerted: false,
            };
        }
        self.count = self.count.saturating_add(1);
        if self.count >= threshold && !self.alerted {
            self.alerted = true;
            return true;
        }
        false
    }
}

/// 登录失败的告警判定：只统计失败（被限流拒绝的请求不算），只告警、不阻断。
/// 账号键是邮箱的哈希（哈希种子进程内随机），内存里不留邮箱原文。
struct LoginWatch {
    hasher: std::hash::RandomState,
    accounts: Mutex<std::collections::HashMap<u64, FailWindow>>,
    global: Mutex<Option<FailWindow>>,
}

/// 一次失败触发的告警。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LoginAlerts {
    account: bool,
    global: bool,
}

impl LoginWatch {
    fn new() -> Self {
        LoginWatch {
            hasher: std::hash::RandomState::new(),
            accounts: Default::default(),
            global: Default::default(),
        }
    }

    fn record_failure(&self, account: &str, now: Instant, l: &DashLimits) -> LoginAlerts {
        use std::hash::BuildHasher;
        let key = self.hasher.hash_one(account);
        let account_alert = {
            let mut map = self.accounts.lock().unwrap_or_else(|e| e.into_inner());
            if !map.contains_key(&key) && map.len() >= MAX_TRACKED_ACCOUNTS {
                map.retain(|_, w| now.saturating_duration_since(w.start) < ACCOUNT_FAIL_WINDOW);
            }
            if !map.contains_key(&key) && map.len() >= MAX_TRACKED_ACCOUNTS {
                false
            } else {
                map.entry(key)
                    .or_insert_with(|| FailWindow {
                        start: now,
                        count: 0,
                        alerted: false,
                    })
                    .hit(now, ACCOUNT_FAIL_WINDOW, l.account_fail_alert)
            }
        };
        let global_alert = self
            .global
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(|| FailWindow {
                start: now,
                count: 0,
                alerted: false,
            })
            .hit(now, GLOBAL_FAIL_WINDOW, l.global_fail_alert);
        LoginAlerts {
            account: account_alert,
            global: global_alert,
        }
    }
}

pub(crate) struct Limiters {
    /// 登录：每 IP 一桶（防喷洒）。
    pub(crate) login_ip: PolicyLimiter,
    /// 登录：每 `(账号, IP)` 一桶（防同一来源爆破一个账号）。**不再有单纯按账号的桶**：
    /// 否则任何人不停用受害者的邮箱登录就能让真用户一直 429（锁号）。
    login_pair: PolicyLimiter,
    /// 改密码：每 `(用户, IP)` 一桶，参数同登录的 `(账号, IP)` 桶（防拿着被盗的会话爆破当前密码）。
    pub(crate) password_change: PolicyLimiter,
    setup_ip: PolicyLimiter,
    share_view: PolicyLimiter,
    /// 邀请页（查询与接受）：每客户端 IP 一桶，参数同共享链接与公开看板。
    pub(crate) invite_view: PolicyLimiter,
    share_password: PolicyLimiter,
    public_view: PolicyLimiter,
    /// 找回密码的请求：每 IP 一桶、每邮箱一桶，参数同登录（发信有成本，也不能拿来轰炸别人的邮箱）。
    pub(crate) reset_ip: PolicyLimiter,
    pub(crate) reset_email: PolicyLimiter,
}

impl Limiters {
    fn new(l: &DashLimits) -> Self {
        let login =
            || PolicyLimiter::per_window(l.login_attempts, l.login_attempts, l.login_window);
        let view =
            || PolicyLimiter::per_window(l.view_burst, l.view_per_min, Duration::from_secs(60));
        Limiters {
            login_ip: PolicyLimiter::per_window(
                l.login_ip_attempts,
                l.login_ip_attempts,
                l.login_window,
            ),
            login_pair: login(),
            password_change: login(),
            setup_ip: login(),
            share_view: view(),
            invite_view: view(),
            share_password: PolicyLimiter::per_window(
                l.password_per_min,
                l.password_per_min,
                Duration::from_secs(60),
            ),
            public_view: view(),
            reset_ip: login(),
            reset_email: login(),
        }
    }
}

pub(crate) type Clock = Arc<dyn Fn() -> Timestamp + Send + Sync>;

/// 内部端点的依赖。进程里建一个并共享：限流状态在它里面。
pub struct DashApi {
    pub(crate) pg: PgPool,
    /// `rivu_reader` 账号的客户端（已绑定库）。
    pub(crate) reader: clickhouse::Client,
    pub(crate) geo: Option<Arc<GeoDb>>,
    pub(crate) passwords: PasswordService,
    /// `RIVU_SHIELD_MAX_RULES_PER_SITE`。
    pub(crate) shield_max_rules: usize,
    /// `RIVU_INGEST_RATE_LIMIT_PER_MIN`（`None` = 不限流）：站点没有自己的限额时用它，设置页展示。
    pub(crate) ingest_rate_default: Option<u32>,
    /// `RIVU_BASE_URL`：邀请链接的前缀。
    pub(crate) base_url: String,
    /// 公开 API 的限额（`RIVU_API_RATE_PER_HOUR`、`RIVU_API_BURST`、`RIVU_API_MAX_CONCURRENCY`），API key 列表展示。
    pub(crate) api_rate: rivu_common::config::ApiRateConfig,
    /// `RIVU_SITE_RESTORE_DAYS`：站点软删除后的恢复窗口。
    pub(crate) site_restore_days: u32,
    /// 用来在写入日期范围删除意图后立即唤醒它。
    pub(crate) stats_delete_kick: Option<rivu_jobs::StatsDeleteKick>,
    /// `rivu_maintainer` 账号的客户端：导入写汇总层、删除导入；`None` 时导入端点返回 503。
    pub(crate) maintainer: Option<clickhouse::Client>,
    /// 发信（`RIVU_SMTP_URL`）；`None` 时邀请只给链接，找回密码不可用。
    pub(crate) mailer: Option<Arc<rivu_mail::Mailer>>,
    /// 两步验证密钥的加解密（`RIVU_SECRET_KEY`）；`None` 时两步验证不可用。
    pub(crate) vault: Option<Arc<rivu_auth::totp::Vault>>,
    /// 密码正确、还差两步验证的登录：challenge 的 SHA-256 → 用户（5 分钟，最多试 5 次）。
    pub(crate) login_challenges: moka::sync::Cache<[u8; 32], crate::account::Challenge>,
    csrf: Csrf,
    cookies: CookieConfig,
    /// `RIVU_SETUP_TOKEN` 的 SHA-256：比较时两边都是定长摘要，常数时间。
    setup_token: Option<[u8; 32]>,
    trusted_proxies: Vec<Cidr>,
    pub(crate) dashboard_timeout_secs: u64,
    public_timeout_secs: u64,
    /// `RIVU_ROLLUP_MIN_ROWS`（§6.4 选层）。
    pub(crate) rollup_min_rows: Option<u64>,
    /// 结果缓存（与公开 API 共用，§6.9）；`None` 不缓存。
    pub(crate) cache: Option<Arc<QueryCache>>,
    /// `RIVU_RETENTION_DAYS`。
    pub(crate) retention_days: u32,
    pub(crate) limiters: Limiters,
    limits: DashLimits,
    login_watch: LoginWatch,
    realtime_cache: moka::sync::Cache<String, Arc<String>>,
    pub(crate) clock: Clock,
}

impl DashApi {
    pub fn new(
        config: &Config,
        pg: PgPool,
        reader: clickhouse::Client,
        geo: Option<Arc<GeoDb>>,
    ) -> anyhow::Result<Self> {
        let csrf = Csrf::new(&config.base_url).map_err(anyhow::Error::msg)?;
        let limits = DashLimits::from_config(config);
        Ok(DashApi {
            pg,
            reader,
            geo,
            passwords: PasswordService::new(rivu_auth::password::DEFAULT_CONCURRENCY),
            shield_max_rules: config.shield_max_rules_per_site,
            ingest_rate_default: config.ingest_rate_limit_per_min,
            base_url: config.base_url.clone(),
            api_rate: config.api_rate,
            site_restore_days: config.site_restore_days,
            stats_delete_kick: None,
            maintainer: None,
            mailer: match &config.smtp {
                Some(s) => Some(Arc::new(rivu_mail::Mailer::smtp(s.url.expose(), &s.from)?)),
                None => None,
            },
            vault: config.secret_key.as_ref().map(|k| {
                Arc::new(rivu_auth::totp::Vault::new(
                    k.expose(),
                    config.secret_key_fallback.as_ref().map(|f| f.expose()),
                ))
            }),
            login_challenges: moka::sync::Cache::builder()
                .max_capacity(100_000)
                .time_to_live(crate::account::CHALLENGE_TTL)
                .build(),
            csrf,
            cookies: CookieConfig {
                secure: config.cookie_secure,
            },
            setup_token: config
                .setup_token
                .as_ref()
                .map(|t| session::hash_token(t.expose())),
            trusted_proxies: config.trusted_proxies.clone(),
            dashboard_timeout_secs: config.query_timeouts.dashboard_secs,
            public_timeout_secs: config.query_timeouts.public_secs,
            rollup_min_rows: Some(config.rollup_min_rows),
            cache: None,
            retention_days: config.retention_days,
            limiters: Limiters::new(&limits),
            limits,
            login_watch: LoginWatch::new(),
            realtime_cache: moka::sync::Cache::builder()
                .max_capacity(10_000)
                .time_to_live(REALTIME_CACHE_TTL)
                .build(),
            clock: Arc::new(Timestamp::now),
        })
    }

    /// 换限流参数（测试用）。
    pub fn with_limits(mut self, limits: DashLimits) -> Self {
        self.limiters = Limiters::new(&limits);
        self.limits = limits;
        self
    }

    /// 使用结果缓存（与公开 API 共用同一个）。
    pub fn with_cache(mut self, cache: Arc<QueryCache>) -> Self {
        self.cache = Some(cache);
        self
    }

    /// 同进程的 `ch_delete_batch` 唤醒器；`None` 时只等它的定时周期（测试）。
    pub fn with_stats_delete_kick(mut self, kick: Option<rivu_jobs::StatsDeleteKick>) -> Self {
        self.stats_delete_kick = kick;
        self
    }

    /// 换发信器（测试用 `Mailer::memory`）。
    pub fn with_mailer(mut self, mailer: rivu_mail::Mailer) -> Self {
        self.mailer = Some(Arc::new(mailer));
        self
    }

    /// 导入用的 `rivu_maintainer` 客户端。
    pub fn with_maintainer(mut self, ch: clickhouse::Client) -> Self {
        self.maintainer = Some(ch);
        self
    }

    /// 替换「现在」（测试用：相对日期范围与封顶都按它换算）。
    pub fn with_clock(mut self, clock: impl Fn() -> Timestamp + Send + Sync + 'static) -> Self {
        self.clock = Arc::new(clock);
        self
    }

    /// 记一次登录失败并按需告警（只告警不阻断）。日志只带存在时的用户 id（不存在的账号为空），不记邮箱与 IP。
    fn note_login_failure(&self, account: &str, user_id: Option<i64>) {
        let alerts = self
            .login_watch
            .record_failure(account, Instant::now(), &self.limits);
        if alerts.account {
            metrics::counter!("rivu_auth_login_alerts_total", "kind" => "account").increment(1);
            match user_id {
                Some(id) => tracing::warn!(
                    user_id = id,
                    threshold = self.limits.account_fail_alert,
                    "同一账号 1 小时内登录失败次数达到告警阈值（疑似爆破，未阻断）"
                ),
                None => tracing::warn!(
                    threshold = self.limits.account_fail_alert,
                    "不存在的账号 1 小时内登录失败次数达到告警阈值（疑似探测，未阻断）"
                ),
            }
        }
        if alerts.global {
            metrics::counter!("rivu_auth_login_alerts_total", "kind" => "global").increment(1);
            tracing::warn!(
                threshold = self.limits.global_fail_alert,
                "全实例每分钟登录失败次数达到告警阈值（疑似撞库，未阻断）"
            );
        }
    }

    /// 邀请页的限流：按客户端 IP（在查库之前，所以乱猜令牌也消耗配额）。
    pub(crate) fn check_invite_rate(&self, peer: SocketAddr, headers: &HeaderMap) -> Res<()> {
        let ip = ip_bucket(self.client_ip(peer, headers));
        self.limiters
            .invite_view
            .check(&ip)
            .map_err(|w| rate_limited("invite_view", w))
    }

    pub(crate) fn client_ip(&self, peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
        let values = crate::public::forwarded_for(headers);
        let xff: Vec<_> = values.iter().map(String::as_str).collect();
        rivu_ingest::client_ip::client_ip(peer.ip(), &xff, &self.trusted_proxies)
    }
}

// ------------------------------------------------------------------------------------------
// 错误与响应
// ------------------------------------------------------------------------------------------

#[derive(Debug)]
pub(crate) enum DashError {
    /// 401：没有有效的会话。
    Unauthorized,
    /// 401：邮箱或密码不对（统一消息，不区分账号不存在、密码错误、账号被禁用）。
    InvalidCredentials,
    /// 401：共享链接要密码，请求没有有效的访问凭证。
    PasswordRequired,
    /// 401：共享链接的密码不对。
    InvalidPassword,
    /// 403：缺少或错误的首次初始化令牌。
    BadSetupToken,
    /// 403：邀请的邮箱与当前登录的账号不是同一个。
    EmailMismatch,
    /// 403：CSRF 判定失败。
    Csrf(CsrfError),
    /// 404：不存在、无权访问、已关闭（全部同一个响应）。
    NotFound,
    /// 403：站点对调用者可见（所以不泄露存在性），但有效角色不够。
    Forbidden,
    /// 409：与现有数据冲突（`conflict` 重复）或已达上限（`limit_reached`）。
    Conflict(&'static str, String),
    /// 400。
    Invalid(String),
    /// 400，带稳定的错误码（页面据此把错误放到对应的字段上）。
    BadRequest(&'static str, &'static str),
    /// 429，带建议的等待时间。
    RateLimited(Duration),
    /// 500（细节只进日志）。
    Internal,
    /// 读体、查询等沿用公开 API 的错误映射。
    Fail(Fail, u64),
}

impl From<anyhow::Error> for DashError {
    fn from(e: anyhow::Error) -> Self {
        tracing::error!(error = %format!("{e:#}"), "内部端点的存储访问失败");
        DashError::Internal
    }
}

pub(crate) fn json_response(status: StatusCode, body: Value) -> Response {
    let mut resp = (status, Json(body)).into_response();
    no_store(&mut resp);
    resp
}

fn error_body(status: StatusCode, code: &str, message: &str) -> Response {
    json_response(status, json!({"error": code, "message": message}))
}

impl IntoResponse for DashError {
    fn into_response(self) -> Response {
        match self {
            DashError::Unauthorized => error_body(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "authentication required",
            ),
            DashError::InvalidCredentials => error_body(
                StatusCode::UNAUTHORIZED,
                "invalid_credentials",
                "invalid email or password",
            ),
            DashError::PasswordRequired => error_body(
                StatusCode::UNAUTHORIZED,
                "password_required",
                "this shared link requires a password",
            ),
            DashError::InvalidPassword => error_body(
                StatusCode::UNAUTHORIZED,
                "invalid_password",
                "invalid password",
            ),
            DashError::BadSetupToken => {
                error_body(StatusCode::FORBIDDEN, "forbidden", "invalid setup token")
            }
            DashError::Csrf(reason) => {
                metrics::counter!("rivu_csrf_rejected_total", "reason" => csrf_reason(reason))
                    .increment(1);
                error_body(
                    StatusCode::FORBIDDEN,
                    "csrf_rejected",
                    "the request was rejected by the cross-site request check",
                )
            }
            DashError::EmailMismatch => error_body(
                StatusCode::FORBIDDEN,
                "email_mismatch",
                "the invitation was sent to a different email address than the account you are signed in with",
            ),
            DashError::NotFound => error_body(StatusCode::NOT_FOUND, "not_found", "not found"),
            DashError::Forbidden => {
                error_body(StatusCode::FORBIDDEN, "forbidden", "insufficient role")
            }
            DashError::Conflict(code, message) => error_body(StatusCode::CONFLICT, code, &message),
            DashError::Invalid(message) => {
                error_body(StatusCode::BAD_REQUEST, "invalid_request", &message)
            }
            DashError::BadRequest(code, message) => {
                error_body(StatusCode::BAD_REQUEST, code, message)
            }
            DashError::RateLimited(wait) => {
                let secs = wait.as_secs() + u64::from(wait.subsec_nanos() > 0);
                let secs = secs.max(1);
                let mut r = json_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    json!({"error": "rate_limited",
                           "message": "too many attempts; retry after the indicated number of seconds",
                           "retry_after": secs}),
                );
                r.headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from(secs));
                r
            }
            DashError::Internal => error_body(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            ),
            DashError::Fail(f, timeout) => query_api::fail_response(f, timeout),
        }
    }
}

fn csrf_reason(r: CsrfError) -> &'static str {
    match r {
        CsrfError::ContentType => "content_type",
        CsrfError::OriginMissing => "origin_missing",
        CsrfError::OriginMismatch => "origin_mismatch",
        CsrfError::CrossSite => "cross_site",
    }
}

pub(crate) type Res<T> = Result<T, DashError>;

pub(crate) fn rate_limited(scope: &'static str, wait: Duration) -> DashError {
    metrics::counter!("rivu_ratelimit_rejected_total", "scope" => scope).increment(1);
    DashError::RateLimited(wait)
}

/// 响应加一条 `Set-Cookie`。
pub(crate) fn with_cookie(mut resp: Response, cookie: &str) -> Response {
    if let Ok(v) = HeaderValue::from_str(cookie) {
        resp.headers_mut().append(header::SET_COOKIE, v);
    }
    resp
}

/// 限流键里的客户端：IPv4 按地址，IPv6 按 /64（一个终端用户通常拥有整个 /64，按单个地址限流形同虚设）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum IpBucket {
    V4([u8; 4]),
    V6Prefix([u8; 8]),
}

pub(crate) fn ip_bucket(ip: IpAddr) -> IpBucket {
    match ip {
        IpAddr::V4(a) => IpBucket::V4(a.octets()),
        IpAddr::V6(a) => {
            let o = a.octets();
            IpBucket::V6Prefix([o[0], o[1], o[2], o[3], o[4], o[5], o[6], o[7]])
        }
    }
}

// ------------------------------------------------------------------------------------------
// 请求体
// ------------------------------------------------------------------------------------------

/// 读并解析 JSON 请求体。解析错误一律是不含内容的 400：serde 的错误消息可能回显取值（密码）。
pub(crate) async fn read_json<T: for<'de> Deserialize<'de>>(req: Request, max: usize) -> Res<T> {
    let bytes = query_api::read_body(req, max)
        .await
        .map_err(|f| DashError::Fail(f, 0))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| DashError::Invalid("the request body is not a valid JSON object".into()))
}

// ------------------------------------------------------------------------------------------
// 会话
// ------------------------------------------------------------------------------------------

/// 一个请求里识别出的登录会话。
pub(crate) struct SessionCtx {
    pub(crate) s: ActiveSession,
    /// 这个请求续了期：响应里要刷新 cookie。
    renewed: Option<String>,
}

impl DashApi {
    /// 从 cookie 认出有效会话；距上次续期超过 10 分钟就续期（写库）。没有 cookie 或无效返回 `None`。
    pub(crate) async fn session(&self, headers: &HeaderMap) -> Res<Option<SessionCtx>> {
        let name = self.cookies.session_name();
        let Some(token) = cookie_value(headers, name).filter(|t| session::is_well_formed(t)) else {
            return Ok(None);
        };
        let hash = session::hash_token(token);
        let Some(s) = rivu_storage_pg::sessions::find_active(&self.pg, &hash).await? else {
            return Ok(None);
        };
        let mut renewed = None;
        if u64::try_from(s.idle_secs).unwrap_or(0) >= RENEW_AFTER.as_secs() {
            match rivu_storage_pg::sessions::renew(&self.pg, s.id, ttl_secs(SESSION_TTL)).await {
                // 只有真的续了期才刷新 cookie；续期失败（极少见）不影响本次请求
                Ok(()) => {
                    renewed = Some(self.cookies.set(name, token, SESSION_TTL));
                }
                Err(e) => {
                    tracing::warn!(session_id = s.id, error = %format!("{e:#}"), "续期会话失败");
                }
            }
        }
        Ok(Some(SessionCtx { s, renewed }))
    }

    pub(crate) async fn require_session(&self, headers: &HeaderMap) -> Res<SessionCtx> {
        self.session(headers).await?.ok_or(DashError::Unauthorized)
    }

    /// 带上续期的 cookie。
    pub(crate) fn finish(&self, resp: Response, ctx: &SessionCtx) -> Response {
        match &ctx.renewed {
            Some(c) => with_cookie(resp, c),
            None => resp,
        }
    }

    /// 建会话并返回 `Set-Cookie` 的值。
    pub(crate) async fn start_session(
        &self,
        user_id: i64,
        client_label: Option<&str>,
        headers: &HeaderMap,
    ) -> Res<String> {
        let ua = headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok());
        let label = device_label(client_label, ua);
        let token = session::generate();
        rivu_storage_pg::sessions::create(
            &self.pg,
            user_id,
            &token.hash(),
            label.as_deref(),
            ttl_secs(SESSION_TTL),
        )
        .await?;
        Ok(self
            .cookies
            .set(self.cookies.session_name(), token.plaintext(), SESSION_TTL))
    }

    /// 登录与初始化时：请求里带着的旧会话令牌一并撤销（不复用，防会话固定）。
    pub(crate) async fn revoke_presented(&self, headers: &HeaderMap) {
        let name = self.cookies.session_name();
        if let Some(t) = cookie_value(headers, name).filter(|t| session::is_well_formed(t))
            && let Err(e) =
                rivu_storage_pg::sessions::revoke_by_hash(&self.pg, &session::hash_token(t)).await
        {
            tracing::warn!(error = %format!("{e:#}"), "撤销请求里带的旧会话失败");
        }
    }
}

fn ttl_secs(d: Duration) -> i64 {
    i64::try_from(d.as_secs()).unwrap_or(i64::MAX)
}

pub(crate) fn user_json(
    id: i64,
    email: &str,
    name: Option<&str>,
    is_instance_admin: bool,
) -> Value {
    json!({"id": id, "email": email, "name": name, "is_instance_admin": is_instance_admin})
}

// ------------------------------------------------------------------------------------------
// POST /api/internal/setup
// ------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct SetupBody {
    #[serde(default)]
    token: Option<String>,
    email: String,
    password: String,
    org_name: String,
    #[serde(default)]
    name: Option<String>,
}

async fn setup(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let ip = ip_bucket(api.client_ip(peer, &headers));
    // 限流在一切判断之前：已初始化的实例上反复探测同样要排队，不会因为「已关闭」而不受限
    api.limiters
        .setup_ip
        .check(&ip)
        .map_err(|w| rate_limited("setup", w))?;
    // 已有用户：一律 404（不是 409：不让外部通过响应探测实例是否已初始化——404 与任何不存在的路径一样）
    if rivu_storage_pg::users::any_user_exists(&api.pg).await? {
        return Err(DashError::NotFound);
    }
    let body: SetupBody = read_json(req, AUTH_BODY_MAX).await?;
    if let Some(want) = &api.setup_token {
        let given = session::hash_token(body.token.as_deref().unwrap_or(""));
        // 两个定长摘要的常数时间比较（长度不同的原文也走同样的路径）
        let ok = rivu_auth::key::ct_eq(want, &given) && body.token.is_some();
        if !ok {
            return Err(DashError::BadSetupToken);
        }
    }
    let email = normalize_email(&body.email).map_err(|m| DashError::Invalid(m.into()))?;
    validate_password(&body.password).map_err(|m| DashError::Invalid(m.into()))?;
    let org_name = normalize_org_name(&body.org_name).map_err(|m| DashError::Invalid(m.into()))?;
    let name = clean_name(body.name.as_deref())?;
    let hash = api
        .passwords
        .hash(body.password)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let done = rivu_storage_pg::users::create_first_user(
        &api.pg,
        &email,
        name.as_deref(),
        &hash,
        &org_name,
        &slugify(&org_name),
    )
    .await?
    // 等锁期间别的请求抢先完成了初始化
    .ok_or(DashError::NotFound)?;
    api.revoke_presented(&headers).await;
    let cookie = api.start_session(done.user_id, None, &headers).await?;
    tracing::info!(
        user_id = done.user_id,
        org_id = done.org_id,
        "首次初始化完成"
    );
    Ok(with_cookie(
        json_response(
            StatusCode::CREATED,
            json!({
                "user": user_json(done.user_id, &email, name.as_deref(), true),
                "organization": {"id": done.org_id, "name": org_name, "slug": done.org_slug, "role": "owner"},
            }),
        ),
        &cookie,
    ))
}

pub(crate) fn clean_name(raw: Option<&str>) -> Res<Option<String>> {
    let Some(n) = raw.map(str::trim).filter(|n| !n.is_empty()) else {
        return Ok(None);
    };
    if n.chars().count() > 100 || n.chars().any(char::is_control) {
        return Err(DashError::Invalid(
            "the name must be at most 100 characters".into(),
        ));
    }
    Ok(Some(n.to_owned()))
}

// ------------------------------------------------------------------------------------------
// 登录、登出、会话列表
// ------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct LoginBody {
    email: String,
    password: String,
    #[serde(default)]
    device_label: Option<String>,
}

async fn login(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let ip = ip_bucket(api.client_ip(peer, &headers));
    let body: LoginBody = read_json(req, AUTH_BODY_MAX).await?;
    // 账号键：规范化的邮箱（不论账号是否存在，所以限流本身不泄露存在性）
    let account = body.email.trim().to_lowercase();

    // IP 桶在前：被 IP 拒绝的请求不消耗 (账号, IP) 的令牌。没有单纯按账号的桶：
    // 别人换着 IP 用受害者的邮箱登录，只会耗光他自己那个 (账号, IP) 桶，不影响真用户
    let limited = api
        .limiters
        .login_ip
        .check(&ip)
        .and_then(|()| api.limiters.login_pair.check(&(&account, ip)));
    if let Err(wait) = limited {
        metrics::counter!("rivu_login_total", "result" => "rate_limited").increment(1);
        return Err(rate_limited("login", wait));
    }

    let user = match normalize_email(&body.email) {
        Ok(email) => rivu_storage_pg::users::find_by_email(&api.pg, &email).await?,
        Err(_) => None,
    };
    // 不存在的账号也做一次等价的哈希运算（`PasswordService::verify` 在没有哈希时校验假哈希）；
    // 超长的密码不可能是真密码，直接判失败（不泄露任何账号信息）
    let too_long = body.password.chars().count() > MAX_PASSWORD_CHARS;
    let verified = !too_long
        && api
            .passwords
            .verify(
                body.password,
                user.as_ref().map(|u| u.password_hash.as_str()),
            )
            .await;
    let user = match user {
        Some(u) if verified && !u.disabled => u,
        other => {
            metrics::counter!("rivu_login_total", "result" => "failure").increment(1);
            // 失败日志只记（存在时的）用户 id 与结果：不记邮箱，不记 IP
            let user_id = other.as_ref().map(|u| u.id);
            tracing::debug!(user_id, "登录失败");
            api.note_login_failure(&account, user_id);
            return Err(DashError::InvalidCredentials);
        }
    };
    // 开了两步验证：密码对了只换来一个 challenge，凭它加验证码（`POST /api/internal/login/totp`）才建会话
    if let Some(challenge) = crate::account::challenge_if_enabled(
        &api,
        user.id,
        &user.email,
        body.device_label.as_deref(),
    )
    .await?
    {
        return Ok(json_response(
            StatusCode::OK,
            json!({"totp_required": true, "challenge": challenge}),
        ));
    }
    api.revoke_presented(&headers).await;
    let cookie = api
        .start_session(user.id, body.device_label.as_deref(), &headers)
        .await?;
    metrics::counter!("rivu_login_total", "result" => "success").increment(1);
    tracing::info!(user_id = user.id, "登录成功");
    Ok(with_cookie(
        json_response(
            StatusCode::OK,
            json!({"user": user_json(user.id, &user.email, user.name.as_deref(), user.is_instance_admin)}),
        ),
        &cookie,
    ))
}

fn no_content_clearing(api: &DashApi) -> Response {
    with_cookie(
        ns(StatusCode::NO_CONTENT.into_response()),
        &api.cookies.clear(api.cookies.session_name()),
    )
}

pub(crate) fn ns(mut resp: Response) -> Response {
    no_store(&mut resp);
    resp
}

async fn logout(State(api): State<Arc<DashApi>>, req: Request) -> Res<Response> {
    // 没有会话也成功（幂等）：总是清掉浏览器里的 cookie
    if let Some(ctx) = api.session(req.headers()).await? {
        rivu_storage_pg::sessions::revoke(&api.pg, ctx.s.user_id, ctx.s.id).await?;
    }
    Ok(no_content_clearing(&api))
}

async fn logout_all(State(api): State<Arc<DashApi>>, req: Request) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    let n = rivu_storage_pg::sessions::revoke_all(&api.pg, ctx.s.user_id).await?;
    tracing::info!(user_id = ctx.s.user_id, revoked = n, "退出所有设备");
    Ok(no_content_clearing(&api))
}

async fn list_sessions(State(api): State<Arc<DashApi>>, req: Request) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    let rows = rivu_storage_pg::sessions::list_active(&api.pg, ctx.s.user_id).await?;
    let sessions: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id,
                "device_label": r.device_label,
                "created_at": r.created_at,
                "last_used_at": r.last_used_at,
                "expires_at": r.expires_at,
                "current": r.id == ctx.s.id,
            })
        })
        .collect();
    Ok(api.finish(
        json_response(StatusCode::OK, json!({"sessions": sessions})),
        &ctx,
    ))
}

async fn delete_session(
    State(api): State<Arc<DashApi>>,
    Path(id): Path<String>,
    req: Request,
) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    let id: i64 = id.parse().map_err(|_| DashError::NotFound)?;
    // 只能撤销自己的会话；别人的与不存在的同一个 404（不泄露别人的会话 id）
    if !rivu_storage_pg::sessions::revoke(&api.pg, ctx.s.user_id, id).await? {
        return Err(DashError::NotFound);
    }
    let resp = if id == ctx.s.id {
        no_content_clearing(&api)
    } else {
        api.finish(ns(StatusCode::NO_CONTENT.into_response()), &ctx)
    };
    Ok(resp)
}

// ------------------------------------------------------------------------------------------
// POST /api/internal/me/password
// ------------------------------------------------------------------------------------------

#[derive(Deserialize)]
struct ChangePasswordBody {
    current: String,
    new: String,
}

/// 改密码（`platform-and-ops.md` §5）：先限流（`(用户, IP)` 桶），再校验新密码的长度，
/// 然后**总是**对当前密码做一次哈希校验；通过后同一事务内换哈希并撤销该用户全部会话，再签发新会话。
async fn change_password(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let ctx = api.require_session(&headers).await?;
    let user_id = ctx.s.user_id;
    let ip = ip_bucket(api.client_ip(peer, &headers));
    let body: ChangePasswordBody = read_json(req, AUTH_BODY_MAX).await?;
    api.limiters
        .password_change
        .check(&(user_id, ip))
        .map_err(|w| {
            metrics::counter!("rivu_password_change_total", "result" => "rate_limited")
                .increment(1);
            rate_limited("password_change", w)
        })?;
    validate_password(&body.new).map_err(|_| {
        DashError::BadRequest(
            "invalid_new_password",
            "the new password must be 12 to 128 characters",
        )
    })?;
    let stored = rivu_storage_pg::users::password_hash(&api.pg, user_id)
        .await?
        .ok_or(DashError::Unauthorized)?;
    // 超长的密码不可能是真密码，直接判错（与登录一致）；其余一律走完整的哈希校验
    let verified = body.current.chars().count() <= MAX_PASSWORD_CHARS
        && api
            .passwords
            .verify(body.current.clone(), Some(&stored))
            .await;
    if !verified {
        metrics::counter!("rivu_password_change_total", "result" => "wrong_password").increment(1);
        return Err(DashError::BadRequest(
            "wrong_password",
            "the current password is incorrect",
        ));
    }
    if body.new == body.current {
        return Err(DashError::BadRequest(
            "same_password",
            "the new password must differ from the current one",
        ));
    }
    let hash = api
        .passwords
        .hash(body.new)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    if !rivu_storage_pg::users::change_password(&api.pg, user_id, &hash).await? {
        return Err(DashError::Unauthorized);
    }
    let cookie = api.start_session(user_id, None, &headers).await?;
    metrics::counter!("rivu_password_change_total", "result" => "success").increment(1);
    tracing::info!(user_id, "修改密码，其他会话已撤销");
    Ok(with_cookie(
        json_response(StatusCode::OK, json!({})),
        &cookie,
    ))
}

// ------------------------------------------------------------------------------------------
// GET /api/internal/me、/sites
// ------------------------------------------------------------------------------------------

async fn me(State(api): State<Arc<DashApi>>, req: Request) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    let orgs = rivu_storage_pg::users::list_user_orgs(&api.pg, ctx.s.user_id).await?;
    let body = json!({
        "user": user_json(ctx.s.user_id, &ctx.s.email, ctx.s.name.as_deref(), ctx.s.is_instance_admin),
        "organizations": orgs.iter()
            .map(|o| json!({"id": o.id, "name": o.name, "slug": o.slug, "role": o.role}))
            .collect::<Vec<_>>(),
    });
    Ok(api.finish(json_response(StatusCode::OK, body), &ctx))
}

/// 有效角色 = max(组织角色, 站点访客角色)（`platform-and-ops.md` §4.2 第 4 步）。无法识别的角色文本不参与（fail-closed）。
pub(crate) fn effective_role(roles: &UserSiteRoles) -> Option<OrgRole> {
    [roles.org_role.as_deref(), roles.guest_role.as_deref()]
        .into_iter()
        .flatten()
        .filter_map(OrgRole::parse)
        .max()
}

/// 用户在这个站点上的有效角色是否允许读统计（`stats.read`，查 `rivu-auth` 的角色矩阵：所有角色都允许）。
pub(crate) fn may_read_stats(roles: &UserSiteRoles) -> bool {
    effective_role(roles).is_some_and(|r| allowed(r, Action::StatsRead))
}
async fn list_sites(State(api): State<Arc<DashApi>>, req: Request) -> Res<Response> {
    let ctx = api.require_session(req.headers()).await?;
    let sites = access::list_visible_sites(&api.pg, ctx.s.user_id).await?;
    let body: Vec<Value> = sites
        .iter()
        .map(|s| {
            site_list_item(
                s,
                effective_role(&s.roles),
                // 站点访客不是组织成员（没有 sites.read 等组织级动作）
                s.roles.org_role.is_some(),
            )
        })
        .collect();
    Ok(api.finish(json_response(StatusCode::OK, json!({"sites": body})), &ctx))
}

/// `GET /sites` 列表里一个站点的形状（建站端点的响应复用它）。
pub(crate) fn site_list_item(
    s: &access::VisibleSite,
    role: Option<OrgRole>,
    is_member: bool,
) -> Value {
    json!({
        "id": s.id,
        "site_key": s.site_key,
        "name": s.name,
        "timezone": s.timezone,
        "is_public": s.is_public,
        "organization": {"id": s.org_id, "name": s.org_name, "slug": s.org_slug},
        "role": role.map(OrgRole::as_str),
        "access": if is_member { "member" } else { "guest" },
    })
}

// ------------------------------------------------------------------------------------------
// 站点访问判定
// ------------------------------------------------------------------------------------------

/// 请求带的共享链接的状态（已经查过库）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareState {
    /// 没有带共享链接。
    NotPresented,
    /// 带了，但无效：slug 不存在、过期、已吊销、不属于这个站点。
    Invalid,
    /// 有效，但链接有密码而请求没有有效的访问凭证。
    PasswordRequired,
    /// 有效且已通过密码（或没有密码）。
    Valid,
}

/// 判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Shared,
    PasswordRequired,
    Session,
    Public,
    NotFound,
}

/// 访问判定（站点已存在、已查过库）：共享链接 > 登录会话 > 公开站点（§4.2 的第 3、4、6 步，见模块文档）。
/// 带了共享链接的请求只看链接：无效的链接不会退回去找别的主体（否则同一个链接 URL 会因调用者是否登录而得到不同的响应）。
pub fn decide(share: ShareState, session_has_access: bool, is_public: bool) -> Decision {
    match share {
        ShareState::Valid => Decision::Shared,
        ShareState::PasswordRequired => Decision::PasswordRequired,
        ShareState::Invalid => Decision::NotFound,
        ShareState::NotPresented => {
            if session_has_access {
                Decision::Session
            } else if is_public {
                Decision::Public
            } else {
                Decision::NotFound
            }
        }
    }
}

/// 通过判定的主体。
enum Principal {
    Session(SessionCtx),
    Shared(LinkInfo),
    Public,
}

impl Principal {
    fn surface(&self) -> &'static str {
        match self {
            Principal::Session(_) => "internal",
            Principal::Shared(_) => "shared",
            Principal::Public => "public",
        }
    }
}

struct Authorized {
    site: SiteOwner,
    principal: Principal,
}

impl DashApi {
    /// 读共享链接头：没带 `None`；带了但格式不对按无效（`Some(None)`）。
    fn share_slug(headers: &HeaderMap) -> Option<Option<&str>> {
        let v = headers.get(SHARE_HEADER)?;
        Some(v.to_str().ok().filter(|s| shared_links::is_slug_shaped(s)))
    }

    /// 链接要密码时，请求带的访问凭证是否有效。
    async fn grant_ok(&self, link: &LinkInfo, headers: &HeaderMap) -> Res<bool> {
        if link.password_hash.is_none() {
            return Ok(true);
        }
        let name = self.cookies.share_name(link.id);
        let Some(token) = cookie_value(headers, &name).filter(|t| session::is_well_formed(t))
        else {
            return Ok(false);
        };
        Ok(shared_links::grant_valid(&self.pg, link.id, &session::hash_token(token)).await?)
    }

    /// 查并限流一个共享链接：按 `(客户端 IP, slug)` 限流（在查库之前，所以乱猜 slug 也消耗配额）。
    async fn find_link(&self, ip: IpBucket, slug: &str) -> Res<Option<LinkInfo>> {
        self.limiters
            .share_view
            .check(&(ip, slug))
            .map_err(|w| rate_limited("share_view", w))?;
        Ok(shared_links::find_active(&self.pg, slug).await?)
    }

    async fn authorize(
        &self,
        peer: SocketAddr,
        headers: &HeaderMap,
        site_seg: &str,
    ) -> Res<Authorized> {
        let site_ref = SiteRef::parse(site_seg).ok_or(DashError::NotFound)?;
        let site = access::find_site(&self.pg, &site_ref)
            .await?
            .ok_or(DashError::NotFound)?;
        let ip = ip_bucket(self.client_ip(peer, headers));

        let mut link = None;
        let share = match Self::share_slug(headers) {
            None => ShareState::NotPresented,
            Some(None) => ShareState::Invalid,
            Some(Some(slug)) => match self.find_link(ip, slug).await? {
                Some(l) if l.site_id == site.id => {
                    let ok = self.grant_ok(&l, headers).await?;
                    link = Some(l);
                    if ok {
                        ShareState::Valid
                    } else {
                        ShareState::PasswordRequired
                    }
                }
                _ => ShareState::Invalid,
            },
        };

        let mut session = None;
        let mut has_access = false;
        if share == ShareState::NotPresented
            && let Some(ctx) = self.session(headers).await?
        {
            let roles = access::user_site_roles(&self.pg, ctx.s.user_id, &site).await?;
            has_access = may_read_stats(&roles);
            session = Some(ctx);
        }
        match decide(share, has_access, site.is_public) {
            Decision::NotFound => Err(DashError::NotFound),
            Decision::PasswordRequired => Err(DashError::PasswordRequired),
            Decision::Shared => Ok(Authorized {
                principal: Principal::Shared(link.expect("Valid 必有链接")),
                site,
            }),
            Decision::Session => Ok(Authorized {
                principal: Principal::Session(session.expect("有访问权必有会话")),
                site,
            }),
            Decision::Public => {
                self.limiters
                    .public_view
                    .check(&(ip, site.id))
                    .map_err(|w| rate_limited("public_view", w))?;
                Ok(Authorized {
                    site,
                    principal: Principal::Public,
                })
            }
        }
    }
}

// ------------------------------------------------------------------------------------------
// 查询
// ------------------------------------------------------------------------------------------

/// 把链接绑定的 segment 的过滤叠加到查询上（顶层数组是隐式 AND）。过滤既可以是单个节点，也可以是节点数组。
/// 读不懂的 segment 过滤按内部错误处理：宁可失败，也不能把未过滤的数据放出去。
fn overlay_segment(spec: &mut QuerySpec, filter_json: &str) -> Result<(), ()> {
    let value: Value = serde_json::from_str(filter_json).map_err(|_| ())?;
    let nodes: Vec<Value> = match &value {
        Value::Array(items) if items.first().is_some_and(Value::is_array) => items.clone(),
        _ => vec![value],
    };
    for n in nodes {
        spec.filters
            .push(serde_json::from_value::<Filter>(n).map_err(|_| ())?);
    }
    Ok(())
}

/// 城市名的语言：看板在请求头 `Accept-Language` 里带界面语言（第一个标签以 `zh` 开头为中文，其余英文）。
fn name_lang(headers: &HeaderMap) -> NameLang {
    NameLang::from_accept_language(
        headers
            .get(header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok()),
    )
}

fn route_name(surface: &str, op: &str) -> &'static str {
    match (surface, op) {
        ("internal", "query") => "internal.query",
        ("internal", "property_keys") => "internal.property_keys",
        ("internal", _) => "internal.realtime",
        ("shared", "query") => "shared.query",
        ("shared", "property_keys") => "shared.property_keys",
        ("shared", _) => "shared.realtime",
        (_, "query") => "public.query",
        (_, "property_keys") => "public.property_keys",
        _ => "public.realtime",
    }
}

impl DashApi {
    /// 查询前的公共准备：共享链接绑定的 segment 叠加到过滤上、读站点上下文、按调用方定超时与 `log_comment`。
    async fn prepare(
        &self,
        auth: &Authorized,
        spec: &mut QuerySpec,
        route: &'static str,
    ) -> Result<(SiteContext, ExecOptions, u64), DashError> {
        let (timeout, comment): (u64, Option<(&str, i64)>) = match &auth.principal {
            Principal::Session(c) => (self.dashboard_timeout_secs, Some(("user", c.s.user_id))),
            Principal::Shared(l) => (self.public_timeout_secs, Some(("shared_link", l.id))),
            Principal::Public => (self.public_timeout_secs, None),
        };
        if let Principal::Shared(l) = &auth.principal
            && let Some(filter) = &l.segment_filter
            && overlay_segment(spec, filter).is_err()
        {
            tracing::error!(link_id = l.id, "共享链接绑定的 segment 过滤无法解析");
            return Err(DashError::Internal);
        }
        let site = self.site_context(auth.site.id, &auth.site.site_key).await?;
        let opts = self.exec_options(timeout, route, auth.site.organization_id, comment);
        Ok((site, opts, timeout))
    }

    /// 查询内核要的站点上下文（时区、目标、保留期、导入覆盖，`query_api::load_site_context`）。
    async fn site_context(&self, site_id: u64, site_key: &str) -> Result<SiteContext, DashError> {
        query_api::load_site_context(
            &self.pg,
            self.geo.as_deref(),
            site_id,
            site_key,
            self.retention_days,
        )
        .await
        .map_err(|e| match e {
            rivu_auth::ApiError::NotFound => DashError::NotFound,
            _ => DashError::Internal,
        })
    }

    /// 执行选项：超时档、注入的「现在」、路由标签、`log_comment`（组织与主体）、选层阈值。
    fn exec_options(
        &self,
        timeout: u64,
        route: &'static str,
        org_id: i64,
        comment: Option<(&str, i64)>,
    ) -> ExecOptions {
        let mut opts = ExecOptions::new(timeout)
            .with_now((self.clock)())
            .with_route(route)
            .with_comment("org", org_id);
        if let Some((k, v)) = comment {
            opts = opts.with_comment(k, v);
        }
        opts.rollup_min_rows = self.rollup_min_rows;
        opts
    }

    /// 以登录用户的身份查一个站点：与看板的 `POST /sites/{site}/query` 同一条路径（站点上下文、看板档超时、
    /// `log_comment`、结果缓存）。**调用方必须已经按 §4.2 判定过这个用户能读这个站点的统计**。
    pub(crate) async fn query_as_user(
        &self,
        user_id: i64,
        site_id: u64,
        site_key: &str,
        org_id: i64,
        spec: QuerySpec,
        route: &'static str,
    ) -> Result<QueryResult, DashError> {
        let timeout = self.dashboard_timeout_secs;
        let site = self.site_context(site_id, site_key).await?;
        let opts = self.exec_options(timeout, route, org_id, Some(("user", user_id)));
        query_api::execute_cached(
            &self.reader,
            self.cache.as_deref(),
            &site,
            spec,
            opts,
            route,
        )
        .await
        .map_err(|e| DashError::Fail(Fail::Query(e), timeout))
    }

    async fn execute(
        &self,
        auth: &Authorized,
        mut spec: QuerySpec,
        op: &'static str,
    ) -> Result<QueryResult, DashError> {
        let route = route_name(auth.principal.surface(), op);
        let (site, opts, timeout) = self.prepare(auth, &mut spec, route).await?;
        query_api::execute_cached(
            &self.reader,
            self.cache.as_deref(),
            &site,
            spec,
            opts,
            route,
        )
        .await
        .map_err(|e| DashError::Fail(Fail::Query(e), timeout))
    }
}

/// 看板请求体里的 QuerySpec：`site`（site_key）可以省略，给出时必须与路径指向的站点一致，否则 404（不是 400：
/// 不让调用者靠「改 site」来探测别的站点），`resolve` 还会再核对一次。
async fn read_spec(api: &DashApi, auth: &Authorized, req: Request) -> Res<QuerySpec> {
    let bytes = query_api::read_body(req, QUERY_BODY_MAX)
        .await
        .map_err(|f| DashError::Fail(f, api.dashboard_timeout_secs))?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        DashError::Fail(
            Fail::Query(QueryError::validation("", "the body is not valid JSON")),
            api.dashboard_timeout_secs,
        )
    })?;
    if let Some(Value::String(key)) = value.get("site")
        && *key != auth.site.site_key
    {
        return Err(DashError::NotFound);
    }
    let timeout = match auth.principal {
        Principal::Session(_) => api.dashboard_timeout_secs,
        _ => api.public_timeout_secs,
    };
    QuerySpec::parse(&value).map_err(|e| DashError::Fail(Fail::Query(e), timeout))
}

/// 属性卡片的键列表：请求体是看板当前的 QuerySpec（只用时间范围与筛选），返回 `{"keys":[{"key","events"}]}`。
async fn site_property_keys(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(site_seg): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let auth = api.authorize(peer, &headers, &site_seg).await?;
    let mut spec = read_spec(&api, &auth, req).await?;
    let route = route_name(auth.principal.surface(), "property_keys");
    let (site, opts, timeout) = api.prepare(&auth, &mut spec, route).await?;
    let keys = rivu_query::property_keys(
        &api.reader,
        &TenantScope::new(site.site_id),
        &site,
        spec,
        opts,
    )
    .await
    .map_err(|e| DashError::Fail(Fail::Query(e), timeout))?;
    let resp = ns(Json(json!({ "keys": keys })).into_response());
    Ok(match &auth.principal {
        Principal::Session(ctx) => api.finish(resp, ctx),
        _ => resp,
    })
}
async fn site_query(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(site_seg): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let auth = api.authorize(peer, &headers, &site_seg).await?;
    let spec = read_spec(&api, &auth, req).await?;
    let mut result = api.execute(&auth, spec, "query").await?;
    query_api::attach_city_names(&mut result, api.geo.as_deref(), name_lang(&headers));
    let resp = ns(Json(result).into_response());
    Ok(match &auth.principal {
        Principal::Session(ctx) => api.finish(resp, ctx),
        _ => resp,
    })
}
/// CSV 导出：请求体 `{"date_range","filters"?,"interval"?}`（看板当前视图），返回 zip（见 [`crate::export`]）。
/// 只认登录会话，有效角色 editor+（`export.create`）；共享链接与公开站点不能导出。
async fn site_export(
    State(api): State<Arc<DashApi>>,
    Path(site_seg): Path<String>,
    req: Request,
) -> Res<Response> {
    let headers = req.headers().clone();
    let c = api
        .authorize_admin(&headers, &site_seg, Action::ExportsRead, false)
        .await?;
    let timeout = api.dashboard_timeout_secs;
    let bytes = query_api::read_body(req, QUERY_BODY_MAX)
        .await
        .map_err(|f| DashError::Fail(f, timeout))?;
    let invalid = |e: QueryError| DashError::Fail(Fail::Query(e), timeout);
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| invalid(QueryError::validation("", "the body is not valid JSON")))?;
    if let Some(Value::String(key)) = value.get("site")
        && *key != c.site.site_key
    {
        return Err(DashError::NotFound);
    }
    let request = crate::export::ExportRequest::parse(&value).map_err(invalid)?;
    let summary = access::site_summary(&api.pg, c.site.id)
        .await?
        .ok_or(DashError::NotFound)?;
    let auth = Authorized {
        site: c.site,
        principal: Principal::Session(c.session),
    };
    const ROUTE: &str = "internal.export";
    let mut keys_spec = request.keys_spec().map_err(invalid)?;
    let (site, opts, _) = api.prepare(&auth, &mut keys_spec, ROUTE).await?;
    let keys: Vec<String> = rivu_query::property_keys(
        &api.reader,
        &TenantScope::new(site.site_id),
        &site,
        keys_spec,
        opts.clone(),
    )
    .await
    .map_err(invalid)?
    .into_iter()
    .map(|k| k.key)
    .collect();
    let (reader, cache, site_ref) = (&api.reader, api.cache.as_deref(), &site);
    let (geo, lang) = (api.geo.as_deref(), name_lang(&headers));
    let export = crate::export::build(&request, &keys, move |spec| {
        let run = query_api::execute_cached(reader, cache, site_ref, spec, opts.clone(), ROUTE);
        async move {
            let mut result = run.await?;
            query_api::attach_city_names(&mut result, geo, lang);
            Ok::<_, QueryError>(result)
        }
    })
    .await
    .map_err(invalid)?;
    let name = crate::export::file_name(&summary.name, &export);
    let mut resp = ns((
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/zip"),
        )],
        export.zip,
    )
        .into_response());
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(match &auth.principal {
        Principal::Session(ctx) => api.finish(resp, ctx),
        _ => resp,
    })
}

async fn site_realtime(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(site_seg): Path<String>,
    req: Request,
) -> Res<Response> {
    let auth = api.authorize(peer, req.headers(), &site_seg).await?;
    // 等价于 `date_range: "realtime"` 的 visitors 查询（`system-design.md` §6.1）
    let value = json!({"metrics": ["visitors"], "date_range": "realtime"});
    let spec = QuerySpec::parse(&value).map_err(|_| DashError::Internal)?;
    // 3 秒的结果缓存（§6.9）：授权已经在上面判定过了；链接绑定了 segment 时，结果随 segment 变，所以键里带上它
    let segment = match &auth.principal {
        Principal::Shared(l) => l.segment_filter.as_deref().unwrap_or(""),
        _ => "",
    };
    let key = format!("{}|{segment}", auth.site.id);
    let body = if let Some(hit) = api.realtime_cache.get(&key) {
        hit
    } else {
        let result = api.execute(&auth, spec, "realtime").await?;
        let text = serde_json::to_string(&result).map_err(|e| {
            tracing::error!(error = %e, "序列化实时结果失败");
            DashError::Internal
        })?;
        let text = Arc::new(text);
        api.realtime_cache.insert(key, Arc::clone(&text));
        text
    };
    let mut resp = (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        )],
        body.as_str().to_owned(),
    )
        .into_response();
    no_store(&mut resp);
    Ok(match &auth.principal {
        Principal::Session(ctx) => api.finish(resp, ctx),
        _ => resp,
    })
}

// ------------------------------------------------------------------------------------------
// 共享链接与公开看板的入口
// ------------------------------------------------------------------------------------------

fn site_json(id: u64, key: &str, name: &str, timezone: &str) -> Value {
    json!({"id": id, "site_key": key, "name": name, "timezone": timezone})
}

async fn shared_info(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(slug): Path<String>,
    req: Request,
) -> Res<Response> {
    if !shared_links::is_slug_shaped(&slug) {
        return Err(DashError::NotFound);
    }
    let ip = ip_bucket(api.client_ip(peer, req.headers()));
    let link = api.find_link(ip, &slug).await?.ok_or(DashError::NotFound)?;
    let authorized = api.grant_ok(&link, req.headers()).await?;
    let mut body =
        json!({"password_required": link.password_hash.is_some(), "authorized": authorized});
    if authorized {
        body["site"] = site_json(
            link.site_id,
            &link.site_key,
            &link.site_name,
            &link.timezone,
        );
    }
    Ok(json_response(StatusCode::OK, body))
}

#[derive(Deserialize)]
struct SharedAuthBody {
    password: String,
}

async fn shared_auth(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(slug): Path<String>,
    req: Request,
) -> Res<Response> {
    if !shared_links::is_slug_shaped(&slug) {
        return Err(DashError::NotFound);
    }
    let ip = ip_bucket(api.client_ip(peer, req.headers()));
    let link = api.find_link(ip, &slug).await?.ok_or(DashError::NotFound)?;
    let body: SharedAuthBody = read_json(req, AUTH_BODY_MAX).await?;
    let site = site_json(
        link.site_id,
        &link.site_key,
        &link.site_name,
        &link.timezone,
    );
    let Some(stored) = link.password_hash.as_deref() else {
        // 没有密码的链接：不需要凭证
        return Ok(json_response(
            StatusCode::OK,
            json!({"authorized": true, "site": site}),
        ));
    };
    // 密码尝试：5 次 / 分钟（按 IP 加链接）
    api.limiters
        .share_password
        .check(&(ip, link.id))
        .map_err(|w| rate_limited("share_password", w))?;
    let too_long = body.password.chars().count() > MAX_PASSWORD_CHARS;
    if too_long || !api.passwords.verify(body.password, Some(stored)).await {
        return Err(DashError::InvalidPassword);
    }
    let token = session::generate();
    shared_links::create_grant(&api.pg, link.id, &token.hash(), ttl_secs(SHARE_GRANT_TTL)).await?;
    let cookie = api.cookies.set(
        &api.cookies.share_name(link.id),
        token.plaintext(),
        SHARE_GRANT_TTL,
    );
    Ok(with_cookie(
        json_response(StatusCode::OK, json!({"authorized": true, "site": site})),
        &cookie,
    ))
}

async fn public_info(
    State(api): State<Arc<DashApi>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(site_seg): Path<String>,
    req: Request,
) -> Res<Response> {
    let site_ref = SiteRef::parse(&site_seg).ok_or(DashError::NotFound)?;
    let site = access::find_site(&api.pg, &site_ref)
        .await?
        .filter(|s| s.is_public)
        .ok_or(DashError::NotFound)?;
    let ip = ip_bucket(api.client_ip(peer, req.headers()));
    api.limiters
        .public_view
        .check(&(ip, site.id))
        .map_err(|w| rate_limited("public_view", w))?;
    let summary = access::site_summary(&api.pg, site.id)
        .await?
        .ok_or(DashError::NotFound)?;
    Ok(json_response(
        StatusCode::OK,
        json!({"site": site_json(site.id, &site.site_key, &summary.name, &summary.timezone)}),
    ))
}

// ------------------------------------------------------------------------------------------
// 路由
// ------------------------------------------------------------------------------------------

async fn csrf_layer(State(api): State<Arc<DashApi>>, req: Request, next: Next) -> Response {
    match api.csrf.check(req.method(), req.headers()) {
        Ok(()) => next.run(req).await,
        Err(reason) => DashError::Csrf(reason).into_response(),
    }
}

pub(crate) async fn method_not_allowed(allow: &'static str) -> Response {
    let mut resp = StatusCode::METHOD_NOT_ALLOWED.into_response();
    resp.headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static(allow));
    no_store(&mut resp);
    resp
}

/// 一个端点：先过 CSRF 判定，再计入 `rivu_api_requests_total{route}`。
pub(crate) fn endpoint(
    api: &Arc<DashApi>,
    route: &'static str,
    path: &'static str,
    handler: MethodRouter<Arc<DashApi>>,
) -> Router {
    let r = Router::new()
        .route(path, handler)
        .route_layer(middleware::from_fn_with_state(Arc::clone(api), csrf_layer))
        .with_state(Arc::clone(api));
    query_api::count_requests(r, route)
}

/// 内部端点的路由。与公开查询 API 的路由合并后交给 [`crate::public::router_full`]。
pub fn router(api: Arc<DashApi>) -> Router {
    let a = &api;
    // 每个路由只允许一种方法，其余 405 + Allow
    let post_only = |h: MethodRouter<Arc<DashApi>>| h.fallback(|| method_not_allowed("POST"));
    let get_only = |h: MethodRouter<Arc<DashApi>>| h.fallback(|| method_not_allowed("GET, HEAD"));
    Router::new()
        .merge(endpoint(
            a,
            "internal.setup",
            "/api/internal/setup",
            post_only(post(setup)),
        ))
        .merge(endpoint(
            a,
            "internal.login",
            "/api/internal/login",
            post_only(post(login)),
        ))
        .merge(endpoint(
            a,
            "internal.logout",
            "/api/internal/logout",
            post_only(post(logout)),
        ))
        .merge(endpoint(
            a,
            "internal.logout_all",
            "/api/internal/logout-all",
            post_only(post(logout_all)),
        ))
        .merge(endpoint(
            a,
            "internal.sessions",
            "/api/internal/sessions",
            get_only(get(list_sessions)),
        ))
        .merge(endpoint(
            a,
            "internal.session_delete",
            "/api/internal/sessions/{id}",
            delete(delete_session).fallback(|| method_not_allowed("DELETE")),
        ))
        .merge(endpoint(
            a,
            "internal.me",
            "/api/internal/me",
            get_only(get(me)),
        ))
        .merge(endpoint(
            a,
            "internal.change_password",
            "/api/internal/me/password",
            post_only(post(change_password)),
        ))
        .merge(endpoint(
            a,
            "internal.sites",
            "/api/internal/sites",
            get_only(get(list_sites)),
        ))
        .merge(endpoint(
            a,
            "internal.site_query",
            "/api/internal/sites/{site}/query",
            post_only(post(site_query)),
        ))
        .merge(endpoint(
            a,
            "internal.site_realtime",
            "/api/internal/sites/{site}/realtime",
            get_only(get(site_realtime)),
        ))
        .merge(endpoint(
            a,
            "internal.site_export",
            "/api/internal/sites/{site}/export",
            post_only(post(site_export)),
        ))
        .merge(endpoint(
            a,
            "internal.site_property_keys",
            "/api/internal/sites/{site}/property-keys",
            post_only(post(site_property_keys)),
        ))
        .merge(endpoint(
            a,
            "internal.shared_info",
            "/api/internal/shared/{slug}",
            get_only(get(shared_info)),
        ))
        .merge(endpoint(
            a,
            "internal.shared_auth",
            "/api/internal/shared/{slug}/auth",
            post_only(post(shared_auth)),
        ))
        .merge(endpoint(
            a,
            "internal.public_info",
            "/api/internal/public/{site}",
            get_only(get(public_info)),
        ))
        .merge(crate::site_admin::routes(a))
        .merge(crate::goals_admin::routes(a))
        .merge(crate::consolidated::routes(a))
        .merge(crate::diagnostics::routes(a))
        .merge(crate::org_admin::routes(a))
        .merge(crate::imports_admin::routes(a))
        .merge(crate::account::routes(a))
        .merge(crate::weekly_report::routes(a))
}
// ------------------------------------------------------------------------------------------
// 后台：活跃会话数与清理
// ------------------------------------------------------------------------------------------

/// 每分钟刷新 `rivu_auth_active_sessions`，每小时清理过期与撤销的会话行与到期的访问凭证
/// （`auth_sessions_cleanup`，`platform-and-ops.md` §6；读取路径始终校验 `expires_at`，清理滞后不影响安全）。
pub fn spawn_maintenance(
    pg: PgPool,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut n = 0u64;
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = stop.wait_for(|s| *s) => return,
            }
            match rivu_storage_pg::sessions::count_active(&pg).await {
                Ok(c) => metrics::gauge!("rivu_auth_active_sessions").set(c as f64),
                Err(e) => tracing::warn!(error = %format!("{e:#}"), "统计有效会话失败"),
            }
            if n.is_multiple_of(60) {
                match rivu_storage_pg::sessions::cleanup(&pg).await {
                    Ok((s, g)) if s + g > 0 => {
                        tracing::info!(sessions = s, grants = g, "已清理过期与撤销的会话");
                    }
                    Ok(_) => {}
                    Err(e) => tracing::warn!(error = %format!("{e:#}"), "清理会话失败"),
                }
            }
            n += 1;
        }
    })
}

/// 注册指标说明。
pub fn describe_metrics() {
    metrics::describe_counter!(
        "rivu_login_total",
        "登录尝试数，按结果（success / failure / rate_limited）"
    );
    metrics::describe_counter!(
        "rivu_auth_login_alerts_total",
        "登录失败告警次数，按类型（account：同一账号 1 小时内失败达阈值；global：全实例每分钟失败达阈值），只告警不阻断"
    );
    metrics::describe_gauge!(
        "rivu_auth_active_sessions",
        "有效的登录会话数（每分钟刷新）"
    );
    metrics::describe_counter!(
        "rivu_csrf_rejected_total",
        "被 CSRF 判定拒绝的内部端点请求，按原因"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_order_is_share_link_then_session_then_public() {
        use Decision::{NotFound, Public, Session, Shared};
        use ShareState::{Invalid, NotPresented, PasswordRequired, Valid};
        // 带了有效的链接：不管有没有会话、站点是否公开，都按链接
        for (session, public) in [(false, false), (true, false), (false, true), (true, true)] {
            assert_eq!(decide(Valid, session, public), Shared);
            assert_eq!(
                decide(PasswordRequired, session, public),
                Decision::PasswordRequired
            );
            // 无效的链接不退回去找别的主体
            assert_eq!(decide(Invalid, session, public), NotFound);
        }
        // 没带链接：会话 > 公开 > 404
        assert_eq!(decide(NotPresented, true, false), Session);
        assert_eq!(decide(NotPresented, true, true), Session);
        assert_eq!(decide(NotPresented, false, true), Public);
        assert_eq!(decide(NotPresented, false, false), NotFound);
    }

    #[test]
    fn effective_role_is_the_higher_of_org_and_guest_roles() {
        let r = |o: Option<&str>, g: Option<&str>| UserSiteRoles {
            org_role: o.map(str::to_owned),
            guest_role: g.map(str::to_owned),
        };
        assert_eq!(
            effective_role(&r(Some("viewer"), Some("editor"))),
            Some(OrgRole::Editor)
        );
        assert_eq!(
            effective_role(&r(Some("admin"), Some("editor"))),
            Some(OrgRole::Admin)
        );
        assert_eq!(
            effective_role(&r(None, Some("viewer"))),
            Some(OrgRole::Viewer)
        );
        assert_eq!(
            effective_role(&r(Some("owner"), None)),
            Some(OrgRole::Owner)
        );
        assert_eq!(effective_role(&r(None, None)), None);
        assert_eq!(
            effective_role(&r(Some("root"), None)),
            None,
            "无法识别的角色不参与"
        );
        // stats.read 对所有角色开放（角色矩阵）；没有任何角色就没有访问权
        assert!(may_read_stats(&r(Some("viewer"), None)));
        assert!(may_read_stats(&r(None, Some("viewer"))));
        assert!(!may_read_stats(&r(None, None)));
        assert!(!may_read_stats(&r(Some("root"), None)));
    }

    #[test]
    fn ipv6_clients_share_one_bucket_per_64_prefix() {
        let b = |s: &str| ip_bucket(s.parse().unwrap());
        assert_eq!(b("2001:db8:1:2::1"), b("2001:db8:1:2:ffff::9"));
        assert_ne!(b("2001:db8:1:2::1"), b("2001:db8:1:3::1"));
        assert_ne!(b("192.0.2.1"), b("192.0.2.2"));
        assert_eq!(b("192.0.2.1"), b("192.0.2.1"));
    }

    #[test]
    fn rate_limit_response_has_retry_after_rounded_up() {
        for (wait, want) in [
            (Duration::from_millis(1), 1),
            (Duration::from_secs(12), 12),
            (Duration::from_millis(11_100), 12),
            (Duration::ZERO, 1),
        ] {
            let r = DashError::RateLimited(wait).into_response();
            assert_eq!(r.status(), 429);
            assert_eq!(r.headers()[header::RETRY_AFTER], want.to_string().as_str());
            assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
        }
    }

    #[tokio::test]
    async fn error_bodies_are_uniform_and_carry_no_details() {
        async fn body(e: DashError) -> (u16, Value) {
            let r = e.into_response();
            let s = r.status().as_u16();
            let b = axum::body::to_bytes(r.into_body(), 1 << 16).await.unwrap();
            (s, serde_json::from_slice(&b).unwrap())
        }
        let (s, v) = body(DashError::InvalidCredentials).await;
        assert_eq!((s, v["error"].as_str()), (401, Some("invalid_credentials")));
        assert_eq!(v["message"], "invalid email or password");
        let (s, v) = body(DashError::NotFound).await;
        assert_eq!((s, v["error"].as_str()), (404, Some("not_found")));
        let (s, v) = body(DashError::Csrf(CsrfError::OriginMismatch)).await;
        assert_eq!((s, v["error"].as_str()), (403, Some("csrf_rejected")));
        assert!(
            !v.to_string().contains("origin"),
            "不告诉调用者具体哪一项没过"
        );
        let (s, v) = body(DashError::Internal).await;
        assert_eq!((s, v["message"].as_str()), (500, Some("internal error")));
    }

    #[test]
    fn segment_overlay_accepts_a_single_node_or_a_list_and_rejects_garbage() {
        let base =
            || QuerySpec::parse(&json!({"metrics": ["visitors"], "date_range": "7d"})).unwrap();
        let mut q = base();
        overlay_segment(&mut q, r#"["is","visit:country",["SE"]]"#).unwrap();
        assert_eq!(q.filters.len(), 1);
        let mut q = base();
        overlay_segment(
            &mut q,
            r#"[["is","visit:country",["SE"]],["is","visit:device",["mobile"]]]"#,
        )
        .unwrap();
        assert_eq!(q.filters.len(), 2);
        // 叠加在请求自己的过滤之后（隐式 AND）
        let mut q = QuerySpec::parse(&json!({"metrics": ["visitors"], "date_range": "7d",
            "filters": [["is", "visit:browser", ["Chrome"]]]}))
        .unwrap();
        overlay_segment(&mut q, r#"["is","visit:country",["SE"]]"#).unwrap();
        assert_eq!(q.filters.len(), 2);
        for bad in ["", "null", "{}", "[1]", r#"["nope","x",[]]"#, "not json"] {
            assert!(overlay_segment(&mut base(), bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn route_names_are_the_documented_low_cardinality_labels() {
        assert_eq!(route_name("internal", "query"), "internal.query");
        assert_eq!(route_name("internal", "realtime"), "internal.realtime");
        assert_eq!(route_name("shared", "query"), "shared.query");
        assert_eq!(route_name("shared", "realtime"), "shared.realtime");
        assert_eq!(route_name("public", "query"), "public.query");
        assert_eq!(route_name("public", "realtime"), "public.realtime");
        assert_eq!(
            route_name("shared", "property_keys"),
            "shared.property_keys"
        );
    }

    #[test]
    fn default_limits_are_the_documented_values() {
        let l = DashLimits::default();
        assert_eq!(
            (l.login_attempts, l.login_window),
            (5, Duration::from_secs(60))
        );
        assert_eq!(l.login_ip_attempts, 20);
        assert_eq!(
            (l.view_per_min, l.view_burst, l.password_per_min),
            (300, 60, 5)
        );
        assert_eq!((l.account_fail_alert, l.global_fail_alert), (50, 300));
    }

    #[test]
    fn view_limits_take_the_configured_values_with_burst_capped_by_the_rate() {
        let l = DashLimits::with_view(300, 60);
        assert_eq!((l.view_per_min, l.view_burst), (300, 60));
        assert_eq!(l, DashLimits::default());
        let l = DashLimits::with_view(100, 500);
        assert_eq!(
            (l.view_per_min, l.view_burst),
            (100, 100),
            "突发不超过每分钟次数"
        );
        assert_eq!(l.login_attempts, 5, "其余沿用默认");
    }

    fn watch_limits(account: u32, global: u32) -> DashLimits {
        DashLimits {
            account_fail_alert: account,
            global_fail_alert: global,
            ..DashLimits::default()
        }
    }

    #[test]
    fn account_failure_alert_fires_once_per_window_and_is_per_account() {
        let w = LoginWatch::new();
        let l = watch_limits(3, 1_000_000);
        let t0 = Instant::now();
        assert_eq!(w.record_failure("a@x", t0, &l), LoginAlerts::default());
        assert_eq!(w.record_failure("a@x", t0, &l), LoginAlerts::default());
        // 第 3 次达到阈值：只告警，之后同一窗口不再重复
        assert!(w.record_failure("a@x", t0, &l).account);
        assert!(!w.record_failure("a@x", t0, &l).account);
        assert!(!w.record_failure("a@x", t0, &l).account);
        // 别的账号单独计数
        assert!(!w.record_failure("b@x", t0, &l).account);
        // 窗口过后重新计数、可以再次告警
        let t1 = t0 + ACCOUNT_FAIL_WINDOW;
        assert!(!w.record_failure("a@x", t1, &l).account);
        assert!(!w.record_failure("a@x", t1, &l).account);
        assert!(w.record_failure("a@x", t1, &l).account);
    }

    #[test]
    fn global_failure_alert_fires_once_per_minute_window() {
        let w = LoginWatch::new();
        let l = watch_limits(1_000_000, 4);
        let t0 = Instant::now();
        // 不同账号的失败合并计数
        for i in 0..3 {
            assert!(!w.record_failure(&format!("u{i}"), t0, &l).global);
        }
        assert!(w.record_failure("u3", t0, &l).global);
        assert!(!w.record_failure("u4", t0, &l).global);
        let t1 = t0 + GLOBAL_FAIL_WINDOW;
        for i in 0..3 {
            assert!(!w.record_failure(&format!("v{i}"), t1, &l).global);
        }
        assert!(w.record_failure("v3", t1, &l).global);
    }

    #[test]
    fn account_table_is_bounded() {
        let w = LoginWatch::new();
        let l = watch_limits(10, 1_000_000);
        let t0 = Instant::now();
        for i in 0..MAX_TRACKED_ACCOUNTS + 50 {
            w.record_failure(&format!("n{i}"), t0, &l);
        }
        assert_eq!(w.accounts.lock().unwrap().len(), MAX_TRACKED_ACCOUNTS);
        // 窗口过期后可以清出空间
        w.record_failure("fresh", t0 + ACCOUNT_FAIL_WINDOW, &l);
        assert_eq!(w.accounts.lock().unwrap().len(), 1);
    }

    #[test]
    fn names_are_trimmed_and_bounded() {
        assert_eq!(clean_name(Some("  Ann ")).unwrap().as_deref(), Some("Ann"));
        assert_eq!(clean_name(Some("   ")).unwrap(), None);
        assert_eq!(clean_name(None).unwrap(), None);
        assert!(clean_name(Some(&"x".repeat(101))).is_err());
        assert!(clean_name(Some("a\u{7}b")).is_err());
    }

    #[test]
    fn share_header_shapes() {
        let mut h = HeaderMap::new();
        assert_eq!(DashApi::share_slug(&h), None);
        h.insert(SHARE_HEADER, HeaderValue::from_static("short"));
        assert_eq!(DashApi::share_slug(&h), Some(None));
        let ok = "a".repeat(32);
        h.insert(SHARE_HEADER, HeaderValue::from_str(&ok).unwrap());
        assert_eq!(DashApi::share_slug(&h), Some(Some(ok.as_str())));
    }
}
