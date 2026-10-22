# 测试数据来源

全部是外部数据源，用来证明 UA 解析与机器人识别的正确性。不得手改；升级时整文件替换并更新本表。

## uap-core 测试集（`uap-test_ua.yaml`、`uap-test_os.yaml`、`uap-test_device.yaml`）

| 项 | 值 |
|---|---|
| 上游 | https://github.com/ua-parser/uap-core，`tests/test_ua.yaml`、`tests/test_os.yaml`、`tests/test_device.yaml` |
| 固定 commit | `73e7340c3ed8055051607b296bf46ead7aa5f19e`，与 `data/uap-core/regexes.yaml` 同一 commit |
| 下载日期 | 2026-10-22 |
| 许可 | Apache-2.0（上游 `LICENSE`，副本在 `../../data/uap-core/LICENSE`） |

| 文件 | 字节 | SHA-256 |
|---|---|---|
| `uap-test_ua.yaml` | 286541 | `029128e0f2e505501cf303ce21464d974faafab6f6309434d28ea7e32be9e169` |
| `uap-test_os.yaml` | 92007 | `d3d00b2ed4e7240048fe574c4b530e0c41c15cd8ce852da8ab78c4ec76ba2e67` |
| `uap-test_device.yaml` | 3296916 | `6b58ec93361cb6f23660f01ffbde8064f3c402fa35e0276665251e228c90177f` |

## crawler-user-agents（`crawler-user-agents.json`）

| 项 | 值 |
|---|---|
| 上游 | https://github.com/monperrus/crawler-user-agents，`crawler-user-agents.json` |
| 固定 commit | `9345a7ad9c49cd0fbd880eb5a84ed1f358fc6a97`（2026-09-29） |
| 下载日期 | 2026-10-22 |
| 许可 | MIT，Copyright (c) 2017 Martin Monperrus（`crawler-user-agents.LICENSE`） |
| 内容 | 1501 条爬虫规则，共 2120 个 `instances` 样例 UA |

| 文件 | 字节 | SHA-256 |
|---|---|---|
| `crawler-user-agents.json` | 543489 | `4218df91a9c2b87f94f1e16bbc71da38487247e8cec4f1df28eebe19d394bff3` |
| `crawler-user-agents.LICENSE` | 1083 | `bd1d54278c29a12c1c3009259965a620c59363b8d7f5740d98d3fc38f319d7a5` |
