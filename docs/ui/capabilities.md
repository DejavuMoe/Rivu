# Rivu UI capabilities and terminology (evidence base for the prototype)

Status: 2026-10-22 snapshot, written before `apps/dashboard` shipped; at that date there was no production UI. Later surfaces record their own evidence (for example `designs/weekly-report/ui-contract.json`). This file is the functional-truth and terminology source for the prototype-first-ui workflow. Rules: facts only, `unspecified` where sources are silent, `proposed` where a term is invented here, no design rationale.

Citation keys (all `file:line`, paths relative to repo root):
- SD = `docs/architecture/system-design.md`
- PO = `docs/architecture/platform-and-ops.md`
- TP = `docs/architecture/tracker-and-protocol.md`
- PG1 = `migrations/postgres/20261021100000_core_tables.sql`, PG2 = `migrations/postgres/20261021100400_ops_tables.sql`
- CH1 = `migrations/clickhouse/20261021100100_events.sql`, CH2 = `...100200_sessions.sql`, CH3 = `...100300_ingest_counters.sql`
- PUB = `bin/rivu/src/public.rs`, INT = `bin/rivu/src/internal.rs`

## Decisions applied (user, 2026-10-22; override the docs where they differ)

| Topic | Decision | Docs still say (being edited by another agent) |
|---|---|---|
| Raw retention default | 731 days | 400 days: SD:37, SD:403, SD:804, PO:358, CH1:55, CH2:45 (TTL clause) |
| Max visit-session length | 7 days | 24 h: SD:310, SD:810 |
| GeoIP | Default DB-IP Lite; attribution mandatory wherever location data is shown; MaxMind and a custom `.mmdb` URL also supported | SD:272 (DB-IP Lite needs attribution), SD:808, PO:361-362 (`country.mmdb`, MaxMind key only) |
| `visit_duration` | Excludes engagement time; separate `engaged_duration` metric | SD:533, SD:805 (Q2, `engaged_duration` named only) |

## Users and primary jobs

Roles are evidenced in the permission matrix (PO:241-250). Jobs below are derived from the permitted actions, not from UX research (none exists).

- Instance admin (`users.is_instance_admin`, PG1:28): first user created at `/setup`; manages users and instance settings; has no implicit access to site statistics (PO:269).
- Org owner / admin: create and delete sites, change site settings, manage members, invitations, shared links, imports, stats deletion, API keys (PO:244-250).
- Editor (org or site guest): read stats, create exports, write goals/funnels/segments; org editor also manages shared links (PO:244-245).
- Viewer (org or site guest): read stats only; org viewer may manage own API keys (PO:243, PO:249).
- Anonymous visitor of a public site (`is_public`) or shared-link holder: read stats only (PO:263, PO:266).
- Site owner installing the tracker: copies the install snippet, verifies traffic arrives, reads ingest diagnostics to learn why data is missing (TP:37-52, SD:565).
- API consumer: reads stats with an API key (SD:621, PO:271-278). Not a UI user.

Primary jobs (all `planned`): sign in; first-run setup; create/manage sites; install tracker and confirm data flows; read dashboard (metrics, breakdowns, time series, comparison, filters); watch realtime visitors; diagnose missing data; configure site settings; manage members and sharing.

## Surfaces

### Existing today

No UI surface exists. `apps/dashboard/README.md` is a placeholder (stated by the task owner; the file is not a source of behavior). The single binary serves only the surfaces below.

| Surface ID | Route | Purpose | Primary actions | Evidence |
|---|---|---|---|---|
| internal-healthz | `GET :9090/healthz` (internal port, not routed by Caddy) | Liveness | Returns text `ok`, 200 | INT:54-65, PO:41 |
| internal-readyz | `GET :9090/readyz` | Readiness | 200 `status: ready` or 503 `not_ready`; JSON body `status`, `version`, `commit`, `role`, `checks.postgres{ok,error?}`, `checks.clickhouse{ok,error?}`, `checks.schema{ok, postgres{ok,required,applied \| error}, clickhouse{...}}`; each check times out at 3 s | INT:17-18, INT:99-133 |
| internal-metrics | `GET :9090/metrics` | Prometheus text (`rivu_build_info`, `rivu_uptime_seconds`) | Scrape | INT:135-159 |
| public-event-stub | `POST /api/event` (roles collector, all) | Placeholder collector | Always 501 JSON `{error:"not_implemented", message:"POST /api/event 尚未实现，计划在 P1 实现", phase:"P1"}` | PUB:10-14, PUB:21-34, PUB:64-70 |
| public-api-stub | `ANY /api/{*rest}` (roles api, all) | Placeholder API | Always 501, same body shape, message `/api/* 尚未实现，计划在 P1 实现` | PUB:15-17, PUB:36-38, PUB:73-90 |
| other public paths | anything else | none | 404 (no static dashboard route in the binary; Caddy is specified to serve static files, PO:40) | PUB:80-82 |

`/readyz` currently checks only PG, ClickHouse and schema version. The specified checks for site cache, session cache, salts and NATS are not implemented (PO:418).

### Specified but not implemented (all `planned`)

| Planned interface | Purpose | Evidence |
|---|---|---|
| `/setup` first-run page; `RIVU_SETUP_TOKEN` | Create first user, instance admin and first org owner | PO:90, PO:295 |
| Login, logout, "log out all devices", session list (`device_label`) | Authentication | PO:298-300, PG1:244-257 |
| Registration / invitation acceptance | `invite_only` default | PO:296, PG1:131-153 |
| Sites CRUD, hostnames, settings, delete with grace period and restore | Site management | PO:112-114, PO:234, PO:244-247; no HTTP paths are specified (`unspecified`) |
| `POST /api/internal/sites/{site_id}/query` | Dashboard query (session, shared link or public dashboard) | SD:622 |
| `GET /api/internal/sites/{site_id}/realtime` | Online visitors, equivalent to `date_range: "realtime"` | SD:624, SD:634 |
| `POST /api/v1/query`, `GET /api/v1/query/schema.json` | Public API (API key, scope `stats:read`) | SD:621, SD:623 |
| "接入诊断" (ingest diagnostics) view on dashboard | Accepted/dropped by reason | SD:564-565; data endpoint `unspecified` |
| `GET /js/<site_key>.js`, `GET /js/t.js`, `GET /js/t.<sv>.js` | Tracker delivery (not a UI) | TP:37, TP:51, TP:85 |
| Shared links, public dashboard (`is_public`) | Read-only sharing, optional password, expiry, bound segment | PG1:189-203, PO:118, PO:263, PO:303 |
| Goals, segments, funnels (P4), shield rules, API keys, members/invitations, imports, exports | Site content and admin | PG1:157-221, PO:116, PO:127-128, SD:796 |
| TOTP (P4), email reports, spike notify (P4) | Account security, notifications | PO:305, PO:330-331 |
| Dashboard query presets replace v1 endpoints `overview`/`timeseries`/`breakdown`/`goals` | Dashboard composition | SD:626 |
| Org switcher | Users may belong to several orgs | PO:153, PO:478 |

## Domain objects and terminology

Roles. Org roles: `owner`, `admin`, `editor`, `viewer` (PG1:55). Site-guest roles: `editor`, `viewer` (PG1:119). Effective role is the higher of org role and site-guest role (PO:254).

Permission matrix (PO:241-250): rows are actions; columns org owner / admin / editor / viewer / guest editor / guest viewer.

| Action | owner | admin | editor | viewer | guest editor | guest viewer |
|---|---|---|---|---|---|---|
| `stats.read` | Y | Y | Y | Y | Y | Y |
| `export.create`, `site.content.write` (goals, funnels, segments) | Y | Y | Y | N | Y | N |
| `shared_link.manage` | Y | Y | Y | N | N | N |
| `site.settings.write` (hostnames, collect, privacy, shield, retention), `site.public.toggle` | Y | Y | N | N | N | N |
| `site.create`, `site.delete`, `stats.delete`, `import.run`, `site_membership.manage` | Y | Y | N | N | N | N |
| `member.invite`, `member.remove`, `member.role.change` | Y | Y (cannot touch owner) | N | N | N | N |
| `api_key.manage_own` | Y | Y | Y | Y | N | N |
| `org.update`, `org.delete`, `owner.assign` | Y | N | N | N | N | N |

| Object | User-visible fields / values (enums, defaults, limits) | Allowed actions | Permission / ownership rules | Evidence |
|---|---|---|---|---|
| User | email (3-320 chars, case-insensitive, unique among non-deleted), password (12-128 chars, no class rules), name (optional), `is_instance_admin`, `disabled_at` | register, login, logout, change password/email, log out all devices | Login limited 5 per 60 s per IP and per account, uniform error text; password via argon2id | PG1:23-35, PO:297, PO:287, PO:302 |
| Organization | name 1-100 chars; slug unique case-insensitively; `api_hourly_limit` > 0 or null (null = `RIVU_API_RATE_PER_HOUR`, default 10000) | update, delete (owner only) | At least one owner must remain; a user may belong to several orgs | PG1:38-47, PO:254, PO:365 |
| Membership | org role owner/admin/editor/viewer | invite, remove, change role | Admin can assign admin/editor/viewer only; invitation cannot create owner | PG1:51-62, PO:252, PG1:135 |
| Site | `name` (no length rule), `site_key`, `timezone` (IANA name, default `UTC`, validated in app layer), `is_public` (default false), created/updated | create, rename, delete (soft), restore within grace, toggle public | Unauthorized access returns 404 not 403; deleted site is 404 except restore | PG1:65-79, PO:257, PO:254 |
| `site_key` | exactly 12 chars `[a-z2-7]`, public opaque id, not a secret, unique forever (tombstones kept) | display, copy | Never reused | PG1:68, PG1:76-77, PO:97. Conflict: TP:244 and examples (TP:38) use 16-char mixed-case keys `[A-Za-z0-9_-]{8,32}`; see Known unknowns |
| Hostname allowlist entry | `pattern` lowercase, regex `^(\*\.)?([a-z0-9-]+\.)*[a-z0-9-]+$`; `*.example.com` matches any depth of subdomain, not the apex (add apex separately); unique per site; main domain auto-added at site creation | add, remove | `site.settings.write` | PG1:82-91, PO:174, TP:327. Normalization before match: lowercase, strip trailing `.`, strip one leading `www.`, strip port, punycode (TP:326); wildcard only leftmost label (TP:327) |
| Site settings | see table below | edit | `site.settings.write`; one row per site | PG1:96-111 |
| Goal | `kind` event/page; `event_name` (event) or `page_path` (page), exactly one; `display_name` optional; unique per site/kind/target; goal types also include scroll-depth threshold; up to 3 props conditions per goal | create, edit, delete | `site.content.write` | PG1:157-172, SD:726 |
| Segment | `name` 1-100 chars; `filter` JSON; site-level (no owner) or personal (`owner_user_id`) | create, edit, delete | `site.content.write`; filter structure per SD §6.2 | PG1:176-186, SD:636 |
| Shared link | `slug` >= 24 random chars; optional password; optional bound segment; optional `expires_at`; read-only | create, revoke | `shared_link.manage`; grants `stats.read` only, bound segment filter forced; password cookie 12 h; rate 30/min, password tries 5/min | PG1:189-203, PO:263, PO:286, PO:303 |
| Shield rule | `type` ip/cidr/country/hostname/page; `action` deny/allow; `value` (country = uppercase alpha-2); `description`; unique per site/type/value; max 500 rules per site (`RIVU_SHIELD_MAX_RULES_PER_SITE`) | create, delete | `site.settings.write` | PG1:207-220, PO:187, PO:367 |
| Invitation | `email`; org invite: `role` admin/editor/viewer; site-guest invite: `site_role` editor/viewer; `expires_at`; one pending per org+email+scope | invite, revoke, accept | `member.invite`; role limited to inviter's assignable set; expired/accepted not reusable. Expiry duration `unspecified` | PG1:131-153, PO:254 |
| API key | `name`; `key_prefix` (first 12 chars of `rivu_`+32 bytes base64url); `scopes` default `{stats:read:*}`; `last_used_at`; `expires_at`; plaintext shown once | create, revoke | `api_key.manage_own`; scope syntax `resource:action:target`, resources `stats:read`, `sites:read`, `sites:write`, `exports:read`; member/org/key management and deletes are never key-accessible | PG1:225-240, PO:273-277 |
| Login session | `device_label`, `last_used_at`, `expires_at` = last use + 14 d, refreshed only when > 10 min since last write | log out, log out all | cookie `__Host-rivu_session` HttpOnly Secure SameSite=Lax | PG1:244-257, PO:298-299 |
| Visit session (analytics) | 30 min inactivity timeout (`RIVU_VISIT_SESSION_TIMEOUT`); max length 7 days (decision) | none (not user-managed) | n/a | PO:357, SD:310 |
| Event | `name` (`pageview`, `engagement`, or custom up to 120 chars), `props` (max 30 keys, key <= 300 B, value <= 2000 B), `interactive` | none | n/a | CH1:16, CH1:43-44, TP:245, TP:248 |
| Pending stats deletion | `scope` site/date_range; `date_from`/`date_to` (site-local dates); `status` pending/running/done/failed/cancelled; `not_before` | request date-range deletion; cancel via restore | `stats.delete`; user-facing fields beyond these `unspecified` | PG2:40-61 |
| Background job | `state` queued/running/succeeded/failed/cancelled; `last_error`; "failed visible in backend" | view `failed` | surface `unspecified` | PG2:13-30, PO:312 |

### `site_settings` fields (PG1:96-111)

| Field | Values | Default | Limits / notes | Evidence |
|---|---|---|---|---|
| `collect_enabled` | boolean | `true` | When off, collector drops with reason `site_disabled` (SD name) | PG1:98, SD:240 |
| `ingest_rate_limit_per_min` | integer > 0 or null | null = global default | Global default value `unspecified` | PG1:99, SD:240 |
| `retention_days` | integer > 0 or null | null = `RIVU_RETENTION_DAYS` (decision default 731) | Upper bound = `RIVU_RETENTION_DAYS`, validated in app layer; effective keep time N to N+31 days with partition TTL | PG1:100, PO:114, PO:358, SD:608 |
| `geo_level` | `country` / `region` / `city` | `city` (marked P0 inference) | Controls precision stored | PG1:101-102, SD:780 |
| `strip_query_params` | boolean | `true` (P0 inference) | Only UTM keys, `source`, `ref`, and click-id names are kept regardless (TP:330, TP:332); how this flag changes that is `unspecified` | PG1:103 |
| `allowed_event_props` | text array or null | null = unrestricted (P0 inference of type) | Optional props allowlist | PG1:104, SD:246 |
| `week_starts_on` | smallint 0-6, 0 = Sunday | 1 (Monday) | Used for `time:week` buckets | PG1:105-106, SD:733 |
| `tracker` | JSON object | `{}` | Keys map to install-script options (`hash`, `engagement`, `outbound`, `downloads`, `forms`, `tagged`, `manual`); exact stored schema `unspecified` | PG1:107, TP:57-67, TP:73 |
| site `timezone` (on `sites`) | IANA name | `UTC` | Storage is UTC; query bounds converted per site tz with DST handling; changing tz rebuilds rollups; visitor-ID salt follows site-local date | PG1:70, SD:731, SD:589-594, SD:287 |

Related env defaults surfaced to users: `RIVU_SITE_DELETE_GRACE_DAYS` 7; `RIVU_REGISTRATION` `invite_only`/`open`/`closed`, default `invite_only`; `RIVU_QUERY_TIMEOUT_SECS` 15, `RIVU_PUBLIC_QUERY_TIMEOUT_SECS` 8 (PO:359, PO:368, PO:366).

Geo display rule (decision): wherever country/region/city is shown, DB-IP attribution must be rendered when DB-IP Lite is the active database. Attribution wording and placement: `unspecified`. Region code format ISO 3166-2 (e.g. `CN-31`), city stored as `city_geoname_id` and named via GeoNames dictionary (CH1:35-36, SD:273).

## State matrix

All flows are `planned`; cells quote specified behavior only. `unspecified` = sources silent. "n/a" = not applicable by design of the flow.

| Flow | Ready | Loading | Empty | Error | Disabled | Permission / offline | Progress / cancel / retry | Platform-specific |
|---|---|---|---|---|---|---|---|---|
| Login | Valid credentials create session, cookie 14 d (PO:298) | unspecified (hashing runs off-thread, PO:297) | n/a | Unknown user and wrong password give identical response and timing; rate limit returns 429 with a uniform message (PO:287, PO:302) | Disabled user (`disabled_at`) cannot log in: sessions invalid (PG1:30; exact message `unspecified`) | Registration policy `invite_only`/`open`/`closed` (PO:296); TOTP prompt is P4 (PO:305) | Retry after 429 `Retry-After` (PO:285 for API; login `Retry-After` `unspecified`) | JSON-only requests with Origin check, no form posts (PO:301) |
| First-run setup | `/setup` open once when `users` empty; first user becomes instance admin and first org owner (PO:295) | unspecified | Zero users is the entry condition (PO:295) | Wrong or missing `RIVU_SETUP_TOKEN` is rejected (message `unspecified`) (PO:295) | After first user exists `/setup` closed (PO:295) | n/a | Advisory lock serializes concurrent attempts (PO:295) | unspecified |
| Sites list | Sites of the org(s) the user belongs to, plus guest-site grants (PG1:78, PG1:115) | unspecified | Org with no sites: unspecified | Not-found/forbidden both 404 (PO:257) | Deleted sites hidden; restorable within 7-day grace (PO:234) | Role gates create/delete (PO:247); org switcher needed (PO:478) | Delete is multi-stage: soft delete, grace, then ClickHouse purge (PO:234) | unspecified |
| Add site + install verification | Site gets `site_key`, tz, main domain auto-added to hostnames (PO:174); snippet in section "Install snippet"; verified when events arrive | unspecified | No events yet: no specified signal. `ingest_counters` `accepted` = 0 (SD:554) | Dropped reasons visible only in diagnostics (SD:226, SD:565); 400 never counts to site (TP:312) | `collect_enabled` off drops all (PG1:98) | `site.create` owner/admin (PO:247) | Script cache up to 5 min delay (TP:83); event visible p95 < 2 s Lite (SD:347). In-UI verify mechanism `unspecified` (only tracker-side `rivu.b`/`rivu.v`, TP:96) | Tracker needs fetch, URL, sendBeacon (TP:33); localhost ignored unless `local:true` (TP:213) |
| Dashboard query | `results`, `meta`(`time_labels`,`warnings`,`tiers`,`metric_semantics`), `query` echo (SD:641) | unspecified | No rows: unspecified; `meta.warnings` code `range_beyond_raw_retention` when range exceeds retention and rollup incompatible (SD:687) | 404 if `site` mismatches auth (SD:632); 429 when org concurrency (default 4) exceeded after queue timeout (SD:743); unknown filter combination errors (SD:727); timeouts dashboard 10 s / API 20 s / shared 5 s (SD:743; PO:290 gives 15 s / 8 s, conflict) | Shared link/public: `stats.read` only (PO:263) | Expired link, wrong password (message `unspecified`); password tries 5/min (PO:286) | Cache buckets realtime 3 s, today 10 s, 7 d 60 s, history 10 min (SD:749); cancel `unspecified` | `meta.tiers` lists raw vs rollup per day (SD:699) |
| Realtime | Distinct visitors, last 5 minutes, events with `name != 'engagement'` (SD:718) | unspecified | 0 visitors natural; message `unspecified` | as dashboard query | n/a | `stats.read` | Refresh interval `unspecified`; result cache 3 s (SD:749) | unspecified |
| Ingest diagnostics | Counts per minute by `metric` and `script_version` for the site, retained 90 days (CH3:3-14, SD:561) | unspecified | No counters: unspecified | unspecified | n/a | Role required `unspecified` (nearest: `stats.read`) | Counters flushed in 10 s buckets, only closed buckets, so recent seconds lag (SD:564) | Sits beside dashboard ("看板上的接入诊断", SD:565) |
| Site settings | Fields in settings table; edits need `site.settings.write` (PO:246) | unspecified | `tracker` `{}` means all defaults (PG1:107) | Validation errors: retention above global cap, bad IANA tz (PG1:70, PG1:100); messages `unspecified` | Viewer/editor read-only (PO:246) | Non-members get 404 (PO:262) | Changes reach collector within 1 s via NOTIFY, else <= 10 s incremental refresh (SD:262, SD:261); tz change triggers rollup rebuild (SD:592); script changes up to 5 min (TP:83) | unspecified |

## System interfaces

| UI action | Interface | Payload / result | Failure behavior | Source |
|---|---|---|---|---|
| Health probe (ops, not UI) | `GET :9090/readyz` | JSON described in Surfaces | 503 `not_ready` with per-check error text | INT:99-133 |
| Run any dashboard query | `POST /api/internal/sites/{site_id}/query` (planned) | `QuerySpec` (below) to `results/meta/query` | 404 wrong site; 429 over concurrency; timeout | SD:622, SD:641, SD:632, SD:743 |
| Realtime | `GET /api/internal/sites/{site_id}/realtime` (planned) | online visitors | as query | SD:624 |
| Public API query | `POST /api/v1/query` with API key | same `QuerySpec` | 401 missing/invalid key, 403 scope, 429 limit + `Retry-After`, `X-RateLimit-*` | SD:621, PO:278, PO:285 |
| Fetch schema | `GET /api/v1/query/schema.json` | JSON schema from Rust types | none | SD:623 |
| Tracker delivery | `GET /js/<site_key>.js` | JS; 304 on ETag match; unknown key 404 `no-store` | 404 | TP:82-84 |
| Send event (tracker, not UI) | `POST /api/event` | JSON body <= 8192 B | 202 accepted or dropped; 400 `{"errors":{field:[msg]}}`; 413; 405; 503 + `Retry-After` | TP:283, TP:289-297, SD:223-229 |
| Auth, setup, sites CRUD, settings, invitations, shared links, goals, segments, shield, API keys | `unspecified` (no paths or payloads in sources; only `/api/*` prefix routed to api, PO:39) | `unspecified` | auth failures 404 for no-permission site access (PO:257) | PO:39 |
| Ingest diagnostics read | `unspecified` | table `ingest_counters(site_id, minute, metric, script_version, value)` | `unspecified` | CH3:3-14 |

## Ingest diagnostics

Storage: `ingest_counters`, SummingMergeTree, key `(site_id, minute, metric, script_version)`, TTL 90 days (CH3). `metric` is `accepted` or `dropped_<reason>` (CH3:7, SD:554). Dimension `script_version` is the `sv` value (TP:243). Dashboard view groups by reason (SD:565). Counters are also used for scan-row estimation and quotas (SD:566-568).

Response semantics (visible to the site owner only via the tracker network tab, never in UI): accepted gives `202` empty body; filtered gives `202` plus `x-rivu-dropped: <count>` and no reason; the single-event endpoint always sends `1` (SD:225-226, TP:291). `400` protocol errors are not counted as dropped (SD:227) but are counted separately as `rejected_<field>` (TP:312). `503` overload counts as `dropped_overloaded` (SD:229).

Two documents name the reasons differently. SD:240-248 is the pipeline table (primary); TP:309-318 lists alternates. Both are given; the final identifiers are `unspecified` until reconciled.

| Counter `metric` (SD form) | TP alternate | Pipeline stage | Per site? | Evidence |
|---|---|---|---|---|
| `accepted` | `buffered` is a Plausible term (SD:89); Rivu `accepted` | after stage 11 handoff | yes | CH3:7, SD:225 |
| `dropped_unknown_site` | `dropped_unknown_site` | 3 admission (site lookup) | no real site: counted under `_unknown_` (TP:320); how stored with `site_id UInt64` is `unspecified` | SD:240, TP:313, TP:320 |
| `dropped_site_disabled` | `dropped_disabled` | 3 admission | yes | SD:240, TP:314 |
| `dropped_hostname_not_allowed` | `dropped_hostname` | 3 admission (hostname allowlist) | yes | SD:240, TP:315 |
| `dropped_rate_limited` | `dropped_throttle` | 3 admission (token bucket) | yes | SD:240, TP:314 |
| `dropped_spam_referrer` | `dropped_spam_referrer` | 3 admission | yes | SD:240, TP:316 |
| `dropped_ip_blocked` | `dropped_shield_*` | 4 rules (IP/CIDR) | yes | SD:241, TP:316 |
| `dropped_page_blocked` | `dropped_shield_*` | 4 rules (page glob) | yes | SD:241, TP:316 |
| `dropped_bot` | `dropped_bot` | 5 bot detection | yes | SD:242, TP:316 |
| `dropped_datacenter_ip` | none | 5 bot detection (optional CIDR table) | yes | SD:242 |
| `dropped_country_blocked` | `dropped_shield_*` | 6 geo (after country rule) | yes | SD:243, TP:316 |
| `dropped_no_session` | `dropped_no_session_for_engagement` | 11 handoff (engagement without session). TP places the check before enrichment (TP:317) | yes | SD:248, SD:313, TP:317 |
| `dropped_overloaded` | `overload` | 11 handoff (buffer full / JetStream down), HTTP 503 | `unspecified` | SD:229, SD:248, TP:318 |
| `dropped_site_deleted` | none | collector, after soft delete | yes | PO:234 |
| `dropped_salt_missing` (written there as `salt_missing`; prefix form assumed, not stated) | none | identity stage (salt lookup) | yes | PO:235 |
| `rejected_<field>` (e.g. `rejected_body_size`, TP:311) | n/a | 1 parse and validate (HTTP 400/413) | no: dimensions are `sv`, not site | TP:311-312, TP:393 |
| `internal_error` | n/a | HTTP 500 | `unspecified` | TP:297 |

Notes: `dropped_<reason>` prefix is the stated schema (CH3:7); SD:240-248 lists bare reason names and PO names `site_deleted`, `salt_missing` bare. Unknown-site key hygiene: `_unknown_` bucket (TP:320). Prometheus metric `rivu_ingest_events_total{outcome,reason}` carries no `site` label (PO:420, PO:430).

## Query DSL (`QuerySpec`, SD:628-639)

Unknown fields are rejected (`deny_unknown_fields`, SD:647). `site` must equal the authenticated site key.

| Part | Values | Evidence |
|---|---|---|
| `metrics` | `visitors` `visits` `pageviews` `events` `views_per_visit` `bounce_rate` `visit_duration` `conversion_rate` `group_conversion_rate` `percentage` `exit_rate` `time_on_page` `scroll_depth`; revenue metrics (P4); `engaged_duration` (decision, definition `unspecified`); realtime count | SD:633, SD:718 |
| `dimensions` | `event:name` `event:page` `event:hostname` `event:goal` `event:props:<k>`; `visit:entry_page` `visit:exit_page` `visit:source` `visit:channel` `visit:referrer` `visit:utm_*` (source, medium, campaign, content, term) `visit:country` `visit:region` `visit:city` `visit:device` `visit:browser` `visit:browser_version` `visit:os` `visit:os_version`; `time` `time:hour` `time:day` `time:week` `time:month` | SD:635 (`time:minute` appears only in SD:732) |
| `date_range` | `day` `24h` `7d` `28d` `30d` `month` `6mo` `12mo` `year` `all`, custom `Nd`, `Nmo`, `[from,to]` (dates or tz-aware datetimes), dashboard-only `realtime`. `7d` = last 7 local days including today | SD:634, SD:734 |
| comparison (`include.compare`) | `previous_period`, `year_over_year`, `[from,to]`; compare query is constrained to the main query's dimension rows | SD:639, SD:658 |
| other `include` | `time_labels`, `total_rows`, `imports` | SD:639 |
| filter leaf | `[op, dim, [values], {case_sensitive}]`; `op` in `is` `is_not` `contains` `contains_not` `matches` `matches_not` `matches_wildcard` `matches_wildcard_not` | SD:636 |
| filter combinators | `and` `or` `not` `has_done` `has_not_done`; `["is","segment",[id]]` | SD:636 |
| filter limits | `matches`: <= 10 patterns per filter, <= 256 chars each | SD:742 |
| `order_by` | only queried metrics or dimensions | SD:637 |
| `pagination` | `limit` <= 10000, `offset` | SD:638 |
| granularity | `time` + hour/day/week/month. Auto granularity chosen in the optimize step (rule `unspecified`, SD:649). Rollup tier requires granularity not finer than day (SD:695). Hour/minute buckets use 15-minute slots for active visitors (SD:732). Week start from `week_starts_on` (SD:733) | SD:635, SD:695, SD:732 |
| order of effect with page filter | with `event:page` filter: `visitors`/`pageviews` count visits to that page; `bounce_rate` counts sessions whose entry page matches; other session metrics count sessions that viewed the page; surfaced in `meta.metric_semantics` | SD:724 |
| goal types | event name, page wildcard, scroll-depth threshold | SD:726 |

### Metric definitions (raw tier, SD:705-718)

| Metric | Definition |
|---|---|
| `visitors` | `uniq(visitor_id)`; visitor ID rotates at site-local midnight, so multi-day totals approximate the sum of daily visitors (SD:602) |
| `visits` | sessions active in range |
| `pageviews` / `events` | count of `pageview` / count of non-`engagement` events |
| `bounce_rate` | sessions with `pageviews <= 1` and zero interactive events, over `visits` |
| `visit_duration` | average seconds `last_event_at - started_at`, engagement excluded (SD:533, decision) |
| `views_per_visit` | average pageviews per session |
| `time_on_page` | `sum(engagement_ms) / uniqIf(session_id, name='engagement')` per page |
| `scroll_depth` | per page, mean over sessions of max scroll depth (<= 100) |
| `exit_rate` | exits from page / its pageviews |
| `conversion_rate` | visitors completing goal / visitors in same range without goal and props filters |
| `percentage` | row visitors / total visitors without dimensions |
| realtime | `uniq(visitor_id)`, last 5 minutes, non-engagement |

Bounce uses `interactive` (CH2:20, CH1:43). Ratios are zero on divide-by-zero (SD:720).

## Install snippet

Recommended site-specific script (TP:37-39). `site_key` is embedded in the URL; the script URL comes from `public_base_url`, never the request Host (TP:76):

```html
<script defer src="https://stats.example.com/js/k7Qm2xV9aTbR4cLd.js"></script>
```

With manual-API queue stub, optional (TP:43-47):

```html
<script>window.rivu=window.rivu||function(){(rivu.q=rivu.q||[]).push(arguments)}</script>
<script defer src="https://stats.example.com/js/k7Qm2xV9aTbR4cLd.js"></script>
<script>rivu('event','Signup',{props:{plan:'pro'}})</script>
```

Generic static/CDN `t.js` (TP:50-53):

```html
<script defer src="https://cdn.example.net/rivu/t.js"
        data-site="k7Qm2xV9aTbR4cLd" data-api="https://stats.example.com/api/event"></script>
```

Attributes for `t.js` (TP:57-67): `data-site` required (`site_key`); `data-api` default script origin + `/api/event`; `data-hash` false; `data-local` false; `data-manual` false (full only); `data-engagement` true (full only); `data-outbound` false; `data-forms` false; `data-downloads` false or `true` or comma list; `data-tagged` true; `data-debug` false (full only). Without `data-site` the script does nothing (TP:55). `core` vs `full` build chosen automatically for site scripts: `full` when any of engagement, outbound, downloads, forms, tagged, manual, manual API is on (TP:81).

Verification mechanisms specified:
- Browser-side probe fields set after load: `rivu.v` (protocol version), `rivu.sv` (build number), `rivu.b` (`"core"` or `"full"`), "for installation detection" (TP:96, TP:142). Duplicate load guarded by `rivu.l` (TP:94).
- Server-side: arrival of `accepted` counters; failures visible as `dropped_*` / `rejected_*` (TP:30, SD:565).
- Ignore mechanism: self-exclusion via `localStorage.rivu_ignore = "true"` set by the site owner in their own browser (TP:215); also ignored: localhost, `file:` (unless `local:true`), `navigator.webdriver`/`window.Cypress` without `window.__rivu` (TP:213-214), missing site (TP:216). Ignored sends no request; `debug` on full prints `[rivu] ignored: <reason>` to console (TP:218).
- How the dashboard itself checks installation (poll interval, success signal, copy): `unspecified`.

## Interaction and accessibility behavior

- Keyboard/focus: unspecified.
- Pointer/touch/drag/drop/context menu: unspecified.
- Announcements, labels, live regions: unspecified.
- Reduced motion, high contrast, localization: unspecified. The dashboard is a SvelteKit static build (`adapter-static`) served by Caddy with `try_files {path} /index.html` (SD:179, PO:40). Charting library ECharts is named (SD:179). Locale and number/date formatting rules: unspecified.

## Platform, viewport, and runtime constraints

- Dashboard is a static SPA from `apps/dashboard`, same origin as the API under Caddy; no CORS (PO:301, PO:32). Dev: Vite proxies `/api` to `127.0.0.1:8080` (PO:87).
- Auth is cookie-based (`__Host-rivu_session`); requests are JSON only with Origin check against `RIVU_BASE_URL` (PO:299-301).
- Security headers include `Content-Security-Policy: default-src 'self'` on static responses (PO:58); inline scripts/styles therefore need verification (`unspecified`).
- Viewport, browser baseline for the dashboard, theme: unspecified. Tracker browser baseline is separate (TP:33).
- Query timeouts and concurrency are enforced server-side (SD:743). `RIVU_GEOIP_DB_PATH`, `RIVU_MAXMIND_*` are operator settings, not UI (PO:361-362).

## Content authority currently evidenced

Domain terminology is the identifier set below. Chinese UI terms already used in docs are marked with a doc citation; everything else is `proposed`. English terms are proposed unless an English word is used in the docs.

Existing strings that must remain compatible: HTTP 501 body `error: "not_implemented"`, `phase: "P1"` (PUB:24-28); `/readyz` JSON keys and `status` values `ready` / `not_ready` (INT:112-123); tracker error vocabulary (`is required`, `must be a string`, ... , `at least one of sd or e is required`, TP:299); `x-rivu-dropped` header (SD:226); cookie name `__Host-rivu_session` (PO:299); probe fields `rivu.v/sv/b` (TP:96).

Approved marketing copy: none.

### Bilingual glossary

| Identifier | zh-CN UI term | en UI term | Evidence / origin |
|---|---|---|---|
| **Metrics** | | | |
| `visitors` | 访客 | Visitors | zh: PO:241 ("站点访客" uses 访客 for guest, metric term) / en proposed; `proposed` |
| `visits` | 访问 | Visits | PO:357 "访问会话" (session); `proposed` for metric label |
| `pageviews` | 页面浏览量 | Pageviews | `proposed` |
| `events` | 事件数 | Events | SD:709 "事件"; count label `proposed` |
| `views_per_visit` | 每次访问浏览页数 | Views per visit | `proposed` |
| `bounce_rate` | 跳出率 | Bounce rate | SD:536 "跳出" |
| `visit_duration` | 访问时长 | Visit duration | SD:793 "停留时长" is used for the sample; `proposed` label |
| `engaged_duration` | 参与时长 | Engaged duration | SD:805 identifier; `proposed` |
| `conversion_rate` | 转化率 | Conversion rate | SD:716 "目标/转化"; `proposed` |
| `group_conversion_rate` | 分组转化率 | Group conversion rate | `proposed` |
| `percentage` | 占比 | Percentage | `proposed` |
| `exit_rate` | 退出率 | Exit rate | `proposed` |
| `time_on_page` | 页面停留时间 | Time on page | `proposed` |
| `scroll_depth` | 滚动深度 | Scroll depth | `proposed` |
| realtime | 实时在线访客 | Current visitors | `proposed` |
| **Dimensions** | | | |
| `event:name` | 事件名称 | Event name | `proposed` |
| `event:page` | 页面 | Page | `proposed` |
| `event:hostname` | 主机名 | Hostname | `proposed` |
| `event:goal` | 目标 | Goal | PO:115 "转化目标"; `proposed` short form |
| `event:props:<k>` | 事件属性 | Event property | `proposed` |
| `visit:entry_page` | 入口页 | Entry page | SD:466 |
| `visit:exit_page` | 退出页 | Exit page | `proposed` |
| `visit:source` | 来源 | Source | SD:245 "来源" |
| `visit:channel` | 渠道 | Channel | SD:245 "渠道分类" |
| `visit:referrer` | 引荐来源 | Referrer | SD:254 "引荐" |
| `visit:utm_source/medium/campaign/content/term` | UTM 来源 / 媒介 / 活动 / 内容 / 关键词 | UTM source / medium / campaign / content / term | `proposed` |
| `visit:country` / `region` / `city` | 国家 / 地区 / 城市 | Country / Region / City | SD:780 uses "国家、地区或城市" |
| `visit:device` | 设备类型 | Device type | SD:244 "设备类型" |
| `visit:browser` / `browser_version` | 浏览器 / 浏览器版本 | Browser / Browser version | SD:244 "浏览器" |
| `visit:os` / `os_version` | 操作系统 / 系统版本 | OS / OS version | SD:244 "系统" |
| `time`, `time:hour/day/week/month` | 时间; 按小时 / 按天 / 按周 / 按月 | Time; by hour / day / week / month | `proposed` |
| **Channel values** | | | |
| channel value set | unspecified | unspecified | Docs say only that channel is classified by versioned Rust rules (SD:253); no value list exists in the listed sources |
| **Device types** | | | |
| `desktop` | 桌面 | Desktop | CH1:37 value; term `proposed` |
| `mobile` | 手机 | Mobile | CH1:37; `proposed` |
| `tablet` | 平板 | Tablet | CH1:37; `proposed` |
| **Roles** | | | |
| `owner` | 所有者 | Owner | PG1:55 id; `proposed` |
| `admin` | 管理员 | Admin | PG1:55; `proposed` |
| `editor` | 编辑者 | Editor | PG1:55; `proposed` |
| `viewer` | 查看者 | Viewer | PG1:55; `proposed` |
| site guest editor / viewer | 站点访客（编辑者 / 查看者） | Site guest (editor / viewer) | PO:241 "站点访客" |
| `is_instance_admin` | 实例管理员 | Instance admin | PO:269 "实例管理员" |
| **Drop / reject reasons** | | | |
| `accepted` | 已接收 | Accepted | `proposed` |
| `dropped_unknown_site` | 未知站点 | Unknown site | `proposed` |
| `dropped_site_disabled` | 站点已停用 | Site disabled | `proposed` |
| `dropped_hostname_not_allowed` | 主机名不在白名单 | Hostname not allowed | `proposed` (白名单 PO:113) |
| `dropped_rate_limited` | 触发限流 | Rate limited | `proposed` |
| `dropped_spam_referrer` | 垃圾引荐 | Spam referrer | SD:240 "垃圾引荐" |
| `dropped_ip_blocked` | IP 已屏蔽 | IP blocked | `proposed` (屏蔽规则 PO:119) |
| `dropped_page_blocked` | 页面已屏蔽 | Page blocked | `proposed` |
| `dropped_bot` | 机器人 | Bot | SD:242 "机器人" |
| `dropped_datacenter_ip` | 数据中心 IP | Datacenter IP | `proposed` |
| `dropped_country_blocked` | 国家已屏蔽 | Country blocked | `proposed` |
| `dropped_no_session` | 无会话的参与度事件 | Engagement without session | SD:226 "没有会话的 engagement" (partial) |
| `dropped_overloaded` | 服务过载 | Overloaded | SD:229 "过载" |
| `dropped_site_deleted` | 站点已删除 | Site deleted | `proposed` |
| `dropped_salt_missing` | 缺少盐值 | Salt missing | `proposed` |
| `rejected_<field>` | 协议错误：字段 {field} | Rejected: field {field} | `proposed` |
| ingest diagnostics | 接入诊断 | Ingest diagnostics | SD:565 |
| **Site settings fields** | | | |
| `collect_enabled` | 启用采集 | Collection enabled | `proposed` |
| `ingest_rate_limit_per_min` | 每分钟接入上限 | Ingest limit per minute | `proposed` |
| `retention_days` | 数据保留天数 | Data retention (days) | SD:608 "保留期"; `proposed` label |
| `geo_level` | 地理精度 | Geo precision | SD:780 "地理精度" |
| `geo_level` values `country` / `region` / `city` | 国家 / 地区 / 城市 | Country / Region / City | SD:780 |
| `strip_query_params` | 去除查询参数 | Strip query parameters | `proposed` |
| `allowed_event_props` | 允许的事件属性 | Allowed event properties | SD:246 "props 白名单"; `proposed` |
| `week_starts_on` | 每周起始日 | Week starts on | SD:733 "周起始日" |
| `timezone` | 时区 | Timezone | SD:731 "站点时区" |
| `is_public` | 公开看板 | Public dashboard | SD:622 "公开看板" |
| hostnames | 主机名白名单 | Allowed hostnames | PO:113 "来源主机名白名单" |
| `site_key` | 站点标识 | Site key | SD:35 "站点标识" |
| **Date-range presets** | | | |
| `realtime` | 实时 | Realtime | `proposed` |
| `day` | 今天 | Today | `proposed` |
| `24h` | 过去 24 小时 | Last 24 hours | `proposed` |
| `7d` | 过去 7 天 | Last 7 days | SD:734 (含今天) |
| `30d` | 过去 30 天 | Last 30 days | `proposed` |
| `month` | 本月 | This month | `proposed` |
| `6mo` | 过去 6 个月 | Last 6 months | `proposed` |
| `12mo` | 过去 12 个月 | Last 12 months | `proposed` |
| `year` | 今年 | This year | `proposed` |
| `all` | 全部时间 | All time | `proposed` |
| `Nd` / `Nmo` / `[from,to]` | 过去 N 天 / N 个月 / 自定义范围 | Last N days / N months / Custom range | `proposed` |
| **Comparison** | | | |
| `previous_period` | 上一周期 | Previous period | `proposed` |
| `year_over_year` | 去年同期 | Year over year | SD:37 "同比" |
| `[from,to]` | 自定义对比范围 | Custom comparison | `proposed` |
| **Filter operators** | | | |
| `is` / `is_not` | 等于 / 不等于 | is / is not | `proposed` |
| `contains` / `contains_not` | 包含 / 不包含 | contains / does not contain | `proposed` |
| `matches` / `matches_not` | 匹配正则 / 不匹配正则 | matches regex / does not match | `proposed` |
| `matches_wildcard` / `_not` | 匹配通配符 / 不匹配通配符 | matches wildcard / does not match | `proposed` |
| `has_done` / `has_not_done` | 做过 / 没做过 | has done / has not done | `proposed` |
| **Objects / misc** | | | |
| organization | 组织 | Organization | PO:477 `organizations`; `proposed` |
| site | 站点 | Site | PG1:65; used throughout docs |
| segment | 分群 | Segment | SD:635 "分群" |
| goal | 目标 | Goal | SD:726 |
| funnel | 漏斗 | Funnel | SD:796 |
| shared link | 共享链接 | Shared link | PO:118 "分享链接", SD:622 "共享链接": variants, choose one |
| shield rule | 屏蔽规则 | Block rule | PO:119 |
| invitation | 邀请 | Invitation | PO:111 |
| API key | API 密钥 | API key | PO:120 |
| site deletion grace period | 删除撤销窗口 | Deletion grace period | PO:234 "宽限期", PO:223 "撤销窗口" |
| DB-IP attribution | unspecified | unspecified | Required text not given; CC BY 4.0 (SD:272) |
| sign-in email | 登录邮箱 | Sign-in email | `proposed` (#37, 2026-11-20) |
| email subscriptions (account card) | 邮件订阅 | Email subscriptions | `proposed` (#38, 2026-11-20) |
| weekly / monthly report | 周报 / 月报 | Weekly report / Monthly report | `proposed` (#38, 2026-11-20) |
| traffic spike alert | 流量突增通知 | Traffic spike alert | `proposed` (#39, 2026-11-20) |
| spike threshold | 提醒人数 | Alert threshold | `proposed` (#39, 2026-11-20) |

## Known unknowns

- `site_key` format conflict: PG1:68 / PO:157 allow exactly `[a-z2-7]{12}`; TP:244 allows `[A-Za-z0-9_-]{8,32}` and shows 16-char mixed-case examples. A generated key shown in the install snippet may not satisfy the other rule.
- Hostname allowlist empty: PG1:82 and PO:174 say "no validation", TP:327 and TP:389 say fail-closed (all dropped). Affects new-site "no data" state.
- Retention default (decision 731) vs ClickHouse TTL `400 DAY` currently in CH1:55 / CH2:45. Retention override per site and its relationship to table TTL (SD:608) need a UI statement.
- Visit session cap 7 days vs docs 24 h; the query look-back `start_date >= from_date - 1` (SD:708) and the "active in range" definition assume 24 h. Impact on `visits` unspecified.
- Drop reason identifiers differ between SD:240-248 and TP:309-318; whether the stored `metric` literally is `dropped_site_disabled` vs `dropped_disabled` is unspecified.
- `x-rivu-dropped` count semantics beyond `1` (batch endpoint) and how `_unknown_` / `rejected_*` fit a `site_id UInt64` column: unspecified.
- No HTTP paths, payloads or error messages specified for auth, setup, sites, settings, invitations, shared links, goals, segments, shield, API keys, ingest-diagnostics read.
- `engaged_duration` definition and its metric table membership; `time:minute` support; auto time-granularity rules; refresh interval for realtime; UI behavior for empty results.
- Channel value set and their zh/en names (only the classifier's existence is documented); referrer-source naming.
- Timeout conflict: dashboard 10 s / API 20 s / shared 5 s (SD:743) vs `RIVU_QUERY_TIMEOUT_SECS` 15 / `RIVU_PUBLIC_QUERY_TIMEOUT_SECS` 8 (PO:290, PO:366).
- GeoIP: where a custom `.mmdb` URL is configured (env or UI), attribution text, and what is shown when no GeoIP database is loaded.
- Invitation expiry duration, org-level password reset and email verification flows (`auth_tokens`, PO:122), pagination of sites list, site-name length limit, whether a viewer-readable diagnostics view is permitted.
- Dashboard accessibility, keyboard, localization framework, supported browsers, theme.
