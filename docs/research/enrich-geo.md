# 富化：GeoIP（数据源、许可、署名与实测）

> 范围：`crates/geo`（`rivu-geo`）与 `crates/jobs` 的 `geoip_refresh`，对应 `system-design.md` §4.4「GeoIP 数据源、更新与署名（Q5）」。
> 查阅与实测日期：2026-10-22（UTC）。本文只记录事实、实测与建议，没有改动 `docs/architecture/`。

## 0. 结论

1. §4.4 的规范可以逐条实现，已实现并测试。需要改文字或补充的地方见 §7，其中有两条影响产品行为：
   - **DB-IP Lite 的 City 库没有 ISO 3166-2 子区划代码，也没有城市的 GeoNames id**，只有英文的州省名和城市名。默认来源下 `region_code`、`city_geoname_id` 永远为空，`geo_level = region/city` 在默认配置下实际等同于 `country`（§5.3）。
   - DB-IP 页面标的文件大小（city 121.1 MB、country 8.0 MB）是**解压后**的大小（MiB）；实际下载的 `.mmdb.gz` 是 60.2 MB 与 4.1 MB（§2）。
2. MaxMind 现行下载接口需要 **Account ID + License Key**（HTTP Basic），返回的是 `tar.gz`（不是 `.mmdb.gz`），要解 tar 取其中的 `.mmdb`（§3）。已新增 `RIVU_MAXMIND_ACCOUNT_ID`，license key 设置时它必填。
3. 地理字段的正确性用两类外部数据印证：MaxMind 官方测试库对照其 JSON 源数据（逐网段，§4）；DB-IP 实测下载后与五个 RIR 的公开分配记录对照（§5）。

## 1. 数据源、许可与署名原文

### 1.1 DB-IP Lite（默认）

| 项 | 内容 | 出处 |
|---|---|---|
| 许可 | “licensed under a Creative Commons Attribution 4.0 International License” | <https://db-ip.com/db/download/ip-to-city-lite>、<https://db-ip.com/db/download/ip-to-country-lite>、<https://db-ip.com/db/lite.php> |
| 署名义务 | 可用于应用，“provided you give attribution to DB-IP.com for the data” | 同上 |
| Web 应用 | “you must include a link back to DB-IP.com on pages that display or use results from the database” | 同上 |
| 页面给出的署名 HTML | `<a href='https://db-ip.com'>IP Geolocation by DB-IP</a>` | 同上 |
| 更新频率 | “updated monthly”；商业版每日更新 | lite.php |
| 精度 | “a subset of the full database with reduced coverage and accuracy” | lite.php |
| city 页面列出的字段 | 首末 IP、洲代码与名称、国家 ISO alpha-2、国家是否属欧盟、国家名、**州/省名（state or province name）**、**城市名**、近似经纬度；**没有 geoname id**，没有子区划代码 | ip-to-city-lite 页 |

Rivu 内置署名与页面一致：文本 `IP Geolocation by DB-IP`，链接 `https://db-ip.com`，许可 `CC BY 4.0`（`Attribution::dbip_lite()`，测试 `builtin_attributions_match_the_spec`；实测下载后 `GeoDb::attribution()` 的输出也与之相同）。

### 1.2 MaxMind GeoLite2

| 项 | 内容 | 出处 |
|---|---|---|
| 署名 | EULA 第 3 条给出示例句：“This product includes GeoLite Data created by MaxMind, available from https://www.maxmind.com.” EULA 称其为示例，要求注明来源 | <https://www.maxmind.com/en/geolite2/eula> |
| 更新 | EULA 第 6.3 条：应及时改用新版本；新版本发布 30 天内停用并销毁旧版本。GeoLite2 页面同样写明 “you must delete GeoLite databases within 30 days of a new release” | EULA、<https://dev.maxmind.com/geoip/geolite2-free-geolocation-data/> |
| 下载额度 | “GeoLite users are limited to 30 database downloads per day”；超限或被限速返回 `429` | geolite2-free-geolocation-data、updating-databases |
| 许可 | 受 GeoLite EULA 约束（页面没有写 CC BY-SA；本次没有核对历史版本的许可条款，内置的 `license` 字段填 `GeoLite2 EULA`） | EULA |

内置署名取示例句原文，链接 `https://www.maxmind.com`。

### 1.3 自定义 URL

许可与署名由运维者负责。两个署名变量都没设置时没有署名（`geo_attribution = null`）；只设置其中一个时，缺的文本取链接、缺的链接取空串。

## 2. DB-IP 下载 URL 核实

`https://download.db-ip.com/free/dbip-<city|country>-lite-<YYYY-MM>.mmdb.gz` 的形式**仍然有效**（2026-10-22 对 HEAD 与真实下载各验证一次）：

| URL（`https://download.db-ip.com/free/…`） | HEAD 状态 | Content-Length | ETag | Last-Modified |
|---|---|---|---|---|
| `dbip-country-lite-2026-10.mmdb.gz` | 200 | 4 123 384 | `"6abdfdc6-3eeaf8"` | 2026-10-01 06:29:26 GMT |
| `dbip-country-lite-2026-09.mmdb.gz` | 200 | 4 116 896 | `"6a967035-3ed1a0"` | 2026-09-01 06:27:01 GMT |
| `dbip-city-lite-2026-10.mmdb.gz` | 200 | 60 193 321 | `"6abdfdc8-3967a29"` | 2026-10-01 06:29:28 GMT |
| `dbip-city-lite-2026-09.mmdb.gz` | 200 | 60 287 600 | `"6a967037-397ea70"` | 2026-09-01 06:27:03 GMT |
| `dbip-country-lite-2026-11.mmdb.gz`（尚未发布） | **404** | | | |

- 响应是直接的 200（没有重定向），`Content-Type: application/octet-stream`，同时带 `ETag` 与 `Last-Modified`，所以**不需要**「无校验头」的哈希路径（但仍实现了，见 §6）。
- **条件请求有效**：`If-None-Match: "6abdfdc6-3eeaf8"` → 304；`If-Modified-Since: <上面的 Last-Modified>` → 304；`If-Modified-Since` 早一天 → 200。实测 `refresh()` 第二次调用得到 `NotModified { by_hash: false }`。
- **发布时间**：两个月的 `Last-Modified` 都在 UTC 06:27–06:30（1 日）。所以每月 1 日 UTC 00:00 到约 06:30 之间当月文件还不存在（404），「先试当月，404 回退上月」是必须的，不是可选的优化。
- **页面标的大小是解压后的 MiB**：页面写 city `121.1 MB`、country `8.0 MB`；实际解压后 city 126 998 165 字节（= 121.1 MiB）、country 8 348 509 字节（= 7.96 MiB）。真正下载的 gz 是 60.2 MB 和 4.1 MB。
- 下载实测（本机网络，含解压、校验、fsync、替换）：country 7.9 s；city 46.5 s。

## 3. MaxMind 认证方式核实

官方文档（<https://dev.maxmind.com/geoip/updating-databases/>）：

- 现行直链形式：`https://download.maxmind.com/geoip/databases/<edition>/download?suffix=…`（文档示例用的是 `GeoIP2-City-CSV` 与 `suffix=zip`）。
- 认证：“Use Basic Authentication with your account ID and license key”，即 `curl -u YOUR_ACCOUNT_ID:YOUR_LICENSE_KEY`。
- 要求客户端跟随重定向：下载会重定向到 `…r2.cloudflarestorage.com`（对象存储）。
- 二进制库的下载是 gzip（`tar.gz`），CSV 是 zip。
- 检查更新：对该 permalink 发 `HEAD`，读 `Last-Modified`；HEAD “won't count against your daily database download limit”。文档**没有**提到 `If-Modified-Since`、`ETag` 或 SHA-256 端点。

我对真实端点做了**不带凭据**的探测（只有这些我能在没有账号的情况下核实）：

| 请求 | 结果 |
|---|---|
| `HEAD …/geoip/databases/GeoLite2-City/download?suffix=tar.gz` | **401**，`WWW-Authenticate: Basic realm="geoip-download"` |
| 同上，`GeoLite2-Country` | 401，同上 |
| 同上，`suffix=mmdb` | **400**（不支持裸 mmdb 后缀） |
| `HEAD …/app/geoip_download?edition_id=GeoLite2-City&license_key=x&suffix=tar.gz`（旧接口，只要 key） | 401，没有 `WWW-Authenticate` |

结论：现行接口是 HTTP Basic（realm `geoip-download`），`suffix=tar.gz` 是二进制库的取法。旧接口仍在，但只认 license key，MaxMind 文档已不再提及；Rivu 只实现现行接口，因此 `RIVU_MAXMIND_LICENSE_KEY` 必须配 `RIVU_MAXMIND_ACCOUNT_ID`（配置校验会报错，不回显 key）。

**没有核实的部分**（没有 MaxMind 账号）：带凭据的真实下载、重定向目标对 `If-None-Match`/`If-Modified-Since` 的反应、响应里是否有 `ETag`。实现不依赖这些：有校验头就带，服务器忽略就整文件下载，再按解压后 mmdb 的 SHA-256 与 meta 比较，相同则不替换。测试用本地服务器模拟了「API 回 302 → 另一个源的对象存储」，验证了 Basic 认证只发给 API、**跨源重定向不带 `Authorization`**（reqwest 的行为）、校验头沿重定向链送到存储端。建议拿到账号后手动验证一次，并考虑「先 HEAD 比较 `Last-Modified`」以免占用 30 次/天的额度（§7 第 9 条）。

## 4. 测试数据：MaxMind 官方测试库

- 仓库 <https://github.com/maxmind/MaxMind-DB>，固定提交 `f4847f28e5d12cd495ad5c1ea297cef9cc45bf4f`（2026-10-19）。
- 许可：Apache-2.0 **或** MIT（README「Copyright and License」：Copyright (c) 2013 - 2026 by MaxMind, Inc.）。两份许可证文本随数据放在 `crates/geo/tests/data/`。
- 复制的文件与 git blob 哈希见 `crates/geo/tests/data/SOURCE.md`，已逐个核对与上游字节一致（`.gitattributes` 保证 mmdb 按二进制、JSON 保持 LF）。
- 期望值来自 `source-data/*.json`（生成 mmdb 的输入）。推导规则（与 Plausible 一致）：country 取 `country.iso_code`（不回退 `registered_country`/`represented_country`）；region = `<国家>-<subdivisions[0].iso_code>`；city = `city.geoname_id`；没有国家时 region 与 city 也为空。

### 4.1 逐网段比对结果（`tests/mmdb_reference.rs`）

对 JSON 里**每一个**网段取首、中、尾三个地址（重叠网段取最长前缀的记录），三个精度各查一次，IPv4 另用 IPv4-mapped IPv6 再查一次，全部与期望值相等：

| 库 | 网段数 | 查询的地址数 | 含国家 | 含区域 | 含城市 | 无国家 |
|---|---|---|---|---|---|---|
| GeoIP2-City-Test | 250 | 750 | 738 | 45 | 42 | 12 |
| GeoLite2-City-Test | 242 | 726 | 720 | 33 | 33 | 6 |
| GeoIP2-Country-Test | 355 | 1065 | 1053 | 0 | 0 | 12 |
| GeoLite2-Country-Test | 244 | 732 | 726 | 0 | 0 | 6 |

（Country 库没有区域与城市，用高精度查询也必须是空的，测试断言了这一点。「无国家」是只有 `registered_country` 的网段，例如 `5.83.124.0/22`，以及 `214.1.1.0/24`。）

另有手写期望的抽查（防止比对逻辑和被测代码同时出错）：`89.160.20.128` → `SE`、`SE-E`、2694762；`81.2.69.142` → `GB`、`GB-ENG`、2643743（只取 subdivisions 第一级）；`175.16.199.1` → `CN`、`CN-22`、2038180；`2001:218::1` → `JP`；`214.0.0.1` → `SG`、无区域、1880252；`2.125.160.217` 国家取 `GB` 而不是 `registered_country` 的 `FR`。`GeoLevel` 截断、IPv4-mapped IPv6、私有与保留地址返回空值都有测试。

## 5. 实测：DB-IP Lite

`tests/live_dbip.rs`（`#[ignore]`，需要网络与 RIR 文件目录，用法见文件头注释）。

### 5.1 下载、校验、署名

- 对真实 `download.db-ip.com` 用 `refresh()` 下载：真实 TLS（rustls + ring + webpki-roots，Windows 上验证通过）、gz 解压、§4.4 的校验全部通过。`database_type` 分别是 `DBIP-Country-Lite`、`DBIP-City-Lite`（含 `Country`/`City`，校验规则覆盖）。
- 第二次 `refresh()` 得到 304（`NotModified { by_hash: false }`），meta 的 `downloaded_at` 不变、`checked_at` 更新。
- 署名：`{source: "dbip-lite", text: "IP Geolocation by DB-IP", url: "https://db-ip.com", license: "CC BY 4.0"}`，与 DB-IP 页面给出的署名 HTML 一致。
- 实测的两个文件：country 解压后 8 348 509 字节，`build_epoch 1790818572`；city 解压后 126 998 165 字节，`build_epoch 1790818881`。

### 5.2 与 RIR 公开分配记录的一致性

数据：五个 RIR 当日发布的 delegated 统计文件（APNIC、ARIN extended、RIPE NCC、LACNIC、AFRINIC；格式 `registry|CC|type|start|value|date|status`）。取 `allocated`/`assigned` 记录，IPv4 只取 /24 及更大的块、取块中点，IPv6 取 /16–/64 的块、同样取块中点；固定种子抽 5000 个 IPv4 块（样本池 262 066）和 2000 个 IPv6 块（样本池 70 974）。RIR 记录的是**注册国家**，DB-IP 是**地理位置**，跨国使用的块本来就会不同，所以只要求一致率不太低（断言 ≥ 85%）。

| | 一致 / 总数 | 一致率 |
|---|---|---|
| **全部** | 6369 / 7000 | **90.99%** |
| IPv4（按块计） | 4508 / 5000 | 90.16% |
| IPv4（按地址数加权） | 58 337 024 / 62 166 016 | 93.84% |
| IPv6 | 1861 / 2000 | 93.05% |
| APNIC | 1602 / 1678 | 95.47% |
| ARIN | 1639 / 1726 | 94.96% |
| RIPE NCC | 2240 / 2697 | 83.06% |
| LACNIC | 740 / 746 | 99.20% |
| AFRINIC | 148 / 153 | 96.73% |

- DB-IP 对这些地址没有返回空值的情况（空值 0）。
- country 库与 city 库的国家结果完全相同（两次实测数字一样）。
- 最常见的不一致是注册国与使用地不同的典型情形：US→CA 17、US→DE 13、CA→US 12、HK→RU 12、AU→BE 10、GB→US 10、US→GB 10。RIPE 一致率偏低，因为它的样本里托管与云厂商的跨国块更多（例：`45.93.0.0/22` 注册 DE、DB-IP 定位 JP）。
- 几个直观的原始记录：`8.8.8.8` → US（city 库：Mountain View、California）；`114.114.114.114` → CN（Jinan、Shandong）；`2001:4860:4860::8888` → CA（Montreal、Quebec；ARIN 登记在 US——同样是注册国与定位不同）。

### 5.3 City 库的字段：没有 ISO 子区划码，没有 geoname id（重要）

`DBIP-City-Lite` 的原始记录（`8.8.8.8`）：

```json
{"city":{"names":{"en":"Mountain View"}},
 "continent":{"code":"NA","geoname_id":6255149,"names":{…}},
 "country":{"geoname_id":6252001,"is_in_european_union":false,"iso_code":"US","names":{…}},
 "location":{"latitude":37.422,"longitude":-122.085},
 "subdivisions":[{"names":{"en":"California"}}]}
```

- `city` 只有 `names`（英文），**没有 `geoname_id`**；`subdivisions[0]` 只有 `names`，**没有 `iso_code`**。
- 实测 7000 个地址里区域非空 0 个、城市 geoname_id 非零 0 个（与 city 页面的字段表一致）。
- 所以默认来源下：`GeoInfo.region_code = ""`、`city_geoname_id = 0`。`rivu-geo` 不猜测：没有代码就返回空，不拿英文名拼凑。
- `country.geoname_id` 与 `continent.geoname_id` 是有的，但库里没有国家级以下的 GeoNames id。

## 6. 实现要点与取舍

- **不用 mmap**：`maxminddb` 的 mmap 入口 `Reader::open_mmap` 是 `unsafe fn`，而工作区 `unsafe_code = "forbid"`。改为把文件读进堆（`Reader<Vec<u8>>`）：City 约 121 MiB、Country 约 8 MiB，每个加载它的进程各占一份；热加载时新旧并存，旧的在途查询结束后释放。我们自己的替换是「写临时文件再 rename」，对 mmap 本来也安全；mmap 的风险只在运维者原地改写库文件。刷新时校验还要再读一份（用完即释放）。内存紧张的部署可以选 DB-IP country。
- **国家码规则**：只接受两个 ASCII 字母；`ZZ`、`XX` 与 `A1`/`A2`/`T1` 之类占位码视为没有（与 Plausible 的 `geolocation.ex` 一致，`01-ingestion-pipeline.md` 里记录了）。没有国家时区域与城市也为空。
- **区域码**：`<国家>-<subdivisions[0].iso_code>`（只取第一级；子区划部分 1–3 位字母数字）。
- **公网判定**：私有、回环、链路本地、CGNAT（100.64/10）、文档、基准测试、组播、保留（240/4）、IPv6 的 ULA/链路本地/文档/丢弃/IPv4 兼容地址，直接返回空值，不查库。IPv4-mapped IPv6 先转成 IPv4。
- **HTTP 客户端**：reqwest 0.13，关闭默认特性；`rustls-no-provider` 加上我们注入的 `rustls::ClientConfig`（ring + webpki-roots），与 clickhouse 的 TLS 选择一致，依赖树里没有 aws-lc。不自动解压（`.mmdb.gz` 要原样落盘），只说 HTTP/1.1，连接超时 10 s、整体超时 10 min，最多 5 次重定向（测试：5 次通过、6 次失败）。启用了 reqwest 的 `system-proxy` 特性，应当遵循 `HTTPS_PROXY` 等代理环境变量（未测试）。所有来自 reqwest 的错误都去掉 URL（`without_url()`），日志与 meta 里只有去掉用户信息和查询串的 URL。
- **解压**：按魔数 `1f 8b` 判断 gzip；解压后再看头部是否是 tar（`ustar`），是就取第一个 `.mmdb` 条目（MaxMind 的 `tar.gz`）。压缩前后各自受 1 GiB 限制，防 gzip 炸弹（测试：压缩后约 8 KB、解压后 8 MiB 的炸弹在 1 MiB 上限下被拒绝）。
- **哈希**：SHA-256 记的是**解压后的 mmdb 内容**，所以传输压缩方式变化不会触发替换。有校验头却回了 200、内容相同时同样不替换。
- **手工放置的文件**：没有 meta 的库，配置的是默认的 DB-IP 来源时直接使用、**不自动刷新**（否则 24 小时后会被 DB-IP 覆盖）；配置了显式来源（URL 或 MaxMind key）时视为没有本地副本，立即下载（新库校验通过前旧文件继续用）。meta 的 `build_epoch` 与库不一致（文件被手工换过）时 meta 作废，署名退回手工署名。
- **署名**：DB-IP、MaxMind 用内置文本；自定义来源的署名在每次刷新（包括 304）时写进 meta，改了环境变量下次刷新生效；`GeoDb` 比较 meta 变化来更新署名，不需要重载库。
- **指标**：`rivu_geoip_refresh_total{source,result}`、`rivu_geoip_loaded`、`rivu_geoip_db_age_seconds`（按 `build_epoch`）。后两个由**持有库的进程**（collector、all）导出，worker 不持有库，所以告警（`geoip_loaded = 0` 持续 1 小时、库年龄 > 45 天）应当针对 collector 的抓取目标。
- **`/readyz`**：`checks.geoip` 只报告，`ok` 恒为 `true`，不参与 `ready`；字段：`file_present`、（持有库时）`loaded`/`source`/`database_type`/`build_epoch`/`age_seconds`、（运行刷新任务时）`refresh.{schedule,last_attempt_at,last_result,last_error,consecutive_failures}`。已用真实 `rivu all` 验证：库下载前 `loaded=false` 且 `status=ready`，约 12 秒后下载完成、`loaded=true`。

## 7. 与 §4.4 的不一致与建议补充

下面每条给出建议改动的文字（没有改 `docs/architecture/`）。

1. **文件大小（§4.4 表第 3 行）**。现文「city 约 121 MB，country 约 8 MB，均为 gz」。
   建议：「DB-IP 页面标注的是解压后的大小（city 121.1 MiB、country 约 8 MiB）；实际下载的 `.mmdb.gz` 约 60 MB（city）与 4 MB（country）」。
2. **DB-IP City 库没有 ISO 子区划码与 geoname id**（§5.3）。§4.4 与 `SessionAttrs` 的 `region_code`（ISO 3166-2）、`city_geoname_id` 假定了这两个字段。
   建议在 §4.4 加一段：「DB-IP Lite 只提供英文的州省名和城市名，没有 ISO 3166-2 代码和 GeoNames id；默认来源下 `region_code` 与 `city_geoname_id` 为空，地区与城市报表需要 MaxMind GeoLite2（含 `subdivisions[].iso_code` 与 `city.geoname_id`）或自定义的同结构库。」再择一决定后续：(a) 接受并在站点设置里提示「当前库不含地区/城市」（`/readyz` 已报告 `database_type`，看板可据此提示）；(b) 用 GeoNames（CC BY 4.0）的 `admin1CodesASCII.txt`、`cities500.txt` 做「（国家，英文名）→ 代码」的映射表——名称有歧义（同名城市），映射需要人工核对，工作量不小，建议作为单独任务评估。
3. **MaxMind 接口（§4.4 表第 2 行）**。现文「带 `If-Modified-Since`。现行接口按 Account ID 加 license key 认证，实现时核对；需要的话新增 `RIVU_MAXMIND_ACCOUNT_ID`」。
   建议：「`https://download.maxmind.com/geoip/databases/<edition>/download?suffix=tar.gz`，HTTP Basic（用户名 Account ID、密码 License Key），返回 `tar.gz`（取其中的 `.mmdb`），会重定向到对象存储，凭据不随跨源重定向转发；`RIVU_MAXMIND_ACCOUNT_ID` 在设置 license key 时必填。每日下载上限 30 次（超限 `429`）」。`platform-and-ops.md` §7 的「下载接口若还要求 Account ID，则新增……」同步改成确定的说法。
4. **手工放置文件的刷新规则（§4.4「文件与元数据」）**。现文只说「没有 meta 的文件视为运维者手工放置，直接使用」，没说会不会被定期刷新覆盖。
   建议补充：「没有 meta 的文件：配置的是默认来源时不自动刷新；配置了 `RIVU_GEOIP_DB_URL` 或 `RIVU_MAXMIND_LICENSE_KEY` 时视为没有本地副本，立即下载」。
5. **启动后的第一次检查时间（§4.4「生命周期」）**。现文没有规定有副本时第一次条件 GET 何时发。
   建议补充：「有本地副本时，第一次检查安排在 meta 的最近检查时间加一个（带抖动的）刷新周期，已过期则立即；避免频繁重启时每次都请求服务器（MaxMind 有每日额度）」。
6. **校验的边界（§4.4「校验」）**。MaxMind 官方的 `GeoIP2-City-Test-Broken-Double-Format.mmdb`（数据区里某个 double 编码损坏）能通过现有的五项校验。整库 `Reader::verify()` 需要遍历全部节点与数据，对 City 库代价较大，没有采用；下载截断会被 gzip CRC、mmdb 元数据（在文件尾部）两道检查挡住。
   建议补充一句：「校验不覆盖数据区内部的损坏；依赖来源服务器的完整性与传输层校验」。
7. **国家码占位值与公网判定（§4.2 第 6 步）**。建议补充：「国家码非两个字母，或为 `ZZ`/`XX`，视为未知；没有国家时区域与城市也为空；私有、保留、回环等非公网地址不查库」。
8. **「内存映射」（§4.3 末行）**。现文「GeoIP 用内存映射的 mmdb」。工作区禁止 `unsafe`，而 mmap 入口是 `unsafe fn`。
   建议改为：「GeoIP 把 mmdb 读入内存（City 约 121 MiB，Country 约 8 MiB），更新后用 `ArcSwap` 原子替换」，并在 §7 容量里记一笔每个 collector 进程的常驻内存。
9. **MaxMind 的 HEAD 预检（可选）**。MaxMind 文档推荐用 `HEAD` 比较 `Last-Modified`，且不计入下载额度；条件 GET 是否被支持、是否计额度文档没写。建议拿到账号后验证；若条件 GET 不被支持，改为 `HEAD` 预检（`Last-Modified` 不变就不下载）。
10. **私有 CA 的自定义 URL**。TLS 根证书是内置的 webpki-roots，不读系统证书库；`RIVU_GEOIP_DB_URL` 指向使用私有 CA 的内网服务器时校验会失败。建议新增 `RIVU_GEOIP_CA_FILE`（追加受信根证书的 PEM 文件）；未实现，等有需求再做。
11. **指标标签（`platform-and-ops.md` §10）**。表里 `geoip_refresh_total`、`geoip_db_age_seconds`、`geoip_loaded` 共用一行标签说明。实现：`refresh_total{source,result}`，另两个无标签（来源切换后旧标签的序列会残留）。建议表里写明。
12. **`geoip_refresh` 的 `unique_key`（§6）**。现在是进程内定时、没有任务队列；多个 worker 同时运行时各自下载。临时文件名带进程号，不会互相破坏，替换是原子的，后到的那个会因为 SHA-256 相同而跳过替换；多 worker 部署接入任务队列后用 `unique_key = geoip:{slot}` 去重即可。

## 8. 未完成与后续

- 管线第 6 步（`GeoDb::lookup` 的接入、`country_blocked`、按站点 `geo_level` 截断）另行进行；`run.rs` 里 collector/all 已经持有 `GeoDb` 并热加载，等接入时从 `geo_db` 取用。
- 没有 MaxMind 账号，带凭据的真实下载没有验证（§3）。
- DB-IP 的区域/城市映射（§7 第 2 条）未做。
- `RIVU_GEOIP_CA_FILE`（§7 第 10 条）未做。
