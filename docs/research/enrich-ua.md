# 富化：UA 解析与机器人识别（`rivu-ua`）

> 状态：2026-10-22，P1 第 3 步的 UA / 机器人一段。代码在 `crates/ua/`，纯库，尚未接入管线。
> 对应 `architecture/system-design.md` §4.2 第 5、7 步、§4.3、§4.4、§7。本文不改 `architecture/`，文末「建议写进 system-design 的内容」供据此更新。
> 方法遵循 `brief-p1.md`：每一项结论都由系统之外的数据源证明，数字都是 `cargo test -p rivu-ua` 实测的。

## 1. 结论一览

| 项 | 结果 |
|---|---|
| uap-core 自带测试集通过率 | `test_ua.yaml` 1601/1601，`test_os.yaml` 483/483，`test_device.yaml` 16129/16129，**全部 100%**，允许失败清单为空 |
| 机器人检测率（crawler-user-agents，2120 个样例 UA） | **84.58%**（1793/2120）；只用 uap-core 的 `Spider` 为 62.36%（1322/2120）；测试里设下限 84% |
| 误判率（uap-core 语料，非 Spider 共 16994 条） | 浏览器形态（`Mozilla/`、`Opera/` 开头）10389 条中判为机器人 **9 条，全部是真机器人**（4 条无头浏览器、3 条 Dynatrace 合成监控、1 条 Catchpoint、1 条 HTTrack），逐条见 §6.3；所有 UA 合计 119 条（0.70%），其余 110 条是 curl、WhatsApp 原生客户端等非浏览器 |
| 单次耗时（release，Windows，690 条真实 UA 样本） | 缓存未命中均值 22–24 µs（p50 12.6 µs，p95 67–77 µs，p99 134–184 µs）；缓存命中均值约 150 ns（p50 100 ns，计时分辨率 100 ns）；`embedded()` 构建 132 ms |
| 新依赖 | `ua-parser` 0.2（uap-core 官方 Rust 实现）、`serde-saphyr` 2（纯 Rust YAML）、`moka` 0.12（`sync`） |

## 2. 数据源、固定版本与许可

### 2.1 uap-core（规则库与测试集）

- 上游：<https://github.com/ua-parser/uap-core>，Apache-2.0。
- 固定 commit：`73e7340c3ed8055051607b296bf46ead7aa5f19e`（2026-08-24，"Clean up errant whitespace in regexes.yaml"）；2026-10-22 下载，原样放入 `crates/ua/data/uap-core/regexes.yaml`（222 536 字节，SHA-256 `acf2c9af…c8e6`），用 `include_str!` 嵌入。
- 同一 commit 的 `tests/test_ua.yaml`、`test_os.yaml`、`test_device.yaml` 放在 `crates/ua/tests/data/`，文件名加 `uap-` 前缀。
- 许可：Apache-2.0，`LICENSE` 随附。**上游在该 commit 没有 `NOTICE` 文件**（根目录只有 `.github docs test_resources tests LICENSE README.md package.json regexes.yaml` 等），所以 `data/uap-core/NOTICE` 只是我们写的署名说明，不是上游文件。完整的来源、日期、哈希在 `data/uap-core/PROVENANCE.md` 与 `tests/data/PROVENANCE.md`。
- 下载文件先放在 scratchpad 的独立目录核对哈希，再复制进仓库。
- uap-core 不含机器人以外的「设备类别」字段，只有 device 的 family / brand / model，所以设备类别要自己映射（§5）。

### 2.2 crawler-user-agents（机器人检测率的外部数据）

- 上游：<https://github.com/monperrus/crawler-user-agents>，MIT（Copyright 2017 Martin Monperrus），`LICENSE` 随附。
- 固定 commit：`9345a7ad9c49cd0fbd880eb5a84ed1f358fc6a97`（2026-09-29），`crawler-user-agents.json` 543 489 字节，SHA-256 `4218df91…94bff3`。1501 条规则，共 2120 个 `instances` 样例 UA。
- 注意：这份名单的 `pattern` 是它自己的检测正则；我们**不嵌入**这些正则，只拿 `instances` 当测试输入，否则检测率就是循环论证。

## 3. 实现选择

### 3.1 UA 匹配：`ua-parser` 0.2.2（官方 uap-rust）

| 候选 | 评价 |
|---|---|
| **`ua-parser` 0.2.2**（ua-parser/uap-rust，2026-03 发布，Apache-2.0） | uap-core 官方组织维护；`#![deny(unsafe_code)]`；用 `regex-filtered` 先做原子预过滤，只对可能命中的规则跑正则；数据模型就是 `regexes.yaml`，`Regexes` 实现 `serde::Deserialize`；依赖只有 `regex`、`regex-filtered`、`serde`（YAML 解析器不绑定）。 |
| `uaparser` 0.6.4（2024-06 发布） | 下载量更大，但最后一次发布是 2024-06；依赖已被弃用的 `serde_yaml`（0.9.34+deprecated）、`lazy_static`、`derive_more 0.99`。我们没有实测它的通过率与耗时，仅凭维护状态与依赖就没有选它。 |
| 自写 | 要重新实现 uap 的模板替换、`$n` 规则、`regex_flag`、PCRE→Rust regex 的差异，没有收益。 |

实测 `ua-parser` 在我们固定的 commit 上对三个测试文件**全部 100% 通过**，这是选择它最硬的证据（§6.1）。它对 uap-core 的 Rust `regex` 与 PCRE 差异（`\d` `\w` 的 Unicode 语义、大范围的 `{0,100}` 重复）自带改写（`rewrite_regex`）。

`regex` crate 保证匹配时间对输入线性，不会灾难性回溯。我们仍然给输入设长度上限（§4.3）。

YAML 解析器：`serde-saphyr` 2.0（纯 Rust 实现、不是 libyaml 绑定；2026-10 发布，自述定位是不 panic 的解析）。备选 `serde_yaml` 已弃用，`serde_norway` 是 libyaml 的 unsafe 绑定。我们的约束只是「自己的代码禁止 unsafe」，但没有理由不选更干净的。该解析器只在 `embedded()` 启动时跑一次（实测 `embedded()` 含 YAML 解析和全部正则编译共 132 ms）。

`regexes.yaml` 在编译期嵌入，运行期不读文件、不联网。

### 3.2 缓存：`moka` 0.12 `sync::Cache`

按 §4.3：10 万条、TTL 1 小时（`CACHE_CAPACITY`、`CACHE_TTL`）。键是**处理后**（去首尾空白、截断）的 UA 字符串，值是 `Arc<UaInfo>`。命中路径用 `&str` 查，不分配；未命中并发时可能重复解析同一 UA，结果相同，不加锁。`default-features = false, features = ["sync"]`（不用 `quanta`、`atomic64` 之外的可选特性）。空 UA 不进缓存，返回共享的同一个 `Arc`。

### 3.3 依赖体积

`cargo tree -p rivu-ua` 共约 80 行（含重复）。新增的直接依赖是 `moka`、`serde-saphyr`、`ua-parser`；`regex`、`regex-filtered` 是 `ua-parser` 的传递依赖。`moka` 带入 `crossbeam-*`、`parking_lot`、`uuid`（v4 特性，其内部依赖 `getrandom`），这是 moka 的固有依赖。

## 4. 对外接口与处理流程

```rust
pub enum DeviceClass { Desktop, Mobile, Tablet }          // as_str: desktop / mobile / tablet
pub enum BotKind { Spider, Headless, Automation, Empty }  // as_str: spider / headless / automation / empty
pub struct UaInfo { browser, browser_version, os, os_version, device: DeviceClass, bot: Option<BotKind> }
impl UaInfo { pub fn is_bot(&self) -> bool }
impl UaParser {
    pub fn embedded() -> anyhow::Result<Self>;
    pub fn parse(&self, ua: &str) -> Arc<UaInfo>;            // 带缓存
    pub fn parse_uncached(&self, ua: &str) -> UaInfo;        // 基准与测试
    pub fn parse_raw(&self, ua: &str) -> RawUa;              // uap-core 原始结果，供一致性测试
    pub fn with_cache(capacity: u64, ttl: Duration) -> anyhow::Result<Self>; // 测试
}
```

- 未知字段是空串（与 Plausible 一致，`SessionAttrs` 的 `""` 约定）；`device` 未知默认 `Desktop`（§5）。
- `device.as_str()` 可直接填 `SessionAttrs.device`；`browser`、`browser_version`、`os`、`os_version` 同名同型。
- 版本取 `major.minor`，缺 minor 只有 major（如 `iOS 17`→`17`），缺 major 为空。每段限 16 字节，超长视为未知（uap 的个别兜底规则会把任意字符串捕获为版本）。
- 浏览器名与系统名限长 64 字节。**基数风险**：uap-core 有若干「把任意 token 当家族名」的兜底规则，攻击者用随机 UA 可以造出任意多的 `browser` 值，查询层展示维度时必须 top-N 加「其它」，不能把全部值列出。

### 4.1 处理顺序

1. 去首尾空白；空串 → 返回共享的 `Empty` 结果。
2. 截断到 `MAX_UA_BYTES = 512` 字节（字符边界）。
3. 查缓存；未命中 → uap-core 三个提取器（UA、OS、设备）→ 机器人判定 → 归一化 → 写缓存。

### 4.2 机器人判定（`bot.rs`）

按优先级，命中即返回：

| 顺序 | 条件 | `BotKind` |
|---|---|---|
| 0 | UA 为空（或只有空白） | `Empty` |
| 1 | uap-core 设备族为 `Spider` | `Spider` |
| 2 | 含 `headless`、`phantomjs`、`slimerjs` | `Headless` |
| 3 | 含自动化框架名（selenium、webdriver、chromedriver、geckodriver、puppeteer、playwright、cypress、casperjs、htmlunit、jsdom），或以脚本 HTTP 客户端名开头（curl、wget、python-requests、okhttp、Go-http-client、java/、node-fetch、axios、libwww-perl 等） | `Automation` |
| 4 | 含 `bot` 作为词尾（`googlebot`、`bot/`，后接字母不算；`Cubot` 手机除外）；含爬虫 / 抓取 / 预览 / 合成监控名（表在代码里）；`+http`、`mailto:`、邮箱；不以 `mozilla/`、`opera/` 开头且带 URL | `Spider` |

全部是子串或前缀比较，没有正则，所以不会回溯，对 UA 长度线性。名字表是自维护的（§4.4 要求），每条都应有证据（访问日志里的 UA 或 crawler-user-agents 条目），增补时跑 §6.2、§6.3 的测试。

**空 UA 的决定：算机器人（`BotKind::Empty`）**。浏览器的 `fetch` / `sendBeacon` 一定带 `User-Agent` 头，空 UA 只来自脚本或被剥头的转发链。Plausible 的做法不同（不解析、不丢弃，事件入库，user_id 里 UA 当 `""`，R01 §8.1），**这是一处刻意偏离**，应记入「刻意偏离清单」。管线用 `info.is_bot()` 决定是否记 `dropped_bot`；如果将来要保守，只需在调用处忽略 `Empty`。

**试验过又放弃的规则**（误判实测，保留在代码注释里）：
- 「UA 含 `http://` 即机器人」：误伤把论坛网址写进 UA 的定制 ROM（uap-core 语料里就有）。现在只认 `+http`，或「非 `Mozilla/`/`Opera/` 开头且含 URL」。
- 「`compatible;` 且不是 IE」：误伤 Konqueror、NetFront（Kindle）、Polaris、Teleca 等老浏览器和功能机，约 60 条。
- 「含 `@` 即机器人」：误伤 Ecosia（`android@119`）、`Build/ICS@ROM`、`th-TH@calendar=gregorian`、`@TAB-NINE`。现在要求 `@` 后是带字母顶级域的域名。
- 子串 `w3c`：误伤型号 `ALLVIEW3CONECT`（`allvie`**`w3c`**`onect`）。改成 `w3c_`、`w3c-`、`w3c/`。这一条是误判测试抓出来的。

### 4.3 超长 UA

上限 **512 字节**，在字符边界截断，截断后再解析、再进缓存键。依据：
- uap-core 三个测试文件共 18 213 条 UA，最长 492 字节；crawler-user-agents 最长 285 字节。真实浏览器 UA 在 100–300 字节。测试 `truncation_cap_does_not_change_corpus_results` 证明语料里没有超过上限的 UA，也就没有截断改变结果的例子。
- 限制未命中成本：`regex` 保证线性时间，没有灾难性回溯，但1270 条规则（UA 433、OS 204、设备 633）乘以输入长度仍是线性成本。测试里 1 MB 的 UA、20 万个 `(`、10 万个 `; `、20 万个空格，在上限之后 `parse_uncached` 都在 1 秒内完成（debug 构建下断言；实际是微秒级）。
- 限制缓存内存：10 万条 × 512 B ≈ 50 MB 是键占用的上界。
- 代价：截断丢尾部。把机器人字样藏在 512 字节之后的 UA 不会被识别；但要躲过检测，直接不写机器人字样即可，没有额外收益。

## 5. 设备类别映射

`device::classify`，按顺序，第一条命中即返回（规则表同时写在代码注释里）：

| # | 条件 | 结果 | 依据 |
|---|---|---|---|
| 1 | UA 含 `ipad`，或 uap 设备族为 `iPad` / `Generic Tablet` | Tablet | iPad「移动版网站」模式发 `iPad;` |
| 2 | 系统为 iOS，或 UA 含 `iphone` / `ipod` | Mobile | |
| 3 | uap 设备族为 `Generic Smartphone` / `Generic Feature Phone` | Mobile | uap-core 对未知机型的兜底分类 |
| 4 | 系统为 Android 且 UA 含电视 / 投屏 / 机顶盒 / 游戏机记号（`android tv`、`smarttv`、`bravia`、`crkey`、`aft*`、`playstation`、`xbox`、`nintendo` 等） | Desktop | 与 Plausible 对 tv / console 的处理一致 |
| 5 | Android 且含 `tablet` | Tablet | Firefox 平板：`Android 14; Tablet;` |
| 6 | Android 且含 `mobile`（`Mobile Safari`、`Mobile;`）或 `opera mini` | Mobile | |
| 7 | Android 其余 | Tablet | Chrome 官方约定：手机带 `Mobile`，平板不带 |
| 8 | 系统是 Windows Phone / BlackBerry OS / Symbian / KaiOS / Bada / MeeGo / Maemo / Series 40 | Mobile | |
| 9 | UA 含 `tablet` / `playbook` / `kindle` | Tablet | |
| 10 | UA 含 `mobile`（`IEMobile`、`Opera Mobi`、`Mobile/`） | Mobile | |
| 11 | 其余（Windows / macOS / Linux / ChromeOS / 电视 / 游戏机 / 未知） | Desktop | |

设计取舍：
- **只有三类**，电视、游戏机、车机、未知归 `Desktop`：手机和平板需要肯定的证据，没有证据不报。Plausible 同样把 tv/console 归 Desktop，但把 car browser 归 Tablet，我们不单列。
- **不用 uap 的 brand / model 猜平板**：型号表只有 `SM-T510` 之类零散条目，覆盖不全，而且 UA 精简之后 Android 型号恒为 `K`。`Mobile` 记号在精简前后都保留，是 Android 上唯一可靠的信号。
- 规则 4–7 对 Android 的判定没有「外部 ground truth」可对照：uap-core 测试集没有设备类别字段。所以我们用 `tests/ua_info.rs::device_class_rules` 里 18 条真实 UA 手工断言（含各类边界）；真实流量上的准确率要等 P1 步骤 5 的访问日志对账。这是**未被外部数据证明**的部分，报告里如实标出。
- Plausible 的映射来自 Matomo device-detector 的设备类型（smartphone、tablet、tv……），设备库本身更细，我们的规则是启发式。预期与 Plausible 并行回放时，`device` 维度的差异主要在：Android 无 `Mobile` 记号的手机 WebView、Chrome「桌面版网站」、iPad 桌面 UA。

## 6. 外部数据证明

### 6.1 uap-core 自带测试集

`cargo test -p rivu-ua --test uap_conformance -- --nocapture`，比对 `parse_raw`（uap-core 原始结果，不含 Rivu 的归一化）：

| 文件 | 用例 | 通过 | 通过率 |
|---|---|---|---|
| `test_ua.yaml` | 1601 | 1601 | 100% |
| `test_os.yaml` | 483 | 483 | 100% |
| `test_device.yaml` | 16129 | 16129 | 100% |

允许失败清单 `ALLOWED_FAILURES` 为空。测试同时断言清单里没有「其实已通过」的多余条目，所以清单不会悄悄变大。比较约定：版本段 `null` 与空串等价（uap-core 的约定）。

### 6.2 机器人检测率

`cargo test -p rivu-ua --test bot_detection -- --nocapture`：

| 指标 | 值 |
|---|---|
| 样例 UA | 2120（1501 条规则的 `instances`） |
| 检测命中 | **1793（84.58%）**：Spider 1735、Automation 54、Headless 4 |
| 仅 uap-core `Spider`（基线） | 1322（62.36%） |
| 自维护规则的增益 | +471 个 UA（+22.2 个百分点） |
| 测试下限 | ≥ 84%，基线 ≥ 62%，且必须有增益 |

**这个检测率偏乐观，如实说明**：规则里的名字表（`SPIDER_NAMES`）有一部分是看了这份名单的漏检项之后补的（例如 Datadog Synthetics、Checkly、DareBoost 这类会执行 JS 的合成监控），不是盲测。写规则前的第一版（uap Spider + `bot` 词 + 少数通用词 + HTTP 客户端 + Headless）实测 69.6%，通用规则（联系 URL、邮箱）把它推到 79–83%，补名字表到 84.6%。

余下 327 个未命中的 UA 是一长串只出现 1–2 次的小众爬虫（`coccoc`、`binlar`、`Lipperhey`、`Pcore-HTTP`、`Nuzzel`、`Zabbix`……），大多数既不带 `bot` 字样也不带联系 URL；它们几乎不执行 JS，触发 tracker 的概率很低。靠通用规则解决不了，靠名单逐个补边际收益递减，且每补一条都增加误判风险。**对 Rivu 真正重要的是会执行 JS 的机器人**（Googlebot、无头浏览器、合成监控），这一类的覆盖要靠 P1 步骤 5 的「已知答案探针」和访问日志里 `dropped_bot` 之外的可疑流量来验证，而不是靠这个总检测率。

### 6.3 误判率

语料：uap-core 三个测试文件的 UA 去重后 17 816 条；排除 uap-core 自己标为 `Spider` 的 822 条，剩 16 994 条。

| 范围 | 条数 | 判为机器人 |
|---|---|---|
| 全部非 Spider | 16 994 | 119（0.70%）：Spider 97、Automation 18、Headless 4 |
| 浏览器形态（`Mozilla/` 或 `Opera/` 开头） | 10 389 | **9（0.09%），全部是真机器人** |
| 非浏览器形态 | 6 605 | 110：curl / wget / okhttp / python-requests 等 HTTP 客户端，WhatsApp 原生客户端（`WhatsApp/2.11…`）、WordPress / Ghost / Terraform / S3 Browser 等服务端客户端，feed 阅读器，`Mediapartners-Google` 等；都不是在浏览器里运行 tracker 的真实用户 |

浏览器形态的 9 条（测试里逐条断言能被已知原因解释）：

| UA 特征 | 数量 | 原因 |
|---|---|---|
| `HeadlessChrome` | 3 | 无头 Chrome（含 `Chromium … HeadlessChrome`） |
| `PhantomJS/1.6.0` | 1 | 无头浏览器 |
| `RuxitSynthetic/1.0` | 3 | Dynatrace 合成监控，会执行 JS，是真机器人，uap-core 没有收录 |
| `Mozilla/5.0 (compatible; Windows NT 6.1; Catchpoint)` | 1 | Catchpoint 合成监控 |
| `Mozilla/4.5 (compatible; HTTrack 3.0x; Windows 98)` | 1 | 整站抓取工具 |

所以对真实浏览器的误判为 0。测试 `false_positives_on_uap_corpus` 断言：浏览器形态里任何被判为机器人的 UA 都必须能用上表的某个原因解释，否则失败。（解释表是测试里的小常量，新增一条要写原因。）

局限：uap-core 语料偏旧（含大量功能机、Symbian、2010 年代浏览器），不代表 2026 年的真实流量分布；误判率为 0 说明规则没有明显越界，不说明真实流量上不会误伤。真实流量上的核对要等访问日志回放。

### 6.4 其它单元测试（`tests/ua_info.rs`、`src/*`）

版本截断到 major.minor；空 UA 与纯空白（`Empty`，共享同一个 `Arc`，不进缓存）；超长 UA（1 MB 垃圾、括号 / 分号 / 空格炸弹、多字节字符横跨截断边界不 panic）；缓存命中返回同一个 `Arc`（含首尾空白、超长 UA 截断后同键）；TTL 过期后重新解析；缓存有界（容量 50、写入 2000 条后不超过 200）；设备类别 18 条真实 UA；UA 精简与冻结版本；浏览器名归一化；机器人与「名字里带 bot / w3c 字样的设备」不误判；`as_str` 取值固定。

## 7. 平台约束：UA 精简、iPadOS 与决定

来源：chromium.org/updates/ua-reduction（Chrome 精简计划）、privacysandbox.google.com 的 Android 型号与版本精简公告、Apple 开发者论坛 thread 119186（iPadOS 13 桌面 UA）、WebKit bugzilla 298260 与 51degrees、Singular 的相关博文（Safari 26 冻结）。我们在 2026-10-22 通过网页检索核对了下表中标注的事实；标「未核实」的是推断。

| 平台 | 事实 | 对 `os_version` / `device` 的影响 | 决定 |
|---|---|---|---|
| Chrome 桌面（Chrome 107 起推出，113 起全量） | 次版本号精简为 `0.0.0`；Windows 恒为 `Windows NT 10.0; Win64; x64`，macOS 恒为 `Intel Mac OS X 10_15_7`（Chrome 90 起已封顶），Linux 恒为 `X11; Linux x86_64`，ChromeOS 恒为 `CrOS x86_64 14541.0.0` | Windows 10 与 11 不可区分；macOS 版本不可得；ChromeOS 版本不可得 | Windows：`os_version` 保留 `10`，文档与看板须注明**「10」代表 Windows 10 / 11**（只有 Client Hints 的 `Sec-CH-UA-Platform-Version` 能区分，需要 JS 主动请求高熵值，tracker 现在不发送）。macOS：`10.15` 视为冻结值，`os_version` 报**空串**；真实的 10.13 等老版本保留。ChromeOS：`os_version` 恒空串。浏览器主版本号仍真实（`Chrome/126.0.0.0`），`browser_version` 取 `126.0` 中的 `126` 有意义，`.0` 没有信息量。 |
| Chrome Android（Chrome 110 起推出，113 起全量） | `Android 10; K`：系统版本固定为 10、型号固定为 `K`；手机保留 `Mobile` 记号，平板没有 | 型号完全不可得；系统版本不可信；手机 / 平板仍可区分 | `Android 10; K)` 这个形状视为精简，`os_version` 报空串；带真实型号的 Android 10（如 `SM-G960F`）保留 `10`（精简前的 UA 必带型号，所以该形状可放心识别）。设备类别只靠 `Mobile` 记号，不使用型号（规则 6、7）。 |
| iOS Safari 26+ | UA 中的 iOS 版本冻结在 `18_6`（WebKit 维护者称是有意且永久的；传闻后续变成 18_6_2 或 18_7，**未核实**），真实版本在 `Version/26.x`；Chrome iOS（CriOS）不受影响 | Safari 的 `os_version` 会误报为 18.x | `Mobile Safari` 且 Safari 主版本 ≥ 26 且 UA 里的 iOS 主版本为 18 时，`os_version` 取 Safari 的 `major.minor`（26 起 Safari 与 iOS 版本号一致）。真实的 Safari 18.6 不被改写（测试覆盖）。 |
| macOS Safari / Firefox | 同样冻结在 `10_15_7` / `10.15` | 同 Chrome 桌面 | 同上，`os_version` 空串 |
| iPadOS 13+ Safari | 默认「请求桌面网站」，UA 是 `Macintosh; Intel Mac OS X 10_15_7 … Version/…`，与 Mac Safari 不可区分；关闭该选项时才发 `iPad; CPU OS …`。已知的区分办法是页面端 `navigator.maxTouchPoints > 1`（Apple 论坛回帖称 iPad 为 5，Mac 为 0，**非官方文档**）。 | iPad 被计为 macOS + Desktop，`device=tablet` 的访问量被低估，`desktop` 被高估 | **服务端不处理，如实记录为已知局限**（测试 `SAFARI_IPADOS_DESKTOP → Desktop`）。可选改进（不属于本 crate，需 tracker 协议配合）：tracker 在 `Macintosh` UA 上附带 `navigator.maxTouchPoints`，服务端据此改判为 `Tablet` + `iPadOS`。Chrome / Firefox 在 iPad 上的 UA 是否也发桌面 UA，我们没有核实，当前规则对 `CriOS` + `iPad;` 判为 Tablet。 |
| Chrome Android「桌面版网站」 | UA 变成 `X11; Linux x86_64` | 计为 Linux + Desktop | 用户自己选择的桌面模式，如实按桌面计，不处理。 |

对 Plausible 的对照：Plausible 的 `operating_system_version` 同样只取 `major.minor`，但不处理冻结值，会把所有现代 Mac 都显示为 `10.15`，Android 显示 `10`。我们把这些「看起来像数据但不是数据」的值置空，是刻意偏离，应记入「刻意偏离清单」。

## 8. 归一化规则（`SessionAttrs` 的取值）

- 浏览器名：`Chrome Mobile` / `Chrome Mobile iOS` → `Chrome`；`Mobile Safari` → `Safari`；`Firefox Mobile` / `Firefox iOS` → `Firefox`；`Opera Mobile` / `Opera Mini` → `Opera`；`Edge Mobile` → `Edge`；`IE Mobile` → `IE`；`Yandex Browser Lite` → `Yandex Browser`；`Android`（原生浏览器）→ `Android Browser`；`Chrome Mobile WebView` 与 `Mobile Safari UI/WKWebView` → `WebView`；`Other` → 空串。其余保持 uap-core 的家族名。与 Plausible 的归一化表同类，区别是 WebView 单列而不是并入 Mobile App（设备维度已经区分手机与桌面，所以「Mobile」后缀是冗余信息）。
- 系统名：`Mac OS X` → `macOS`；`Other` → 空串；其余保持。
- 浏览器与系统的版本都取 `major.minor`。

## 9. 性能

`cargo test -p rivu-ua --release --test perf -- --ignored --nocapture`（Windows，release，690 条 uap-core `test_ua.yaml` 里 `Mozilla/` 开头的 UA，非线上流量分布）：

| 路径 | 实测 | `system-design.md` §7 预算 |
|---|---|---|
| 缓存未命中（`parse_uncached`，每条 3 轮） | mean 21.3–24.1 µs；p50 12.6–12.8 µs；p95 67–77 µs；p99 134–184 µs；max 0.4–1.5 ms | 50–200 µs：**在预算内**，均值远低于下限；p99 接近上限 |
| 缓存未命中（`parse`，含 moka 写入） | p50 11.9–12.5 µs，p95 28–44 µs | 同上 |
| 缓存命中（`parse`） | mean 149–154 ns，p50 100 ns，p95 200 ns，p99 2.2 µs | ~1 µs：**在预算内**（计时分辨率 100 ns，p99 的 2.2 µs 多半是时钟或 moka 维护抖动） |
| `embedded()` 构建 | 132 ms（解析 222 KB YAML + 编译全部正则） | 启动一次 |

注意：单线程测量，没有并发压测；moka 在多线程下的命中开销需要在整条管线的 oha / k6 固定到达率压测里验证。样本是 uap-core 的测试 UA（覆盖面广但偏冷门），真实流量以少数热门 UA 为主，命中率应更高。

## 10. 建议写进 system-design 的内容

1. **§4.4「UA 解析」「机器人」两行**：UA 用 `ua-parser` 0.2（uap-core 官方 Rust 实现）加固定 commit 的 `regexes.yaml`（随仓库嵌入，不联网）；固定 commit 与升级流程见 `crates/ua/data/uap-core/PROVENANCE.md`；一致性测试 100%。机器人 = uap-core 的 `Spider` + 自维护的 Headless / 自动化 / 脚本客户端 / 爬虫名与自报家门规则，在 crawler-user-agents 上检测率 84.6%（uap 单独 62.4%），非 JS 小众爬虫的长尾不追求覆盖。
2. **§4.2 第 5 步「机器人」补一句**：空 UA 视为机器人（`BotKind::Empty`），**偏离 Plausible**（它不解析不丢弃）。丢弃时统一记 `dropped_bot`，`BotKind` 只用于统计与排错，不进 `ingest_counters.metric`。
3. **§4.2 第 7 步「UA 富化」补**：浏览器名归一化表（§8）；UA 输入截断到 512 字节；`os_version` 对冻结 / 精简值报空串（macOS 10.15、`Android 10; K`、ChromeOS），iOS Safari 26+ 用 `Version/` 修正；Windows `10` 含 11。
4. **§6 或 §8 的「维度可信度」（`brief-p1.md` 要求写明平台约束）**：`os_version` 对 Windows 无法区分 10 与 11、对 macOS / Android / ChromeOS 基本不可得；`device=tablet` 因 iPadOS 桌面 UA 而被低估；`browser` 的基数不受 uap 控制，看板必须 top-N。
5. **§4.3 UA 缓存**：键为截断后的 UA；空 UA 不进缓存。
6. **§7 性能预算**：UA 缓存命中约 150 ns，未命中均值约 22 µs（p99 约 130–180 µs），均在预算内；`embedded()` 约 130 ms 启动成本。
7. **刻意偏离清单**新增：空 UA 视为机器人；冻结的 OS 版本置空；iOS Safari 26+ 的 OS 版本修正；WebView 单列。
8. **可选项（需 tracker 协议配合，建议放入 P2 以后）**：tracker 在 `Macintosh` UA 上附带 `navigator.maxTouchPoints`，让服务端把 iPadOS 桌面 UA 改判为平板；用 Client Hints 的 `platformVersion` 区分 Windows 10 / 11。

## 11. 未完成与风险

- **设备类别没有外部 ground truth**：uap-core 测试集没有类别字段，规则是启发式，靠 18 条手工断言支撑。真实流量上的准确率要等访问日志回放，才能与 Plausible 的结果并行对比。
- **机器人检测率是对一份名单的数字**，不是对线上流量的数字；名单里的小众爬虫（327 个 UA 未命中）几乎不触发 tracker。「已知答案探针」（brief 验收项）要覆盖 headless Chrome、Playwright、Selenium 这类会执行 JS 的机器人，这部分在本 crate 里只有单元测试，没有端到端验证。
- **数据中心 CIDR**（§4.4「可选」）不在本 crate 范围（`DropReason::DatacenterIp` 由别处负责）。
- **uap-core 的升级**需要手工换 commit、重新下载并跑测试；没有自动化。
- **未实测 `uaparser` crate 的通过率与耗时**，选择理由基于维护状态与依赖。
- 没有做并发性能压测；单次耗时在 Windows 上测得，Linux 上的数字应通过 `linux-task.ps1` 再测一次（本次按要求没有运行）。
- `serde-saphyr` 2.0 发布不到两周（2026-10-13），较新；风险被限制在启动时解析一个固定的内嵌文件，一致性测试 100% 通过即证明它读对了这份数据；若将来出问题，可换成 `yaml-rust2` 手工遍历，或在构建期把 YAML 转成 JSON。
- 一处与任务不一致的说明：`UaParser::parse` 的缓存对同一 UA 的并发未命中不做合并（两个线程可能各解析一次），结果相同，只多花一次 CPU。
