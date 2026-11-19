# 富化：来源（referrer → source）与渠道（channel）

> 范围：`system-design.md` §4.2 第 8 步里的「referrer → source」「渠道分类」，实现为 `crates/source`（`rivu-source`），纯库，尚未接入管线。
> 记号：**事实**=在本文所列来源里直接看到并核对过；**决定**=Rivu 的取舍。一手来源都附 URL 与查阅日期（2026-10-22）。
> Plausible 服务端是 AGPL-3.0，本文和代码只借鉴行为，没有复制其代码或数据文件（`custom_sources.json`、`ga4-source-categories.csv` 都没有拷贝）。

## 0. 结论

| # | 结论 |
|---|---|
| 1 | 渠道取值集合 = GA4 默认渠道分组的 19 个渠道，去掉 `Unassigned`（§3.1）。英文标识沿用 GA4 官方名称（同时也是 Plausible 存的值，只有 `AI Assistant` 单复数不同），中文名取自 Google 官方简体中文帮助页。 |
| 2 | 规则是一张 21 条的有序表，先匹配先赢（§3.3）；顺序以 GA4 帮助页「Channels for manual traffic」表为准，与 Plausible 的 Elixir 版、SQL 版都不完全一致，Rivu 只有一份实现。 |
| 3 | 来源名数据选 **Matomo `searchengine-and-social-list`（CC0-1.0）**，不选 Snowplow `referers.yml`：后者的 README 写明数据按 **GPL-3.0** 提供，不能随 MIT 项目分发（§4.1）。固定到 commit `4254b8d`，放在 `crates/source/data/matomo/`，构建时核对 SHA-256。 |
| 4 | 视频、购物、网页邮箱三类来源 Matomo 没有；AI 助手 Matomo 有 19 个。差额由自维护的 `data/supplement.yml`（135 条，每条写明依据）补齐。类别以 Google 官方「来源与类别」清单为对照（§2.2）。 |
| 5 | 来源优先级：`utm_source`（含 `source`、`ref` 回退）> referrer host > click id 推断（§5）。内部引荐由调用方传 `None`，此时没有其他信号即 Direct。 |
| 6 | 用 `build.rs` 在编译期把 YAML 转成排序的静态表，运行时二分查找、无初始化、无解析（§4.5）。`classify` 在 release 下约 143 ns/次（含返回值的一次 `String` 分配）。 |
| 7 | 与 Plausible 有 12 处刻意偏离（§6），其中 6 处有行为对照用例；GA4 页面本身没有的规则（click id 推断、webmail、AI 优先）单独标出。 |

## 1. GA4 默认渠道分组：一手来源

- 页面：<https://support.google.com/analytics/answer/9756891?hl=en>「Default channel group」，查阅于 **2026-10-22**，HTML 的 SHA-256 为 `9140300438dee97238298cb83a38759d153ec47b60617c84a36fdbfaac58cb21`（本地留存于会话临时目录，未入库）。
- 简体中文版：<https://support.google.com/analytics/answer/9756891?hl=zh-Hans>，同日查阅，SHA-256 `bad87c15fe39f1754c959c03197c895844e0e21de4dc725d12395a28071aba2f`。

### 1.1 摘录：渠道说明（Default channel descriptions）

页面开头：「Channels are rule-based definitions of your website's traffic sources …」。共 19 个渠道：

| 页面里的名称 | 官方简体中文名 | 页面对它的描述（节选） |
|---|---|---|
| Affiliates | 联属营销方 | via links on affiliate sites |
| AI Assistant | AI 助理 | from sources like ChatGPT, Gemini, Deepseek, Copilot, or Grok. It excludes Google's AI Overviews and AI Mode |
| Audio | 音频 | via ads on audio platforms (e.g., podcast platforms) |
| Cross-network | 跨广告网络 | via ads that appear on a variety of networks (e.g., Search and Display) |
| Direct | 直接 | via a saved link or by entering your URL |
| Display | 展示广告 | via display ads, including ads on the Google Display Network |
| Email | 邮件 | via links in email |
| Mobile Push Notifications | 移动推送通知 | via links in mobile-device messages when they're not actively using the app |
| Organic Search | 自然搜索 | via non-ad links in organic-search results, including Google's AI Overviews and AI Mode |
| Organic Shopping | 自然购物 | via non-ad links on shopping sites like Amazon or ebay |
| Organic Social | 自然社交 | via non-ad links on social sites like Facebook or Twitter |
| Organic Video | 自然视频 | via non-ad links on video sites like YouTube, TikTok, or Vimeo |
| Paid Other | 其他付费渠道 | via ads, but not through an ad identified as Search, Social, Shopping, or Video |
| Paid Search | 付费搜索 | via ads on search-engine sites like Bing, Baidu, or Google |
| Paid Shopping | 付费购物 | via paid ads on shopping sites like Amazon or ebay or on individual retailer sites |
| Paid Social | 付费社交媒体广告 | via ads on social sites like Facebook and Twitter |
| Paid Video | 付费视频 | via ads on video sites like TikTok, Vimeo, and YouTube |
| Referral | 引荐 | via non-ad links on other sites/apps (e.g., blogs, news sites) |
| SMS | 短信 | via links from text messages |

### 1.2 摘录：「Channels for manual traffic」定义表（页面顺序）

页面声明：「Channel definitions aren't case sensitive and can't be edited.」「Unassigned is the value Analytics uses when there are no other channel rules that match the event data.」页面**没有**用文字声明表内顺序即优先级；本文按表的行序理解为先匹配先赢。Plausible 的 Elixir 版与 SQL 版排列互不相同（§6 D2），所以不能拿它当顺序依据。

| 页面顺序 | Channel | Analytics definition（原文，保留拼写） |
|---|---|---|
| 1 | Direct | Source exactly matches "(direct)" AND Medium is one of ("(not set)", "(none)") |
| 2 | Cross-network | Campaign Name contains "cross-network" |
| 3 | Paid Shopping | Source matches a list of shopping sites OR Campaign Name matches regex `^(.*(([^a-df-z]\|^)shop\|shopping).*)$` AND Medium matches regex `^(.*cp.*\|ppc\|retargeting\|paid.*)$` |
| 4 | Paid Search | Source matches a list of search sites AND Medium matches regex `^(.*cp.*\|ppc\|retargeting\|paid.*)$` |
| 5 | Paid Social | Source matches a regex list of social sites AND Medium matches regex（同上） |
| 6 | Paid Video | Source matches a list of video sites AND Medium matches regex（同上） |
| 7 | Display | Medium is one of ("display", "banner", "expandable", "interstitial", "cpm") |
| 8 | Paid Other | Medium matches regex `^(.*cp.*\|ppc\|retargeting\|paid.*)$` |
| 9 | Organic Shopping | Source matches a list of shopping sites OR Campaign name matches regex `^(.*(([^a-df-z]\|^)shop\|shopping).*)$` |
| 10 | Organic Social | Source matches a regex list of social sites OR Medium is one of ("social", "social-network", "social-media", "sm", "social network", "social media") |
| 11 | Organic Video | Source matches a list of video sites OR Medium matches regex `^(.*video.*)$` |
| 12 | Organic Search | Source matches a list of search sites listed under "SOURCE_CATEGORY_SEARCH" OR Medium exactly matches organic |
| 13 | AI Assistant | The medium exactly matches "ai-assistant". The medium is set to "ai-assistant" and the campaign is set to "(ai-assistant)" if the referrer matches a list of AI Assistants. |
| 14 | Referral | Medium is one of ("referral", "app", or "link") |
| 15 | Email | Source = email\|e-mail\|e_mail\|e mail OR Medium = email\|e-mail\|e_mail\|e mail |
| 16 | Affiliates | Medium = affiliate |
| 17 | Audio | Medium exactly matches audio |
| 18 | SMS | Source exactly matches sms OR Medium exactly matches sms |
| 19 | Mobile Push Notifications | Medium ends with "push" OR Medium contains "mobile" or "notification" OR Source exactly matches "firebase" |

页面里 Google Ads、Display & Video 360、Campaign Manager 360、Search Ads 360、Merchant Center 的渠道规则依赖这些产品传来的字段（来源平台、广告网络类型、广告系列类型、创意格式），采集端拿不到，**不实现**（§3.4）。

### 1.3 页面内部的不一致（事实）

- 说明表里写「Organic Video：video sites like YouTube, TikTok, or Vimeo」，但 §2.2 的官方来源清单把 `tiktok`、`tiktok.com` 归为 `SOURCE_CATEGORY_SOCIAL`。规则引用的是清单（「Source matches a list of video sites」），Rivu 取清单：TikTok 是社交（偏离清单 D11）。
- 清单里 `niconico`（名字）是 SOCIAL，`nicovideo.jp`（域名）是 VIDEO。Rivu 取 VIDEO，因为是视频站。

## 2. 来源分类清单

### 2.1 Plausible 的对应物（只读行为）

Plausible 的 `lib/plausible/ingestion/acquisition.ex` 注释写明它用 `priv/ga4-source-categories.csv`（来自上面的 GA4 页面）加一份手工追加类别（AI 助手、Hacker News、Mastodon、Bluesky、Gmail……）。对照结论见 §6。

### 2.2 Google 官方「来源与类别」清单

- GA4 页面「Analytics' list of sources and categories」一节的链接：<https://storage.googleapis.com/support-kms-prod/qn1xhBu8MVcZPIZ2WZMNdI40FtZXFPGYxj2K>（标题「Download a spreadsheet of GA4 default-channel-group sources and categories」），2026-10-22 下载。
- 实际返回的是 **PDF**（`application/pdf`，27 页，126 019 字节，SHA-256 `d7704668595329588e74c7550a464995cdad81023e28929b94129a71fe46b169`），不是 CSV；文本抽取后得到 819 行 `source<空格>SOURCE_CATEGORY_X`：SEARCH 129、SOCIAL 592、VIDEO 46、SHOPPING 52，没有重复的 source。
- **许可结论：未找到许可声明**（页面和文件都没有），按「不可再分发」处理：**不入库**，不拷贝文件。补充表里的类别依据写成 `GA4-LIST`，意思是「该清单把这个来源列为这一类」，条目本身是公开可验证的事实（某个站点是视频站），不是清单的文字拷贝。Rivu 不复制清单的整体选择：它的 592 个社交来源里有约 460 个是 2012 年前后的长尾站点（`43things`、`activeworlds`……），Rivu 只收常见的（D8）。
- 复现对照统计：把清单转成每行 `source<TAB>category` 的文本，设置 `RIVU_GA4_SOURCES_TSV=<文件>`，运行
  `cargo test -p rivu-source --test source_data ga4_list -- --ignored --nocapture`。统计结果见 §7.3。
- GA4 页面注明清单由 `googleanalytics-channelgrouprequests@google.com` 受理增补，「reviews these requests at least once per year」——清单会变，所以 Rivu 不跟随它，规则版本由 `RULES_VERSION` 管。

## 3. 渠道集合与有序规则

### 3.1 取值集合（19 个）

`Channel::as_str()` 是写入 ClickHouse `channel` 列的稳定值，发布后不能改；新增、改名都要加 `RULES_VERSION` 并回填。

| 英文标识（`as_str()`） | 中文名（Google 官方中文页） | 判定条件（一句话；完整见 §3.3） |
|---|---|---|
| `Direct` | 直接 | 没有来源，且没有任何 medium 规则命中；或 `(direct)` 配 `(none)`/`(not set)` |
| `Cross-network` | 跨广告网络 | campaign 含 `cross-network` |
| `Paid Search` | 付费搜索 | 搜索来源 + 付费 medium；或 Google/Bing 的 click id 配同平台来源 |
| `Paid Social` | 付费社交媒体广告 | 社交来源 + 付费 medium；或 TikTok 的 `ttclid` 配 TikTok 来源 |
| `Paid Video` | 付费视频 | 视频来源 + 付费 medium |
| `Paid Shopping` | 付费购物 | （购物来源或购物 campaign）+ 付费 medium |
| `Paid Other` | 其他付费渠道 | 付费 medium，且上面都没命中 |
| `Display` | 展示广告 | medium ∈ {display, banner, expandable, interstitial, cpm} |
| `Organic Search` | 自然搜索 | 搜索来源，或 medium = organic |
| `Organic Social` | 自然社交 | 社交来源，或社交类 medium |
| `Organic Video` | 自然视频 | 视频来源，或 medium 含 video |
| `Organic Shopping` | 自然购物 | 购物来源，或购物 campaign |
| `AI Assistant` | AI 助理 | AI 助手来源，或 medium = ai-assistant |
| `Email` | 邮件 | 来源/medium 提到 email，或网页邮箱的 referrer |
| `Affiliates` | 联属营销方 | medium = affiliate |
| `Audio` | 音频 | medium = audio |
| `SMS` | 短信 | 来源或 medium = sms |
| `Mobile Push Notifications` | 移动推送通知 | medium 以 push 结尾、含 mobile/notification，或来源 = firebase |
| `Referral` | 引荐 | medium ∈ {referral, app, link}，或有来源 |

没有 `Unassigned` 和 `(other)`：见 D9。UI 展示顺序建议用 `Channel::ALL`（直接和自然类在前，付费类在后），它不是匹配顺序。

### 3.2 输入归一

- 所有比较 ASCII 大小写不敏感，不分配（GA4：「Channel definitions aren't case sensitive」）；非 ASCII 字符不做折叠。
- `utm.medium`、`utm.campaign`、`utm.source` 先 trim，空串视为缺失（调用方 `Utm` 已保证，库自己也容忍）。
- 正则的手写等价物按 RE2 语义：`.` 不匹配 `\n`，整串锚定的 `^(.*x.*)$` 在含换行的输入上整体失败。
- 付费 medium `^(.*cp.*|ppc|retargeting|paid.*)$` = 含 `cp`，或整串 = `ppc`，或整串 = `retargeting`，或以 `paid` 开头。`unpaid`、`ppc-brand`、`retargeting-fb` **不是**付费；`tcp`、`a-cp-b` **是**（`cp` 可以出现在任意位置，GA 同样）。
- 购物 campaign `^(.*(([^a-df-z]|^)shop|shopping).*)$` = 含 `shopping`，或 `shop` 位于开头，或 `shop` 前面是 `e`、数字、符号、非 ASCII：`eshop`、`e-shop`、`my_shop`、`coffeeshop` 命中，`workshop`、`photoshop`、`bookshop` 不命中。

### 3.3 有序规则表（先匹配先赢）

`来源类别` 指已解析的来源（§4、§5）所属类别：search / social / video / shopping / ai / email / other（只命名，无类别）。`付费证据` = 付费 medium 或来源写法本身是付费别名（`fb-ads`、`adwords`，§4.6）。

| 序 | 渠道 | 条件 | GA4 页面行 | 与 GA4 的差异 |
|---|---|---|---|---|
| R01 | Direct | 来源恰为 `(direct)` 且 medium 为空/`(not set)`/`(none)` | 1 | 同；medium 缺失按 `(not set)` |
| R02 | Cross-network | campaign 含 `cross-network` | 2 | 同 |
| R03 | Paid Shopping | （来源类别=shopping 或 购物 campaign）且 付费证据 | 3 | 加了付费别名 |
| R04 | Paid Search | 来源类别=search 且 付费证据 | 4 | 加了付费别名 |
| R05 | Paid Social | 来源类别=social 且 付费证据 | 5 | 加了付费别名 |
| R06 | Paid Video | 来源类别=video 且 付费证据 | 6 | 加了付费别名 |
| R07 | Display | medium ∈ {display, banner, expandable, interstitial, cpm}（整串） | 7 | 同；排在 R04–R06 之后，所以 `cpm`+搜索来源是 Paid Search |
| R08 | Paid Other | 付费 medium | 8 | 同（只看 medium，不看付费别名） |
| R09 | Paid Search / Paid Social | click id 付费推断：click id 的平台 = 已解析来源（`gclid`/`gbraid`/`wbraid` 配 Google，`msclkid` 配 Bing → Paid Search；`ttclid` 配 TikTok → Paid Social） | —（对应 Google Ads 自动标记） | **Rivu 追加**（D6、D2） |
| R10 | Organic Shopping | 来源类别=shopping，或购物 campaign | 9 | 同 |
| R11 | Organic Social | 来源类别=social，或 medium ∈ {social, social-network, social-media, sm, social network, social media} | 10 | 同 |
| R12 | Organic Video | 来源类别=video，或 medium 含 `video` | 11 | 同 |
| R13 | AI Assistant | 来源类别=ai，或 medium = `ai-assistant` | 13 | **提前到 Organic Search 之前**（D10） |
| R14 | Organic Search | 来源类别=search，或 medium = `organic` | 12 | 同（位置见 R13） |
| R15 | Email | 来源类别=email（网页邮箱 referrer），或 utm_source/medium 含 `email`、`e-mail`、`e_mail`、`e mail`、`newsletter` | 15 | 放宽成「包含」并加 `newsletter`；网页邮箱 referrer（D5） |
| R16 | Affiliates | medium = `affiliate` | 16 | 同 |
| R17 | Audio | medium = `audio` | 17 | 同 |
| R18 | SMS | 来源 = `sms` 或 medium = `sms` | 18 | 同 |
| R19 | Mobile Push Notifications | medium 以 `push` 结尾，或含 `mobile`/`notification`，或来源 = `firebase` | 19 | 同 |
| R20 | Referral | medium ∈ {referral, app, link}，或来源非空 | 14 | 「或来源非空」是 Plausible 的做法，GA4 里 referrer 流量的 medium 恒为 referral；打标签但 medium 不认识的流量落这里而不是 Unassigned（D9） |
| R21 | Direct | 其余 | —（GA4 的 Unassigned 的一部分） | D9 |

每条规则在 `tests/ga4_rules.rs` 里有对应测试 `r01_…`–`r21_…`，至少一个正例和一个边界反例（共 43 个测试）。

### 3.4 不实现的 GA4 规则

页面里「Channels for Google Ads traffic」「Display & Video 360」「Campaign Manager 360」「Search Ads 360」「Merchant Center」几节需要广告平台回传的字段：来源平台、广告网络类型（Google Search/Partners/YouTube/Display Network/…）、广告系列类型（Shopping/Performance Max/Demand Gen/…）、创意格式。Rivu 的采集端只有 URL 与 referrer，拿不到。所以 Google Ads 自动标记流量只能靠 `gclid` 等 click id 推断到 `Paid Search`（R09），无法区分 Paid Shopping、Paid Video、Display、Cross-network；用户想要这些，需要在广告链接里打 UTM（`utm_medium=cpc` + `utm_campaign=cross-network` 之类）。

### 3.5 规则引用的来源类别列表出自哪里

| 类别 | 数据 | 说明 |
|---|---|---|
| search | Matomo `SearchEngines.yml`（297 个名字）+ 补充表 19 条 | 补充表主要是把 GA4-LIST 里有而 Matomo 没有的入口补上（`yahoo.com`、`msn.com`、`daum.net`……）和 utm_source 写法 |
| social | Matomo `Socials.yml`（91 个）+ 补充表 57 条 | 补充表来自 GA4-LIST 的常见站点、Plausible 行为（Slack、Microsoft Teams、Steam 社区）、Rivu 自行补充（知乎、小红书） |
| video | 补充表 18 条 | Matomo 没有视频类别；以 GA4-LIST 的 VIDEO 为对照（46 条里 44 条一致，余下 2 条见 §7.3）。YouTube、Vimeo、Niconico 在 Matomo 里是社交，补充表覆盖 |
| shopping | 补充表 20 条 | Matomo 没有购物类别；以 GA4-LIST 的 SHOPPING 为对照（52 条里 51 条一致） |
| ai | Matomo `AIAssistants.yml`（19 个）+ 补充表 12 条 | 补充表加了 Kimi、豆包、腾讯元宝、Poe、Pi、Phind 等与 utm_source 别名 |
| email | 补充表 6 条（Gmail、Outlook、Yahoo! Mail、Proton Mail、QQ 邮箱、163 邮箱） | 网页邮箱的 referrer；GA4 页面没有对应规则，参照 Plausible 把 Gmail 归 Email（D5） |
| other | 补充表 3 条（Wikipedia、Microsoft 365、Linktree） | 只命名，渠道落 Referral |

## 4. 来源命名与数据

### 4.1 候选数据的许可核对（逐个核对**数据文件本身**）

| 候选 | 数据文件 | 许可核对 | 结论 |
|---|---|---|---|
| Snowplow referer-parser | <https://github.com/snowplow-referer-parser/referer-parser>，`resources/referers.yml`，查阅时 master 为 `a0f766fa0c54…`（2026-07-30），85 334 字节，SHA-256 `3ddb71dd3647d5e6474ddf737a395b96ffd3ac46d1faadddb3cab775281fb347d`；6 个类别（search 237、social 90、email 49、paid 45、chatbot 9、unknown 3），2506 个域名 | 仓库根目录**没有 LICENSE 文件**，GitHub 不识别许可。README「Copyright and license」一节原文：「`referers.yml` is based on Matomo's `SearchEngines.php` and `Socials.php`, copyright 2012 Matthieu Aubry and available under the **GNU General Public License v3**」。代码的许可（历史上是 Apache-2.0）不适用于这个数据文件 | **不可用**：GPL-3.0 数据随 MIT 项目分发会把 GPL 义务带给所有分发者 |
| Matomo searchengine-and-social-list | <https://github.com/matomo-org/searchengine-and-social-list>：`SearchEngines.yml`、`Socials.yml`、`AIAssistants.yml`；固定 commit `4254b8d3182b23b4a4708902ff68631d8ec925ae`（2026-09-23） | 仓库根 `LICENSE` 是完整的 **CC0 1.0 Universal** 文本；`package.json` 的 `license` 为 `CC0-1.0`，`composer.json` 为 `CC0-1.0`；GitHub API 识别为 `cc0-1.0`。这是该仓库自己的内容（数据文件），不是代码许可对数据的推论 | **可用**（CC0，无署名义务；仍在 `data/matomo/LICENSE` 保留原文） |
| Google GA4 来源清单 | §2.2 | 无许可声明 | 不入库，只作对照 |
| Plausible `custom_sources.json`、`ga4-source-categories.csv` | 在 AGPL-3.0 的 Plausible 仓库里 | AGPL-3.0 | 不拷贝；只读行为 |

补充说明：Snowplow 的数据比 Matomo 大得多（2506 个域名；与 Matomo 的 SearchEngines+Socials 相比，只在 Snowplow 的 1432 个、只在 Matomo 的 388 个、共有 484 个），而且有 `email`、`paid`、`chatbot` 类别；但它的授权来源是 Matomo 的 2012 年 GPL 版本，Matomo 后来把同一批列表以 CC0 重新发布。要用 Snowplow 的增量，需要先取得它的贡献者/Snowplow 的书面许可，或以 Rivu 自己核实的事实重写；本次不做。

来源 URL、commit、下载日期、SHA-256：`crates/source/data/matomo/PROVENANCE.md`。

### 4.2 选择

Matomo 三份列表 + 自维护补充表。合并优先级（先登记的赢）：**补充表 > AI 助手 > 社交 > 搜索**。补充表还承担三件事：`renames`（把 Matomo 的名字改成展示名：`Twitter`→`X (Twitter)`、`reddit`→`Reddit`、`Google%2B`→`Google+`、`Haboo`→`Habbo`、`Deepseek`→`DeepSeek`、`Copilot`→`Microsoft Copilot`、`Gemini`→`Google Gemini`；并把 `Google Images/Maps/News/Scholar/…`、`Bing Images`、`Yahoo! Images/Directory`、`Yandex Images` 等 17 个子产品名合并回主名，因为用户要的是「来自 Google」，不是「来自 Google 图片」）、覆盖类别（YouTube 等归视频）、加 utm_source 别名。

### 4.3 匹配规则

1. **键的规范化**：小写；去掉前导 `www.`；带路径的条目跳过（`google.com/products`、`x.com/i/grok` 等 14 条，因为接口只有 host，把它们降级成 host 会让整个站点变成一个子产品——`x.com` 会整站变成 Grok）。
2. **后缀匹配**：host 从最长到最短依次尝试它的后缀（按 `.` 切），第一个命中者胜出。`www.google.co.jp` 命中键 `google.co.jp`；`a.b.google.co.jp` 也命中。只在标签边界上匹配：`notyoutube.com` 不命中 `youtube.com`，`google.com.evil.com` 不命中任何东西。单独的顶级标签（`com`、`co.uk` 里的 `uk`）不会被尝试。
3. **`.{}` 占位符**（Matomo README：「`searchengine.{}` would also match `searchengine.de` or `searchengine.nl`」「For TLDs only `{}` would also match combined TLDs like `co.uk`. (Full list `com.*, org.*, net.*, co.*, it.*, edu.*`)」）：结尾的 `.{}` 代表
   - 一个两字母的国家/地区代码，或 `com`/`net`/`org`/`edu`（`google.de`、`google.com`）；
   - 或 `com`/`org`/`net`/`co`/`it`/`edu` 加一个两字母代码（`google.co.uk`、`google.com.br`、`google.edu.cn`）。

   `google.xyz`、`google.info`、`google.evil.de` 不匹配。非结尾位置的 `{}`（`{}.bing.com`、`images.{}.ask.com`）表示国家子域，被后缀匹配天然覆盖，保留它后面那段（`bing.com`、`ask.com`）。
4. **特异性**：精确/后缀键与 `.{}` 键同时命中时，覆盖标签更多的赢，平手取精确键。所以 `gemini.google.com` → `Google Gemini`，`mail.google.com` → `Gmail`，`accounts.google.com` → `Google`；`blog.naver.com` → `Naver Blog`，`search.naver.com` → `Naver`。
5. 大小写与结尾的 `.`：容忍（`WWW.GOOGLE.COM`、`google.com.` → Google）；非 ASCII 字符不折叠。
6. 查不到：用 host 本身作来源名（Plausible 同）。IP 字面量、punycode、`localhost` 也一样，不特殊处理（`localhost` 由调用方在内部引荐/规范化阶段处理）。

### 4.4 数据驱动测试：一致性与冲突

`tests/source_data.rs` 只从 YAML 文件计算期望值，对每个域名键构造样本 host（域名本身、`www.` 前缀、`rivu-test.` 子域、大写、结尾点；`.{}` 键展开成 `.com .de .co.uk .com.br .co.jp .fr .it` 等 9 个），断言 `classify` 返回胜出条目的名字和类别对应的渠道。**1013 个域名键、5165 个 host 样本，0 个不一致**（样本被更具体的键合法地抢走时跳过，判定由测试里独立的实现完成）。

同一域名被多个条目声明的冲突共 **11 条**，逐条列在测试里，数据升级后出现新冲突会失败：

| 域名 | 胜者 | 败者 | 取舍理由 |
|---|---|---|---|
| `chatgpt.com`、`chat.openai.com` | AIAssistants:ChatGPT | SearchEngines:ChatGPT | Rivu 有独立的 AI Assistant 渠道 |
| `perplexity.ai` | AIAssistants:Perplexity | SearchEngines:Perplexity | 同上 |
| `you.com` | AIAssistants:You | SearchEngines:You | 同上 |
| `facebook.com` | Socials:Facebook | SearchEngines:Facebook | Matomo 把 `www.facebook.com`（站内搜索）当搜索引擎；整站应归社交 |
| `youtube.com`、`youtu.be` | 补充表:YouTube（视频） | Socials:YouTube | GA4-LIST 归视频 |
| `vimeo.com` | 补充表:Vimeo（视频） | Socials:Vimeo | 同上 |
| `nicovideo.jp` | 补充表:Niconico（视频） | Socials:Niconico | GA4-LIST 里该域名是 VIDEO |
| `digg.com` | 补充表:Digg（社交） | SearchEngines:Digg | GA4-LIST 归社交 |
| `friendfeed.com` | 补充表:FriendFeed（社交） | SearchEngines:FriendFeed | 同上 |

没有「同一域名、同一类别、两个不同名字」的冲突，没有同级冲突。

### 4.5 为什么用 build.rs，而不是启动时解析

二选一，选 `build.rs`：

- 运行时只有 `&'static [(&str, u16)]` 三张排序表（域名 988 条、`.{}` 通配 25 条、别名 531 条，条目 489 个，生成的 `tables.rs` 约 70 KB），二分查找，**没有 YAML 依赖、没有解析、没有 `LazyLock` 初始化、没有「数据缺失/损坏」的启动期失败路径**；
- 数据文件的 SHA-256 在构建时核对：上游文件被改动（包括被换行转换）直接让构建失败，数据升级必须同时改 `build.rs` 的哈希、`PROVENANCE.md`、`RULES_VERSION`、本文；
- 补充表的格式错误（缺 `basis`、未知 `kind`）也在构建期暴露；
- 代价：`serde-saphyr`（工作区里 `crates/ua` 已在用）和 `sha2` 作为 build-dependency 多编译一遍；改数据要重编本 crate（几秒）。没有新增工作区依赖。

`DATA_FINGERPRINT`（`data/` 下四个文件内容的 SHA-256）是公开常量，`tests/source_data.rs` 把 `(RULES_VERSION, DATA_FINGERPRINT)` 钉在一起：数据改了而版本没加会测试失败。渠道规则（Rust 代码）的改动靠 `tests/ga4_rules.rs`、`plausible_parity.rs` 的断言兜住，仍需要人记得加版本。

### 4.6 补充表（`data/supplement.yml`，135 条）

- 每个条目有 `name`、`kind`、`domains`、`aliases`、`paid_aliases`、`basis`；没有 `basis` 构建失败。`basis` 的缩写：`GA4-LIST`（Google 清单把该来源列为该类别）、`FACT`（公开可验证的事实，如官方域名）、`CONVENTION`（业界常见的 utm_source 写法，行为参照 Plausible）。
- 类别数：social 57、shopping 20、search 19、video 18、ai 12、email 6、other 3。
- **别名**：名字自动成为别名（不区分大小写）；`aliases` 是常见写法（`fb`、`ig`、`yt`、`tw`……）；`paid_aliases`（31 个）是「写法本身表示付费」的来源（`fb-ads`、`google_ads`、`adwords`、`yt-ads`、`twitter-ads`……），解析到对应来源，并作为付费证据参与 R03–R06。这是 Plausible 的行为（它的 `@paid_sources`），GA4 没有（D12）。
- 每个别名/名字在测试里都验证：解析到胜出条目的名字，渠道 = 类别对应的自然渠道；付费别名验证 → 对应的 Paid 渠道。

## 5. 优先级

### 5.1 来源取哪个

1. **`utm.source` 存在** → 取它（`Utm.source` 已包含 `utm_source` → `source` → `ref` 的回退，空串视为缺失）。先查别名表（不区分大小写，名字本身也是别名），再把像域名的写法（含 `.`）当 host 查表（`utm_source=l.facebook.com` → `Facebook`），都不认识就**原样保留，包括大小写**（`BetaList` 还是 `BetaList`）。referrer 此时不参与来源命名。
2. 否则 **referrer host 存在** → 按 §4.3 查表，查不到用 host。
3. 否则 **click id 存在** → 按参数名推断来源（下表）。
4. 否则来源为空串，渠道由 medium 等规则决定，没有命中就是 `Direct`。

Plausible 的 `Source.resolve` 在前两步完全一致（utm 优先，`canonical()` 先不区分大小写查别名，未命中保留原样）。

### 5.2 click id

`click_id_param` 只有参数名（`rivu-domain` 已规范化）。推断规则：

| 参数 | 推断来源 | 付费证据？ | 依据 |
|---|---|---|---|
| `gclid`、`gbraid`、`wbraid` | Google | 是（→ Paid Search） | Google Ads 自动标记；Plausible 同（`source == google and gclid`） |
| `msclkid` | Bing | 是（→ Paid Search） | Microsoft Advertising 自动标记；Plausible 同 |
| `ttclid` | TikTok | 是（→ Paid Social） | TikTok 官方帮助页「About TikTok Click ID」：点击广告时附加到落地页 URL |
| `fbclid` | Facebook | 否 | 会附加到 Facebook 上**任何**外链点击（含自然帖子），不能证明付费 |
| `twclid`、`li_fat_id` | X (Twitter)、LinkedIn | 否 | 只找到第三方文档，没有平台官方说明，保守处理 |

- **付费证据只在 click id 的平台等于已解析来源时才算**：`google.com` + `gclid` → Paid Search；`duckduckgo.com` + `gclid` → Organic Search；`google.com` + `msclkid` → Organic Search（与 Plausible 的测试一致）。
- **来源推断只在没有 utm_source、也没有 referrer 时发生**（隐私设置抹掉 referrer 的常见情形）：只带 `gclid` → 来源 `Google`、渠道 `Paid Search`。Plausible 在这里因来源为空而是 `Direct`（D6）。
- 显式的 `utm_medium=display` 优先于 click id（R07 在 R09 之前）。Plausible 的 Elixir 版和 SQL 版在「Google + `medium=display` + `gclid`」上结果不同（前者 Paid Search，后者 Display，见 `01-ingestion-pipeline.md` §8.4），Rivu 取 Display，并只有一份实现（D2）。

### 5.3 内部引荐与 Direct

调用方（管线第 8 步）判定「referrer 的 host 命中站点 hostname 白名单」后传 `referrer_host: None`，库不区分这和「没有 referrer」。Direct 的条件：

- 没有 `utm.source`，没有 referrer（含内部引荐），没有可推断的 click id；
- 且没有 medium 类规则命中（`email`、`sms`、`affiliate`、`audio`、`cpm`、`display`、`organic`、`social`、`video`、`ai-assistant`、含 `mobile`/`push`/`notification`、付费 medium、含 `cross-network` 的 campaign……这些规则不依赖来源）；
- 或者 `utm_source=(direct)` 配 `(none)`/`(not set)`/无 medium（R01）。

内部跳转的落地页自带 `utm_source` 时，来源按 utm_source（Plausible 同；内部引荐本来就不该带标签，带了就是用户的意图）。

## 6. 刻意偏离清单

「Plausible」列写 Plausible 的行为；「GA4」列写 GA4 页面；有测试对照的标出用例名。表里 `| Dn |` 的格式是 `tests/plausible_parity.rs::deviations_are_all_documented` 检查的对象。

| 编号 | 主题 | Rivu | Plausible | GA4 | 理由 / 测试 |
|---|---|---|---|---|---|
| D1 | AI 渠道名 | `AI Assistant` | `AI Assistants`（自定义） | `AI Assistant` | 取 GA4 官方名称，一手来源优先。`plausible_parity`：8 行（Perplexity、Gemini、ChatGPT、Claude、Phind、DeepSeek、Copilot、Grok） |
| D2 | Display 与 click id 的顺序 | 显式 `display` medium 优先于 click id；Display 排在 Paid Search/Social/Video 之后 | Elixir 版：Paid Search 在 Display 之前；SQL 版：Display 在最前，两者对「Google + display + gclid」结果不同 | Display 在 Paid Video 之后 | 只有一份实现，顺序随 GA4。`plausible_parity`：`google + display + gclid` |
| D3 | `medium=organic` | → Organic Search | 无此规则，无来源时是 Direct | 有（Medium exactly matches organic） | 补上 GA4 规则。`plausible_parity`：`organic medium without source` |
| D4 | `medium=ai-assistant` | → AI Assistant | 无 | 有 | 补上 GA4 规则。`plausible_parity`：`ai-assistant medium` |
| D5 | Email 的范围 | utm_source/medium **包含** `email\|e-mail\|e_mail\|e mail\|newsletter`；网页邮箱 referrer（Gmail、Outlook 等 6 个）→ Email | 同样是「包含」并带 `newsletter`；只有 Gmail 是邮件类别 | 整词匹配，不含 `newsletter`；邮箱 referrer 是 Referral | 取 Plausible 的宽松版：`utm_medium=email-campaign`、`utm_source=Newsletter-UK` 这类真实标签应算邮件。网页邮箱 referrer 补了 5 个。`ga4_rules::r15_*` |
| D6 | 只有 click id | 推断来源（Google/Bing/…）和 Paid Search/Paid Social | 来源为空 → Direct | 自动标记下 source=google, medium=cpc → Paid Search | 隐私设置抹掉 referrer 时，gclid 仍是可靠的付费证据。`plausible_parity`：`gclid only`、`msclkid only` |
| D7 | `(direct)` 标签 | `utm_source=(direct)` + `(none)`/`(not set)` → Direct | 来源非空 → Referral | R01 | 补上 GA4 的第一条规则（从 GA 导入或 GA 风格标签）。`plausible_parity`：`(direct)/(none) tagged` |
| D8 | 社交来源的覆盖面 | 只收常见站点：Matomo 91 个社交名 + 补充 57 条 | 全量使用 GA4 清单（592 条，含大量 2012 年前后的长尾站） | 清单 | 长尾站点（`43things`、`activeworlds`、`vampirefreaks`……）几乎没有流量，落到 Referral 并保留域名，信息不丢；Rivu 不跟随 Google 的清单更新（§2.2）。对照统计见 §7.3 |
| D9 | `Unassigned` | 没有这个取值：有来源 → Referral，没有来源 → Direct | 同 Rivu（没有这个取值） | 「Unassigned is the value Analytics uses when there are no other channel rules that match」 | 打了标签但 medium 不认识（`?ref=producthunt`、`utm_medium=foo`）的流量，Referral 比一个没有行动意义的 Unassigned 更有用。以后要加，是新增取值 + `RULES_VERSION` + 回填。`ga4_rules::r20_unrecognised_medium…` |
| D10 | AI Assistant 的位置 | 在 Organic Search **之前** | 在 Organic Search 之前 | 在 Organic Search 之后（因为它靠 medium） | Rivu 按来源类别判定 AI，`chatgpt.com` + `medium=organic` 应是 AI 而不是搜索。`ga4_rules::r13_boundary…` |
| D11 | TikTok 的类别 | 社交 | 社交（取自 CSV） | 说明页写在视频里，清单里是社交 | 规则引用清单（§1.3） |
| D12 | 付费来源别名 | `fb-ads`、`yt-ads`、`google_ads`、`adwords` 等 31 个别名本身是付费证据 | 有（`@paid_sources`，`ads`/`ad` 结尾的别名） | 无 | 常见的 utm_source 写法，行为同 Plausible；名单是 Rivu 自己写的，不是 Plausible 数据的拷贝 |

另有两处与 Plausible 不同、但不是渠道规则，记在这里：

- **路径条目不能用**：Matomo 数据里带路径的条目（`x.com/i/grok` 等 14 条）被跳过，因为 `SourceInput` 只有 host。Grok 靠 `grok.com` 识别；`x.com/i/grok` 发出的流量仍记为 `X (Twitter)`。Plausible 的 Snowplow 数据同样有路径条目，其 `RefInspector.parse(referrer)` 是整个 URL，能区分。若要支持，需要 referrer 的 path 进入 `SourceInput`（`Referrer.stored` 已有 `host + path`），列入未完成项。
- **内部引荐**：按站点白名单由调用方判定（§5.3，`system-design.md` §4.2 已定），不是本库的行为；Plausible 要求 host 全等。

## 7. 测试统计

运行：`cargo test -p rivu-source`（Windows，`cargo clippy -p rivu-source --all-targets -- -D warnings` 无警告）。

### 7.1 数量

| 文件 | 测试数 | 内容 |
|---|---|---|
| `tests/ga4_rules.rs` | 43 | GA4 规则逐条（R01–R21 各一正一反），渠道集合往返 |
| `tests/host_matching.rs` | 19（+1 个手动计时） | 后缀匹配、标签边界、`.{}` 边界、特异性、优先级、click id、空白值 |
| `tests/source_data.rs` | 7（+1 个需要 Google 清单的忽略测试） | 全量数据驱动、冲突、别名、补充表格式、版本与指纹钉 |
| `tests/plausible_parity.rs` | 6 | 93 行对照用例（79 行一致、14 行偏离且逐条标注编号），偏离必须出现在本文 |
| `tests/properties.rs` | 6 个性质，每个 5000 例 | 不 panic、确定、来源名无首尾空白、无来源信号则来源为空、大小写不敏感、空白不敏感 |

### 7.2 数据驱动

- Matomo 三份文件（名字, 域名）对 908 个，其中 14 个带路径被跳过，**894 个参与匹配（98.5%）**；合并后域名键 1013 个（精确 988 + `.{}` 通配 25）；
- 5165 个 host 样本，**0 个不一致**；
- 冲突 11 条（§4.4），都有取舍理由；
- 补充表别名/名字全部可解析；31 个付费别名全部给出对应的 Paid 渠道。

### 7.3 与 Google 官方清单对照（`ga4_list_agreement`，忽略测试，需本地清单）

对清单的 819 行，每行按 `utm_source=<source>` 和（带点号时）`referrer host=<source>` 分类，看是否落在清单类别对应的自然渠道。清单里每个站点常以「名字」和「域名」两行出现，所以分母约是站点数的 1.5–2 倍。

| 类别 | 一致 / 总数 | 说明 |
|---|---|---|
| SOURCE_CATEGORY_VIDEO | 44 / 46 | 不一致：`justin.tv`（已关闭）、`viadeo.journaldunet.com`（法国职场网站的子域，归类可疑） |
| SOURCE_CATEGORY_SHOPPING | 51 / 52 | 不一致：`s3.amazonaws.com`（把 AWS 存储域名当购物来源，Rivu 不跟） |
| SOURCE_CATEGORY_SEARCH | 113 / 129（87.6%） | 16 条不一致：归类可疑的 `cnn`、`google-play`、`rakuten.co.jp`、`globo`、`tut.by`；旧工具栏/地区门户 `alice`、`avg`、`conduit`、`incredimail`、`najdi`、`firmy.cz`、`search-results`、`ukr` 系列；`dogpile`（名字写法；域名 `dogpile.com` 已归 InfoSpace） |
| SOURCE_CATEGORY_SOCIAL | 215 / 592（36.3%） | 长尾，见 D8；`niconico` 名字被 Rivu 归视频（§1.3） |
| 合计 | 423 / 819（51.6%） | |

这个统计的用途是看漏了什么常见站点，不是追求 100%：补充表的 `GA4-LIST` 条目就是看这份差集挑出来的。

### 7.4 性能（粗略）

`cargo test -p rivu-source --release --test host_matching perf_smoke -- --ignored --nocapture`：6 种 referrer 轮换 + `medium=cpc`，200 万次 `classify` 平均约 **143 ns/次**（Windows，release），其中包含返回值 `String` 的一次分配。查表本身无分配（大小写折叠在比较时就地完成）；只有 referrer host 含大写字母且查不到时才多一次小写化分配。

## 8. 建议写进 `system-design.md` 的文字

**§4.2 第 8 步「来源」行的「数据源」列**：`来源库（Matomo CC0 + 自维护补充表）、渠道规则（GA4，`rivu-source`）`。

**§4.2 表后两段「渠道分类」「内部引荐的判定」之间新增**：

> - **来源与渠道的实现**在 `rivu-source`（`classify(&SourceInput) -> SourceInfo`），只有这一份。渠道取值是 GA4 默认渠道分组的 19 个渠道（不含 `Unassigned`），规则是一张有序表，先匹配先赢；英文标识（`Organic Search`、`Paid Social`、`AI Assistant`、`Cross-network`、`Mobile Push Notifications` 等）就是 ClickHouse `channel` 列的值，中文名在 UI 层按标识映射。规则、取值或来源数据任何一处变化，`RULES_VERSION` 加一，并用离线 mutation 回填历史数据。
> - **来源优先级**：`utm_source`（含 `source`、`ref` 回退）> referrer host > click id 推断。utm_source 先按别名表（不区分大小写）规范化，认不出的原样保留；referrer 用 host 后缀匹配（`www.google.co.jp` → `Google`），认不出的用 host 本身。内部引荐由接入层判定后传 `None`。
> - 详见 `docs/research/enrich-source.md`。

**§4.4 表「来源」行**改为：

| 来源 | Matomo `searchengine-and-social-list`（CC0-1.0，固定 commit，`crates/source/data/matomo/`）加自维护补充表（`supplement.yml`，每条写明依据） | Snowplow referer-parser 的 `referers.yml` 是 **GPL-3.0**（其 README 写明），**不用**；Google GA4 来源清单无许可声明，只作对照不入库 |

**§10 已决问题**可补一条：「渠道取值与中文名」：19 个，GA4 官方名称与官方简体中文页（见研究文档 §3.1）。

## 9. 未完成与后续

- **未接入管线**：`SourceInput.referrer_host` 由管线用站点 hostname 白名单判定内部引荐后传入。
- **路径条目**：`x.com/i/grok`、`google.com/products`（Google Shopping）等需要 referrer 的 path；`Referrer.stored` 已有 `host + path`，要支持就把 path 加入 `SourceInput`（版本升级）。
- **垃圾引荐列表**（`system-design.md` §4.4 另一行）不在本次范围；它是第 3 步的丢弃判断，不是来源分类。
- **Snowplow 增量**：取得许可或自行核实后，可补 email、paid 类别和更多域名。
- **付费证据的覆盖**：`twclid`、`li_fat_id` 因没有平台官方文档只推断来源；找到官方说明后可升级为付费证据（`RULES_VERSION` 加一）。
- **Google Ads/DV360/CM360/SA360 规则**不可实现（§3.4）。
- **非 ASCII 大小写**：规则关键字全是 ASCII，不受影响；但含非 ASCII 字符的来源名（`ABCsøk`）只做 ASCII 折叠，`ABCSØK` 不会命中，这是已知限制。
