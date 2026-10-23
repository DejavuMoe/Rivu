# Matomo referrer-spam-list：来源与许可

`spammers.txt` 是上游文件的**逐字节副本**（LF 换行），没有任何改动；`upstream-README.md`、
`upstream-composer.json` 是同一 commit 里声明许可的两个文件，同样逐字节保留。Rivu 自己的逻辑
（小写化、后缀匹配）都在 `../../src/spam.rs`，不改这里的文件。

| 项 | 值 |
|---|---|
| 上游仓库 | <https://github.com/matomo-org/referrer-spam-list> |
| 固定 commit | `eaaa5eaa6990be274fb560488a155459f735318d`（2026-09-22，`Merge pull request #1486 from ericguirbal/bestofthefirstcoast.com`） |
| 下载日期 | 2026-10-23 |
| 下载地址 | `https://raw.githubusercontent.com/matomo-org/referrer-spam-list/eaaa5eaa6990be274fb560488a155459f735318d/<文件名>`（与 `git cat-file blob` 取出的内容逐字节一致） |
| 许可 | **公有领域 / CC0 1.0**。两处声明：`README.md` 末尾「## License — Public Domain (no copyright).」（2015-04-20 的 `a8cd194` 加入）；`composer.json` 的 `"license": "CC0-1.0"`（2018-01-29 的 `b6188cc` 加入）。仓库根没有 `LICENSE` 文件，GitHub 的许可检测为空；两处声明一致，都是放弃版权，可随 MIT 项目分发，不需要署名 |
| 内容性质 | 每行一个主机名的事实性名单（域名不受版权保护），社区投稿，Matomo 维护；README 的 Disclaimer 声明「按现状提供，可能不完整，也可能含已过期条目」 |

| 文件 | 字节 | SHA-256 |
|---|---:|---|
| `spammers.txt` | 37727 | `640a63e8324b441c5f22240308e3925cea57464ffaafe6174cfd46c3925c6751` |
| `upstream-README.md` | 4291 | `f5c5a21412eb5fe3a09e25c1c36eb35badedb3832204d8cf87df90b5fd0f9c77` |
| `upstream-composer.json` | 254 | `736792998634f7f0d8b7983f97f5f04acd0500db77805db2b9144f4520bc1fd5` |

`spammers.txt` 的哈希同时写在 `../../build.rs`（`PINNED`），构建时核对（CRLF 先归一成 LF）。
共 2352 行，没有空行，没有重复；其中 1 条含大写（`QIWI.xyz`），加载时统一小写。

## 数据与 Rivu 用法的差异

- Matomo 对名单做**子串匹配**（README「Subdomains」一节）；Rivu 对 referrer 的 host 做**后缀匹配**：
  host 等于条目，或以 `.<条目>` 结尾才命中（`semalt.com` 命中 `semalt.com`、`a.semalt.com`，
  不命中 `notsemalt.com`，也不命中 URL 路径里恰好含该串的页面）。后缀匹配更严格，不会误伤。
- 名单里有「只列子域、不列根域」的条目（README 的 `referrerspammer.tumblr.com` 例子），后缀匹配保持这个语义。

## 与来源库的重叠（已复核）

`tests/spam_data.rs` 把 Matomo 搜索引擎 / 社交 / AI 助手表和补充表里展开出的 1000 多个 host 逐个过一遍名单，
只有两个命中：`search.fbdownloader.com`、`search.iminent.com`（浏览器劫持类「搜索引擎」，Matomo 的垃圾名单
列了它们的根域 `fbdownloader.com`、`iminent.com`）。管线里垃圾引荐判定先于来源分类，所以这两个来源的流量被丢弃。
新增重叠会让测试失败，必须人工复核。

## 不选的名单

- Plausible 的 `referrer-blocklist`：Plausible 的数据放在 AGPL-3.0 仓库里，不拷贝（与 `system-design.md`
  §10 Q20 对 Plausible 数据的处理一致）；它的许可本次没有逐项核对。
- 其它转载 Matomo 名单的仓库（nginx / Apache 配置生成器等）：数据同源，许可声明各异，直接用上游。

## 升级流程（对应 `platform-and-ops.md` §6 `sources_update`：垃圾名单是唯一允许在线更新的来源数据，尚未实现）

1. 下载新 commit 的三个文件到临时目录，核对 README 与 composer.json 仍声明公有领域 / CC0；
2. 覆盖本目录，更新上表的 commit、日期、哈希，以及 `build.rs` 的 `PINNED`；
3. 重新跑 `tests/spam_data.rs`（条目数、全量命中、合法来源不被误判）。
