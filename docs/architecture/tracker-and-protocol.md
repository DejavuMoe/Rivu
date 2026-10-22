# Rivu 采集脚本与采集协议规范
> 状态：草案 v0.1 ｜ 日期：2026-10-21 ｜ 协议版本：`v=1` ｜ 第 4 阶段内容已在正文标注
> 范围：浏览器脚本（构建、嵌入、运行时行为）、`POST /api/event` 线协议与 HTTP 契约、服务端规范化规则、版本策略、Plausible 兼容层设计、测试清单。不含：UA/地理/来源分类富化、访客哈希与会话、写缓冲与 ClickHouse 表结构（只写交接要求）。
> 出处记号：`[01 §x]` = `docs/research/plausible/01-ingestion-pipeline.md` 第 x 节；`[00 §x]` = `00-rea-analysis.md`；`[v1 §x]` = v1 架构草案「三、事件数据流架构」；`[src]` = 为核对行为只读过的 Plausible `tracker/src/*.js`。
> 许可：Plausible tracker 为 MIT（`tracker/npm_package/LICENSE.md`），服务端为 AGPL-3.0，`extra/`(EE) 另有许可 [01 头部]。本规范只描述行为，不含 Plausible 代码；未参考 EE 实现。「必须/不得」为强制，「应」为默认遵循，「可」为可选。

## 1. 设计目标与非目标
### 1.1 目标
| 类别 | 约束 | 依据 |
|---|---|---|
| 隐私 | 不读写 Cookie；不写任何浏览器存储；唯一的读取是站点所有者手动设置的 `localStorage.rivu_ignore === "true"`（`try/catch`，失败即忽略）。只读：脚本从不写入该键，也不用它标识访客，不随事件上报 | 决策 7；已决定，`system-design.md` §10 Q3；同类开关 `[01 §2.7]` |
| 隐私 | 客户端不发送屏幕尺寸、语言、时区等指纹字段；设备与地理由服务端从 UA、IP 短暂推导，IP 不落库 | `[01 §8.2]` `[01 §9.1]` |
| 隐私 | URL 在服务端收敛为 hostname、pathname、白名单 query 参数；其余 query 与 fragment 直接丢弃 | 决策 5 |
| 可信 | 时间戳只由服务端在接收时生成（毫秒，UTC）；客户端不可传时间 | 决策 4；`[01 §0 #3]` |
| 体积 | `core` gzip ≤ 1 KB；`full` gzip ≤ 2 KB；CI 强制 | 决策 6 |
| 可靠 | 不重试；engagement 与页面隐藏时的事件 beacon 优先；任何失败静默，脚本不得向宿主页面抛异常 | 决策 7 |
| 可运维 | 被过滤一律 `202`，原因只进内部计数器；`400` 只给协议错误 | 决策 2；`[01 §3.3]` |
| 可演进 | 协议版本 `v` 与构建号 `sv` 分离；服务端至少兼容 N−1 | 决策 4；`[01 §4.1]` |

### 1.2 非目标
| 非目标 | 理由 |
|---|---|
| 按特性组合出变体（Plausible 的 `COMPILE_*` 体系） | 维护成本爆炸；只保留 `core`/`full` 两个构建 `[00 §3.6]` `[00 §7]` `[01 §2.1]` |
| XHR 回退、IE/老旧浏览器 | 回退代码占体积；`[01 §2.5]` 中 XHR 仅用于旧兼容变体 |
| 客户端路径包含/排除（`data-include/exclude`）、`transformRequest` 脱敏钩子 | 改由服务端规则（页面屏蔽、URL 收敛）实现，客户端不保留第二套规则 `[01 §2.7]` `[01 §7.2]` |
| 客户端重试、离线队列、本地持久化；客户端生成访客/会话 ID | 违反「不写存储」，失败事件接受丢失；访客标识由服务端按日轮换盐计算 `[01 §9]`，不在本规范范围 |
| hostname 白名单当作鉴权 | `site_key` 公开可见，白名单只是降噪与防误报，不是安全边界 |
| 对抗广告拦截 | 仅提供可配置 `endpoint`（可经站点第一方路径反代），不做混淆 |
| DNT/GPC、同意管理 | v1 不读取；是否作为站点开关留待第 4 阶段评估 |
| 脚本错误上报端点（Plausible 的 `GET /api/error`） | 不收集；安装问题靠 `rivu.b`/`rivu.v` 探测与服务端 400 计数 `[01 §3.1]` |

### 1.3 浏览器基线
需要 `fetch`、`URL`、`navigator.sendBeacon`（full 另需 `ResizeObserver`），约等于 Chrome/Edge 66+、Safari 13.1+、Firefox 69+。无 `fetch` 时普通事件不发送（同 `[01 §2.5]`）。旧版 Firefox 对 `fetch keepalive` 支持不完整，这是 engagement 采用 beacon 优先的又一理由。esbuild 目标 `es2018`，源码不使用 `?.`、`??`（会被降级而增体积）。

## 2. 嵌入方式
### 2.1 站点专属脚本（推荐）
```html
<script defer src="https://stats.example.com/js/k7qm2xv5atbr.js"></script>
```

页面若要在脚本加载前调用手动 API，先放队列桩（也可省略，省略时早于加载的调用会丢失）：

```html
<script>window.rivu=window.rivu||function(){(rivu.q=rivu.q||[]).push(arguments)}</script>
<script defer src="https://stats.example.com/js/k7qm2xv5atbr.js"></script>
<script>rivu('event','Signup',{props:{plan:'pro'}})</script>
```

### 2.2 通用 `t.js`（静态/CDN 托管）
```html
<script defer src="https://cdn.example.net/rivu/t.js"
        data-site="k7qm2xv5atbr" data-api="https://stats.example.com/api/event"></script>
```

`t.js` 与 `full` 构建字节相同，站点无关。`data-site` 必填；`data-api` 缺省取脚本 `src` 的 origin + `/api/event`（`t.js` 由 Rivu 自己托管时可省，同 `[00 §7]` 的「默认取脚本源，允许覆盖」）；缺 `data-site` 时脚本不做任何事。

| 属性 | 配置键 | 默认 | 说明 |
|---|---|---|---|
| `data-site` | `site` | 必填 | `site_key` |
| `data-api` | `endpoint` | 脚本 origin + `/api/event` | 采集端点，可指向第一方反代路径 |
| `data-hash` / `data-local` | `hash` / `local` | `false` | hash 路由模式 / 允许 localhost 采集 |
| `data-manual` | `manual` | `false` | 关闭自动 pageview（仅 full） |
| `data-engagement` | `engagement` | `true` | 参与度事件（仅 full） |
| `data-outbound` / `data-forms` | `outbound` / `forms` | `false` | 外链点击 / 表单提交 |
| `data-downloads` | `downloads` | `false` | `true` 用默认扩展名；或逗号分隔扩展名列表（替换默认） |
| `data-tagged` | `tagged` | `true` | tagged events（仅匹配 class 时才有动作） |
| `data-debug` | `debug` | `false` | 在 `console.warn` 输出忽略原因（仅 full） |

### 2.3 站点脚本的配置前缀
`GET /js/<site_key>.js` 的响应 = 前缀 + 构建产物。前缀由服务端从站点配置的**唯一映射函数**生成（该函数有测试，防止库中开关与脚本脱节 `[01 §0 #7]` `[01 §11.2 #12]`）：

```js
window.rivu=window.rivu||function(){(rivu.q=rivu.q||[]).push(arguments)};rivu.c={"site":"k7qm2xv5atbr","endpoint":"https://stats.example.com/api/event","hash":false,"engagement":true,"outbound":true,"downloads":["pdf","zip"],"forms":false,"tagged":true};
```

规则：`endpoint` 来自服务端配置项 `public_base_url`，不得取请求的 `Host`（防止 CDN 缓存被污染）；字符串用 JSON 转义并额外转义 `<`、U+2028/2029；`site_key` 仅含 `[A-Za-z0-9_-]`。`t.js` 无前缀，配置来自 `data-*`。

### 2.4 构建选择与缓存
| 项 | 规则 |
|---|---|
| 选 `full` 的条件 | 站点启用了任一 full 专属能力：`engagement`、`outbound`、`downloads`、`forms`、`tagged`、`manual`、手动 API/属性/收入；否则选 `core` |
| 响应头（站点脚本） | `Content-Type: application/javascript; charset=utf-8`、`ETag`、`Cache-Control: public, max-age=300`、`X-Content-Type-Options: nosniff`、`Cross-Origin-Resource-Policy: cross-origin`、`Access-Control-Allow-Origin: *`（头集合参照 `[01 §3.5]`） |
| ETag 与生效延迟 | ETag = `sha256(构建号 ‖ 前缀)` 截取 16 字节，`If-None-Match` 命中返回 `304`；开关变更最多 5 分钟生效（Plausible 为 60 秒 `[01 §3.5]`，Rivu 以 ETag 换更长缓存） |
| 未知 `site_key` | `404`，`Cache-Control: no-store`，空体 |
| `t.js` | `Cache-Control: public, max-age=86400`，另提供不可变的 `/js/t.<sv>.js` 供固定版本（旧式静态脚本 1 天缓存见 `[01 §3.5]`） |

### 2.5 队列桩与 `init` 的行为
| 场景 | 行为 |
|---|---|
| 桩 | 仅把调用的 `arguments` 压入 `rivu.q`，无其他副作用（形态同 `[01 §2.6]`/`[src]` 的 `plausible.q`） |
| 脚本加载后 | 先合并配置，再回放队列，最后替换 `window.rivu` 为真实实现 |
| 回放顺序 | ① 所有 `init`、`set` 命令（可能影响首个 pageview）→ ② 启动自动 pageview/路由监听 → ③ 其余命令按入队顺序 |
| `rivu('init', opts)` | 仅在脚本加载前入队才有效；可覆盖开关与 `endpoint`，不可覆盖 `site`；加载后调用忽略 |
| 重复加载 | `rivu.l` 已置位则整段初始化跳过（先置位，后注册监听） |
| `core` 构建 | 不含手动 API：把 `window.rivu` 替换为空函数（避免队列无限增长），`rivu.b` 为 `"core"` |
| 探测字段 | `rivu.v`（协议版本）、`rivu.sv`（构建号）、`rivu.b`（`core`/`full`），供安装检测使用 `[01 §2.6]` |

## 3. 构建产物与模块划分
### 3.1 构建原则
同一份 TypeScript 源码，esbuild 以编译期常量 `__FULL__` 产出两个 IIFE：`dist/core.js`、`dist/full.js`（`t.js` 即 `full.js` 副本）。**除 `__FULL__` 与 `__SV__`（构建号）外不得新增编译期开关**；运行时开关只来自配置对象。esbuild 参数：`--bundle --minify --format=iife --target=es2018 --legal-comments=none`。

### 3.2 模块与体积预算（gzip -9，字节，**估算值，待实现后实测**）
| 模块 | 职责 | core | full |
|---|---|---|---|
| `boot` | 幂等守卫、读 `rivu.c`/`data-*`、探测字段、全局 try/catch | 110 | 110 |
| `guard` | localhost、自动化、`rivu_ignore` | 130 | 130 |
| `payload` | 组装 `v/sv/s/n/u/r/h` | 110 | 110 |
| `send` | `fetch keepalive` | 90 | 90 |
| `pageview` | 首个 pageview、hidden/prerender、bfcache | 170 | 170 |
| `router` | `pushState`/`popstate`/`hashchange`、pathname 去重 | 170 | 170 |
| `beacon` | `sendBeacon` 优先与回退 | — | 60 |
| `engagement` | 状态机、滚动深度、`ResizeObserver` | — | 360 |
| `links` | 外链 + 文件下载（含扩展名表） | — | 230 |
| `forms` | 表单提交 | — | 70 |
| `tagged` | class 标记事件（含 revenue 标记） | — | 220 |
| `api` | 队列回放、`rivu()` 命令、全局/单次属性、收入、callback、`url` | — | 260 |
| `debug` | 忽略原因 `console.warn` | — | 50 |
| **合计估算** | | **≈ 780** | **≈ 2,030** |
| **预算** | | **≤ 1,024** | **≤ 2,048** |

`full` 估算仅余约 18 B，**视为高风险**。超预算时按此顺序削减（不改变决策 6 的功能范围）：① `debug` 日志；② 下载扩展名表缩回 26 项；③ tagged 父层搜索 3 层→1 层；④ tagged 的 revenue 标记；⑤ 仍超则需拍板调整预算（见附录）。

### 3.3 CI 检查
| 检查 | 方法 | 失败条件 |
|---|---|---|
| 体积预算 | 构建后 `gzip -9` 与 `brotli -q 11` 测量（brotli 仅报告） | core > 1,024 B 或 full > 2,048 B |
| 体积回归 | 与仓库内 `tracker/size-baseline.json` 比较，输出基线/当前对照表（做法参考 Plausible `compiler/analyze-sizes.js`，该脚本只比 brotli/gzip/原始三列） | 增长 > 16 B 在 PR 评论预警；> 64 B 需 label 批准 |
| 模块泄漏 | esbuild `--metafile`，断言 core 的 inputs 不含 full 专属模块 | 出现即失败 |
| 禁用 API | 扫描产物：`cookie`、`sessionStorage`、`indexedDB`、`setItem`、`XMLHttpRequest`、`eval`、`new Function`、`console.log` | 命中即失败；`localStorage` 只允许出现 1 次且紧接 `rivu_ignore` |
| 语法与确定性 | `es-check es2018`；同一提交构建两次哈希一致 | 任一不满足即失败 |
| 前缀上限 | 最坏配置（全开 + 28 个扩展名）的前缀原始长度 | > 512 B 失败（前缀不计入 gzip 预算） |
| 构建号 | `__SV__` 为单调递增整数，不得小于上一发布 | 违反即失败 |

## 4. 运行时行为
### 4.1 初始化流程（幂等）
| 步 | 动作 |
|---|---|
| 1 | `rivu.l` 已置位 → 返回；否则立刻置位（先于任何监听注册） |
| 2 | 解析配置，优先级：`rivu.c`（站点脚本前缀）> `document.currentScript` 的 `data-*`（`t.js`）> 默认值；无 `site` → 终止 |
| 3 | 回放 `init`/`set` 队列（见 2.5）；注册自动捕获监听（full） |
| 4 | 若非 `manual`：按 4.2 发首个 pageview；注册 4.3 的路由监听 |
| 5 | 回放其余队列命令；替换 `window.rivu`；写 `rivu.v/sv/b` |

整个初始化与每次事件发送包在 `try/catch` 内，脚本任何异常不得影响宿主页面。

### 4.2 首个 pageview 的时机
| 页面状态 | 行为 | 依据 |
|---|---|---|
| `visibilityState === 'visible'` | 立即发送 | `[01 §2.6]` |
| `hidden` 或 `prerender` | 监听 `visibilitychange`，首次变为 `visible` 才发；一旦已发出过 pageview 就不再发 | `[01 §2.6]` |
| `document.prerendering === true`（Speculation Rules 预渲染） | 另监听 `prerenderingchange`，激活后重新判断可见性。**Rivu 新增**：预渲染期间 `visibilityState` 为 `hidden`，仅靠 `visibilitychange` 可能漏报 | 新增 |
| 发送时的 `u` | 发送瞬间的 `location.href`，不是加载时的 | — |
| 后台标签从未被查看 | 不产生 pageview（预期） | `[00 §3.4]` |

`r` 规则：**仅文档的第一个 pageview** 带 `r = document.referrer`（空串视为缺失，不发该键）；SPA 内跳转与 bfcache 恢复不带 `r`。Plausible 每个事件都带 `document.referrer` `[src]`，会让会话超时后的 SPA 页面继承过期的外部来源。

### 4.3 SPA 路由
| 项 | 规则 |
|---|---|
| History 模式（默认） | 包装 `history.pushState`（先调用原函数，保留 `this`、参数与返回值）并监听 `popstate` `[01 §2.6]` |
| `replaceState` | 不包装、不算新 pageview `[01 §2.6]`；用 `replaceState` 做真实跳转的应用须用手动 `rivu('pageview')` |
| pathname 去重 | 记 `lastPath` = 上一个已发 pageview 的 `location.pathname`；SPA 导航后 pathname 相同（仅 query 或 hash 变化）则不发。`lastPath` 在每次实际发送时更新 `[01 §2.6]` |
| Hash 模式（`hash:true`） | 只监听 `hashchange`，**不**包装 `pushState`；不做 pathname 去重；所有事件带 `h:1`。站点若用 `#section` 锚点，每次点击都会产生 pageview，须在文档提示 `[01 §2.6]` |
| 次序 | 新 pageview 发出前，先对上一页 flush engagement（4.5），再重置 engagement 状态，最后发 pageview |
| 不采集 | `document.title`、页面内容、DOM 状态 |

### 4.4 bfcache 与生命周期
| 事件 | 行为 |
|---|---|
| `pageshow` 且 `persisted` | 视为新 pageview（不做 pathname 去重、不带 `r`），并重置 engagement `[01 §2.6]` |
| 进入 bfcache 前 | 依赖 `visibilitychange→hidden` 触发 engagement flush |
| `unload`/`beforeunload` | 不得使用（会使页面失去 bfcache 资格）；`pagehide` 不单独监听，`visibilitychange` 已覆盖 `[01 §2.5]` |

### 4.5 engagement 状态机（仅 `full`）
语义沿用 `[01 §2.4]`。状态变量：`url`、`props`（取自该页 pageview）、`ignored`、`maxPx`（最大滚动像素，初始即当前视口底部）、`sentPx`（初始 −1）、`acc`（累计可见毫秒）、`runStart`（0 表示未计时）。时钟用 `performance.now()`（单调；Plausible 用 `Date.now()` `[src]`）。

| 当前状态 | 触发 | 动作 | 新状态 |
|---|---|---|---|
| 无页面 | pageview 已发且未被忽略 | 重置变量；`runStart` = 页面可见**且有焦点**？now：0（Plausible 无条件设 now `[src]`） | Running 或 Paused |
| 无页面 | pageview 被忽略（4.7） | `ignored = true`，该页永不发 engagement `[01 §2.4]` | Idle |
| Running | `visibilitychange→hidden` 或 `blur` | `acc += now − runStart`；`runStart = 0`；`flush()` | Paused |
| Paused | `visibilitychange→visible` 或 `focus`，且可见且有焦点 | `runStart = now` | Running |
| 任意 | 下一个 pageview（SPA）发出前 | 先累计，再 `flush()`，再重置 | 见第 1 行 |
| 任意 | `scroll` / 文档高度变化（`ResizeObserver`） | 更新 `maxPx`、文档高度 | 不变 |

`flush()`：若 `ignored` 则返回；仅当 `maxPx > sentPx` 或 `acc ≥ 3000 ms` 才发送，否则**保留** `acc` 留待下次（短暂失焦不丢时长）。发送后：`sentPx = maxPx`，`acc = 0`，`runStart = 0`。首个 flush 必发（`sentPx = −1`）。

| 字段 | 计算 |
|---|---|
| `sd` | 文档高度 = body 与 documentElement 的 `scrollHeight/offsetHeight/clientHeight` 最大值；视口高 ≥ 文档高时 `maxPx` 取文档高（即 100）；`sd = clamp(round(100 × maxPx / 文档高), 0, 100)` `[01 §2.4]` |
| `e` | 自上次发送以来的增量毫秒（整数），不是累计 `[01 §2.4]` |
| `u`/`p`/`h` | 取自对应 pageview（即使之后 URL 已因 `replaceState` 改变）；`p` 为该页合并后的属性 |
| 发送方式 | `sendBeacon` 优先，不可用或返回 `false` 时回退 `fetch keepalive`（决策 7） |

已知限制：标签页被直接杀死而无 `visibilitychange` 时，最后一段参与度丢失（接受）。Plausible 运行时抓包未覆盖 engagement `[00 §5.5]`，本节行为来自源码，需补抓包验证（见 11.2）。

### 4.6 自动捕获（仅 `full`，经文档级委托监听 `click`、`auxclick`（仅中键）、`submit`）
| 能力 | 开关 | 事件名 | 触发与规则 | 属性 |
|---|---|---|---|---|
| 外链 | `outbound` | `Outbound Link: Click` | 从点击目标向上找 `<a href>`；`link.host` 非空且 `!== location.host`（含端口；子域算外链）`[01 §2.8]` | `url`：`link.href` 原样 |
| 文件下载 | `downloads` | `File Download` | 取 `link.pathname` 最后一个 `.` 之后、转小写后匹配扩展名表（不受 query/hash 影响，大小写不敏感；Plausible 对 `.PDF`、`#page=2` 不匹配 `[src]`） | `url`：去 query 与 hash |
| 表单提交 | `forms` | `Form: Submission` | `submit` 捕获阶段；表单有 `novalidate` 或 `checkValidity()` 为真；被 tagged 的表单不重复发 `[01 §2.8]` | 无（`pathname` 已在事件行，不补 `path` 属性） |
| tagged events | `tagged` | class 指定 | 见下 | class 指定 |

- 优先级：tagged > 外链 > 下载；一次点击只产生一个事件（外链先于下载，外部站点上的 `.pdf` 记为外链）。
- 默认扩展名（28 项）：`pdf xlsx xls docx doc txt rtf csv exe key pps ppt pptx 7z pkg rar gz zip avi mov mp4 mpeg wmv midi mp3 wav wma dmg`。前 26 项等同 Plausible 默认 `[01 §2.8]`，Rivu 追加 `xls`、`doc`；`downloads` 为数组时整体替换默认表。
- tagged 语法（前缀 `rivu-event-`）：class `rivu-event-name=Signup`（或 `rivu-event-name--Signup`，供不允许 `=` 的 CMS）定义事件名；`rivu-event-<k>=<v>` 定义属性，值中 `+` 换成空格；`rivu-revenue-amount=…`、`rivu-revenue-currency=…` 定义收入。元素本身或向上最多 3 层父元素命中；**多层嵌套时就近优先**（Plausible 取最外层 `[src]`）。表单内的点击不算，只算 `submit`；命中的元素是链接则自动补 `url` 属性 `[01 §2.8]`。
- 不拦截导航、不 `preventDefault`、不延迟跳转（`keepalive` 保证送达；Plausible 仅兼容变体做 5 秒延迟 `[01 §2.5]`）。

### 4.7 排除逻辑（每次发送前判定）
| 规则 | 条件 | 适用 |
|---|---|---|
| localhost | `hostname` 为 `localhost`、`127.x.x.x`、`[::1]`，或 `protocol === 'file:'`；配置 `local:true` 时跳过此条 `[01 §2.7]` | core+full |
| 自动化 | `navigator.webdriver` 或 `window.Cypress` 为真，且 `window.__rivu` 非真。Rivu 不再检查 `_phantom`/`__nightmare`（项目已停更，省体积） `[01 §2.7]` | core+full |
| 自助排除 | `localStorage.rivu_ignore === "true"`；读取抛异常则视为未设置。站点所有者在自己浏览器控制台手动设置。脚本只读该键：不写入、不删除、不用于标识，也不随事件发送（`system-design.md` §10 Q3） | core+full |
| 缺站点 | 无 `site` | core+full |

被忽略时：不发请求；调用 `callback()`（无参）；若是 pageview 则该页 engagement 禁用（4.5）；`full` 在 `debug`（默认关）时输出 `console.warn('[rivu] ignored: <reason>')`，`core` 静默。Plausible web 变体默认就打印警告 `[src]`（可选 `console.warn` 见 `[01 §2.7]`），Rivu 默认静默并提供 `debug` 与 `rivu.b` 供排查，对应 `[00 §7]` 对「忽略原因可观测」的要求。

### 4.8 手动 API（仅 `full`）
| 调用 | 说明 |
|---|---|
| `rivu('event', name, opts?)` | 自定义事件。`opts`：`props`（对象，值为 string/number/boolean，客户端转字符串；其余类型跳过）、`revenue`（`{amount, currency}`）、`interactive`（`false` 不计入跳出率）、`url`（覆盖 `u`）、`callback` |
| `rivu('pageview', opts?)` | 手动 pageview，等价 `rivu('event','pageview',opts)`；与自动 pageview 并存时不去重 `[01 §2.3]` |
| `rivu('set', {props})` | 全局属性，**替换**上一次的 `set`；与单次 `props` 合并，单次优先；对所有事件（含 pageview、engagement）生效 `[01 §2.9]` |
| `rivu('init', opts)` | 仅加载前有效，见 2.5 |
| 保留名 | `engagement` 不可经手动 API 发送（忽略）；`pageview` 走 pageview 流程（触发 engagement flush/重置） |
| `callback(result)` | 恰好调用一次：fetch 成功 `{status}`；网络错误 `{error}`；beacon 路径 `{}`；被忽略时无参 `[01 §2.5]` |
| 异常 | 所有命令内部吞异常，不抛给页面 |

```js
rivu('event', 'Purchase', { props: { plan: 'pro' }, revenue: { amount: 29, currency: 'USD' } })
rivu('event', 'Video Play', { interactive: false })   // 发 i:false，不影响跳出率
```

## 5. 线协议（`v=1`）
### 5.1 字段总表
请求体为单个 JSON 对象（UTF-8），键区分大小写；未知键忽略（前向兼容）；多错误一次性汇总返回。长度单位以各行为准（字节 ≠ 字符，见 `[01 §11.2 #1]`）。

| 键 | 类型 | 必填 | 适用事件 | 限制 | 服务端处理 | 依据 |
|---|---|---|---|---|---|---|
| `v` | integer | 是 | 全部 | 当前仅 `1`（N−1 见第 8 节） | 不支持 → 400 `v` | 决策 4 |
| `sv` | integer | 否 | 全部 | 0–999999 | 非法/缺失按 0，**永不 400**；只作计数标签，不影响行为 | `[01 §4.1]` |
| `s` | string | 是 | 全部 | `^[A-Za-z0-9_-]{8,32}$` | 格式错 → 400 `s: has an invalid format`；格式合法但不存在 → 202+计数（`unknown_site`）。实际生成的 key 是 12 位 base32 小写（`^[a-z2-7]{12}$`，PG 迁移与 `platform-and-ops.md` §3.3），线协议的正则故意更宽松：它只是向前兼容的语法检查，将来调整 key 的生成格式时，已发布的脚本和协议都不用改 | 决策 3；`system-design.md` §10 Q11 |
| `n` | string | 是 | 全部 | trim 后非空，≤120 **字符**，不含控制字符 | `pageview`、`engagement` 为系统事件名，其余为自定义 | `[01 §2.2]` |
| `u` | string | 是 | 全部 | 只受请求体 8 KB 上限约束；scheme 仅 `http`/`https` | 规范化见第 7 节。超长不回 400：先丢弃白名单以外的 query 参数，再把 pathname 截断到 1024 字节。Plausible 对超过 2000 字节的 URL 回 400，会丢掉整条 pageview，而长 URL 多数是广告和跟踪参数造成的。无法解析或 scheme 不合法时仍回 400 | `[01 §4.1]` |
| `r` | string 或 null | 否 | 主要 pageview | ≤2000 **字符** | null/空串视为缺失；超长**截断**不报错；其余类型 → 400 | `[01 §2.2]` |
| `p` | object | 否 | 全部 | ≤30 键；键 ≤300 **字节**；值 ≤2000 **字节** | 值须为 string（number/boolean 容忍并转字符串，null/数组/对象 → 400）；空键或空值丢弃；超限 → 400（不截断） | 决策 4；`[01 §4.2]` `[00 §5.4]` |
| `i` | boolean | 否 | 自定义事件 | 默认 `true`，客户端仅在 `false` 时发送 | 对 `pageview`/`engagement` 忽略（恒为 true） | `[01 §2.2]` |
| `h` | integer | 否 | 全部 | 仅 `1` 生效 | `1` 时 pathname 追加 `#fragment`；其他整数视为未设置；非整数 → 400 | `[01 §4.1]` |
| `sd` | integer | 条件 | `engagement` | 0–100 | >100 夹到 100；null/负数/非整数视为缺失；非 engagement 忽略 | `[01 §4.3]` |
| `e` | integer | 条件 | `engagement` | 毫秒，< 30 天（2,592,000,000） | 越界/缺失/非法视为 0；非 engagement 忽略 | `[01 §4.3]` |
| `$` | object | 否 | 自定义事件（第 4 阶段） | `{"a":"12.50","c":"USD"}`；`a` 匹配 `^\d{1,15}(\.\d{1,4})?$`，`c` 为 ISO 4217 大写三字母 | 非法 → 400 `$`；对 `pageview`/`engagement` 忽略；是否入库由站点收入目标决定 | `[01 §2.9]` |

`engagement` 必须至少带有效的 `sd` 或 `e` 之一，否则 400（`[01 §4.3]`，决策 8）。时间字段不存在：任何 `t`/`ts` 之类的键一律忽略。

### 5.2 请求示例
示例 1：pageview（服务端据此得到 hostname=`example.com`、pathname=`/pricing`、`utm_source=newsletter`、`utm_medium=email`；`token`、`#plans` 丢弃；referrer 存为 `news.example.org/post/42`）

```json
{"v":1,"sv":3,"s":"k7qm2xv5atbr","n":"pageview","u":"https://www.Example.com/pricing?utm_source=newsletter&utm_medium=email&token=abc123#plans","r":"https://news.example.org/post/42?id=7"}
```

示例 2：自定义事件（带属性与收入；`i` 缺省即 `true`；`$` 在第 4 阶段前被服务端忽略）

```json
{"v":1,"sv":3,"s":"k7qm2xv5atbr","n":"Purchase","u":"https://example.com/checkout/done","p":{"plan":"pro","seats":"5"},"$":{"a":"29.00","c":"USD"}}
```

示例 3：engagement（用户滚到 72%，自上次上报起参与 18.45 秒）

```json
{"v":1,"sv":3,"s":"k7qm2xv5atbr","n":"engagement","u":"https://example.com/pricing","sd":72,"e":18450}
```

## 6. HTTP 契约
### 6.1 方法、头与响应
| 项 | 规则 |
|---|---|
| 路由 | `POST /api/event`、`OPTIONS /api/event`；其他方法 `405` + `Allow: POST, OPTIONS`；其他路径 `404` |
| 请求 `Content-Type` | 接受 `text/plain`（含 `;charset=UTF-8`，`sendBeacon` 字符串体即此类型）与 `application/json`；**实际忽略该头**，一律按 UTF-8 JSON 解析（容忍 `Accept: */*`，同 `[01 §11.2 #13]`） |
| 请求头大小 | 请求行与单个头值 ≤ 8192 B（同 `[01 §3.1]`），总头部 ≤ 16 KB；超限由 HTTP 层回 `414`/`431`，不属协议错误 |
| 请求体 | JSON 对象，≤ **8192 B**；先看 `Content-Length`，分块传输按流式累计，超限即 `413` 并断开；读体超时 5 s 直接断开 |
| 客户端 IP | 仅当对端地址属于配置的可信代理名单时才采用 `X-Forwarded-For`/`Forwarded`/`CF-Connecting-IP` 等头；否则用连接对端 IP。解析失败回退对端 IP，不得 panic `[01 §4.4]` `[01 §11.2 #7]` `[v1 §2]` |
| Cookie | 服务端忽略请求 `Cookie`，响应不得带 `Set-Cookie`（`sendBeacon` 恒带凭证，故必须保证无副作用） |
| 所有响应 | `Access-Control-Allow-Origin: *`、`Access-Control-Expose-Headers: x-rivu-dropped, Retry-After`、`Cache-Control: no-store`、`Cross-Origin-Resource-Policy: cross-origin`；**不带** `Allow-Credentials` |

### 6.2 状态码
| 状态 | 条件 | 响应体 | 附加头 |
|---|---|---|---|
| `202` | 事件被接受，或被过滤（机器人、未知站点、站点停用、限流、hostname 不在白名单、屏蔽规则、无会话的 engagement 等） | 空 | 有丢弃时 `x-rivu-dropped: 1`（本端点一请求一事件，值恒为 1；保留数量语义供批量端点使用） |
| `400` | JSON 不合法、非对象、缺必填、类型错、超长、`v` 不支持、`s` 格式错、engagement 缺 `sd`/`e`、`u` 非法 | `{"errors":{"字段":["消息"]}}`，`application/json` | — |
| `404`/`405` | 路径/方法错误 | 空 | `Allow`（405） |
| `408` | 读请求体超过 5 s | 空 | `Connection: close` |
| `413` | 请求体 > 8192 B | `{"errors":{"body":["is too large (max 8192 bytes)"]}}` | `Connection: close` |
| `414`/`431` | HTTP 层 | 空 | — |
| `503` | 写缓冲满，或 JetStream 不可用 | 空 | `Retry-After: <秒>`（1–10 随机抖动）；客户端不重试，供代理与监控使用 |
| `500` | 未预期错误 | 空 | 计数 `internal_error`，不得泄露细节 |

错误消息固定词表：`is required`、`must be a string`、`must be an integer`、`must be a boolean`、`must be an object`、`must be a JSON object`、`is not valid JSON`、`is too long (max N)`、`has too many keys (max 30)`、`is not supported`、`has an invalid format`、`must be a valid url`、`scheme is not allowed`、`at least one of sd or e is required`。其中请求体整体错误的键为 `body`。`has an invalid format` 用于 `s` 格式错、`n` 含控制字符、`$` 字段格式错（`a`、`c` 不符合格式）；`is not supported` 只用于 `v`（`system-design.md` §10 Q18）。

### 6.3 CORS 与预检
| 场景 | 行为 |
|---|---|
| 浏览器脚本（`text/plain`） | 属简单请求，不触发预检 `[01 §2.5]` |
| `application/json` 或带自定义头的跨域调用 | 先发 `OPTIONS`：`204`，`Access-Control-Allow-Origin: *`、`Access-Control-Allow-Methods: POST, OPTIONS`、`Access-Control-Allow-Headers: Content-Type`、`Access-Control-Max-Age: 86400` |
| 凭证与 `sendBeacon` | 不带 `Access-Control-Allow-Credentials`（Plausible 在 `*` 下仍回 `true` `[00 §5.4]`，对 `*` 无效，Rivu 不沿用）；beacon 字符串体为简单请求，响应不可读，无影响 |

### 6.4 Collector 处理次序与响应映射（衔接 `[v1 §2]`，其中「返回 204」以本规范 `202` 为准）
| # | 阶段 | 失败结果 | 内部计数 |
|---|---|---|---|
| 1 | 方法/路径、体积（413）、读体超时（408） | `413` / `408` | `rejected_body_size`（413，只进 Prometheus） |
| 2 | 解析 JSON 与字段校验、`u` 解析（400） | `400` | `rejected_<field>`，只进 Prometheus，维度含 `sv`，**不含**站点，不进 `ingest_counters` |
| 3 | 站点配置（进程内缓存，不查 PG）：未知 `s`（已删除的站点从快照摘除后同样如此） | `202`+dropped | `dropped_unknown_site`（`site_id = 0`） |
| 4 | 站点状态、配额、限流 | `202`+dropped | `dropped_site_disabled` / `dropped_rate_limited` |
| 5 | hostname 白名单（第 7 节） | `202`+dropped | `dropped_hostname_not_allowed` |
| 6 | 屏蔽规则、垃圾引荐、机器人、国家 | `202`+dropped | `dropped_ip_blocked` / `dropped_page_blocked` / `dropped_spam_referrer` / `dropped_bot` / `dropped_datacenter_ip` / `dropped_country_blocked` |
| 7 | 身份：事件的站点本地日期没有盐（collector 不现场生成盐） | `202`+dropped | `dropped_salt_missing` |
| 8 | engagement 先查会话（在昂贵的 UA/地理富化之前） | `202`+dropped | `dropped_no_session` |
| 9 | 富化、写缓冲/JetStream | `503` | `dropped_overloaded` |

`ingest_counters` 的维度为 `{分钟, 指标, site_id, script_version}`（进程内按 10 秒桶累加，桶用接收请求时的墙钟）；`sv` 超出 UInt16 的钳到 65535。未知站点统一记 `site_id = 0`，避免用户可控标签撑爆基数 `[01 §10.1]`。丢弃原因以 Rust 枚举 `DropReason` 为唯一来源（`system-design.md` §4.2、§10 Q13），指标名为 `dropped_<原因>`，仅内部可见。

## 7. 服务端规范化规则
| 对象 | 规则 | 依据 |
|---|---|---|
| URL 解析 | 按 WHATWG URL 解析（Rust `url` crate）；失败 → 400 `u: must be a valid url`；scheme 非 `http/https` → 400 `u: scheme is not allowed`；无 host 不可能通过解析（Plausible 回退字面量 `(none)`） | 决策 4/5；`[01 §4.1]` |
| hostname | 取 host（IDN 为 punycode）→ 小写 → 去结尾 `.` → 去**一个**前导 `www.` → 去端口。IP 字面量原样保留。不 trim 之外的任何改写 | 决策 5；`[01 §4.1]` `[01 §11.2 #2]` |
| 白名单 | 站点 `hostnames` 以相同规则规范化后存储；精确匹配，或 `*.example.com` 通配（匹配任意深度子域，**不含**根域 `example.com`；因 `www.` 被去除，`www.example.com` 规范为根域，需单独列根域）。通配只允许最左一段。**白名单为空 → 全部丢弃**（fail-closed；Plausible 无规则即放行） | 决策 3；`[01 §7.2]` |
| pathname | `url.path()` 空则 `/`；保留大小写与结尾 `/`；不含 query。再做**安全解码**：按字节解码 `%XX`，但保留 `%2F %5C %3F %23 %25` 与控制字符（<0x20、0x7F）的转义形态；若出现非法转义或解码结果非 UTF-8，则整串保留原样（不报错）。解码后长度 ≤ 原长 | 决策 5；`[01 §4.1]` `[01 §11.2 #3]` |
| hash 模式 | 仅 `h=1` 且 fragment 非空：取 fragment，**在首个 `?` 处截断**（hash 路由里的 query 同样视为敏感）；若不以 `/` 或 `!/` 开头且含 `=`（形如 `#access_token=…`）则丢弃；其余追加 `"#" + 安全解码(fragment)`。截去的 `?` 部分按下一行规则仅提取白名单参数，且优先级低于真实 query | `[01 §2.6]` `[01 §4.1]` |
| query 白名单 | 用 `form_urlencoded` 解析（非法转义原样保留，非 UTF-8 以 U+FFFD 替换，不报错）。取 `utm_source`、`utm_medium`、`utm_campaign`、`utm_content`、`utm_term`；来源回退键 `source`、`ref`（顺序 `utm_source` → `source` → `ref`）。键区分大小写；同名取首个；值 trim、去控制字符、截断 255 字符。**其余参数一律丢弃**，不存储、不写日志 | 决策 5；`[01 §4.5]` |
| 空串即缺失 | 上述所有参数、`r`、`p` 的键/值，空串（trim 后）视为缺失，不压过回退项：`?utm_source=&ref=x` 取 `ref=x`；`utm_medium=` 为空则按未设置 | 决策 5；`[01 §4.5]` `[01 §11.2 #5]` |
| click id | 按顺序取第一个**值非空**的参数，**只存参数名**：`gclid`、`gbraid`、`wbraid`、`msclkid`、`fbclid`、`twclid`、`ttclid`、`li_fat_id`（前 6 项同 `[01 §4.5]`，后 2 项为 Rivu 追加）。来源与渠道推断（只有 click id 时按参数名推断来源，`gclid` `gbraid` `wbraid` `msclkid` `ttclid` 配同平台来源算付费证据；不像 Plausible 那样改写 medium）属富化阶段，不在本规范（`system-design.md` §4.2、§10 Q20） | 决策 5；`[01 §4.5]` `[01 §8.4]` |
| referrer | 先按字符截断到 2000；解析为 URL，scheme 须为 `http`/`https`/`android-app`，host 非空；host 按 hostname 规则规范化；**若 host 命中本站白名单则视为内部**（不作来源；Plausible 为 host 全等，子域互跳算外部 `[01 §8.3]` `[01 §11.2 #6]`）。存储形态：`host + path`（去结尾 `/`，无 query、无 fragment）。浏览器默认 Referrer Policy 下跨站 referrer 只有 origin，所以 path 通常为空（可信度见 `system-design.md` §6.10） | `[01 §8.3]` |
| props | 键 trim；空键/空值丢弃；number/boolean 转字符串（布尔为 `true`/`false`）；null/数组/对象 → 400；> 30 键 → 400（不静默截断）；键 > 300 B 或值 > 2000 B → 400。保留 JSON 对象顺序，同名键后者覆盖 | 决策 4；`[01 §4.2]` `[01 §11.2 #4]` |
| 事件名 | trim；≤120 字符；含控制字符 → 400；不改大小写 | `[01 §4.1]` |
| interactive | `pageview`/`engagement` 恒 `true`；自定义事件仅 `i === false` 为 false | `[01 §2.2]` |
| engagement 字段 | 入库哨兵：`sd` 缺失/非法 → **255**（未上报），有效为 0–100；`e` 缺失/非法 → **0**；非 engagement 事件的 `sd=255、e=0`。聚合时必须排除 255 | `[01 §4.3]` |
| 时间戳 | 接收时 `now_utc`，毫秒精度，整请求共用 | 决策 4；Plausible 为秒 `[01 §4.1]` |

## 8. 版本与兼容策略
| 项 | 规则 |
|---|---|
| `v` 的含义 | 线协议的整数版本，当前 `1`。**不随脚本发布变化**；Plausible 的 `v` 是脚本版本，仅用于计数 `[01 §4.1]` `[00 §5.4]`，Rivu 把两者拆开 |
| 不升版的变更 | 新增可选字段、新增事件名、放宽限制、新增响应头。服务端忽略未知字段，客户端不依赖未文档化的响应字段 |
| 必须升版的变更 | 已有字段的语义/单位/取值域改变、删除字段、新增必填字段、收紧限制导致原合法请求被拒 |
| 服务端兼容窗口 | 当前版本 N 时，必须同时接受 N 与 N−1；`v` 超出 `[N−1, N]` → 400 `v: is not supported`。`v=1` 阶段不存在 N−1，仅接受 1 |
| 下线 N−1 | 须同时满足：发布 N 后 ≥ 12 个月；连续 30 天 `v=N−1` 事件占比 < 0.1%（用 6.4 的计数）。下线前至少一个次要版本在发行说明中公告 |
| 为什么需要窗口 | 站点脚本最多缓存 5 分钟，但第三方自托管的 `t.js` 副本可能长期不更新 |
| `sv` 的用途 | CI 生成的单调递增整数（`__SV__`）。只用于计数标签、排障（定位哪个构建在产生 400 或异常流量）、安装检测；服务端**不得**依据 `sv` 改变协议行为（行为分支只看 `v`） |

## 9. Plausible 兼容层设计（第 4 阶段提供，已决定）
`system-design.md` §10 Q4 已决定在第 4 阶段提供兼容端点；本节是设计，第 4 阶段之前不实现。端点 `POST /api/plausible/event`，目标：Plausible 用户把 `data-api`（旧式脚本）或 `endpoint`（`plausible-web`/npm 的 init 选项）指向 Rivu 即可复用现有集成 `[01 §2.1]` `[01 §2.10]`。Plausible tracker 为 MIT，可以在文档中引导用户继续使用其脚本；兼容层只按公开协议行为独立实现。限制与 HTTP 契约（8 KB、CORS、可信代理 IP、不读 `x-plausible-ip` 等无条件信任头 `[01 §4.4]`）与原生端点一致。

| Plausible 字段（短键/长键） | Rivu 内部字段 | 转换与差异 |
|---|---|---|
| `n` / `name` | `n` | 整数转字符串（Plausible 用于 404 跟踪 `[01 §2.2]`） |
| `u` / `url` | `u` | 同原生规则 |
| `d` / `domain` | `s`（经站点解析） | 规范化（小写、去 `www.`）后与站点的**非通配**白名单项精确匹配；缺失时回退 `u` 的 host `[01 §2.2]`；命中 0 个或多个站点 → `202`+dropped；含 `,`（多域名扇出 `[01 §1.2]`）→ 400 `d`，不支持 |
| `r` / `referrer` | `r` | 非字符串忽略，按字符截断 2000 `[01 §2.2]` |
| `p` / `props` / `m` / `meta` | `p` | 对象或 JSON 字符串；**宽松模式**：数组/对象值静默丢弃，> 30 键取前 30（按键序，确定性），键/值超长仍 400 `[01 §4.2]` |
| `i` / `interactive` | `i` | 同原生 |
| `h` / `hashMode` | `h` | 仅整数 `1` 生效 |
| `sd`、`e` | `sd`、`e` | 接受整数或整串数字字符串 `[01 §4.3]` |
| `$` / `revenue` | `$` | `{amount, currency}` → `{a, c}`；amount 为数字或可解析字符串，转十进制字符串 `[01 §2.9]` |
| `v`（脚本版本，如 36 `[00 §5.4]`） | `sv` | 协议版本固定为 `v=1`；`sv` 用于计数，标签 `compat=plausible` |

响应：`202` + 文本体 `ok`（`[00 §5.4]` 实测）+ `x-plausible-dropped` + `x-rivu-dropped`；400 体同原生格式（Plausible 的 `{"errors":{字段:[消息]}}` `[01 §3.2]`，键名为其长键名时保持原键，如 `url`、`domain`）。不提供 `/js/plausible*.js`，也不复刻 `GET /api/error`。

## 10. 与 Plausible 的差异
### 10.1 逐条差异
| # | Rivu | Plausible | 原因 | 证据 |
|---|---|---|---|---|
| 1 | 不透明 `site_key`（`s`），一请求一站点 | 域名 `d`，可逗号分隔扇出多站点 | 域名变更与过渡期会造成缓存键/访客 ID 不一致；扇出增加复杂度 | `[01 §1.2]` `[01 §6.5]` `[01 §2.10]` |
| 2 | 时间戳毫秒 | 秒 | 事件排序与 engagement 对齐更精细 | `[01 §4.1]` |
| 3 | `202` 空体 + `x-rivu-dropped` | `202` + 文本 `ok` + `x-plausible-dropped` | 省字节；原因不外露 | `[00 §5.4]` `[01 §3.2]` |
| 4 | 被过滤一律 `202`，400 仅协议错误；`i` 对系统事件忽略（均为**借鉴**，无差异） | 同 | 不泄露站点存在性，客户端不重试 | `[01 §0 #1]` `[01 §3.3]` `[01 §2.2]` |
| 5 | 新增 `503`+`Retry-After` | 无（写缓冲交接内部） | 写路径要给代理/监控明确过载信号 | `[v1 §2]`；决策 2 |
| 6 | CORS 不带 Allow-Credentials，并 Expose 自定义头 | `*` 同时 `Allow-Credentials: true` | `*` 与凭证不可同用；beacon 响应不可读 | `[00 §5.4]` |
| 7 | engagement/隐藏页 `sendBeacon` 优先，回退 `fetch keepalive` | 仅 `fetch keepalive`，无 beacon | 卸载时送达率；旧 Firefox 的 keepalive 支持 | `[01 §2.5]` `[00 §3.3]` |
| 8 | 两个构建（core/full） | 约 17 个变体，旧式文件名为特性幂集 | 维护与缓存成本 | `[00 §3.6]` `[01 §2.1]` `[00 §7]` |
| 9 | 全部开关内联进站点脚本，且由单一映射函数生成并测试 | 只内联 5 个键，其余开关入库但不进脚本 | 避免库内开关与脚本脱节 | `[01 §0 #7]` `[01 §11.2 #12]` |
| 10 | 请求体 ≤ 8 KB（413） | 默认 1,000,000 B，超限被当 JSON 错误回 400 | 采集端点无需大体；413 语义明确 | `[01 §3.2]` |
| 11 | 超过 30 个属性 → 400；非标量 → 400；空值丢弃 | 超 30 个取舍不确定；非标量静默丢弃 | 确定性，错误可见 | `[01 §4.2]` `[01 §11.2 #4]` |
| 12 | `u` 仅 `http/https` 且必须有 host；hostname 小写、去结尾 `.`、去 `www.`、punycode | 只拒绝 `data:`；无 host 记 `(none)`；只去小写前缀 `www.`，未见统一转小写 | 防 `javascript:` 与垃圾 hostname；避免大小写导致 `not_found` | `[01 §4.1]` `[01 §11.2 #2]` |
| 13 | pathname 安全解码，非法转义保留原串；保留 `%2F` 等 | `URI.decode`，非法转义行为未定义 | 不让用户输入触发异常或改变路径结构 | `[01 §11.2 #3]` |
| 14 | hash 路由截断 `?` 之后，丢弃 `#access_token=…` 类 fragment | 追加整个 fragment | 敏感信息不入库 | `[01 §4.1]`（决策 5 原则的延伸） |
| 15 | 空串参数视为缺失；追加 `ttclid`、`li_fat_id` | 空串压过回退；6 种 click id | 修正陷阱 | `[01 §4.5]` `[01 §11.2 #5]` |
| 16 | 内部引荐按站点白名单判定；`r` 仅文档首个 pageview 携带 | host 全等；每个事件带 `document.referrer` | 子域互跳不应算外部来源；避免过期来源 | `[01 §8.3]` `[01 §11.2 #6]` `[src]` |
| 17 | hostname 白名单必填，fail-closed | 可选 shield 规则，无规则放行 | 降噪与防冒用 | `[01 §5.2 #4]` `[01 §7.2]` |
| 18 | `v` 协议版本 / `sv` 构建号分离，服务端兼容 N−1 | `v` 即脚本版本，仅计数 | 协议可演进 | `[01 §4.1]` `[00 §5.4]` |
| 19 | 不补 `path` 属性 | 对 `404`/表单事件补 `path` | 事件行已有 pathname | `[01 §4.2]` 步 3 |
| 20 | engagement 无会话先查再丢，在富化之前 | 先做完 UA/地理/shield 才发现无会话 | 省 CPU | `[01 §11.2 #14]` |
| 21 | 400 也计数（按字段、`sv`，不含站点）；未知站点计入 `_unknown_` | 400 不计数；域名作标签，基数不受控 | 发现脚本发布引入的协议回归；防内存膨胀 | `[01 §10.1]` |
| 22 | 排除：仅 `webdriver`/`Cypress`；`rivu_ignore`；无客户端路径规则/`transformRequest` | 另含 `_phantom`、`__nightmare`、`data-include/exclude`、`transformRequest` | 体积与单一规则源 | `[01 §2.7]` |
| 23 | 额外监听 `prerenderingchange` | 仅 `visibilitychange` | Speculation Rules 预渲染 | `[01 §2.6]` |
| 24 | 下载扩展名小写且按 `pathname` 匹配；`xls`、`doc` 入默认表；tagged 就近优先 | 区分大小写、含 hash 时失配；最外层优先 | 减少漏报 | `[01 §2.8]` `[src]` |
| 25 | 无导航延迟、无 XHR | 兼容变体有 5 秒延迟与 XHR | 体积与体验 | `[01 §2.5]` |
| 26 | 收入格式 `$`:{a,c} 公开定义（第 4 阶段） | 仅 EE 解析，CE 为空操作 | 开源版需完整功能；只参考行为契约 | `[01 §2.9]` |

### 10.2 相对 v1 草案的修订
以本规范为准：产物名 `tracker.min.js`/`tracker.ext.js` → `core`/`full`；字段 site/type/path/ref → 短键 s/n/u/r（pathname 由服务端提取）；成功返回 204 → 202 空体；「不使用 localStorage」→ 仅只读 `rivu_ignore`（`system-design.md` §10 Q3 已决定）；「不直接信任 X-Forwarded-For」沿用，落实为可信代理名单（6.1）。

## 11. 测试清单
### 11.1 协议契约测试（Rust，表驱动）
| 类别 | 用例 |
|---|---|
| 状态码 | 合法 → 202 空体；未知 `s`/hostname 不在白名单/机器人 UA → 202 + `x-rivu-dropped: 1`；各 400 场景逐一断言 JSON 体键与消息；`503` 带 `Retry-After`（注入写缓冲满/JetStream 故障） |
| 体积边界 | 8192 B 通过、8193 B → 413；分块无 `Content-Length` 超限 → 413；非 UTF-8、非对象、`null`、嵌套超深 → 400 |
| 字段边界 | `n` 120/121 字符（含多字节）；`u` 超长 query 被丢弃、pathname 截断到 1024 字节，不回 400；`r` 超长截断；30/31 个属性；键 300/301 B；值 2000/2001 B；`p` 值为数字/布尔被转换，为 `null`/数组/对象 → 400 |
| engagement | 仅 `sd`、仅 `e`、两者皆缺 → 400；`sd=101` 夹为 100；`e` ≥ 30 天 → 0；`sd:null`；无会话 → 202 + 计数；非 engagement 的 `sd/e` 被忽略 |
| 规范化 | `www.`/大小写/尾点/端口/punycode；`*.example.com` 不匹配根域；空白名单 fail-closed；`%zz`、`%2F`、`%00`、非 UTF-8 转义保留；hash 模式 `?`、`#access_token=` 处理；`?utm_source=&ref=x` 取 `ref`；click id 只存名；非白名单参数不出现在任何落库/日志字段 |
| 头与版本 | `text/plain;charset=UTF-8`、`application/json`、缺失、`*/*` 均可解析；`Set-Cookie` 永不出现；`v=1` 通过，`v=0/2/"1"` → 400；`sv` 非法按 0 不 400 |
| 脚本端点 | 构建选择表驱动（开关组合 → core/full）；ETag/304；未知 key → 404；前缀转义（`</script>`、U+2028）；前缀由映射函数生成，改一个开关则 ETag 变化 |
| 兼容层（第 4 阶段） | 回放 `[00 §5.4]` 抓到的真实请求体（含布尔属性 `logged_in:false`、`r:null`、`v:36`）→ 202；长键与短键等价；`d` 含逗号 → 400 |
| 模糊/属性 | 对规范化函数做幂等与不 panic 的 fuzz；`cargo-fuzz` 对 URL、JSON 解析入口 |

### 11.2 浏览器行为测试（Playwright，夹具在 `localhost`，故测试页须设 `local:true` 与 `window.__rivu = true`）
| 场景 | 预期 |
|---|---|
| 前置断言：Playwright Chromium 下 `navigator.webdriver === true` | 通过（`[00 §5.1]` 仅为推断，此处实测验证） |
| 默认设置下加载页面（webdriver 为真，无 `__rivu`） | 0 个 `/api/event` 请求；`full`+`debug` 有一条 `ignored` 警告；`core` 无输出 |
| `addInitScript` 设 `window.__rivu = true` | 发出 1 个 pageview；覆盖 `navigator.webdriver` 为 `false` 同理 |
| `localStorage.rivu_ignore = "true"`；`localStorage` 抛异常（`defineProperty`） | 前者 0 请求；后者照常发送 |
| `hidden` 页面加载，之后切为 `visible` | 切换前 0 请求，切换后恰好 1 个 pageview；`prerendering` 路径同理 |
| `pushState` 到新 pathname / 仅改 query / `replaceState` / `popstate` | 新 pathname → 1 个 pageview；仅改 query、`replaceState` → 0；`popstate` 回到不同 pathname → 1 |
| hash 模式 `hashchange` | 每次变更 1 个 pageview，均带 `h:1`；不去重 |
| bfcache：导航离开再 `back` | `persisted` 恢复产生新 pageview，无 `r` |
| 请求形态 | 方法 POST、`Content-Type: text/plain`、`credentials` 不带 Cookie、无 `OPTIONS` 预检、体 < 8 KB；资源类型 `ping`（Chromium 的 beacon）可区分 |
| engagement | 用 `page.clock` 与新开标签 `bringToFront`/CDP `Emulation.setFocusEmulationEnabled` 制造可见性与焦点变化：<3 s 且未更深滚动 → 不发；≥3 s → 发且 `e` 为增量；更深滚动 → 发；SPA 跳转前先发 engagement 再发 pageview；被忽略页面不发；使用 beacon，`sendBeacon` 返回 `false` 时回退 fetch。**并补抓真实站点 engagement 请求**（`[00 §5.5]` 遗留） |
| 自动捕获 | 外链/下载（含大写扩展名、`?x=1#h`）/表单（校验失败不发）/tagged（嵌套就近、`--` 写法、`+`→空格、点击表单内元素不发）；一次点击一个事件；导航不被拦截 |
| 手动 API | 桩队列回放顺序（`init`/`set` 先于首个 pageview）；`set` 与单次 `props` 合并；`callback` 恰好一次；异常不外泄；`rivu('init')` 加载后调用无效；重复加载脚本只初始化一次 |
| 无副作用 | 全流程 `document.cookie` 为空；`localStorage`/`sessionStorage`/IndexedDB 无新增键（Playwright 检查存储状态） |

### 11.3 体积与产物测试
| 测试 | 断言 |
|---|---|
| 预算 | core ≤ 1,024 B、full ≤ 2,048 B（`gzip -9`）；输出表含 brotli 与原始大小 |
| 回归 | 与基线对照，按 3.3 阈值预警/拦截 |
| 泄漏与禁用 API | core 不含 full 模块；扫描结果见 3.3 |
| 其他 | 最坏配置前缀 ≤ 512 B；`t.js` 与 `full.js` 逐字节相同；两次构建哈希一致；`es-check es2018` 通过 |

## 附录：待拍板问题
1. `full` 估算仅余约 18 B；超预算时是否允许把预算提到 2.5 KB，还是按 3.2 顺序削功能。
2. `r` 超长是截断（本稿，同 Plausible）还是按决策 2 的「超长」返回 400。
3. ~~`u` 超 2000 B 返回 400 会丢整条 pageview~~：已定，服务端对超长 `u` 不回 400，先丢弃白名单以外的 query，再把 pathname 截断到 1024 字节（§5.1、`system-design.md` §10 Q18）；客户端不需要预先剥离。
4. 兼容层 `d` 含逗号（多站点扇出）按 400 拒绝，是否可接受。
5. 白名单为空 fail-closed、`*.example.com` 不含根域；`r` 仅文档首个 pageview 携带（偏离 Plausible）；DNT/GPC 是否进入后续阶段。
6. 可选的新字段（新增可选字段不升版，§8）：在 `Macintosh` UA 上附带 `navigator.maxTouchPoints` 可把 iPadOS 桌面 UA 改判为平板；高熵 Client Hints 的 `platformVersion` 可区分 Windows 10 / 11。两者都会让 tracker 多发数据或多读一个 API，不属于 P1，P2 以后再议（`system-design.md` §10 Q22、Q23）。
