# Plausible Analytics：平台、业务数据与运维盘点

> 源码根（只读）：`D:\Forgejo\Rivu\examples\plausible-analytics\analytics-master`
> 下文路径均相对该根目录；`路径:行号` 为证据。标注「推断」的内容不是直接读到的代码事实。
> 范围：PostgreSQL 业务模型、权限、后台任务、运行配置、CE/EE、可观测性、部署产物、导入导出。接入管线与统计查询不在本文范围。

---

## 0. 关键发现速览

| # | 发现 | 证据 |
|---|---|---|
| 1 | `priv/repo/structure.sql` 是**陈旧快照**，只到 migration `20240924085157`；之后的 migrations（含 teams、SSO、OAuth、删除调度等）未体现在快照里。当前 schema = 快照 + 之后全部 migrations。 | `priv/repo/structure.sql`（末尾 `schema_migrations` INSERT 行，约 2738 行）；`priv/repo/migrations/` 共 243 个文件，最新 `20261006120000` |
| 2 | 数据模型以 **Team** 为一等实体，站点归属于 team，成员关系分 `team_memberships` 与 `guest_memberships`（站点级）两层。 | `priv/repo/migrations/20240924115329_add_teams_tables_fields.exs:5-74`；`lib/plausible/teams/team.ex:48-77` |
| 3 | API 限流是**团队级**小时配额（`team.hourly_api_request_limit`），key 级配额字段已在 EE 迁移中删除。 | `lib/plausible_web/plugs/authorize_public_api.ex:95,109`；`priv/repo/migrations/20250407110434_remove_unused_tables_and_columns.exs:7-20` |
| 4 | 后台任务用 **Oban + Postgres**，cron 与队列只在 `SELFHOST`/`DISABLE_CRON` 配置下切换；云端专属任务（计费、删除、锁站）仅在非自托管时加载。 | `config/runtime.exs:824-935` |
| 5 | CE/EE 不是运行时开关，而是**编译期**：`Mix.env()` 决定 `ee?()`，`extra/lib` 只在非 CE 构建中编入。Dockerfile 默认 `MIX_ENV=ce`。 | `lib/plausible.ex:6,39-49`；`mix.exs:62-69`；`Dockerfile:11,72` |
| 6 | 指标默认**关闭**（`PROMEX_DISABLED` 默认 true），OTel 不设 `OTLP_ENDPOINT` 时采样器为 `always_off`。 | `config/runtime.exs:1039-1046,1005-1011` |
| 7 | 迁移**不随容器启动自动执行**，需要显式运行 `db migrate.sh`（`Plausible.Release.interweave_migrate`）。 | `rel/docker-entrypoint.sh:4-5`；`rel/overlays/migrate.sh:6`；`lib/plausible_release.ex:27-75` |

---

## 1. PostgreSQL 业务表

### 1.1 快照与当前 schema 的关系

| 项 | 内容 | 证据 |
|---|---|---|
| 快照版本 | 最后一条 `schema_migrations` 为 `20240924085157` | `priv/repo/structure.sql`（末尾） |
| 快照内含但后来已删除的表 | `invitations`、`site_memberships`（EE 下在 `20250407110434` 中 drop） | `priv/repo/migrations/20250407110434_remove_unused_tables_and_columns.exs:39-40`（位于 `if enterprise_edition?()` 分支内，:7） |
| 快照内含但后来已删除的列 | `api_keys.hourly_request_limit`（EE 下删除） | 同上 `:20` |
| 快照中不存在、后来新增的表 | `teams`、`team_memberships`、`guest_memberships`、`team_invitations`、`guest_invitations`、`team_site_transfers`、`sso_*`、`oauth_*`、`team_deletion_schedules`、`email_suppressions`、`tracker_script_configuration`、`trial_prospects`、`segments`、`audit_entries` 等 | 对应 migration 文件名（见 1.2） |
| 推断 | 读 schema 必须以 migrations 为准；快照仅作为历史参考。 | 推断 |

### 1.2 核心业务表

表格列：表名 | 用途 | 关键列 | 主键 / 外键 / 唯一约束与状态列 | 证据

| 表名 | 用途 | 关键列 | 约束 / 状态 | 证据 |
|---|---|---|---|---|
| `users` | 账号 | `email`(citext)、`password_hash`(bcrypt)、`totp_enabled`、`totp_secret`(加密)、`totp_token`、`last_team_identifier`、SSO 相关字段（`type` standard/sso、`sso_identity_id`、`last_sso_login`） | 主键 id；SSO 字段位于 `user.ex:56-58` 所在的嵌套块中（未逐行核对是否为 EE 分支） | `structure.sql`（`users`，约 1306 行）；`lib/plausible/auth/user.ex:29-58`；`lib/plausible/auth/password.ex:6-7` |
| `teams` | 团队（计费、权限、API 限额的归属单位） | `identifier`(uuid)、`name`、`locked`、`setup_complete`、`hourly_api_request_limit`（默认 CE 1,000,000 / EE 600）、`policy`(embed)、`grace_period`(embed) | 有 has_many sites / memberships / invitations；`team_deletion_schedule` 有效状态过滤 | `lib/plausible/teams/team.ex:29,48-77`；`migrations/20240924115329_add_teams_tables_fields.exs:5-13` |
| `team_memberships` | 用户在团队内的角色 | `role`（enum：guest/viewer/editor/admin/owner/billing）、`is_autocreated`、`user_id`、`team_id` | 唯一 `(team_id,user_id)`；唯一 `user_id WHERE role != 'guest'`（即一个用户只能是一个团队的非访客成员）；`on_delete` 级联 | `lib/plausible/teams/membership.ex:10,16-23`；`migrations/20240924115329_add_teams_tables_fields.exs:15-32` |
| `guest_memberships` | 访客对**指定站点**的 viewer/editor 权限 | `team_membership_id`、`site_id`、`role`（viewer/editor） | 唯一 `(team_membership_id, site_id)` | `lib/plausible/teams/guest_membership.ex:13-16`；`migrations/20240924115329_add_teams_tables_fields.exs:34-45` |
| `team_invitations` | 团队邀请 | `email`、`role`、`invitation_id`、`inviter_id`、`team_id` | 唯一 `invitation_id`；唯一 `(team_id,email)` | `lib/plausible/teams/invitation.ex:14-22`；`migrations/20240924115329_add_teams_tables_fields.exs:47-61` |
| `guest_invitations` | 邀请时附带的站点访问 | `team_invitation_id`、`site_id`、`role` | 唯一 `(team_invitation_id, site_id)` | `lib/plausible/teams/guest_invitation.ex:14-17`；`migrations/...20240924115329...:63-74` |
| `team_site_transfers` | 站点在团队间转移 | `site_id`、`initiator_id`、`destination_team_id`、`transfer_id`、`email` | 唯一 `transfer_id`；唯一 `(destination_team_id,site_id)`、`(email,site_id)` | `lib/plausible/teams/site_transfer.ex:17-19`；`migrations/...20240924115329...:76-90` |
| `team_membership_user_preferences` | 成员级偏好（团队内） | `team_membership_id` | 唯一 `team_membership_id` | `migrations/20251112070338_team_membership_user_preferences.exs:5-11` |
| `sites` | 站点 | `domain`、`team_id`（可空，删团队置 NULL）、`timezone`、`public`（公开看板开关，默认 false）、`locked`、`has_stats`、`imported_data`(jsonb)、`ingest_rate_limit_threshold` / `_scale_seconds`（默认 60s）、`domain_changed_from/_at`、`allowed_event_props`、`conversions_enabled` / `funnels_enabled` / `props_enabled`、`accept_traffic_until`、`installation_meta` | `team_id` 外键 `nilify_all`；有 `team_id` 索引 | `structure.sql`（`sites`，约 1118 行）；`migrations/20240924115329_add_teams_tables_fields.exs:93-96`；`lib/plausible/site.ex:16` |
| `goals` | 转化目标（事件或页面路径；EE 下可带 currency 做收入目标） | `site_id`、`event_name`、`page_path`、`currency`、`display_name` | 无额外唯一约束（未核） | `structure.sql`（`goals`，约 400 行）；`lib/plausible/goal.ex:19,199-207` |
| `funnels` / `funnel_steps` | 漏斗及步骤（功能受 Funnels 权益控制） | `funnels.name`、`site_id`；`funnel_steps.funnel_id`、`goal_id`、`step_order` | 后续迁移加 `funnel_type`（`20260921080122`）、动态步骤（`20260921183042`）、`first/last`（`20260909121418`） | `structure.sql`（约 335、368 行）；`lib/plausible/billing/feature.ex:71` |
| `shared_links` | 公开分享链接 | `site_id`、`slug`、`name`、`password_hash`（可空） | 密码为 bcrypt；无密码即公开 | `structure.sql`（约 917 行）；`lib/plausible/site/shared_link.ex:43-55` |
| `api_keys` | 站点/团队 API 密钥 | `user_id`、`team_id`、`name`、`key_prefix`、`key_hash`（唯一）、`scopes text[]`（默认 `stats:read:*`） | 唯一 `key_hash`；唯一 `(team_id,user_id)`；**无 key 级限额列**（EE 下已删） | `lib/plausible/auth/api_key.ex:41-65`；`migrations/20250407110434...:20` |
| `shield_rules_ip` | Shield：IP 规则 | `inet`、`action`（默认 deny）、`description`、`added_by` | uuid 主键；`site_id` 外键 | `structure.sql`（约 982 行）；迁移 `20240123085318_add_ip_block_list_table.exs` |
| `shield_rules_country` | Shield：国家规则 | `country_code`、`action`（默认 deny） | uuid 主键 | `structure.sql`（约 951 行）；迁移 `20240221122626_shield_country_rules.exs` |
| `shield_rules_hostname` | Shield：hostname 规则 | `hostname`、`hostname_pattern`、`action`（默认 allow） | uuid 主键 | `structure.sql`（约 966 行）；迁移 `20240407104659_shield_hostname_rules.exs` |
| `shield_rules_page` | Shield：页面路径规则 | `page_path`、`page_path_pattern`、`action`（默认 deny） | uuid 主键 | `structure.sql`（约 998 行）；迁移 `20240307083402_shield_page_rules.exs` |
| `tracker_script_configuration` | 每站 tracker 脚本的功能开关（404、hash 路由、外链、下载、收入、tagged events、表单、pageview props 等） | `id`(string)、`site_id`（唯一）、`installation_type`、8 个 boolean 开关 | `site_id` 外键 `delete_all`；唯一 `site_id` | `migrations/20250515164952_add_tracker_script_configuration_table.exs:5-22` |
| `salts` | 会话盐（用于访客标识哈希） | `salt`(bytea)、`inserted_at` | 每日轮换写入 | `structure.sql`（约 693 行）；`lib/workers/rotate_salts.ex:7-12` |
| `site_imports` | 历史数据导入任务 | `site_id`、`source`（google_analytics_4 / universal_analytics / csv / noop）、`status`、`start_date`/`end_date`、`imported_by_id`、`legacy`(默认 true)、`label` | — | `structure.sql`（约 1014 行）；`lib/plausible/imported/site_import.ex:18`；`lib/plausible/imported/import_sources.ex:6-11` |
| `user_sessions` | 登录会话 | `user_id`、`token`(bytea，32 字节随机)、`device`、`last_used_at`、`timeout_at`（默认 14 天） | — | `structure.sql`（约 1272 行）；`lib/plausible/auth/user_session.ex:15-16,67` |
| `totp_recovery_codes` | 2FA 恢复码 | `user_id`、`code_digest`(bytea，摘要存储) | — | `structure.sql`（约 1241 行）；`lib/plausible/auth/totp/recovery_code.ex:16` |
| `segments` | 保存的筛选段（个人或站点级） | `site_id`、`owner_id`（可空） | 索引 `site_id` | `migrations/20241107120000_create_segments.exs:10-20` |
| `annotations` | 站点注解 | 见迁移 | — | `migrations/20260421101200_*`；`lib/plausible/annotations/annotations.ex:11-12` |
| `sso_integrations` | SSO 集成（每团队一个） | `team_id`（唯一）、`identifier`（唯一） | `team_id` 外键 `delete_all` | `migrations/20250520084130_add_sso_tables_columns.exs:8-23`（EE 功能，代码在 `extra/`） |
| `sso_domains` | SSO 绑定的邮箱域名 | `sso_integration_id`、`domain`（唯一）、`identifier`（唯一） | — | 同上 `:25-40` |
| `oauth_authorization_codes` / `oauth_grants` | OAuth 授权码与令牌（hash 存储） | `user_id`、`team_id`、`code_hash` / `access_token_hash` / `refresh_token_hash`（唯一）、`previous_refresh_token_hash`、`expires_at` | 外键 `delete_all` | `migrations/20260812120000_create_oauth_tables.exs:5-59`；`lib/plausible/oauth.ex:24` |
| `plugins_api_tokens` | 插件集成的站点级令牌 | `site_id`、`token_hash`(bytea)、`hint`、`description`、`last_used_at` | uuid 主键 | `structure.sql`（约 677 行） |
| `subscriptions` | 订阅（Paddle） | `team_id`（EE 迁移后）、`paddle_subscription_id`、`paddle_plan_id`、`status`、`next_bill_*`、`last_bill_date`、`currency_code` | 迁移中 `user_id` 被移除、改为 `team_id`（EE） | `structure.sql`（约 1201 行）；`migrations/20250407110434...:24-32` |
| `enterprise_plans` | 企业定制套餐 | `team_id`、`paddle_plan_id`、`billing_interval`、`monthly_pageview_limit`、`hourly_api_request_limit`、`site_limit`、`team_member_limit`（默认 -1）、`features[]` | 唯一索引（文件名 `20261006120000_add_team_id_paddle_plan_id_unique_index...`） | `structure.sql`（约 235 行）；`lib/plausible/billing/enterprise_plan.ex:12-25` |
| `plans` | 标准套餐目录（quota、features、product id） | `generation`、`kind`、`features[]`、`monthly_pageview_limit`、`site_limit`、`team_member_limit`、`volume`、`monthly/yearly_product_id`、`data_retention_in_years` | — | `structure.sql`（约 637 行） |
| `team_deletion_schedules` | 团队删除流程（提醒 → 执行，可 snooze） | `team_id`、`status`、`deletion_date`、`first_notice_due_date` | 外键 `delete_all`；多个索引 | `migrations/20260820120000_create_team_deletion_schedules.exs:5-29` |
| `trial_prospects` | 试用线索打分（云端/EE） | `team_id`（唯一）、`estimated_mrr` | — | `migrations/20260617120000_create_trial_prospects.exs:5-22` |
| `email_suppressions` | 邮件抑制名单（退信/投诉等） | `email`（唯一）、`reason`、`reactivated_by_user_id` | 有清理任务 `clean_email_suppressions` | `migrations/20260916070000_create_email_suppressions.exs:8-23` |
| `audit_entries` | 审计日志（EE） | 见迁移 | — | `migrations/20250708102205_*`；`extra/lib/plausible/audit/entry.ex` |
| `site_user_preferences` | 用户对站点的偏好（如置顶 `pinned_at`） | `user_id`、`site_id`、`pinned_at` | — | `structure.sql`（约 1085 行） |
| `spike_notifications` | 流量突增/骤降通知配置 | `site_id`、`threshold`、`last_sent`、`recipients citext[]`、`type`（默认 spike） | — | `structure.sql`（约 1166 行）；`lib/workers/traffic_change_notifier.ex` |
| `google_auth` | Google 授权（用于 GA 导入等） | `user_id`、`site_id`、`refresh_token`、`access_token`、`expires`、`property` | — | `structure.sql`（约 435 行） |
| `oban_jobs` | Oban 任务表 | 标准 Oban 列；`state`、`queue`、`worker`、`args`、`scheduled_at` | 2026-09 升级到 Oban v14 | `structure.sql`（约 569 行）；`migrations/20260907103006_upgrade_oban_jobs_to_v14.exs` |
| `pending_stats_deletions` | 待执行的统计数据删除（推断：与 ClickHouse 清理相关） | 未逐列核对 | — | `migrations/20260810060815_*` |
| 其他（邮件去重记录） | `check_stats_emails`、`create_site_emails`、`intro_emails`、`feedback_emails`、`setup_help_emails`、`setup_success_emails`、`sent_*_reports`、`sent_renewal_notifications`、`sent_accept_traffic_until_notifications`、`monthly_reports`、`weekly_reports`、`email_activation_codes` | 记录已发送邮件，防重复 | 一句话带过 | `structure.sql` |
| 已删除（历史） | `daily/weekly/monthly_stats`、`pageviews`、`events`、`sessions`、`tweets`、`custom_domains`、`fingerprint_sessions` 等 | 早期统计与功能表，已删除 | — | `migrations/20190820140747_*`、`20200605134616_*`、`20211202094732_*`、`20231127132321_*` |

未覆盖项：`sites.domain` 唯一性索引、`goals` 唯一约束未逐条核对；`priv/ingest_repo/structure.sql` 属 ClickHouse 侧，不在本文范围。

---

## 2. 权限模型

### 2.1 团队角色

| 角色 | 含义 | 证据 |
|---|---|---|
| `owner` | 团队所有者，可管理一切、分配任何角色 | `lib/plausible/teams/membership.ex:10`；`lib/plausible/teams/team_role_permissions.ex:105,116` |
| `admin` | 团队管理员，不能分配/降级 owner | `team_role_permissions.ex:22,106,117` |
| `editor` / `viewer` | 站点内容编辑 / 只读（团队级或站点级） | `membership.ex:10` |
| `billing` | 仅计费相关，可看/管订阅；邀请与修改规则单独定义 | `team_role_permissions.ex:124-126,134`；`lib/plausible/teams/billing.ex:20`（`@free_features`，计费模块） |
| `guest` | 不是团队成员，只通过 `guest_memberships` 对指定站点有 viewer/editor 权限；不可被分配为团队角色 | `membership.ex:10`；`team_role_permissions.ex:21`（`@assignable_roles` 去掉 guest）；`guest_membership.ex:13` |

团队管理权限（纯函数，与状态无关）：

| 规则 | 实现 | 证据 |
|---|---|---|
| 能否管理团队（邀请、改角色、移除） | 仅 owner、admin | `team_role_permissions.ex:31` |
| 能分配哪些角色 | owner 全部；admin 不含 owner；其他人无 | `team_role_permissions.ex:105-107` |
| 能改哪个成员的角色 | owner 任意；admin 只能改 admin/editor/viewer；自己改自己按同角色规则 | `team_role_permissions.ex:45-53,116-118` |
| 能改待处理邀请的角色 | owner 任意；admin 可改 admin/editor/viewer/billing（注释说明 billing 邀请不授予任何访问权） | `team_role_permissions.ex:131-136` |
| 状态相关规则（最后一个 owner 不能降级、强制 2FA 等） | 留在具体 service 中，不在此模块 | `team_role_permissions.ex:11-13` |

个人/站点级对象的可见性按角色区分：`segments` 中 billing 也可有个人 segment，但站点级 segment 只给 editor 及以上；annotations 规则相同。证据：`lib/plausible/segments/segments.ex:9-10`；`lib/plausible/annotations/annotations.ex:11-12`。

### 2.2 站点访问判定

| 步骤 | 说明 | 证据 |
|---|---|---|
| 1 | 超管（`ADMIN_USER_IDS`）直接放行 | `lib/plausible_web/plugs/authorize_public_api.ex:242`；`lib/plausible/auth/auth.ex:277-279`；`config/runtime.exs:94-95` |
| 2 | 站点所属团队是否 `locked` | `authorize_public_api.ex:248` |
| 3 | 团队套餐是否包含对应 feature（`Plausible.Billing.Feature.*`） | `authorize_public_api.ex:251`；`lib/plausible/billing/feature.ex:68-80` |
| 4 | 成员身份（团队成员或 guest 的站点授权） | `authorize_public_api.ex:235,254`（调用 `Teams.Memberships.site_member?` / `team_member?`，模块文件位置未核） |
| 5 | consolidated view 站点默认不允许通过普通 API 访问 | `authorize_public_api.ex:239` |

### 2.3 API key

| 项 | 内容 | 证据 |
|---|---|---|
| 归属 | key 属于 user，可绑定 team（唯一 `(team_id,user_id)`） | `lib/plausible/auth/api_key.ex:51-52,65` |
| 存储 | 只存 `key_hash` 与 `key_prefix`；明文仅在创建时以虚拟字段返回 | `api_key.ex:47-49,64` |
| 作用域 | 字符串前缀匹配，如 `stats:read:*`、`sites:read:*`、`sites:provision:*`；`stats:read:*` 与 `sites:read:*` 为所有 key 隐式具备 | `lib/plausible/auth/scopes.ex:7-11`；`authorize_public_api.ex:39-44,155-172` |
| 作用域描述（OAuth 同意页） | 只有两条描述 | `lib/plausible/auth/scopes.ex:13-22` |
| 限流 | 按**团队**的 `hourly_api_request_limit` 每小时计数；无团队的老 key 走 legacy 限额 | `authorize_public_api.ex:95,109`；`lib/plausible_web/controllers/api/rate_limit.ex:8,34-41` |
| 限流存储 | 基于 `Plausible.RateLimit`（ETS 表） | `lib/plausible/rate_limit.ex:27,33` |
| 错误码 | 缺失 → 401；无效或越权 → 401；超限 → 429；套餐不含 feature → 402；站点锁定 → 402 | `authorize_public_api.ex:287-347` |
| 创建/管理 UI | 列表和 key 创建页中的团队列仅 EE 显示 | `lib/plausible_web/templates/settings/api_keys.html.heex:43,59`；`new_api_key.html.heex:10`（`ee?()` 判断） |
| 推断 | 团队级限额意味着同团队多把 key 共享一个小时配额，限额归属与计费单位一致。 | 推断 |

### 2.4 公开看板、分享链接、SSO、2FA、会话

| 机制 | 实现要点 | 证据 |
|---|---|---|
| 公开看板 | `sites.public` 布尔开关，默认 false | `structure.sql`（sites） |
| 分享链接 | `shared_links`；可选密码（bcrypt 哈希）；无密码即任何拿到 slug 的人可看 | `lib/plausible/site/shared_link.ex:43-55`；`lib/plausible/auth/password.ex:6-7` |
| 登录限流 | 生产默认：登录按 IP 与按用户各 5 次/60 秒；激活、TOTP 设置、OAuth 授权等各有独立限流 | `lib/plausible/auth/auth.ex:20-63`（`@rate_limits`，:53 起） |
| 2FA | `users.totp_enabled`；secret 用 `TOTP_VAULT_KEY` 加密（支持 fallback key 轮换）；恢复码只存摘要；团队可 `force_2fa` | `lib/plausible/auth/user.ex:44-49`；`config/runtime.exs:167-209`；`lib/plausible/auth/totp/recovery_code.ex:16`；`lib/plausible/teams/policy.ex:51-52` |
| 会话 | `user_sessions`，随机 32 字节 token，默认 14 天超时，每次使用续期；每 2 小时清理过期会话 | `lib/plausible/auth/user_session.ex:15-16,46-57,67`；`config/runtime.exs:838` |
| SSO（EE） | 每团队一个 integration，绑定邮箱域名；可强制 SSO（`force_sso`: none / all_but_owners）；SSO 账号默认角色 viewer，会话超时 30–720 分钟，默认 360 | `lib/plausible/teams/policy.ex:11,42-49,16-18`；`extra/lib/plausible/auth/sso.ex`；`runtime.exs:353` 默认 SAML adapter 为 `fake` |
| 超管 | `ADMIN_USER_IDS` 列表，不走角色 | `config/runtime.exs:94-95`；`lib/plausible/auth/auth.ex:277-279` |

---

## 3. 后台任务（Oban）

### 3.1 Oban 配置

| 项 | 内容 | 证据 |
|---|---|---|
| 仓库 | `Plausible.Repo`（PostgreSQL） | `config/runtime.exs:918-919` |
| 插件 | `Pruner`（保留 30 天）、`Cron`、`Lifeline`（120 分钟后救援孤儿任务）、`Reindexer`（每日 01:00） | `config/runtime.exs:920-927` |
| 选主 | cron 开启时用 `Oban.Peers.Database`，多副本只有一个执行 cron | `config/runtime.exs:929-930` |
| `DISABLE_CRON=true` | **连 queues 也清空**：该节点不处理任何队列，只做 Web | `config/runtime.exs:929-930` |
| 测试环境 | PromEx 不挂 Oban 插件（避免连接冲突） | `lib/plausible/prom_ex.ex:26-35` |
| 错误上报 | Oban 异常 → 日志 + Sentry | `lib/oban_error_reporter.ex:22-37,71-79`；`lib/plausible/application.ex:375` |

### 3.2 队列与 cron 与 worker 一览

队列并发：自托管基础队列（`base_queues`，并发均为 1）；云端追加 `cloud_queues`，其中 `sso_domain_ownership_verification` 并发 32（`config/runtime.exs:878-912`）。

Cron（`config/runtime.exs:824-874`）。`base_cron` 自托管也运行；`cloud_cron` 仅 `SELFHOST=false` 时追加（:876）。

| Worker（`lib/workers/` 除注明外） | 队列 | 触发方式 | 用途（来源） |
|---|---|---|---|
| `RotateSalts` | `rotate_salts` | cron 每日 00:00（base） | 轮换会话盐（`rotate_salts.ex:11`） |
| `ScheduleEmailReports` | `schedule_email_reports` | cron 每小时（base） | 调度周报/月报，周一 9:00 按时区发（`schedule_email_reports.ex:3-4,11`） |
| `SendEmailReport` | `send_email_reports` | 由调度器入队（`max_attempts: 1`） | 发送单站报告（`send_email_report.ex:8`） |
| `SendSiteSetupEmails` | `site_setup_emails` | cron 每小时（base） | 站点接入引导邮件（`send_site_setup_emails.ex:7`） |
| `SendCheckStatsEmails` | `check_stats_emails` | cron 每日 12:00（base） | 提醒用户查看已收集到的数据（`send_check_stats_emails.ex:3`） |
| `TrafficChangeNotifier` | `spike_notifications` | cron 每 15 分钟（base） | 流量突增/骤降通知（推断，依据队列名与表 `spike_notifications`） |
| `CleanInvitations` | `clean_invitations` | cron 每日 01:00（base） | 清理过期邀请（`clean_invitations.ex:3`） |
| `CleanUserSessions` | `clean_user_sessions` | cron 每 2 小时第 30 分（base） | 清理过期会话，带宽限期（`clean_user_sessions.ex:3`） |
| `ExpireDomainChangeTransitions` | `domain_change_transition` | cron 每 2 小时（base） | 站点换域名的过渡期到期后释放旧域（`expire_domain_change_transitions.ex:3-4`） |
| `LocationsSync` | `locations_sync` | cron 每日 00:00（base） | 同步地理位置数据（推断，无 moduledoc；依队列名） |
| `CleanEmailSuppressions` | `clean_email_suppressions` | cron 每日 23:00 UTC（base） | 垃圾回收邮件抑制名单（`clean_email_suppressions.ex:3`） |
| `ClickhouseCleanSites` | `clickhouse_clean_sites` | cron 周二 03:00 UTC（base） | 异步清理已删站点在 ClickHouse 中的数据，分批 lightweight delete，遇 projection 回退 mutation（`clickhouse_clean_sites.ex:3-12,35-37`） |
| `ImportAnalytics` | `analytics_imports` | 用户触发入队（`max_attempts: 3`，`unique` 按 import_id，60 秒） | 历史数据导入（`import_analytics.ex:8-11`） |
| `LocalImportAnalyticsCleaner` | `analytics_imports` | 导入后入队（唯一 3600 秒） | 清理导入残留的本地文件（`local_import_analytics_cleaner.ex:3`） |
| `ExportAnalytics` | `analytics_exports` | 用户触发入队（`max_attempts: 3`，超时 15 分钟） | CSV 导出，支持 S3/本地（`export_analytics.ex:3-4,7-9,23`） |
| `NotifyExportedAnalytics` | `notify_exported_analytics` | 导出完成后入队（`max_attempts: 5`） | 导出完成通知（`notify_exported_analytics.ex:4-6`） |
| `SetLegacyTimeOnPageCutoff` | `legacy_time_on_page_cutoff` | cron 每日 05:00 UTC（cloud） | 按站点是否有旧版 time-on-page 数据设置截止点（`set_legacy_time_on_page_cutoff.ex:3`） |
| `CheckUsage` | `check_usage` | cron 每日 14:00（cloud） | 检查团队用量（推断，无 moduledoc） |
| `SendTrialNotifications` | `trial_notification_emails` | cron 每日 12:00（cloud） | 试用结束提醒（`send_trial_notifications.ex:3`） |
| `NotifyAnnualRenewal` | `notify_annual_renewal` | cron 每日 15:00（cloud） | 年付续费前 1–7 天提醒（`notify_annual_renewal.ex:3,20`） |
| `LockSites` | `lock_sites` | cron 每日 00:00（cloud） | 锁定站点（推断：配合 `teams.locked` / `sites.locked`） |
| `UnsnoozeTeamDeletions` | `unsnooze_team_deletions` | cron 每日 06:00（cloud） | 删除提醒 snooze 到期后恢复提醒（`unsnooze_team_deletions.ex:7`） |
| `ScanInactiveTeams` | `scan_inactive_teams` | cron 每日 07:00（cloud） | 找出试用/订阅已结束的团队并排入删除流程（`scan_inactive_teams.ex:3-7`） |
| `AcceptTrafficUntil`（文件 `accept_traffic_until_notification.ex`） | `check_accept_traffic_until` | cron 每日 08:00（cloud） | 在"停止接收流量"日期前通知站点所有者，每天最多一次（`accept_traffic_until_notification.ex:3-11`） |
| `SendDeletionNotifications` | `deletion_notification_emails` | cron 工作日 09:00（cloud） | 发送删除前提醒，再次核对订阅状态（`send_deletion_notifications.ex:3-9`） |
| `ExecuteTeamDeletions` | `execute_team_deletions` | cron 每日 10:00（cloud） | 执行到期的团队删除，`max_attempts: 1`（`execute_team_deletions.ex:3-8,13`） |
| `PurgeCDNCache` | `purge_cdn_cache` | 事件触发入队（cloud 队列） | 按 tag 清理 Bunny CDN 上 tracker 脚本缓存（`purge_cdn_cache.ex:3-8,11-13`） |
| `ScoreTrialProspects`（在 `extra/lib/plausible/workers/`） | `score_trial_prospects` | cron 每日 02:00 UTC（cloud，EE） | 给试用团队打 MRR 估值（`runtime.exs:871`；`extra/lib/plausible/workers/score_trial_prospects.ex`） |

说明：`ExportAnalytics`、`ImportAnalytics`、`LocalImportAnalyticsCleaner`、`PurgeCDNCache`、`SendEmailReport` 不在 cron 列表中，由业务代码入队。

---

## 4. 运行配置

### 4.1 通用机制

| 机制 | 说明 | 证据 |
|---|---|---|
| 配置来源 | 每个变量先查 `CONFIG_DIR/<VAR_NAME>` 文件（默认目录 `/run/secrets`，可用于 Docker secrets），找不到再查环境变量 | `lib/plausible/helpers/config.ex:6-14`；`config/runtime.exs:21` |
| 额外配置 | `EXTRA_CONFIG_PATH` 指向一个额外的 `.exs`，在 release 中 import | `rel/overlays/import_extra_config.exs:4-8`；`mix.exs:22-25` |

### 4.2 关键环境变量

| 变量 | 默认 / 约束 | 作用 | 证据 |
|---|---|---|---|
| `BASE_URL` | 必填，须以 http(s) 开头 | 外部访问地址；邮件 From 域名默认取其 host | `config/runtime.exs:69,78,106` |
| `SECRET_KEY_BASE` | 必填，≥ 32 字节，否则启动报错 | Phoenix 签名密钥 | `config/runtime.exs:81-92` |
| `SELFHOST` | 默认 `true` | `false` 时加载云端 cron/队列并使用 `SITE_DEFAULT_INGEST_THRESHOLD` | `config/runtime.exs:296,876,912,1052-1054` |
| `DISABLE_CRON` | 默认 `false` | `true` 时该节点不跑任何 Oban 队列，也不参加选主 | `config/runtime.exs:322,913,929-930` |
| `DISABLE_REGISTRATION` | CE 默认 `invite_only`，其他默认 `false`；可选 `true` / `false` / `invite_only` | 注册开关 | `config/runtime.exs:298-313` |
| `ENABLE_EMAIL_VERIFICATION` | 默认 `false` | 注册后邮箱验证 | `config/runtime.exs:293-294` |
| `ADMIN_USER_IDS` | 默认空 | 超管 user id 列表 | `config/runtime.exs:94-95` |
| `LISTEN_IP` / `HTTP_PORT` / `HTTPS_PORT` | 默认 `127.0.0.1` / `8000` / 未设则不开 HTTPS；Dockerfile 设 `LISTEN_IP=0.0.0.0` | 监听 | `config/runtime.exs:51,64-67`；`Dockerfile:90` |
| `ACME_DIRECTORY_URL` | 默认 Let's Encrypt 生产目录 | CE 自动证书（site_encrypt） | `config/runtime.exs:465-470,400-488` |
| `LOG_LEVEL` / `LOG_FORMAT` | CE 默认 `notice`，其他 `warning`；格式默认 `standard` | 日志 | `config/runtime.exs:24-30,40-42` |
| `LOG_FAILED_LOGIN_ATTEMPTS` | 默认 `false` | 记录登录失败 | `config/runtime.exs:325` |
| `SECURE_COOKIE` | 默认按 `BASE_URL` 的 scheme：http 为 false，https 为 true | Cookie secure 标志 | `config/runtime.exs:340-347` |
| `DATABASE_URL` | 默认 `postgres://postgres:postgres@plausible_db:5432/plausible_db` | PostgreSQL 连接串（默认凭据需在生产替换） | `config/runtime.exs:503-508` |
| `DATABASE_CACERTFILE` / `sslmode` | `sslmode` 支持 disable / require / verify-ca / verify-full | PG TLS | `config/runtime.exs:523,556-591` |
| `DATABASE_SOCKET_DIR` / `ECTO_IPV6` | 已弃用 | 旧式连接方式 | `config/runtime.exs:490-521` |
| `CLICKHOUSE_DATABASE_URL` | 默认 `http://plausible_events_db:8123/plausible_events_db` | ClickHouse 地址 | `config/runtime.exs:117-122` |
| `CLICKHOUSE_CACERTFILE` / `CLICKHOUSE_DEFAULT_STORAGE_POLICY` | — | CH TLS；表存储策略 | `config/runtime.exs:640,698` |
| `CLICKHOUSE_INGEST_POOL_SIZE` | 默认 5 | 写入连接池 | `config/runtime.exs:124-130,693` |
| `CLICKHOUSE_FLUSH_INTERVAL_MS` | 默认 5000 | 写入缓冲刷新间隔 | `config/runtime.exs:132-135,691` |
| `CLICKHOUSE_MAX_BUFFER_SIZE_BYTES` | 默认 100000；旧名 `CLICKHOUSE_MAX_BUFFER_SIZE` 已弃用 | 写入缓冲字节上限 | `config/runtime.exs:137-146,692` |
| ClickHouse 查询限制（硬编码） | 查询 `timeout` 15s；`max_execution_time` 20s；`readonly=1`；EE 构建关闭 query condition cache 的规避设置 | 只读查询护栏 | `config/runtime.exs:667-684` |
| `PERSISTOR_BACKEND` | `embedded`（默认）/ `embedded_with_relay` / `remote`；`PERSISTOR_BACKEND_PERCENT_ENABLED` 默认 0；`PERSISTOR_URL`、`PERSISTOR_COUNT`(200)、`PERSISTOR_TIMEOUT_MS`(10000) | 事件持久化后端（推断：用于灰度切换写入路径） | `config/runtime.exs:148-165` |
| `IMPORTED_MAX_BUFFER_SIZE` | 默认 10000 | 导入数据缓冲 | `config/runtime.exs:630` |
| `TOTP_VAULT_KEY` / `TOTP_VAULT_KEY_FALLBACK` | Base64 编码 32 字节 | 2FA secret 加密密钥及轮换备用 | `config/runtime.exs:167-209` |
| `ENABLE_SESSION_TRANSFER` | 默认 `true`（仅 prod） | 会话迁移目录（`persistent_cache_dir/sessions`） | `config/runtime.exs:286-291` |
| `DATA_DIR` / `PERSISTENT_CACHE_DIR` / `DEFAULT_DATA_DIR` | Dockerfile 设为 `/var/lib/plausible` | 数据与缓存目录 | `config/runtime.exs:279-284`；`Dockerfile:93` |
| `MAILER_ADAPTER` / `MAILER_EMAIL` / `MAILER_NAME` | 默认 `Bamboo.Mua` / `plausible@<BASE_URL host>` | 邮件发送适配器 | `config/runtime.exs:104-113` |
| 邮件服务 | `POSTMARK_API_KEY`、`MAILGUN_API_KEY`+`MAILGUN_DOMAIN`（+`MAILGUN_BASE_URI`）、`MANDRILL_API_KEY`、`SENDGRID_API_KEY`、`SMTP_HOST_ADDR`（+`SMTP_HOST_PORT` 默认 587、`SMTP_HOST_SSL_ENABLED`）、`SMTP_USER_NAME`/`SMTP_USER_PWD`、`SMTP_MIDDLEBOX_COMP_MODE` | 各适配器凭据 | `config/runtime.exs:736-795` |
| S3 | `S3_DISABLED` 默认 `true`；启用后需 `S3_ACCESS_KEY_ID`、`S3_SECRET_ACCESS_KEY`、`S3_REGION`、`S3_ENDPOINT`、`S3_EXPORTS_BUCKET`、`S3_IMPORTS_BUCKET`，缺一即报错 | 导出/导入对象存储 | `config/runtime.exs:1066-1130` |
| `IP_GEOLOCATION_DB` | 默认 `priv/geodb/dbip-country.mmdb.gz`（可用 `GEOLITE2_COUNTRY_DB` 指定） | IP 国家库 | `config/runtime.exs:268-275`；`Dockerfile:58`（构建时下载） |
| `MAXMIND_LICENSE_KEY` / `MAXMIND_EDITION`（默认 `GeoLite2-City`）/ `GEONAMES_SOURCE_FILE` | — | 城市级地理数据源 | `config/runtime.exs:276-278` |
| `SENTRY_DSN` | 未设则不上报 | Sentry | `config/runtime.exs:252,594-606` |
| `OTLP_ENDPOINT` | 未设则 OTel `sampler: :always_off` | 链路追踪导出 | `config/runtime.exs:265-266,1005-1011` |
| `BEAM_METRICS_ENABLED` / `BEAM_METRICS_INTERVAL_MS`（5000）/ `OTEL_EXPORTER_OTLP_ENDPOINT` | 默认关闭 | BEAM 指标经 OTel 推送 | `config/runtime.exs:1013-1035` |
| `PROMEX_DISABLED` | 默认 `true` | Prometheus 指标开关 | `config/runtime.exs:1039-1046` |
| `FRIENDLY_CAPTCHA_SITEKEY` / `FRIENDLY_CAPTCHA_API_KEY` | — | 注册/表单验证码 | `config/runtime.exs:315-316,937-939` |
| `SSO_SAML_ADAPTER` / `SSO_VERIFICATION_NAMESERVERS` | 默认 `fake` | SSO（EE） | `config/runtime.exs:353-359` |
| `LICENSE_KEY` | 未设则 EE 构建启动失败 | EE 许可校验 | `config/runtime.exs:350`；`lib/plausible/application.ex:12`；`extra/lib/license.ex:16-35` |
| `PADDLE_VENDOR_ID` / `PADDLE_VENDOR_AUTH_CODE` | — | Paddle 计费（EE 路由） | `config/runtime.exs:253-254,608-610`；`router.ex:432-433` |
| `GOOGLE_CLIENT_ID` / `GOOGLE_CLIENT_SECRET` | — | Google 授权（GA 导入等） | `config/runtime.exs:255-256` |
| `BUNNY_PULLZONE_ID` / `BUNNY_API_KEY` | — | 云端 CDN 缓存清除 | `config/runtime.exs:950-952` |
| `BROWSERLESS_TOKEN` / `BROWSERLESS_ENDPOINT` | 默认 `dummy_token` / `http://0.0.0.0:3000` | 接入检测用的无头浏览器 | `config/runtime.exs:1048-1050` |
| `SENTRY_FINCH_POOL_TIMEOUT` / `SENTRY_FINCH_RECEIVE_TIMEOUT` | 5000 / 15000 | Sentry HTTP 客户端超时 | `config/runtime.exs:944-948` |
| `WEBSOCKET_URL` | 默认空 | LiveView 的 WebSocket 地址覆盖 | `config/runtime.exs:327` |

Repo 连接池：生产环境未在 runtime 中设置 `pool_size`，使用 Ecto 默认值（推断）；测试环境 `pool_size: 15`（`config/test.exs:11-17`）。

---

## 5. CE 与 EE 划分

### 5.1 构建机制

| 机制 | 说明 | 证据 |
|---|---|---|
| 宏 | `on_ee`/`on_ce` 为编译期分支；`ee?()`/`ce?()` 为运行时包装的编译期常量（用 `always/1` 规避编译器优化） | `lib/plausible.ex:15-21,27-49,59-73` |
| 判定 | `ee? = Mix.env() not in [:ce, :ce_test, :ce_dev]` | `lib/plausible.ex:6,39-43` |
| 源码目录 | `elixirc_paths`：`:test/:dev/:e2e_test` 含 `extra/lib`；`:ce_test/:ce_dev` 不含；`:ce` 只含 `lib`；其他（如 `:prod`、`:load`）含 `extra/lib` | `mix.exs:62-69` |
| 发布 | `Dockerfile` 的 `MIX_ENV` 默认 `ce`，即默认镜像是 CE | `Dockerfile:11,72,81` |
| CE 专属依赖 | `happy_tcp`、`phoenix_bakery`、`site_encrypt`（自动 HTTPS） | `mix.exs:174-178` |
| 运行时许可 | EE 启动时校验 `LICENSE_KEY` 的 SHA-256；不符则启动失败 | `lib/plausible/application.ex:12`；`extra/lib/license.ex:29-35` |
| 文件头 | `extra/lib/license.ex:3-4` 声明 `ee/` 与 `assets/js/dashboard/ee` 版权归 Plausible Insights OÜ | `extra/lib/license.ex:3-4` |

### 5.2 `extra/` 目录（EE 代码，109 个文件）

| 域 | 路径（相对 `extra/lib`） | 说明 |
|---|---|---|
| 许可 | `license.ex` | 许可校验 |
| SSO | `plausible/auth/sso*`、`plausible_web/controllers/sso_controller.ex`、`plausible_web/live/sso_management.ex` | SAML、域名验证、身份 |
| 审计 | `plausible/audit*`、`plausible/audit/encoder.ex` | 审计条目与编码 |
| 漏斗 | `plausible/funnel*.ex`、`plausible/funnels.ex`、`plausible/stats/funnel.ex`、`plausible_web/live/funnel_settings*` | 漏斗的 DB 逻辑与 UI |
| 收入目标 | `plausible/goal/revenue.ex`、`plausible/stats/goal/revenue.ex`、`plausible/ingestion/*/revenue.ex` | 收入类目标 |
| 抽样与缓存 | `plausible/stats/sampling*.ex` | 大数据量查询抽样 |
| 合并视图 | `plausible/consolidated_view*.ex` | 多站点合并视图 |
| 接入检测 | `plausible/installation_support/*` | 检查 tracker 是否装好（使用 BrowserLess） |
| 客服后台 | `plausible_web/live/customer_support/*`、`plausible/customer_support/*` | 内部工具 |
| 线索评分 | `plausible/workers/score_trial_prospects.ex` | 试用线索打分 |
| HelpScout | `plausible/help_scout*`、`plausible_web/controllers/help_scout_controller.ex` | 客服集成 |
| 探索与旅程 | `plausible/stats/exploration.ex`、`plausible/stats/journey/step.ex` | 探索类分析 |
| API 扩展 | `plausible/plugins/api/funnels.ex`、`plausible_web/plugins/api/*` | 插件 API 中的漏斗 |

### 5.3 EE 功能门控与 CE 行为（代码路径）

| 项 | 代码事实 | 证据 |
|---|---|---|
| 路由 | Postmark webhook 仅 EE；Paddle webhook 与 currency 不在 `on_ee` 内 | `lib/plausible_web/router.ex:428-433` |
| 首次启动 | CE 下未登录用户访问时由 `FirstLaunchPlug` 引导到 `/register`，EE 不加载该 plug | `router.ex:12,27`；`lib/plausible_web/plugs/first_launch_plug.ex:30` |
| 注册默认 | CE 默认 `invite_only` | `config/runtime.exs:299-304` |
| 团队默认 API 限额 | CE 1,000,000/小时；EE 600/小时 | `lib/plausible/teams/team.ex:29` |
| 试用判断 | EE 用真实试用日期；CE 中 `on_trial?` 恒为 true | `lib/plausible/teams.ex:56-67` |
| 套餐功能（推断） | 当团队无订阅时，CE 因 `on_trial?` 恒真，返回 `Feature.list() -- [SitesAPI, SSO]`，即除 Sites API 与 SSO 外的功能全部可用 | `lib/plausible/teams/billing.ex:683-705`；`lib/plausible/billing/feature.ex:68-80`（推断，未运行验证） |
| 迁移 | EE 才执行删除 `invitations`、`site_memberships`、`api_keys.hourly_request_limit` 等的迁移分支（CE 保留这些表/列） | `priv/repo/migrations/20250407110434_remove_unused_tables_and_columns.exs:7` |
| 管理中心页面 | API key 页面中团队列仅 EE 显示 | `lib/plausible_web/templates/settings/api_keys.html.heex:43,59` |

推断：自托管 CE 与 EE 的 schema 已经分叉（EE 删了一些 CE 仍保留的表和列）。维护两版时需要分别验证迁移。

---

## 6. 可观测性

| 机制 | 入口 / 配置 | 默认 | 暴露内容 | 证据 |
|---|---|---|---|---|
| Prometheus（PromEx） | `PromEx.Plug` 挂在 endpoint；`Plausible.PromEx` | `PROMEX_DISABLED=true`（关） | 插件：Application、Beam、PhoenixLiveView、Phoenix、Ecto（4 个 repo：PG、CH 查询、Ingest、AsyncInsert）、Oban（非测试）、自定义 `PlausibleMetrics`；附 5 个 Grafana dashboard JSON | `lib/plausible/prom_ex.ex:6-56`；`lib/plausible_web/endpoint.ex:73`；`config/runtime.exs:1039-1046` |
| 自定义指标 | `lib/plausible/telemetry/plausible_metrics.ex` | 随 PromEx | 缓存 hit/miss；ingest buffer 入队/丢弃；persistor 耗时；UA 解析超时；tracker v1/v2 脚本请求计数；ClickHouse 清理阶段耗时与分区数；扫描团队创建数；团队删除相关计数 | `lib/plausible/telemetry/plausible_metrics.ex:86-266,332-428` |
| 推断 | OpenTelemetry 链路 | `OTLP_ENDPOINT`（gRPC） | 未设则 `sampler: :always_off` | Phoenix、Ecto（PG 与 CH，`db_statement: :enabled`）、Logger、Oban；span 属性含 `plausible.site.id/domain/team_id`、`plausible.user.*`（注意：用户 email 进入 trace） | `config/runtime.exs:1000-1011`；`lib/plausible/application.ex:382-386`；`lib/plausible/open_telemetry.ex:21-45` |
| BEAM 指标（OTel） | `BEAM_METRICS_ENABLED`，间隔 5 秒 | 关 | VM 指标经 OTLP 推送 | `config/runtime.exs:1013-1035` |
| Sentry | `SENTRY_DSN` | 不设则不上报 | Logger 的 `:error` 级别；Oban 异常；`before_send` 经 `Plausible.SentryFilter` 过滤；HTTP 客户端 `send_max_attempts: 1` | `config/runtime.exs:36-38,596-606,944-948`；`lib/plausible/application.ex:370`；`lib/sentry_filter.ex`；`lib/oban_error_reporter.ex:22-37` |
| 请求日志 | `Plausible.RequestLogger`，单行记录状态、方法、路径+查询串、耗时 | 随 Logger | — | `lib/plausible/request_logger.ex:10-18`；`lib/plausible/application.ex:364` |
| 日志格式/级别 | `LOG_FORMAT`（standard / json）、`LOG_LEVEL` | notice（CE）/ warning | — | `config/runtime.exs:24-42` |
| 存活检查 | `GET` `Api.SystemController.liveness` | — | 进程存活 | `lib/plausible_web/router.ex:420-422`；`lib/plausible_web/controllers/api/system_controller.ex:21` |
| 就绪检查 | `Api.SystemController.readiness` | — | 同时检查 PostgreSQL `SELECT 1`、ClickHouse `SELECT 1`、以及关键缓存 `ready?()`，全部通过才 200 | `lib/plausible_web/controllers/api/system_controller.ex:36-85` |
| 版本信息 | `Api.SystemController.info` | — | 版本、commit、构建时间、tags（来自构建元数据） | `lib/plausible_web/controllers/api/system_controller.ex:6-10`；`config/runtime.exs:242-250` |

---

## 7. 部署产物

### 7.1 Dockerfile（多阶段）

| 阶段 | 基础镜像 | 主要动作 | 证据 |
|---|---|---|---|
| `nodejs` | `node:24.17.0-alpine3.23`（固定 digest） | 只为 builder 提供 node/npm 二进制 | `Dockerfile:6,30-33` |
| `buildcontainer` | `hexpm/elixir:1.20.4-erlang-28.5.0.5-alpine-3.23.5` | 装依赖；`npm run deploy`（tracker）；`mix assets.deploy`；`mix download_country_database`（下载 GeoIP 库）；`mix sentry.package_source_code`；`mix release plausible` | `Dockerfile:9,35-63` |
| 运行时 | `alpine:3.23.5` | 非 root（uid 999）；装 openssl、libstdc++ 等；CE 额外装 `certbot`；复制 release 到 `/app` | `Dockerfile:66-88` |
| 入口 | `/entrypoint.sh` = `rel/docker-entrypoint.sh` | `run` → `bin/plausible start`；`db <name>` → 执行 `/app/<name>.sh`；其他直接 exec | `Dockerfile:82,91-95`；`rel/docker-entrypoint.sh:4-12` |
| 端口与卷 | `EXPOSE 8000`；`VOLUME /var/lib/plausible`；`DEFAULT_DATA_DIR=/var/lib/plausible` | — | `Dockerfile:92-94` |

### 7.2 数据库脚本与迁移

| 脚本 | 调用 | 作用 | 证据 |
|---|---|---|---|
| `createdb.sh` | `Plausible.Release.createdb` | 对 `repos()` 中每个 repo 执行 `ensure_repo_created`（建库，已存在则跳过） | `rel/overlays/createdb.sh:7`；`lib/plausible_release.ex:151-158` |
| `migrate.sh` | `Plausible.Release.interweave_migrate` | 按版本号交织执行 PG 与 CH 的待执行迁移（跨库依赖的排序） | `rel/overlays/migrate.sh:6`；`lib/plausible_release.ex:27-75` |
| `seed.sh` | `Plausible.Release.seed` | 对每个 repo 执行种子（`run_seeds_for/1`，推断为 `priv/repo/seeds.exs` 等） | `rel/overlays/seed.sh:5`；`lib/plausible_release.ex:143-149` |
| `pending-migrations.sh` / `rollback.sh` | 列出待执行迁移 / 回滚（`:down`） | 运维 | `rel/overlays/pending-migrations.sh`；`lib/plausible_release.ex:268` |
| 首次启动 | 没有任何用户时（CE），首访问被重定向到 `/register` | — | `lib/plausible_release.ex:18-24`；`lib/plausible_web/plugs/first_launch_plug.ex:30`；`lib/plausible_web/router.ex:12,27` |

推断：`start` 分支不执行迁移（`rel/docker-entrypoint.sh:4-5`），仓库内对 `interweave_migrate` 的调用只有 `rel/overlays/migrate.sh`，因此需要在升级时先执行 `db migrate.sh`。

### 7.3 CI（`.github/workflows`）

| 项 | 版本 / 配置 | 证据 |
|---|---|---|
| PostgreSQL | `postgres:18` | `.github/workflows/elixir.yml:26,33,122` |
| ClickHouse | `clickhouse/clickhouse-server:25.11.5.8-alpine` | `.github/workflows/elixir.yml:39,128` |
| Elixir / OTP | 通过 `steps.versions` 读取 | `.github/workflows/elixir.yml:54-55,149-150` |
| 浏览器测试 / tracker 更新 | Playwright `v1.56.1-noble`；tracker 更新流程 apt 安装 `clickhouse-server` | `.github/workflows/tracker.yml:19`；`tracker-script-update.yml:31-38` |
| Docker Compose | 不在本 zip 中，在 `plausible/community-edition` 仓库 | 本仓库无 compose 文件（已确认 `Get-ChildItem` 列表） |

---

## 8. 导入与导出

| 方向 | 机制 | 证据 |
|---|---|---|
| 导入来源 | `GoogleAnalytics4`、`UniversalAnalytics`（旧 UA，通过 Google 授权拉取）、`NoopImporter`（占位）、`CSVImporter`（用户上传 CSV） | `lib/plausible/imported/import_sources.ex:6-11` |
| 导入任务 | `site_imports` 记录状态；`ImportAnalytics` 作为 Oban 任务执行，`max_attempts: 3`，按 `import_id` 去重 60 秒；失败分瞬时与永久两类 | `lib/workers/import_analytics.ex:8-11,18-28,50-55,68-87`；`priv/repo/structure.sql`（`site_imports`） |
| CSV 导入 | 解析上传文件名与存储位置（`parse_args`）；以流方式写入 ClickHouse（`Ch.stream`）；超大流量缓冲由 `IMPORTED_MAX_BUFFER_SIZE` 控制 | `lib/plausible/imported/csv_importer.ex:20,27,163-164,271-301`；`config/runtime.exs:630` |
| 导入清理 | `LocalImportAnalyticsCleaner` 清理本地残留文件，唯一 3600 秒 | `lib/workers/local_import_analytics_cleaner.ex:3-6` |
| 导出格式 | 每次导出 CSV（`CSVWithNames`，`.csv`），由 ClickHouse 流式生成 | `lib/workers/export_analytics.ex:40,81,95` |
| 导出存储 | `s3`：流式 multipart 上传；`local`：先写临时文件再原子移动到目标路径 | `lib/workers/export_analytics.ex:52-53,68-84,88-104` |
| 导出超时与并发 | 任务超时 15 分钟；ClickHouse 查询取消执行超时（`get_config_without_ch_query_execution_timeout`）以允许大查询 | `lib/workers/export_analytics.ex:23,47` |
| 导出通知 | 完成后入队 `NotifyExportedAnalytics`（最多 5 次），发邮件通知下载 | `lib/workers/export_analytics.ex:119`；`lib/workers/notify_exported_analytics.ex:4-6` |

---

## 9. 对 Rivu 的启示

1. **以团队/组织为根建模，站点归属团队**。Plausible 把计费、API 限额、SSO、站点权限都挂在 `teams` 上，用户与站点通过 `team_memberships` 连接，访客通过 `guest_memberships` 只获得指定站点的 viewer/editor。Rivu 即使先做单用户，也建议先建 `team` 和 `site.team_id`，避免日后迁移（证据：1.2 节 `teams`、`guest_memberships`，`migrations/20240924115329`）。
2. **权限规则做成纯函数表，与状态规则分开**。`TeamRolePermissions` 只回答"谁能对哪种角色做什么"，"最后一个 owner 不能降级"之类的状态规则留给服务层（`lib/plausible/teams/team_role_permissions.ex:11-13,31-136`）。Rivu 可把角色矩阵写成可单测的表，并在文档中明确两类规则的边界。
3. **限额挂在团队而不是 key 上，并随计费套餐同步**。Plausible 先在 key 上有 `hourly_request_limit`，后来改为团队级 `hourly_api_request_limit`，并由套餐同步（`lib/plausible/billing/billing.ex:332`；`authorize_public_api.ex:95,109`；迁移 `20250407110434:20`）。Rivu 若按订阅计费，直接用团队级配额更少歧义。
4. **配置支持"文件优先、环境变量兜底"**，便于 Docker secrets（`lib/plausible/helpers/config.ex:6-14`；`config/runtime.exs:21`）。Rivu 可同样支持 `*_FILE` 或 `CONFIG_DIR/<VAR>`，并把所有配置集中在一处校验（如 `SECRET_KEY_BASE` 长度检查，`runtime.exs:81-92`）。
5. **周期任务与 Web 进程解耦，用数据库选主**。`DISABLE_CRON` 让节点只做 Web，多副本时由 `Oban.Peers.Database` 选出一个执行 cron（`config/runtime.exs:913,929-930`）。若 Rivu 用 Rust + PG，可用同样思路：任务表 + `SKIP LOCKED` + 单 leader 锁，并提供"只跑 Web"的开关。
6. **schema 真相以迁移为准，快照必须自动校验**。Plausible 的 `structure.sql` 停留在 2024-09，而迁移已到 2026-10，且 EE/CE 迁移分叉（`structure.sql` 末尾；`20250407110434:7`）。Rivu 应在 CI 中把 dump 与迁移结果比对，并把 CE/EE 差异限定在明确的迁移分支里。
7. **跨存储迁移需要排序器**。Plausible 为 PG 与 ClickHouse 的迁移写了交织执行器，理由写在 docstring 里（`lib/plausible_release.ex:27-61`）。Rivu 若同时用 PG 与 ClickHouse（或其他分析存储），迁移工具应从一开始支持跨库有序执行。
8. **可观测性默认关闭，但就绪检查要覆盖全部依赖**。`PROMEX_DISABLED` 与 OTLP 默认关（`config/runtime.exs:1039-1046,1005-1011`），而 readiness 同时测 PG、ClickHouse 与关键缓存（`system_controller.ex:36-85`）。Rivu 自托管可以默认关 trace、默认开 `/health/ready`，并把关键缓存的就绪状态一并暴露。
9. **破坏性操作拆成多阶段，带提醒与 snooze**。团队删除依次经过 `ScanInactiveTeams` → `SendDeletionNotifications`（工作日）→ `UnsnoozeTeamDeletions` → `ExecuteTeamDeletions`，执行前再次核对订阅（`config/runtime.exs:852-865`；`send_deletion_notifications.ex:3-9`；`execute_team_deletions.ex:3-8`）。Rivu 删站点或数据时可借鉴：先通知、可撤销窗口、执行前二次校验。
10. **导出走流式 + 可插拔存储 + 长超时隔离**。CSV 导出用 ClickHouse 流式输出，S3 用 multipart、本地先写临时文件再原子移动，超时设为 15 分钟，并把"完成通知"拆成独立任务（`lib/workers/export_analytics.ex:23,47,81-104,119`）。Rivu 的导出可直接沿用这一分层，避免大结果集占用内存。

---

## 附：未覆盖项

`sites.domain` 与 `goals` 唯一约束、`pending_stats_deletions` 用途、`Plausible.Session.Salts` 内部存储、站点角色计算细节均未逐行核对；Docker Compose 不在本 zip 中。未运行任何构建、测试或脚本。
