# 与 Plausible CE 并行回放（2026-10）

brief-p1 的验收项：「与 Plausible CE 并行回放同一请求流，所有差异都能对应到「刻意偏离清单」」。本文记录一次完整回放的结果，并把每一类差异对应到 `system-design.md` 附录 C。

## 1. 结论

- 访客数、浏览量、跳出率、平均访问时长：三个站点里 rivu 与 Plausible 的差距最多 1 个访客、3 个浏览量，跳出率与访问时长在 Plausible 的取整精度内相同。
- 20 000 个事件中，只有 23 个（0.12%）两边判定不同，起因都是机器人名单不同（附录 C 的 UA-7）。
- 维度上的差异有四类：UA 解析库的命名与粒度（UA-6）、设备类别只看 UA（UA-8）、来源展示名（Source-R1）、地理库版本（Geo-1）。没有找到归不进清单的差异。

## 2. 方法

`tools/replay/run.sh`（WSL 里运行）：

1. 用 `tools/replay/plausible/compose.yml` 起 Plausible CE v3.0.1（PostgreSQL 16、ClickHouse 24.12，端口只绑 127.0.0.1）。
2. 用 `/app/bin/plausible rpc` 建用户和 3 个公开站点，时区 UTC。再等 70 秒，等 Plausible 的站点缓存刷新：新站点在缓存刷新前的事件会被当作未知站点丢弃（R01 §11.2 #11）。第一次回放没有等，开头 405 个事件因此被丢弃。
3. 在 rivu 里建同域名的站点，再建一个只读 API key，然后启动 `rivu all`。
4. 用 `loadgen --dump` 生成固定的事件流：20 000 个事件，覆盖 3 个站点，带 IP、UA、referrer、UTM、engagement，其中混有约 1% 的机器人 UA。UA 取自 uap-core 测试语料，偏旧。
5. `replay send` 以每秒 80 个事件的速率，把同一个事件同时发给两边。两边各用自己的协议：rivu 用 `/api/e`，Plausible 用 `/api/event`；`X-Forwarded-For` 相同。发完后，把两边判定不同的事件写入 `send.mismatches.jsonl`。
6. 等 20 秒，让两边都写入，然后 `replay compare` 拉取当天的统计：rivu 用查询 API，Plausible 用内部统计 API（`/api/stats/<domain>/…`）。

产物在 `plausible-replay-2026-10/`，含 `compare.json`、`compare.txt`、`send.json`、`send.mismatches.jsonl`、`run.log`。事件流本身有 6.6 MB，没有放进来，用同样参数的 `loadgen --dump` 可以重新生成。

## 3. 核心指标

| 站点 | 访客（rivu / Plausible） | 浏览量 | 跳出率 % | 平均访问时长 s |
|---|---|---|---|---|
| site1 | 2899 / 2899 | 6183 / 6185 | 42.53 / 43 | 51.65 / 52 |
| site2 | 1424 / 1423 | 3019 / 3016 | 42.35 / 42 | 52.01 / 52 |
| site3 | 926 / 926 | 1963 / 1963 | 44.17 / 44 | 50.10 / 50 |

回放用时 250 秒。两边对 20 000 个请求都回 202，错误数为 0。rivu 丢弃 196 个，Plausible 丢弃 199 个。

浏览量的小差异（site1 少 2 个，site2 多 3 个）都来自下面 §4 判定不同的 pageview。按页面看，所有差异都是个别页面上相差 1，没有一个页面只出现在一边，site2 的 `/docs/guide/page-694` 除外：这个页面只出现在 rivu，它唯一的浏览来自一个被 Plausible 判为机器人的 UA。

## 4. 判定不同的事件（23 个）

| 只被谁丢弃 | pageview | engagement | UA | 原因 |
|---|---|---|---|---|
| Plausible | 7 | 6 | `… Android 2.3.7; … CyanogenMod 7.2 RC1 for HTC Flyer by XK Project …`（8 个）、`… Android 2.3.3; fr-fr; X10project Build/GRI40 …`（5 个） | UAInspector 的机器人规则误判。用 RPC 实测，`UAInspector.bot?/1` 对这两个 UA 都返回 `true`，大概是 `XK Project` 和 `X10project` 碰上了它的某条爬虫正则。两者都是 Android 2.3 定制 ROM 的真实浏览器 UA |
| rivu | 6 | 4 | `… HeadlessChrome/59.0.3071.109 …`（8 个）、`Mozilla/4.0 (compatible; MSIE; ZyBorg; Win32)`（2 个） | rivu 的自维护规则判为机器人（`enrich-ua.md` §4）。Plausible 本来也想丢弃 Headless Chrome，但 UAInspector 把这个 UA 解析成 `Chromium`，`bot?` 返回 `false`，所以没丢。ZyBorg 是一个老爬虫，不在 UAInspector 的名单里 |

两边的机器人名单是各自维护的，对少数 UA 判定不同是预期内的。对应附录 C 的 **UA-7**。在这批样本里，rivu 判错的为 0，Plausible 漏判了 10 个、误判了 13 个。

## 5. 维度差异

### 5.1 浏览器、操作系统（UA-6）

rivu 用 uap-core，归一化规则见 `enrich-ua.md` §8；Plausible 用 UAInspector（Matomo device-detector 的数据）。两者的家族名和粒度不同：

- 命名不同：`BlackBerry WebKit` 对 `BlackBerry Browser`，`Dolfin` 对 `Dolphin`，`Edge` 对 `Microsoft Edge`，`IE` 对 `Internet Explorer`，`Linux` 对 `GNU/Linux`，`macOS` 对 `Mac`，`BREW` 对 `Brew`，`Nokia Series 40` 对 `Java ME`。
- 粒度不同：UAInspector 把 CyanogenMod、MocorDroid、Fire OS、Android TV 从 Android 里拆出来（site1 的 Android 是 2397 对 2341），把 Symbian 拆成 Series 40、Series 60 等（site1 的 `Symbian OS` 是 59 对 6）。
- 识别范围不同：site1 中 OS 为 `(not set)` 的，rivu 有 93 个，Plausible 有 38 个。差额主要是功能机和老 Windows Mobile UA：uap-core 认不出这些 UA，而 UAInspector 的设备库认得出。

两边的合计相等（site1 都是 2899），只是同一批访客被分进了名字不同的桶。浏览器分布的情况相同：Android Browser 1147 对 1123，Chrome 890 对 904，差额来自 UAInspector 单独列出的 360 Phone Browser、Huawei Browser 等。

### 5.2 设备类别（UA-8）

| | desktop | mobile | tablet | (not set) |
|---|---|---|---|---|
| rivu | 158 | 2131 | 610 | 0 |
| Plausible | 97 | 1965 | 549 | 288 |

（site1 的数据。）rivu 的 tracker 不发送屏幕宽度（`tracker-and-protocol.md` 的隐私原则），设备类别只按 UA 推导（`enrich-ua.md` §5）。Plausible 优先按屏幕宽度分类，没有宽度时退回 UAInspector 的设备类型，认不出的记为 `(not set)`。回放时发给 Plausible 的请求同样不带宽度，所以这里比较的是两套 UA 设备规则。rivu 对未知机型有兜底规则（`Generic Smartphone` 等记为 Mobile），因此没有 `(not set)`。另外，取值的大小写也不同（`mobile` 对 `Mobile`），这是展示层的问题。

### 5.3 来源与渠道（Source-R1）

- 唯一的命名差异是 `X (Twitter)` 对 `Twitter`：rivu 的补充表把 Matomo 的 `Twitter` 改名为 `X (Twitter)`（`enrich-source.md` §4）。
- 渠道与其余来源只差 1 到 2 个访客（site1 的 Direct 是 1625 对 1627，Organic Search 是 594 对 592；site2 的 Organic Social 是 180 对 179）。这些访客正是 §4 中被单方面丢弃的那几个，他们的首个 pageview 带有对应的 referrer。渠道规则本身没有不一致：回放流里没有触发 Source-D1–D12 的标签组合，比如 `medium=organic` 而没有来源。这些规则的差异由 `plausible_parity` 测试逐条覆盖。

### 5.4 国家（Geo-1）

site1 有 29 个国家的计数不同，每个国家相差 1 到 9 个；rivu 另有少量 `(not set)`。两边用的都是 DB-IP Lite country 库，但版本不同：rivu 用 dev 环境里下载的那一期，Plausible CE 镜像自带它构建时的那一期。loadgen 的 IP 是从公开网段里随机取的，其中有不少段在两期之间换了归属国家。这是数据版本的差异，不是规则的差异：`enrich-geo.md` 的国家码规则与 Plausible 一致。

## 6. 局限

- 只回放了一个 UTC 日，没有覆盖跨午夜的会话。跨午夜续会话（D5）只有单元测试和 probe，没有和 Plausible 对照。
- UA 来自 uap-core 语料，偏向老设备，这放大了 §5.1 和 §5.2 的差异。2026 年的真实流量以主流浏览器为主，差异应当更小。
- 地理差异没有逐个 IP 核对两期 DB-IP 的记录，只是从「两边规则相同」推断出来的。
- 自定义事件、Goals、UTM 维度没有比较，因为 Plausible 的对应端点要求 Goals 先配置好。
- 没有比较 Plausible 的 `screen-sizes` 在带宽度时的行为：rivu 永远不发宽度，比较没有意义。

## 7. 复现

```
pwsh linux-task.ps1 -Mode build -Project D:\Forgejo\Rivu -Command 'EVENTS=20000 RATE=80 SITES=3 bash tools/replay/run.sh'
docker compose -f tools/replay/plausible/compose.yml down -v   # 用完清理
```

需要 dev 的 PostgreSQL 与 ClickHouse（rivu 用），以及 Docker Desktop。
