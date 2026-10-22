# Matomo searchengine-and-social-list：来源与许可

本目录的四个文件是上游文件的**逐字节副本**（LF 换行），没有任何改动。Rivu 自己的修改全部放在
`../supplement.yml`（改名、合并、类别覆盖、补充条目），不改这里的文件。

| 项 | 值 |
|---|---|
| 上游仓库 | <https://github.com/matomo-org/searchengine-and-social-list> |
| 固定 commit | `4254b8d3182b23b4a4708902ff68631d8ec925ae`（2026-09-23，`add noai.duckduckgo.com as searchegine (#134)`） |
| 下载日期 | 2026-10-22 |
| 下载地址 | `https://raw.githubusercontent.com/matomo-org/searchengine-and-social-list/4254b8d3182b23b4a4708902ff68631d8ec925ae/<文件名>` |
| 许可 | **CC0 1.0 Universal**（仓库根 `LICENSE`、`package.json` 的 `"license": "CC0-1.0"`、GitHub 识别为 `cc0-1.0`）。数据文件本身就是该仓库的内容，不是代码，许可对数据文件直接适用 |

| 文件 | 字节 | SHA-256 |
|---|---:|---|
| `AIAssistants.yml` | 628 | `d4713bb4905e61eed35b89f29fe3808d0464cd269c48f9d37a1d3ee86a078ab5` |
| `Socials.yml` | 4754 | `2e9c40cac1e3a976cad8101e0892895a6b865618930018633c9f08a4b2f3cb51` |
| `SearchEngines.yml` | 45215 | `809c1733728e258cf8f8c63233132a0aae2fad14fd855ee2b190938f044ac422` |
| `LICENSE` | 6555 | `36ffd9dc085d529a7e60e1276d73ae5a030b020313e6c5408593a6ae2af39673` |

这些哈希同时写在 `../../build.rs`，构建时核对（CRLF 先归一成 LF）。

## 为什么不用 Snowplow referer-parser 的 `referers.yml`

`snowplow-referer-parser/referer-parser` 的 README「Copyright and license」一节写明：
`referers.yml` 基于 Matomo 的 `SearchEngines.php` 和 `Socials.php`，版权属于 Matthieu Aubry，
以 **GNU GPL v3** 提供；仓库根目录没有 LICENSE 文件。GPL-3.0 的数据文件不能随 MIT 项目分发，所以不选。
Matomo 自己现在把同一批数据以 CC0 发布，直接用它。详见 `docs/research/enrich-source.md` §4。

## 升级流程

1. 下载新 commit 的四个文件到临时目录，核对 LICENSE 仍是 CC0；
2. 覆盖本目录，更新上表的 commit、日期、哈希，以及 `build.rs` 的 `PINNED`；
3. `RULES_VERSION` 加一，更新 `tests/source_data.rs` 里钉住的 `(RULES_VERSION, DATA_FINGERPRINT)`；
4. 重新跑 `tests/source_data.rs`，把冲突和覆盖率的变化写进研究文档。
