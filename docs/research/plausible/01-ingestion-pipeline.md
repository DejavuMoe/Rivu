# Plausible 接入前半段：从浏览器到会话层交接点

> 范围：tracker、脚本分发、`POST /api/event`、Request 解析、Event 管线、站点准入/限流、过滤、富化、身份、计数器。
> 不含：会话缓存（`Plausible.Session.*`）、写缓冲、ClickHouse 表结构（仅写交接点，见 1.3）。
> 源码根：`examples/plausible-analytics/analytics-master`，下文路径均相对它。Plausible 为 AGPL-3.0，本文只抽象设计，不复制代码。`extra/` 目录是 EE 代码（带 EE 许可证），Rivu 不应参考其实现，只记录其行为契约。
> 记号：**事实**=代码中可直接看到，附 `路径:行号`；**推断**=由代码或第三方库默认行为推出，未在本仓库内验证。

## 0. 先看结论（对设计影响最大）

| # | 结论 | 出处 |
|---|---|---|
| 1 | 所有被丢弃的事件（机器人、垃圾引荐、未知站点、限流、shield）都回 `202 ok`，只靠 `x-plausible-dropped: N` 头区分；只有请求体本身不合法才回 `400`。 | `lib/plausible_web/controllers/api/external_controller.ex:13-45` |
| 2 | 浏览器端只用 `fetch(keepalive:true)` + `Content-Type: text/plain`，没有 `sendBeacon`，没有重试。`text/plain` 避开 CORS 预检，服务端因此必须自己读原始 body 再解 JSON。 | `tracker/src/networking.js:22-41`；`lib/plausible/ingestion/request.ex:180-195` |
| 3 | 时间戳完全由服务端在 `Request.build` 时取 `utc_now` 并截断到秒，客户端不能传时间。 | `lib/plausible/ingestion/request.ex:79-86` |
| 4 | `user_id` = SipHash(salt, UA + IP + domain + 根域名)，salt 全局共享、每天 UTC 00:00 轮换；会话层同时用当前和上一个 salt 的 id 查会话，实现跨午夜续会话。IP 不落库。 | `lib/plausible/ingestion/event.ex:566-590`、`:427-446`；`lib/plausible/session/salts.ex` |
| 5 | 客户端 IP 的几个头（`x-plausible-ip`、`cf-connecting-ip`、`x-forwarded-for` 等）无条件信任，没有可信代理名单。数据中心 IP 判定不在本进程，靠上游注入 `x-plausible-ip-type` 头。 | `lib/plausible_web/remote_ip.ex:6-36`；`lib/plausible/ingestion/request.ex:389-396` |
| 6 | acquisition channel 是 ClickHouse 的 MATERIALIZED 列 + 字典；Elixir 的 `get_channel/5` 仅测试对照，且分支顺序与 SQL 不一致。 | `lib/plausible/data_migration/acquisition_channel.ex:40-57`；`priv/data_migrations/AcquisitionChannel/sql/acquisition_channel_functions.sql.eex:212-235` |
| 7 | `pa-xxx` 脚本只内联 5 个配置键；`hash_based_routing` 等其它开关入库但不进脚本。 | `lib/plausible_web/tracker.ex:37-47` |

## 1. 端到端时序

### 1.1 有序步骤

| 步 | 位置 | 动作 | 失败/丢弃时 |
|---|---|---|---|
| 1 | 浏览器 | 页面加载 snippet：`<script async src=".../js/pa-<id>.js">` + 内联 `plausible.init()` 桩（把调用排队到 `plausible.q`，把选项存到 `plausible.o`）。 | 脚本 404 则无采集 |
| 2 | 服务端 `TrackerPlug` | 命中 `/js/pa-<id>.js`，从缓存取已内联站点配置的脚本，`max-age=60`。 | 未知 id：`404 Not found` |
| 3 | 浏览器 | 脚本执行 `init(plausible.o)`：合并选项、回放 `plausible.q`、注册 SPA/engagement/点击监听、发首个 `pageview`。 | 已初始化则跳过 |
| 4 | 浏览器 `track()` | 客户端排除：localhost、自动化、`localStorage.plausible_ignore`、`transformRequest` 返回假值。 | 静默忽略（可选 console.warn） |
| 5 | 浏览器 | `fetch POST {endpoint}`，`text/plain`，body 是 JSON。 | 无重试 |
| 6 | Endpoint 插件链 | `TrackerPlug`→`Plug.Parsers`（`text/plain` 原样放行）→`CORSPlug`→Router `:external_api`（`accepts json`）。 | |
| 7 | `ExternalController.event` → `Request.build` | 解析 body、取服务端时间戳、解出 IP/UA/URL/hostname/pathname/referrer/props/engagement/query/revenue/domains。 | JSON 非法/校验失败：`400 {errors}` |
| 8 | `Event.build_and_buffer` | 先做**垃圾引荐**检查（对所有域名统一丢弃）。 | `:spam_referrer` |
| 9 | 对 `d` 里的每个域名 | `GateKeeper.check`：站点缓存→是否 regular→`accept_traffic_until`→限流。 | `:not_found/:block/:throttle/:payment_required` |
| 10 | 每个域名的事件 | 跑 18 步管线（见第 5 节）：过滤→富化→salt→user_id→校验。 | 各阶段 drop reason |
| 11 | `register_session`（**交接点**） | `Persistor.persist_event(event, previous_user_id, opts)`：会话层找/建会话，事件合并会话属性后进事件写缓冲。 | `lock_timeout/no_session_for_engagement/persist_*` |
| 12 | 遥测与响应 | 成功发 `buffered`、丢弃发 `dropped(reason)`，由 `Counters` 聚合，每 10 秒落 ClickHouse；响应 `202 "ok"`，有丢弃则加 `x-plausible-dropped`。 | |

### 1.2 一个请求产生 N 个事件

`d` 可为逗号分隔的多域名，每个域名独立走 GateKeeper 和整条管线，共享同一 `timestamp`（`request.ex:238-263`；`event.ex:68-82`；测试 `external_controller_test.exs:71-94`）。`domain` 参与 user_id 哈希，同一访客在不同域名下 user_id 不同。

### 1.3 交接点（给会话/写缓冲同学）

| 项 | 内容 | 出处 |
|---|---|---|
| 调用 | `Persistor.persist_event(event, previous_user_id, opts)` | `lib/plausible/ingestion/event.ex:427-446` |
| 输入 `event.clickhouse_event` | 已通过 `ClickhouseEventV2.new` 校验的事件行：`name, site_id, hostname, pathname, user_id, timestamp, meta.key, meta.value, scroll_depth, engagement_time, revenue_*, interactive?` | `lib/plausible/clickhouse_event_v2.ex:61-84` |
| 输入 `event.clickhouse_session_attrs` | 会话级属性：`referrer, referrer_source, click_id_param, utm_*, country_code, subdivision1_code, subdivision2_code, city_geoname_id, screen_size, operating_system(_version), browser(_version)` | `lib/plausible/ingestion/event.ex:272-278, 305-322, 337-346` |
| 输入 `previous_user_id` | 用**上一个 salt** 算出的 user_id，可能为 `nil`（无上一个 salt）。 | `event.ex:430-436`、`:566-571` |
| 会话层契约 | 先按 `{site_id, user_id}` 找会话，找不到再按 `previous_user_id` 找；`engagement` 事件找不到会话则返回 `:no_session_for_engagement`，不建会话。 | `lib/plausible/session/cache_store.ex:29-31, 49-58` |
| 输出 | `{:ok, ingest_event}`（`clickhouse_event` 已合并会话字段 `session_id` 等）或 `{:error, reason}`，`reason ∈ no_session_for_engagement, lock_timeout, persist_timeout, persist_error, persist_decode_error`。 | `lib/plausible/ingestion/persistor/embedded.ex:26-37`；`remote.ex:25-50` |
| 合并字段 | 事件行从会话拷回 `session_id,user_id,referrer…browser_version`（反范式）。 | `clickhouse_event_v2.ex:86-110` |
| 后端选择 | `embedded`（默认）/`embedded_with_relay`/`remote`；灰度按 `phash2(user_id,100)+1 <= percent` 保证同一访客稳定落在同一后端。 | `lib/plausible/ingestion/persistor.ex:15-36`；`config/runtime.exs:148-158` |
| 远程协议 | `POST` 到 persistor，请求头 `x-site-id/x-current-user-id/x-previous-user-id`，body 为 Erlang term 序列化后 base64。**Rivu 不要照搬序列化**（见第 11 节）。 | `persistor/remote.ex:19-23, 119-144` |

## 2. Tracker 协议

### 2.1 构建变体

一份源码按编译期常量 `COMPILE_*` 裁剪出多个产物（`tracker/ARCHITECTURE.md:20-47`；`tracker/compiler/index.js:16-31`）。

| 产物 | 特点 | 出处 |
|---|---|---|
| `plausible-web.js`（新式 `pa-xxx` 脚本的基础） | 配置在运行时由 `init(options)` 与服务端内联对象合并；开启 CONFIG/HASH/LOCAL/MANUAL/OUTBOUND/FILE/CUSTOM_PROPERTIES/TAGGED/REVENUE；**没有** EXCLUSIONS、PAGEVIEW_PROPS、COMPAT（不含 XHR 兜底） | `tracker/compiler/variants.json:3-17` |
| `npm_package/plausible.js` | 同上，但 `init` 只能调一次、必须带 `domain`，默认 endpoint `https://plausible.io/api/event` | `tracker/src/config.js:46-57` |
| 旧式 `plausible.<变体>.js` | 靠 `data-domain`、`data-api`、`data-include/exclude`、`event-*`、`file-types` 属性配置；数量是变体幂集（上千个） | `tracker/src/config.js:58-63`；`lib/plausible_web/plugs/tracker_plug.ex:10-35` |

当前脚本版本号 `tracker_script_version = 36`（`tracker/package.json:2`），随每个事件以 `v` 字段上报。

### 2.2 上报字段（POST body，JSON）

服务端同时接受短键和长键（`request.ex` 里 `request_body["n"] || request_body["name"]` 这种写法）。

| 短键 | 长键 | 类型 | 含义 | 取值/限制 | 出处 |
|---|---|---|---|---|---|
| `n` | `name` | string 或 integer | 事件名 | 必填；空串视为缺失；整数会转成字符串（为了 404 跟踪）；长度 ≤120 | `request.ex:1-16, 197-206, 114`；测试 `external_controller_test.exs:1262-1279` |
| `u` | `url` | string | 页面完整 URL（含 query、hash） | 必填；≤2000 **字节**；scheme 不能是 `data`；不满足回 400 | `request.ex:29, 265-276` |
| `d` | `domain` | string | 站点域名，可逗号分隔多个 | 必填（缺省时回退为 URL 的 host，仍为空则 400） | `request.ex:238-263` |
| `r` | `referrer` | string/null | `document.referrer` | 非字符串忽略；截断到前 2000 个字符 | `request.ex:208-217` |
| `h` | `hashMode` | integer | 是否哈希路由模式 | 仅 `1` 时 pathname 追加 `#fragment` | `request.ex:197-206, 407-420` |
| `p` | `props`/`m`/`meta` | object 或 JSON 字符串 | 自定义属性 | 见 4.2 | `request.ex:288-301` |
| `$` | `revenue` | `{amount,currency}` | 收入（EE 才解析） | 见 2.7 | `extra/lib/plausible/ingestion/request/revenue.ex:6-25` |
| `i` | `interactive` | boolean | 是否计入跳出率 | 只有 `false` 生效；系统事件强制 `true` | `request.ex:303-314`；`lib/plausible/event/system_events.ex:37` |
| `v` | — | integer | 脚本版本 | 非整数则记 0；只用于计数器标签 | `request.ex:369-377` |
| `sd` | — | integer/字符串 | 滚动深度百分比（仅 engagement） | 见 2.4 | `request.ex:346-367, 439-448` |
| `e` | — | integer/字符串 | 本次增量参与时长，毫秒（仅 engagement） | 见 2.4 | `request.ex:450-461` |

客户端只在对应开关为真时才带 `p/i/$/h`（`tracker/src/track.js:86-146`）。旧版变体还可能带 `m`（JSON 字符串化的 meta，`track.js:100-102`）。

### 2.3 事件类型

| 事件名 | 谁发 | 说明 | 出处 |
|---|---|---|---|
| `pageview` | 自动捕获或手动 `plausible('pageview')` | 系统事件，`interactive` 恒为 true | `tracker/src/autocapture.js:6-13`；`system_events.ex:15` |
| `engagement` | tracker 在离开/隐藏/下一次 SPA 跳转时 | 携带 `sd`、`e`、前一页 URL 与 props；不建会话 | `tracker/src/engagement.js:48-87` |
| `Outbound Link: Click` | 外链点击 | `props.url` | `custom-events.js:92-99` |
| `File Download` | 下载链接点击 | `props.url`（去掉 query） | `custom-events.js:101-108` |
| `Form: Submission` | 表单提交 | 仅 `formSubmissions` 开启；`props.path` 由服务端补 | `custom-events.js:263-275`；`system_events.ex:45-49` |
| `404` | 用户手动上报 | `props.path` 由服务端补成 pathname | `system_events.ex:45-49`；`request.ex:226-236` |
| 其它任意名称 | 自定义/tagged | 可 `interactive:false` | `request.ex:303-314` |

### 2.4 engagement 触发与字段

| 项 | 行为 | 出处 |
|---|---|---|
| 状态 | 记录当前页 URL、props、最大滚动像素、可见开始时间、累计可见毫秒。 | `engagement.js:9-21` |
| 滚动深度 | `sd = round(maxScrollDepthPx / documentHeight * 100)`；文档高度取 body/documentElement 的 scroll/offset/client 高度最大值；视口比文档高时按文档高度算（即 100）。 | `engagement.js:67-70, 126-158` |
| 高度变化 | 非兼容版用 `ResizeObserver` 监听 `documentElement`；兼容版 load 后每 200ms 刷新 15 次。 | `engagement.js:164-177` |
| 参与时长 | 只在页面 `visible` 且有焦点时累计；`visibilitychange`/`blur`/`focus` 驱动。 | `engagement.js:89-103, 115-121` |
| 触发时机 | (a) 页面变 hidden 或失焦；(b) SPA 里下一次 `pageview` 之前（作为“离开上一页”）。 | `engagement.js:23-32, 89-103` |
| 发送条件 | 当前页未被忽略，且（最大滚动深度比上次发送时更大，或自上次发送起参与时长 ≥3000ms）。首个 engagement 必发，因为初始已发送深度是 -1。 | `engagement.js:60-64` |
| 发送后 | `e` 清零（所以 `e` 是**增量**不是累计），重新开始计时。 | `engagement.js:77-79` |
| 字段 | `n:"engagement", sd, d, u(当前页URL), p(当页props), e, v, h?` | `engagement.js:67-75, 81-83` |
| 忽略 | 若该页 pageview 被 `onIgnoredEvent` 忽略，则对应 engagement 也不发。 | `engagement.js:44-46`；`track.js:173-175` |

### 2.5 发送方式取舍

| 方式 | 是否使用 | 说明 | 出处 |
|---|---|---|---|
| `fetch(..., {keepalive:true})` | 是（默认） | `POST`、`Content-Type: text/plain`、无 credentials 设置；`window.fetch` 不存在则**什么也不发**。 | `networking.js:22-41` |
| `XMLHttpRequest` | 仅兼容(COMPAT)旧变体 | 同样 `text/plain`。 | `networking.js:2-21` |
| `navigator.sendBeacon` | 否 | 全 tracker 源码无 `sendBeacon`、`beforeunload`、`pagehide`（grep 无结果）。 | `tracker/src/*` |
| 回调 | `options.callback({status})` 或 `{error}`；被忽略时调用且无参。 | `networking.js:32-38`；`track.js:171` |
| 兼容版导航延迟 | 外链/下载/带标签表单会 `preventDefault`，等请求完成或 5 秒后再跳转，防止导航掐断请求。非兼容版靠 `keepalive` 不拦截。 | `custom-events.js:111-146, 286-305` |

选 `text/plain` 的动机（推断）：它属于 CORS 简单请求，跨域上报不触发预检；服务端对应的写法是 `Plug.Parsers` 对未知类型放行（`endpoint.ex:76-80`）后自行读 body（`request.ex:180-195`）。

### 2.6 SPA 路由与自动捕获

| 项 | 行为 | 出处 |
|---|---|---|
| 初次 pageview | 页面可见时立即发；`hidden/prerender` 时等到首次变可见才发。 | `autocapture.js:33-46` |
| History 模式 | 重写 `history.pushState` + 监听 `popstate`。**不处理 `replaceState`**。 | `autocapture.js:21-30` |
| 去重 | 非 hash 模式下，SPA 导航若 `location.pathname` 与上次相同则不发（仅 query 变化不算新 pageview）。 | `autocapture.js:7-9` |
| Hash 模式 | 监听 `hashchange`，不做 pathname 去重，pageview 带 `h:1`。 | `autocapture.js:19-20`；`track.js:144-146` |
| bfcache | `pageshow` 且 `persisted` 时重发 pageview。 | `autocapture.js:48-53` |
| 手动模式 | `autoCapturePageviews:false` 不安装上述监听；web 变体允许 `options.url` 覆盖 `u`。 | `plausible.js:21-23`；`track.js:90-96` |
| 初始化幂等 | `window.plausible.l` 已设则跳过；`plausible.q` 里排队的调用在 init 后回放。 | `plausible.js:10-15, 30-45` |

### 2.7 排除逻辑

| 排除 | 条件 | 出处 |
|---|---|---|
| localhost | `hostname` 匹配 `localhost`、`127.x`、`[::1]`/`[::]`，或 `file:` 协议；`captureOnLocalhost` 为真则不排除 | `track.js:22-30` |
| 自动化 | `window._phantom`、`__nightmare`、`navigator.webdriver`、`window.Cypress` 任一为真，且没有 `window.__plausible` 覆盖标记 | `track.js:31-39` |
| 本地开关 | `localStorage.plausible_ignore === 'true'`（读取异常忽略） | `track.js:41-47` |
| 包含/排除路径 | 仅旧变体：`data-include`/`data-exclude`，`*` 匹配单段、`**` 匹配多段，hash 模式下路径含 hash | `track.js:49-84` |
| `transformRequest` | 返回假值则丢弃，可改写 payload（如脱敏 URL） | `track.js:148-157`；`plausible.d.ts:49-58` |
| 忽略时 | 调 `options.callback()`；若是 pageview 则标记 engagement 忽略 | `track.js:166-176` |

### 2.8 自动捕获（外链、下载、表单、tagged）

| 能力 | 开关 | 逻辑 | 出处 |
|---|---|---|---|
| 外链 | `config.outboundLinks` | `link.host !== location.host`；事件 `Outbound Link: Click`，`props.url=link.href`；监听 `click` 与 `auxclick`（仅中键） | `custom-events.js:75-99, 148-155, 237-238` |
| 文件下载 | `config.fileDownloads`（布尔或 `{fileExtensions}`） | 去掉 `?` 后按最后一个 `.` 的扩展名匹配默认 26 种（pdf、xlsx、docx、zip、mp4、dmg…）；`props.url` 为去 query 的地址 | `custom-events.js:6-33, 101-108, 157-166, 240-261` |
| 表单 | `config.formSubmissions` | `submit`（捕获阶段），要求 `novalidate` 或 `checkValidity()` 通过；被 tagged 的表单不重复发 | `custom-events.js:263-275` |
| tagged events | web 变体恒开 | 元素或最多向上 3 层父元素带 class `plausible-event-name=X`（或 `--`）；其它 `plausible-event-<k>=<v>` 变成 props，值里 `+` 换成空格；表单内点击不算，只算 submit；若是链接自动补 `props.url`；点击已 tagged 的链接时，通用外链/下载逻辑直接返回避免重复 | `custom-events.js:36, 84-90, 168-234, 277-371` |

### 2.9 自定义属性与收入

| 项 | 行为 | 出处 |
|---|---|---|
| 调用 | `plausible('Signup', {props:{plan:'pro'}, revenue:{amount:10, currency:'USD'}})` | `plausible.d.ts:67-101` |
| 全局属性 | `customProperties` 为对象或 `(eventName)=>object`，与单次 props 合并（单次优先） | `track.js:131-142` |
| 页面 props 属性 | 旧变体读取 `event-*` 属性，同名以事件 props 优先 | `track.js:115-129` |
| 类型 | props 键值在客户端类型上都是 string；服务端会把非字符串标量转成字符串 | `plausible.d.ts:103`；`request.ex:316-322` |
| 收入（EE） | 要 `{amount, currency}`，`currency` 必须在合法币种表，`amount` 可为数字或可解析字符串；非法就忽略收入但不拒绝事件；仅当站点存在同名事件的 revenue goal 才落收入字段，币种不同则转换成 goal 币种 | `extra/.../request/revenue.ex:6-34`；`extra/.../event/revenue.ex:6-37`；测试 `external_controller_test.exs:807-887` |
| CE 行为与依赖 | CE 中收入解析是 no-op；EE 收入依赖 `Site.Cache` 预加载的 `revenue_goals`，所以站点缓存里不只是站点行 | `request.ex:134-136`；`event.ex:387-394`；`site/cache.ex:48-59` |

### 2.10 站点级脚本配置（`pa-xxx`）

| 项 | 内容 | 出处 |
|---|---|---|
| 配置表 | `tracker_script_configuration`：`id`(`pa-`+nanoid)、`installation_type`(manual/wordpress/gtm/npm)、`track_404_pages`、`hash_based_routing`、`outbound_links`、`file_downloads`、`revenue_tracking`、`tagged_events`、`form_submissions`、`pageview_props`，全部默认 false，属于某个 site | `lib/plausible/site/tracker_script_configuration.ex:23-39`；`lib/plausible/ecto/types/nanoid.ex:37-51` |
| 脚本 id 与域名解耦 | URL 里是随机 id 而非域名；换域名后脚本 URL 不变 | `nanoid.ex:50`；`tracker_plug.ex:43-49` |
| 内联内容 | 把服务端模板里的占位串 `"<%= @config_js %>"` 替换为对象字面量，仅含：`domain`、`endpoint`(`{base_url}/api/event`)、`outboundLinks`、`fileDownloads`、`formSubmissions`；字符串 JSON 编码，`true` 写成 `!0`，`false` 省略 | `tracker.ex:37-47, 49-67`；`tracker/src/config.js:41` |
| 配置合并 | `Object.assign(config, options, {domain: config.domain})`：init 选项可覆盖其它键，**不能覆盖 domain**（除非用 `transformRequest` 改 payload） | `config.js:38-45` |
| init 选项 | `autoCapturePageviews, logging, lib, hashBasedRouting, captureOnLocalhost, customProperties, transformRequest, outboundLinks, fileDownloads, formSubmissions, endpoint` | `config.js:21-36`；`plausible.d.ts:7-65` |
| 目标同步 | 更新配置时按 404/外链/下载/表单的开关变化自动创建或删除同名 goal | `tracker.ex:213-230` |
| 观察 | 事实：`hash_based_routing`、`revenue_tracking`、`tagged_events`、`pageview_props`、`track_404_pages` 存库并经 Plugins API 暴露，但**不在** `plausible_main_config` 里；web 变体的 tagged/revenue 编译期恒开，hash 路由靠 `init({hashBasedRouting:true})`。推断：库里的 hash 开关是否影响已安装脚本不确定，Rivu 做站点级开关应把全部开关内联进脚本。 | `tracker.ex:37-47`；`variants.json:3-17` |

## 3. HTTP 端点契约

### 3.1 路由与中间件

| 项 | 内容 | 出处 |
|---|---|---|
| 采集 | `POST /api/event` → `Api.ExternalController.event` | `lib/plausible_web/router.ex:409-413` |
| 管道 | `:external_api` = `plug :accepts, ["json"]` | `router.ex:61-63` |
| 脚本错误 | `GET /api/error` → 记 Sentry，回 `200` 空 | `external_controller.ex:47-50`；`router.ex:414` |
| 脚本 | `GET /js/pa-<id>.js`、`/js/p.js`、`/js/plausible[.变体].js`（`script`/`analytics` 为别名前缀），由 Endpoint 最前面的 `TrackerPlug` 处理 | `endpoint.ex:39`；`tracker_plug.ex:41-60` |
| 解析器 | `Plug.Parsers [:urlencoded,:multipart,:json]`，`pass: ["*/*"]`（`text/plain` 不解析，原样放行） | `endpoint.ex:76-80` |
| CORS | `CORSPlug.call(conn, CORSPlug.init([]))`，除 `/login/oauth/authorize` 外对所有路由生效，即用库默认值（推断：`Allow-Origin: *`、允许 POST/OPTIONS，具体头列表本仓库无源码可核对；Rivu 应自行明确） | `endpoint.ex:89, 98-107` |
| 服务器限制 | HTTP 请求行/头值各 ≤8192 字节 | `config/runtime.exs:392-395` |

### 3.2 请求与响应

| 项 | 内容 | 出处 |
|---|---|---|
| Content-Type | 接受 `text/plain`（tracker 用）与 `application/json`；其它类型因 `pass */*` 也会进入控制器。`text/plain` 且 body 未被解析时由 `Request.parse_body` 读原始 body | `request.ex:180-195`；测试 `external_controller_test.exs:40-60` |
| Body 上限 | 默认 1,000,000 字节（`conn.assigns[:read_body_limit]` 可覆盖，库内未发现设置处）；超限因读取返回非 `{:ok,...}` 被当 `invalid_json` → 400 | `request.ex:183-190`；测试 `request_test.exs:539-562` |
| Body 必须是 JSON 对象 | `"<script>"` 之类合法 JSON 但非对象 → 400 `{"errors":{"request":["Unable to parse request body as json"]}}` | `request.ex:125-127`；`request_test.exs:533-537` |
| 成功 | `202` + 文本 `ok` | `external_controller.ex:17-21` |
| 部分/全部丢弃 | `202` + 文本 `ok` + `x-plausible-dropped: <丢弃数>` | `external_controller.ex:22-37` |
| 请求错误 | `400` + `{"errors":{字段:[消息]}}`，例：缺 url → `{"url":["is required"]}`，缺域名 → `{"domain":["can't be blank"]}` | `external_controller.ex:40-43`；测试 `external_controller_test.exs:1244-1316` |
| 未知站点/限流/垃圾引荐/机器人 | 一律 `202`（测试里被限流站点依然 `202 "ok"` 且无事件） | `external_controller_test.exs:302-320` |

### 3.3 为什么失败也返回 2xx（推断）

客户端“发了就算”、无重试（`networking.js`），非 2xx 只会污染站点控制台；不向机器人/攻击者泄露“被识别/站点不存在”；一个请求可扇出多站点而状态码只有一个，所以用计数头表达部分丢弃；只有协议级错误（缺字段、坏 JSON）才值得调用方修复，故 400。

### 3.4 死分支（事实+推断）

`find_first_invalid_changeset` 只匹配 `{:error, %Ecto.Changeset{}}` 形式的 `drop_reason`（`external_controller.ex:52-59`），而管线所有 `drop/3` 的 reason 都是原子（`event.ex:185-193`，全库仅此两处用到 `drop_reason`）。推断：该分支不会触发，实际 400 全部来自 `Request.build`。

### 3.5 脚本响应头

`application/javascript`、`nosniff`、`cross-origin-resource-policy: cross-origin`、`access-control-allow-origin: *`、`cache-control: public, max-age=60, no-transform`、`cdn-tag: tracker_script::<id>`（CDN 按 id 清缓存）；旧式静态文件 `max-age=86400, must-revalidate`（`tracker_plug.ex:64-119`）。

## 4. Request 解析与校验

### 4.1 字段与限制总表

| 字段 | 规则 | 出错表现 | 出处 |
|---|---|---|---|
| 时间戳 | 服务端 `NaiveDateTime.utc_now()` 截断到秒；整请求共享；EE 重放头 `x-replay-time` 可覆盖，但必须 ≤ 现在 | — | `request.ex:79-86, 142-168` |
| 事件名 | `n`/`name`，字符串或整数；必填；≤120 | 400 | `request.ex:197-206, 114` |
| URL | `u`/`url`；必填；`byte_size ≤ 2000`；`URI.parse` 后 scheme 不能是 `data` | 400 `is required`/`must be a valid url`/`scheme is not allowed` | `request.ex:29, 265-276` |
| hostname | URL 的 host，去首尾空白，只去掉**小写**前缀 `www.`；host 缺失则为字面量 `"(none)"` | — | `request.ex:278-286, 425-436` |
| domain(s) | `d`/`domain`，trim；按 `,` 拆分；每个同样 `sanitize_hostname`；空串 400；缺省回退 URL host | 400 `domain can't be blank` | `request.ex:238-263` |
| pathname | `uri.path`（空→`/`）经 `URI.decode`，去尾部空白；保留尾部 `/`，不改大小写，不含 query | — | `request.ex:219-224, 407-420`；测试 `external_controller_test.exs:155-183` |
| hash 模式 | `h` 为整数 1 且 URL 有 fragment 时：`pathname <> "#" <> URI.decode(fragment)` | `h` 非整数→cast 错误 400 | `request.ex:415-419` |
| query 参数 | `URI.decode_query(uri.query)` 得到字符串映射；无 query 则不设 | — | `request.ex:379-387` |
| referrer | `r`/`referrer`，非字符串忽略，截取前 2000 个字符（按字形，不是字节） | — | `request.ex:208-217` |
| props | 见 4.2 | 400 键/值超长 | `request.ex:288-344` |
| engagement | 见 4.3 | 400 | `request.ex:346-367` |
| interactive | `i`/`interactive` 为 `false` 且不是系统事件才为 false | — | `request.ex:303-314` |
| 脚本版本 / IP / UA / IP 分类 | `v` 整数否则 0；IP 见 4.4；`user-agent` 头第一个值（可 nil）；`x-plausible-ip-type` 头原值 | — | `request.ex:369-377, 176-178, 398-405, 389-396` |

### 4.2 自定义属性处理顺序

| 步 | 动作 | 出处 |
|---|---|---|
| 1 | 取 `m`/`meta`/`p`/`props` 之一（按此优先级），可以是对象或 JSON 字符串；其它类型→空映射 | `request.ex:291`；`lib/plausible/helpers/json.ex:6-14` |
| 2 | 过滤：键或值是数组/对象的丢弃；键或值去前导空白后为空的丢弃；其余 `to_string` | `request.ex:316-322` |
| 3 | 对 `404`、`WP Form Completions`、`Form: Submission`，若未带 `path` 属性，把 `path = pathname` 插到列表**头部**，避免被截断 | `request.ex:226-236`；`system_events.ex:45-49, 96-100` |
| 4 | `Enum.take(30)` 再转映射 | `request.ex:288, 295-296` |
| 5 | 任一键 >300 字节或值 >2000 字节 → 整个请求 400（不是仅丢该属性） | `request.ex:324-344`；`lib/plausible/props.ex:13-17` |

注意：
- 上限 30 是**每事件属性数**（`@max_props 30`）；`Plausible.Props.max_props = 300` 是站点允许的属性名总数（`props.ex:10`），两者不同。
- 步骤 2 用 `Enum.reduce([], ...)` 做头插，会反转遍历顺序，再取前 30，所以超过 30 个时保留哪些属性是**不确定的（推断）**，测试只断言个数为 30（`request_test.exs:524-530`）。
- 长度限制单位是字节，而 goal 校验用字符数（`goal.ex:243-253`），Rust 里要统一口径。

### 4.3 engagement 字段规范

| 项 | 规则 | 出处 |
|---|---|---|
| 缺失哨兵 | `sd` 缺失/非法/负数 → `255`；`e` 缺失/非法/负数/过大 → `0` | `request.ex:30-31, 439-461` |
| 两个都缺失 | 400：`engagement event requires a valid integer value for at least one of 'sd' or 'e' fields` | `request.ex:351-358`；测试 `external_controller_test.exs:1325-1335` |
| `sd` | 字符串必须整串是整数；`>100` 夹到 100 | `request.ex:439-448`；测试 `:1415-1447` |
| `e` | 字符串必须整串是整数；`< 30 天的毫秒数`，否则当 0（兼容旧脚本曾上报巨大值） | `request.ex:33-35, 450-461` |
| 非 engagement 事件 | 忽略 `sd`、`e` | `request.ex:347`；测试 `:1374-1393` |

### 4.4 客户端 IP 解析（优先级）

| 序 | 来源 | 处理 | 出处 |
|---|---|---|---|
| 1 | `x-plausible-ip` | 去端口、去 `[]` | `remote_ip.ex:14-15, 38-51` |
| 2 | `cf-connecting-ip` | 同上 | `:17-18` |
| 3 | `b-forwarded-for`（BunnyCDN） | 取逗号分隔的第一个 | `:20-21, 53-58` |
| 4 | `x-forwarded-for` | 取第一个 | `:23-24` |
| 5 | `forwarded` | 正则取 `for=`，去引号 | `:26-31` |
| 6 | 连接对端 | `:inet.ntoa` | `:33-34` |

事实：没有可信代理判断，任何客户端都可伪造前 5 项（`remote_ip.ex` 全文无信任校验）。推断：`forwarded` 头存在但不含 `for=` 时，`Regex.named_captures` 返回 `nil`，随后 `Map.get(nil, ...)` 会崩溃（`remote_ip.ex:27-28`），Rust 里要显式处理。IP 以原始字符串形式进入哈希和 shield 匹配，未规范化（IPv6 大小写、压缩写法）。

### 4.5 来源相关 query 参数提取

| 目标 | 规则 | 出处 |
|---|---|---|
| `utm_source`（存储用） | `utm_source` → `source` → `ref`，用 Elixir 的 `||`（只有 `nil` 才继续回退） | `event.ex:308-311` |
| `utm_medium/campaign/content/term` | 同名参数原值 | `event.ex:318-321` |
| click id | 按顺序取第一个存在的键名：`gclid, gbraid, wbraid, msclkid, fbclid, twclid`，存的是**参数名**不是值 | `event.ex:448-455` |
| 推断 medium | `utm_medium` 为 nil 且 source=`Google` 且 click id=`gclid` → `"(gclid)"`；source=`Bing` 且 `msclkid` → `"(msclkid)"`；已有字符串 medium 保持 | `event.ex:325-335`；测试 `external_controller_test.exs:1562, 1595` |
| 空参数陷阱（推断） | `?utm_source=` 得到空字符串，在 Elixir 里是真值，会压过 `ref` 与 referrer；Rust 里必须决定空串是否视为缺失 | `event.ex:308-311`；`source.ex:119-123` |

## 5. Event 管线阶段

### 5.1 预阶段（`build_and_buffer`）

| 阶段 | 作用 | drop reason | 数据源 | 出处 |
|---|---|---|---|---|
| spam referrer | 解析 `referrer` 的 host，去 www，查垃圾引荐库；命中则对 **所有** 域名丢弃，不做站点检查 | `:spam_referrer` | 外部依赖 `referrer_blocklist`（git: plausible/referrer-blocklist，规则不在本仓库） | `event.ex:64-67, 604-608`；`mix.exs:146` |
| GateKeeper | 站点缓存 + 状态 + 限流 | `:not_found :block :throttle :payment_required` | `Site.Cache`、`RateLimit` | `event.ex:68-82`；第 6 节 |

### 5.2 18 步管线（`process_unless_dropped`，遇 drop 立即停止）

| # | 步骤 | 作用 | drop reason | 依赖数据 | 出处 |
|---|---|---|---|---|---|
| 1 | `drop_verification_agent` | EE：丢弃安装验证器自己的 UA；CE 空操作 | `:verification_agent` | 常量 UA | `event.ex:203-217` |
| 2 | `drop_datacenter_ip` | 头 `x-plausible-ip-type == "dc_ip"` | `:dc_ip` | 上游头 | `:219-227` |
| 3 | `drop_threat_ip` | 头值 `"threat_ip"` | `:threat_ip` | 上游头 | `:229-237` |
| 4 | `drop_shield_rule_hostname` | 站点若配置了主机名白名单，则 hostname 必须匹配其中一条 | `:site_hostname_allowlist` | `HostnameRuleCache` | `:247-253` |
| 5 | `drop_shield_rule_page` | pathname 匹配任一页面 deny 规则 | `:site_page_blocklist` | `PageRuleCache` | `:255-261` |
| 6 | `drop_shield_rule_ip` | `{domain, remote_ip}` 命中 deny | `:site_ip_blocklist` | `IPRuleCache` | `:239-245` |
| 7 | `put_geolocation` | 头 `anonymous_vpn_ip` → `country_code="A1"`；否则 mmdb 查询，失败则空 | 无 | `Plausible.Geo` | `:337-346` |
| 8 | `drop_shield_rule_country` | 用上一步国家码（无则 `ZZ`）查 deny | `:site_country_blocklist` | `CountryRuleCache` | `:348-373` |
| 9 | `put_user_agent` | 解析 UA；机器人/Headless Chrome 丢弃；否则写 OS/浏览器/屏幕类型进会话属性；UA 缺失或解析超时则跳过且**不丢** | `:bot` | `UAInspector`、`:user_agents` 缓存 | `:263-283, 457-483` |
| 10 | `put_basic_info` | 写事件属性：`domain, site_id, timestamp, name, hostname, pathname, scroll_depth, engagement_time, interactive?` | 无 | 站点缓存里的 `site.id` | `:285-303` |
| 11 | `put_source_info` | 引荐来源、格式化 referrer、click id、utm_* | 无 | `Source`、query 参数 | `:305-323` |
| 12 | `maybe_infer_medium` | 见 4.5 | 无 | — | `:325-335` |
| 13 | `put_props` | 属性映射拆成 `meta.key`/`meta.value` 两个数组 | 无 | — | `:375-385` |
| 14 | `put_revenue` | EE 收入；CE 空操作 | 无 | `site.revenue_goals` | `:387-394` |
| 15 | `put_salts` | 从 ETS 读 `{current, previous}` | 无 | `Salts` | `:396-398` |
| 16 | `put_user_id` | 用 `current` salt 算 user_id | 无 | 第 9 节 | `:400-410` |
| 17 | `validate_clickhouse_event` | Ecto 校验：`name, site_id, hostname, pathname, user_id, timestamp` 必填 | `:invalid` | — | `:412-425`；`clickhouse_event_v2.ex:83` |
| 18 | `register_session` | 用 `previous` salt 算 `previous_user_id`，调用持久化 | `:no_session_for_engagement :lock_timeout :persist_*` | Persistor | `:427-446` |

每步会量一次耗时遥测，标签 `step=<步骤名>`（`event.ex:160-168`，事件名 `[:plausible,:ingest,:pipeline,:step]` 见 `:99-102`）。

顺序设计观察（推断）：便宜且必然的丢弃放前面（头信息、shield），昂贵的 UA 解析放在 shield 之后；地理查询必须先于国家 shield；`put_salts`/`put_user_id` 靠后，因为只有活下来的事件需要身份。`engagement` 事件同样会白跑 UA 解析和地理查询，直到第 18 步才发现没有会话。

### 5.3 drop reason 全集

`bot spam_referrer not_found block throttle payment_required invalid dc_ip threat_ip site_ip_blocklist site_country_blocklist site_page_blocklist site_hostname_allowlist verification_agent lock_timeout no_session_for_engagement persist_timeout persist_error persist_decode_error`（`event.ex:24-40`；`gate_keeper.ex:2`）。

## 6. 站点解析 / GateKeeper / 限流 / 站点缓存

### 6.1 通用缓存框架 `Plausible.Cache`

| 项 | 内容 | 出处 |
|---|---|---|
| 存储 | ConCache 包装的 ETS，读并发；`ttl_check_interval: false`（无过期，靠刷新） | `lib/plausible/cache/adapter.ex:29-45`；`application.ex:67-72` |
| 读 | `get(key)`：缓存启用时**只读 ETS，未命中不回源**；缓存关闭（测试）才回源 `get_from_source` | `lib/plausible/cache.ex:99-111` |
| 全量刷新 | `refresh_all`：执行 `base_db_query` 全表，`delete_stale_items?: true`，删除缓存里已不存在的键 | `cache.ex:134-141, 164-187` |
| 增量刷新 | `refresh_updated_recently`：条件 `updated_at > now - 15分钟`，不删除任何键 | `cache.ex:143-160` |
| 刷新容错 | 查询异常只记日志，缓存保持旧值 | `cache.ex:222-239` |
| 其它 | `ready?`（缓存非空或源表为空才算就绪）；`unwrap_cache_keys` 让站点同时挂在 `domain` 与 `domain_changed_from` 下；每个缓存两个 `Warmer`（`gen_cycle`） | `cache.ex:196-213`；`site/cache.ex:95-104`；`cache/warmer.ex:34-77` |

### 6.2 各缓存的内容与周期

| 缓存 | 键 → 值 | 全量周期 | 增量周期 | 出处 |
|---|---|---|---|---|
| `Site.Cache`（`:sites_by_domain`） | `domain` / `domain_changed_from` → Site（仅 `id, domain, domain_changed_from, ingest_rate_limit_scale_seconds, ingest_rate_limit_threshold`，预加载 `revenue_goals` 与 `team`），只含 regular 站点 | 15 分钟 + 1~10000ms 随机抖动 | 30 秒 | `site/cache.ex:28-59`；`application.ex:67-80` |
| `IPRuleCache` | `{domain, ip字符串}` → 规则 | 3 分钟 + 抖动 | 35 秒 | `shield/ip_rule_cache.ex:53-64`；`application.ex:97-110` |
| `CountryRuleCache` | `{domain, 大写国家码}` → 规则 | 3 分钟 + 抖动 | 35 秒 | `country_rule_cache.ex:52-64`；`application.ex:111-124` |
| `PageRuleCache` | `domain` → 多条规则（ETS `:bag`） | 3 分钟 + 抖动 | 35 秒 | `page_rule_cache.ex:52-64`；`application.ex:125-138` |
| `HostnameRuleCache` | `domain` → 多条规则（`:bag`） | 3 分钟 + 抖动 | 25 秒 | `hostname_rule_cache.ex:57-69`；`application.ex:139-152` |
| `TrackerScriptCache`（CE） | 脚本 id → 已内联配置的完整脚本字符串 | 180 分钟 + 抖动 | 30 秒 | `tracker_script_cache.ex:21-55`；`application.ex:184-197` |
| `:user_agents` | 原始 UA 字符串 → 解析结果 | 无（TTL 60 分钟） | — | `application.ex:53-58` |

**增量刷新的盲区（推断）**：新增/修改靠 `updated_at` 在 15 分钟窗口内被发现；**删除**只会在全量刷新时清掉。`Shields.remove/3` 只做数据库删除、不广播缓存删除（`shields.ex:245-248`），所以撤销 shield 规则最多要等一个全量周期（约 3 分钟）。站点删除则显式 `broadcast_delete`（`lib/plausible/site/removal.ex:36-39`）。域名过渡期结束时 `update_all` 清空 `domain_changed_from`，通常不会刷新 `updated_at`，旧域名键也要等全量刷新（≤15 分钟）才消失（`site/domain.ex:29-42`）。

### 6.3 GateKeeper 判定顺序与结果

| 序 | 条件 | 结果 | 出处 |
|---|---|---|---|
| 1 | `Site.Cache.get(domain)` 取不到，或站点不是 regular（EE 的 consolidated 站点不接收直接采集） | `{:deny, :not_found}` | `gate_keeper.ex:43-62`；`lib/plausible/sites.ex:17, 32` |
| 2 | `team.accept_traffic_until` 非空且今天（UTC）晚于它 | `:payment_required` | `gate_keeper.ex:47-50` |
| 3 | 调用方传 `skip_rate_limit?`（EE 重放） | `{:allow, site}` | `:52-53`；`event.ex:57-62` |
| 4 | `ingest_rate_limit_threshold == nil` | `{:allow, site}`（自托管默认无限制） | `:64-66`；`config/runtime.exs:1052-1064` |
| 5 | `threshold == 0` | `:block` | `:68-70` |
| 6 | 其它整数 | 走限流，超限 `:throttle` | `:72-81` |

对外只有 `{:allow, site}` 与 `{:deny, :not_found | :block | :throttle | :payment_required}`。注释声称“限流机制内部错误时放行”（`gate_keeper.ex:22-25`），但代码里没有对应分支（事实，文档与实现不一致）。

### 6.4 限流算法

| 项 | 内容 | 出处 |
|---|---|---|
| 键 | `"ingest:site:" <> site.domain`（用缓存里的**主域名**，因此新旧域名共用额度） | `gate_keeper.ex:38-40, 74` |
| 算法 | 固定窗口计数器：`bucket = div(now_ms, scale_ms)`，键 `{key, bucket}`；`:atomics` 原子累加；`count <= limit` 放行，否则拒绝 | `lib/plausible/rate_limit.ex:33-57` |
| 窗口 | `site.ingest_rate_limit_scale_seconds`（默认 60，且 ≥1）× 1000 | `site.ex:31`；`:155-158`；`gate_keeper.ex:75` |
| 清理 | 每 10 分钟删除 `expires_at < now` 的条目 | `rate_limit.ex:85-92`；`application.ex:209` |
| 配置入口 | 仅 CRM 修改（`crm_changeset`），默认值来自环境变量 `SITE_DEFAULT_INGEST_THRESHOLD`（云端） | `site.ex:111-131, 144-163` |

推断：计数在**本节点**内存里，多节点部署时每节点各算各的，实际总额度是 N 倍；固定窗口在边界处可瞬时放行 2 倍；被拒绝的请求也会累加计数；因为限流发生在管线之前，机器人、被 shield 的事件同样消耗额度。

### 6.5 域名变更过渡

改域名后 72 小时内旧域名仍被接受（缓存里同一站点挂两个键），之后由每 2 小时一次的 Oban 任务清掉（`site/domain.ex:2-21, 29-42`；`config/runtime.exs:839-840`）。`event.domain` 是**请求带的字符串**而非 `site.domain`，参与 user_id 哈希（`event.ex:56-83, 566-590`），过渡期内同一访客用新旧域名上报会得到不同 user_id（推断）。

## 7. 过滤

### 7.1 总览

| 过滤 | 判定位置 | 数据来源 | 本进程实现？ | 出处 |
|---|---|---|---|---|
| 垃圾引荐 | 预阶段，早于站点检查 | `referrer_blocklist` 依赖 | 是（规则在依赖里） | `event.ex:604-608` |
| 机器人 UA | 管线 #9 | UAInspector（Matomo device-detector 数据，`priv/ua_inspector`） | 是 | `event.ex:263-271`；`lib/plausible_release.ex:184-187` |
| 无头 Chrome | 管线 #9 | UA 解析结果 `client.name == "Headless Chrome"` | 是 | `event.ex:265-266` |
| 数据中心/威胁 IP | 管线 #2、#3 | 请求头 `x-plausible-ip-type` | **否**，由上游注入 | `request.ex:389-396`；`event.ex:219-237` |
| 匿名 VPN | 管线 #7 不丢弃，国家记 `A1` | 同一个头 | 否 | `event.ex:339-340` |
| Tor/未知国家 | 国家码 `T1/XX/ZZ` 置空 | mmdb | 是 | `geolocation.ex:43-51` |
| 站点 IP 规则 | 管线 #6 | Postgres → ETS | 是 | `shields.ex:69-77` |
| 站点国家规则 | 管线 #8 | 同上 | 是 | `shields.ex:103-111` |
| 站点主机名白名单 | 管线 #4 | 同上 | 是 | `shields.ex:32-45` |
| 站点页面黑名单 | 管线 #5 | 同上 | 是 | `shields.ex:84-96` |

事实：全库只读取 `x-plausible-ip-type`、不写入（grep）；推断：CE 默认没有数据中心判定，云端由前置代理设置。

### 7.2 Shield 规则语义

| 规则 | 存储字段 | 上限（每站） | 校验 | 匹配语义 | 出处 |
|---|---|---|---|---|---|
| IP | `inet`、`action(deny/allow，默认 deny)` | 30 | 只允许 /32 或 /128 | 精确字符串相等；只有 `deny` 生效 | `ip_rule.ex:11-40`；`shields.ex:10, 69-77` |
| 国家 | `country_code`、`action` | 30 | 2 位，必须是 `ZZ` 或有效 ISO alpha-2 | 键是大写国家码；无国家时用 `ZZ` 查；`A1` 因校验不允许而**无法被屏蔽（推断）** | `country_rule.ex:22-44`；`event.ex:348-373`；`shields.ex:103-111` |
| 页面 | `page_path`、`page_path_pattern`(正则)、`action` | 30 | ≤250 字符，必须以 `/` 开头 | `Regex.escape` 后 `**`、`*` **都**换成 `.*`，加 `^…$`；对 pathname（含 hash 模式的 `#frag`）做**区分大小写**匹配 | `page_rule.ex:23-59`；`shields.ex:84-96` |
| 主机名 | `hostname`、`hostname_pattern`(正则)、`action(默认 allow)` | 10 | ≤250 字符 | 同上的通配编译；**没有任何规则=全放行**；有规则则必须命中一条 `allow` 才放行 | `hostname_rule.ex:23-66`；`shields.ex:32-45` |

补充：
- 正则在加载时以 `"E"`（dollar_endonly）编译一次（`lib/plausible/ecto/types/compiled_regex.ex:12`）。
- 页面和主机名缓存用 ETS `:bag`，同一域名多条规则逐条匹配，线性扫描（`application.ex:125-152`；`shields.ex:38-41, 90-92`），上限很小所以可接受。
- IP 规则的缓存键用 `to_string(inet)`（`ip_rule_cache.ex:56-61`），而请求侧是原始头字符串（`remote_ip.ex`），IPv6 的大小写/压缩差异会导致不匹配（推断）。
- 新增规则有数据库唯一约束，加规则在事务里先数再插，防止超限（`shields.ex:210-239`）。

### 7.3 机器人识别细节

- 每次 UA 解析放在独立 Task（`PartitionSupervisor` 下），等 200ms，超时放弃并发遥测（`event.ex:457-483`；`application.ex:37-38`）。
- 超时返回 `{:error,:timeout}` 落入 `_any -> event` 分支，事件**被接受且没有 UA 属性**，机器人检测被绕过（`event.ex:280-281`）；ConCache 的 `fetch_or_store` 不缓存错误结果（推断）。
- UA 缓存键是原始字符串，TTL 60 分钟，随机 UA 可撑大缓存（推断）（`application.ex:53-58`）。
- 无 UA：不解析、不丢弃，user_id 里 UA 当 `""`（`event.ex:458-464, 575`）。

## 8. 富化

### 8.1 User-Agent

| 产出字段 | 规则 | 出处 |
|---|---|---|
| `operating_system` | `os.name`，未知为 `""` | `event.ex:539-544` |
| `operating_system_version` | 版本只取前两段 `major.minor`，未知 `""` | `event.ex:546-564` |
| `browser` | 归一化：Mobile Safari→Safari，Chrome Mobile(含 iOS)→Chrome，Firefox Mobile(含 iOS)→Firefox，Opera Mobile/Mini→Opera，Yandex Browser Lite→Yandex Browser，Chrome Webview 与 `mobile app` 类型→`Mobile App`，未知 `""` | `event.ex:485-501` |
| `browser_version` | `major.minor`；`mobile app` 为 `""` | `event.ex:531-537` |
| `screen_size` | 设备类型映射：smartphone/feature phone/portable media player/phablet/wearable/camera→`Mobile`；car browser/tablet→`Tablet`；tv/console/desktop→`Desktop`；其它 `nil` | `event.ex:503-529` |

库：`ua_inspector ~> 3.12`（`mix.exs:152`），数据目录 `priv/ua_inspector`；其数据许可证独立，Rivu 复用需单独评估（推断：来自 Matomo device-detector）。

### 8.2 地理位置

| 项 | 内容 | 出处 |
|---|---|---|
| 库 | Erlang `locus`（`mix.exs:99`），读 `.mmdb` | `lib/plausible/geo.ex:44-74, 191-206` |
| 来源（二选一） | (a) 本地文件 `IP_GEOLOCATION_DB`，默认指向随包的 `priv/geodb/dbip-country.mmdb.gz`（**仅国家级**）；(b) MaxMind 授权码 + `MAXMIND_EDITION`（默认 `GeoLite2-City`），可缓存到 `PERSISTENT_CACHE_DIR`，异步下载 | `config/runtime.exs:268-278, 960-992` |
| 启动 | 启动时 `await_loader`，数据库没加载完不开服 | `application.ex:228-230` |
| 查询失败 | `:not_found`、地址非法、其它错误都返回 `nil`（其它错误记 error 日志），事件保留但无地理字段 | `geo.ex:191-206`；`geolocation.ex:22-23` |
| 产出 | `country_code`（`entry.country.iso_code`，`ZZ/XX/T1` 置 `nil`）；`subdivision1_code = "国家码-第一级iso"`；`subdivision2_code` 同理第二级；`city_geoname_id`（仅国家码非空时取，再经覆盖表） | `geolocation.ex:4-51` |
| 城市覆盖 | 手工映射表 geoname_id→规范 geoname_id，把行政区/辖区合并到主城市（维也纳各区→维也纳、多伦多各区→多伦多、布拉格、哥本哈根、奥登塞等，覆盖奥地利、加拿大、捷克、丹麦、爱沙尼亚、德国、印度、墨西哥、荷兰、挪威、罗马尼亚、斯洛伐克、瑞典、瑞士） | `lib/plausible/ingestion/city_overrides.ex:4-199`（`get/2` 在 `:201`） |
| 展示名 | 城市/区划名称不存库，查询时用 GeoNames 数据补（`GEONAMES_SOURCE_FILE`，每日 `LocationsSync` 任务） | `runtime.exs:276, 994`；`runtime.exs:842` |
| IP 去向 | IP 不进入 `ClickhouseEventV2`/会话属性；只用于查地理、哈希与 shield。但 `Sentry.Context.set_extra_context(%{request: request})` 与 `Jason.Encoder` 实现会让整个 Request（含 `remote_ip`、UA）进入错误上报（推断的隐私风险） | `external_controller.ex:15`；`request.ex:468-475` |

### 8.3 来源（referrer source）分类

`Source.resolve(request)` 的顺序（`source.ex:72-78`）：

1. 有 `utm_source` → `source` → `ref` 之一：`canonical(tagged)`，即先**不区分大小写**去别名表匹配，匹配上用规范名，否则保留原样（含大小写）（`source.ex:119-127`；测试 `external_controller_test.exs:396-460`）。
2. 否则，若引荐有效：`from_referrer`。
3. 否则 `nil`。

“引荐有效”需同时满足：scheme ∈ `http, https, android-app`；host 非空；不是内部引荐（去 www 后与页面 host **完全相等**，所以子域名之间互相跳转**不算**内部）；host 不是 `localhost`（`source.ex:137-151`）。

`from_referrer` 的解析顺序（`source.ex:85-111`）：

| 序 | 规则 | 例 |
|---|---|---|
| 1 | `priv/custom_sources.json` 的精确 host 匹配（小写） | `gemini.google.com`→Google Gemini，`bsky.app`→Bluesky，`chatgpt.com`→ChatGPT |
| 2 | 后缀族：`officeapps.live.com`→Microsoft 365，`wikipedia.org` 及其子域→Wikipedia | |
| 3 | `RefInspector.parse(referrer).source`（`referers.yml` 数据库），再经别名表规范化（如 Twitter→X (Twitter)） | google.co.uk→Google |
| 4 | 未知则直接用 host | `source.ex:106` |

编译期把自定义表和 RefInspector 全量名称合成一个以小写名为键的查找表，并生成函数子句（`source.ex:30-49`）；这意味着修改来源表需要重新编译（推断）。付费来源集合 `@paid_sources` 由别名里以 `ads`/`ad` 结尾的键加 `adwords` 得出（`source.ex:13-16`）。

`format_referrer`：host（去 www，`android-app` 保留前缀）+ path（去尾 `/`），不含 query；无效引荐为 `nil`（`source.ex:129-158`；测试 `external_controller_test.exs:262-283`）。

### 8.4 acquisition channel

| 项 | 内容 | 出处 |
|---|---|---|
| 计算位置 | ClickHouse 里 `events_v2`/`sessions_v2` 的 `acquisition_channel` 列，是 `MATERIALIZED acquisition_channel(referrer_source, utm_medium, utm_campaign, utm_source, click_id_param)`；Ecto 里该列 `writable: :never` | `priv/data_migrations/AcquisitionChannel/sql/acquisition_channel_update_materialized_column.sql.eex:1-4`；`clickhouse_event_v2.ex:50` |
| 输入 | `referrer_source, utm_medium, utm_campaign, utm_source, click_id_param`，全部先转小写（click id 参数名除外） | SQL `:202-210` |
| 数据 | 谷歌 GA4 来源分类 CSV（`priv/ga4-source-categories.csv`）+ 手工追加类别（AI 助手、Hacker News、Mastodon、Bluesky 等），写入 CH 表后以字典 `acquisition_channel_source_category_dict`（hashed，`LIFETIME(0)` 不自动刷新）查询；付费来源另有字典 | `acquisition.ex:20-57`；SQL `:1-62` |
| 规则顺序（CH 版，优先级由高到低） | Cross-network → Display → Paid Shopping → Paid Search → Paid Social → Paid Video → Paid Other → Organic Shopping → Organic Social → Organic Video → AI Assistants → Organic Search → Email → Affiliates → Audio → SMS → Mobile Push Notifications → Referral → Direct | SQL `:212-235` |
| 关键判定 | 付费 medium 正则 `^(.*cp.*\|ppc\|retargeting\|paid.*)$`；购物 campaign 正则；Email：`e[-_ ]?mail\|newsletter`；`Google+gclid`、`Bing+msclkid` 直接算 Paid Search | SQL `:140-146, 198-200, 104-118` |
| 与 Elixir 版差异（事实） | Elixir `get_channel_lowered` 把 Paid Shopping/Search/Social/Video 放在 Display **之前**，SQL 把 Display 放在最前面（测试里有 “display channel with gclid”） | `acquisition.ex:71-92`；SQL `:214-217`；`external_controller_test.exs:1649` |
| 可疑点（推断） | 字典键来自 CSV 原样（如 `Google Shopping`、`Hatena` 首字母大写，见 CSV 第 7-8 行），而查询键是小写，大写键永远命不中 | `priv/ga4-source-categories.csv:7-8`；SQL `:204-210` |

对 Rivu 的含义：渠道可以放 ClickHouse 物化列（改规则可回填），也可以在接入层算好存列；两种都要规定好“源数据版本”。这一项属于存储层同学，但输入字段（`referrer_source/utm_*/click_id_param`）由接入层产出，需要保持不变。

UTM 与 click id 见 4.5；它们属于会话属性，经 `merge_session` 回填到事件行（`clickhouse_event_v2.ex:30-37, 86-110`）。

## 9. 身份

### 9.1 user_id 公式

```
user_id = SipHash(key = salt, data = user_agent ++ remote_ip ++ domain ++ root_domain ++ replay_session_id_str)
```

| 输入 | 取值 | 出处 |
|---|---|---|
| `salt` | 16 字节随机数，`:current` 或 `:previous` | `lib/plausible/session/salts.ex:84-88`；`event.ex:408, 434` |
| `user_agent` | 请求头原值，缺失为 `""`（未经解析、未归一化） | `event.ex:575` |
| `remote_ip` | 4.4 的结果字符串，原样拼接 | `event.ex:587` |
| `domain` | 请求里的域名字符串（不是 `site.domain`） | `event.ex:402-406` |
| `root_domain` | 页面 URL 的 hostname：`(none)` 保持；IPv4 字面量保持；否则取 PublicSuffix 可注册域，取不到就用 hostname 本身 | `event.ex:576, 592-602` |
| `replay_session_id_str` | EE 重放会话号转字符串；CE 恒 `""` | `event.ex:578-583` |
| 哈希 | `SipHash.hash!/2`（库 `siphash ~> 3.2`，默认 2-4，输出 64 位整数；推断），存 `user_id UInt64` | `mix.exs:150`；`clickhouse_event_v2.ex:15` |
| 空值 | `salt` 或 `domain` 为 `nil` → `user_id = nil` → 第 17 步校验失败 `:invalid` | `event.ex:567-572` |

字符串直接拼接**没有分隔符**（推断：理论上存在边界歧义，如 `domain` 与 `root_domain` 相邻）。

要点：同一访客在同站点的 `www.example.com` 与 `blog.example.com` 因 `root_domain` 相同而共享 user_id；跨域站点（`d=a.com,b.com`）则不同。

### 9.2 salt 的生命周期

| 项 | 内容 | 出处 |
|---|---|---|
| 存储 | Postgres 表 `salts`（`salt`, `inserted_at`），内存里用 ETS `{:state, %{current, previous}}` | `salts.ex:13-30, 33-48` |
| 读取 | 每次事件 `Salts.fetch()`，ETS 读，无锁 | `salts.ex:58-61` |
| 同步 | 每个节点每 90 秒（可配）重读最近 2 条（`order by id desc limit 2`）刷新 ETS | `salts.ex:33, 49-50` |
| 轮换 | Oban cron `0 0 * * *`（每日 00:00，Oban 默认 UTC，推断），队列并发 1；轮换：删除 >48 小时的盐，生成新盐，`previous := 旧 current` | `config/runtime.exs:826, 879`；`lib/workers/rotate_salts.ex:9-12`；`salts.ex:64-76` |
| 保留 | 数据库保留 48 小时内；内存只用最新两个 | `salts.ex:90-95, 33` |
| 首启 | 表为空则立即生成一个，`previous=nil` | `salts.ex:43-46` |
| 范围 | **全局共享**，不分站点 | `salts.ex` 全文 |

隐患（推断）：`refresh/2` 复用启动时的 `now`（`salts.ex:50`），表被清空后新盐 `inserted_at` 会是旧时间；多节点在轮换后最长约 90 秒内对 `current` 认识不一致。
### 9.3 跨日与会话

- 午夜后新事件用新盐算 `user_id`，同时把旧盐算出的 `previous_user_id` 传下去；会话层先按新 id 找会话，找不到按旧 id 找，因此跨午夜的活跃会话保持不变（测试 `external_controller_test.exs:1281-1297` 断言轮换一次后 session/user_id 仍唯一）（`event.ex:427-446`；`cache_store.ex:29-31`）。
- 第二次轮换后旧盐消失，“跨天回访”无法识别（隐私设计，没有长期访客 id）；轮换时刻是 UTC，与站点时区无关。

## 10. 计数器与可观测性

### 10.1 内部计数（按域名，非 Prometheus）

| 项 | 内容 | 出处 |
|---|---|---|
| 来源 | 订阅遥测 `[:plausible,:ingest,:event,:buffered]` 与 `[:plausible,:ingest,:event,:dropped]` | `counters/telemetry_handler.ex:12-27` |
| 指标名 | 成功 `buffered`；丢弃 `dropped_<reason>`，如 `dropped_bot`、`dropped_not_found`、`dropped_throttle`、`dropped_invalid`、`dropped_no_session_for_engagement` | `telemetry_handler.ex:30-62` |
| 维度 | `{10秒桶, metric, domain, tracker_script_version}`；桶按**请求时间戳**对齐 | `counters/buffer.ex:48-73, 99-106` |
| 内存结构 | ETS `ordered_set`、`write_concurrency`，用 `update_counter` 原子自增 | `buffer.ex:27-32, 65-70` |
| 刷新 | 每 10 秒一轮；只刷新“已完成”的桶（桶时间 ≤ now−10s），避免“读后删”之间的写入丢失 | `counters.ex:28`；`buffer.ex:75-97, 108-112` |
| 落库 | 桶对齐到整分钟，`site_id` 此时才用 `Site.Cache.get_site_id(domain)` 补（可为空），批量异步插入 `ingest_counters`（SummingMergeTree，自动汇总） | `counters.ex:70-86, 122-127`；`counters/record.ex:10-17` |
| 容错 | 插入失败只发 Sentry，不重试，也不阻塞 | `counters.ex:82-94` |
| 停机/开关 | `terminate` 把当前时间拨快 60 秒强制刷出全部桶；`Counters enabled` 配置可关 | `counters.ex:40-43, 108-114`；`config/config.exs:78` |

事实：400 的请求（`Request.build` 失败）不产生任何计数（没有走到遥测）。推断：`domain` 是用户可控标签，未知域名也会形成键，10 秒内可堆积，但刷新后清空。

### 10.2 其它遥测

管线每步耗时 `[:plausible,:ingest,:pipeline,:step]`，标签 `step`（`event.ex:99-102, 160-168`）；UA 解析超时（`event.ex:104-112`）；缓存刷新耗时（`cache.ex:217-239`）；脚本请求带 `status`（`tracker_plug.ex:62-71`）；会话锁等待 `[:plausible,:sessions,:cache,:lock]`（`cache_store.ex:12, 27`）；EmbeddedWithRelay 的远程成败单独发 `[:plausible,:remote_ingest,...]`，**不进入**上面的每域计数器（`embedded_with_relay.ex:54-84`）。

## 11. 可抽象的设计模式与陷阱（Rust 重写清单）

### 11.1 应保留的不变量

| # | 不变量 | 理由/出处 |
|---|---|---|
| 1 | 事件时间只取服务端接收时刻（秒精度），不信任客户端 | 防伪造/回填；`request.ex:79-86` |
| 2 | 采集端点对“被过滤”一律 2xx，只对“协议错误”4xx，并给出丢弃计数头 | `external_controller.ex:13-45` |
| 3 | 一个请求可扇出多站点，每站点独立准入、独立 user_id、共用时间戳 | `event.ex:56-83`；`request.ex:238-263` |
| 4 | 管线是**有序阶段数组**，每阶段返回“继续/丢弃(reason)”，所有 reason 枚举化并全部计数 | `event.ex:137-175` |
| 5 | 便宜的静态判断在前，UA 解析/地理在 shield 后；国家 shield 必须在地理后 | `event.ex:137-158` |
| 6 | 准入缓存只读内存，不在热路径回源；站点变更靠“15 分钟窗口增量 + 定期全量” | `cache.ex:99-111, 143-160` |
| 7 | 身份哈希包含 `domain`，salt 全局共享、每日轮换，**同时保留上一个 salt** 供会话层桥接跨午夜 | `event.ex:427-446, 566-590`；`salts.ex` |
| 8 | 原始 IP 不入库、不进事件行；引荐来源“URL 标记参数 > Referer”，同一函数服务实时与导入 | `clickhouse_event_v2.ex:9-59`；`source.ex:72-92` |
| 9 | engagement 是增量事件：`e` 是“上次发送以来”的时长，`sd` 是最大深度；服务端允许缺一个、不允许都缺 | `engagement.js:77-79`；`request.ex:351-358` |

### 11.2 边界条件与易错点

| # | 陷阱 | 类别 | 出处 |
|---|---|---|---|
| 1 | URL 长度限制是**字节**，referrer 截断是**字符**，props 键值是**字节**，事件名是**字符**（≤120），Rust 里要分别用 `len()` 与 `chars()` | 事实 | `request.ex:29, 212, 331, 114` |
| 2 | 域名比较：站点域名入库时小写并去 `www.`，但请求里的 `d` 只 trim 并去小写 `www.`，不转小写；大小写不同的 `d` 会 `not_found`（推断） | 推断 | `site.ex:221-231`；`request.ex:429-433` |
| 3 | `URI.decode`/`decode_query` 遇到非法百分号转义在 Elixir 里会抛异常；本仓库无 rescue 也无测试覆盖（库行为，推断）。Rust 里要决定是 400、还是保留原串 | 推断 | `request.ex:411-418, 379-387` |
| 4 | `props` 超过 30 个时取哪 30 个是不确定的；极端长度导致整个请求 400 而不是只丢一个属性 | 事实+推断 | `request.ex:289-344` |
| 5 | 空字符串参数在 Elixir 里是真值：`?utm_source=` 会盖掉 `ref` 和 Referer；`utm_medium=` 为空串则不触发 gclid 推断 | 推断 | `event.ex:308-335` |
| 6 | 内部引荐判断是 host 全等，不是同站点（子域互跳会算外部来源） | 事实 | `source.ex:145-146` |
| 7 | `Forwarded` 头无 `for=` 会崩溃；IP 不规范化会让 shield 与哈希不稳定；所有转发头都被信任，应改成“仅信任配置的代理/CDN 头” | 推断/事实 | `remote_ip.ex:26-31` |
| 8 | UA 解析超时会放过机器人（事件被接受）；失败策略要显式选择（放行或丢弃），并计数 | 事实 | `event.ex:280-281, 457-483` |
| 9 | 限流计数在节点本地、固定窗口、先于过滤；要精确限额需集中计数或一致性哈希路由 | 推断 | `rate_limit.ex:33-57`；`event.ex:70` |
| 10 | 缓存“删除”靠全量刷新才生效（规则、域名过渡期、新旧键）；Rust 版应让删除也走增量（墓碑或 `deleted_at`） | 推断 | `cache.ex:155-159`；`shields.ex:245-248`；`site/domain.ex:29-42` |
| 11 | 缓存读未命中即 `not_found`：新站点首事件可能在最多 30 秒内被判未知站点（推断：除非调用了触发 `updated_at` 的路径，如 `touch_site!`，`goals.ex:71`） | 推断 | `cache.ex:99-111`；`site/cache.ex:83-93` |
| 12 | 脚本里的站点开关与数据库开关可能脱节（本快照只内联 5 个键）；Rust 版应有一份“权威配置→内联脚本”的单一映射并带测试 | 事实 | `tracker.ex:37-47` |
| 13 | `text/plain` 承载 JSON、`202 ok` 文本体、400 的 JSON 错误体是兼容旧 tracker 的契约；`/api/event` 不应强制 `application/json`，也要容忍 `Accept: */*`（`accepts json` 管道行为为推断） | 事实 | `external_controller_test.exs:40-60`；`router.ex:61-63` |
| 14 | engagement 现实现对无会话的事件也做完 UA、地理、shield，最后才丢弃；Rust 版可先查会话或走更短路径 | 推断 | `event.ex:137-158`；`cache_store.ex:49-58` |
| 15 | 远程 persistor 用 `:erlang.binary_to_term`（无 safe 选项）反序列化 `base64(term_to_binary)`；Rivu 的 NATS/跨进程交接要用显式 schema（protobuf/JSON/MessagePack）并带版本 | 事实 | `persistor/remote.ex:119-144` |
| 16 | 同一访客的事件必须路由到同一个会话处理者（`Session.Balancer.dispatch(user_id)`；灰度用 `phash2(user_id)`）；NATS 分区键取 `user_id`；`previous_user_id` 是交接契约的一等字段 | 事实+推断 | `cache_store.ex:23-25`；`persistor.ex:30`；`event.ex:430-436` |
| 17 | 计数器“按请求时间戳分桶 + 只刷完成桶 + 停机强刷” | 事实 | `counters/buffer.ex:75-97`；`counters.ex:108-114` |
| 18 | 字符串拼接做哈希无分隔符；换成带长度前缀会使 user_id 与旧数据不兼容，迁移时要明确 | 推断 | `event.ex:587` |

## 附录：未能从本仓库核实的点

- `referrer_blocklist` 匹配规则：外部 git 依赖，源码不在仓库（`mix.exs:146`）。
- `CORSPlug`、`Plug.Parsers`、ConCache、`SipHash`、Oban 的默认行为：仓库内无 deps，文中均标推断。
- `priv/geodb`、`priv/ua_inspector`、`priv/ref_inspector` 的实际数据：Glob 未找到。
- `x-plausible-ip-type` 由谁设置：仓库只读取该头。
- `Session.Balancer`、`Session.WriteBuffer` 内部：属于另一位同学的范围。
