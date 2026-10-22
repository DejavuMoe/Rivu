# uap-core 数据来源

| 项 | 值 |
|---|---|
| 上游 | https://github.com/ua-parser/uap-core |
| 固定 commit | `73e7340c3ed8055051607b296bf46ead7aa5f19e`（2026-08-24，"Clean up errant whitespace in regexes.yaml"） |
| 下载日期 | 2026-10-22 |
| 下载方式 | `https://raw.githubusercontent.com/ua-parser/uap-core/<commit>/regexes.yaml`（`LICENSE` 同） |
| 许可 | Apache-2.0（`LICENSE`）；上游在该 commit 没有 `NOTICE` 文件，本目录的 `NOTICE` 只记录署名 |
| 修改 | 无，逐字节原样 |

| 文件 | 字节 | SHA-256 |
|---|---|---|
| `regexes.yaml` | 222536 | `acf2c9af91158a00c444c3b795e7769f8af3d894106327a299c70bd64a81c8e6` |
| `LICENSE` | 608 | `0c6f6cd66573dd12f68c71bffca774e58087c3c9168aed0eabf0a5ae10d3800f` |

升级流程：换 commit 号，重新下载，更新上表，同时更新 `tests/data/` 里同一 commit 的三个测试文件，
跑 `cargo test -p rivu-ua`，一致性测试必须仍为 100%（或更新允许失败清单并写明原因）。
