# 测试数据来源

这些文件来自 MaxMind 官方的 mmdb 格式测试仓库，**原样复制，字节与上游一致**（逐个核对过 git blob 哈希）。

- 仓库：<https://github.com/maxmind/MaxMind-DB>
- 固定提交：`f4847f28e5d12cd495ad5c1ea297cef9cc45bf4f`（2026-10-19，`Merge pull request #301`）
- 许可：Apache License 2.0 **或** MIT License，任选其一（仓库 README「Copyright and License」：
  Copyright (c) 2013 - 2026 by MaxMind, Inc.）。两份许可证文本在本目录的 `LICENSE-APACHE`、`LICENSE-MIT`。

| 本目录路径 | 上游路径 | git blob |
|---|---|---|
| `test-data/GeoIP2-City-Test.mmdb` | `test-data/GeoIP2-City-Test.mmdb` | `d8cbc5dd7ac71fc08c9eee83ec42ef32a691d79c` |
| `test-data/GeoLite2-City-Test.mmdb` | 同 | `03263a4d1e753d19bea0c739db2f24942d1f2d35` |
| `test-data/GeoIP2-Country-Test.mmdb` | 同 | `50aa9a62da6cc65c2e4f9d80a9bd4ce192e2fdaf` |
| `test-data/GeoLite2-Country-Test.mmdb` | 同 | `04604ef2aa0ccd549641636c730fd184342d5752` |
| `test-data/GeoLite2-ASN-Test.mmdb` | 同（`database_type` 不是 Country/City，用来测校验失败） | `42a3e877a2e343a65267a57aa5849f687effaf0a` |
| `test-data/GeoIP2-City-Test-Broken-Double-Format.mmdb` | 同（格式损坏） | `271043724b340f98fbb59498410d5986a8d2f733` |
| `test-data/GeoIP2-City-Test-Invalid-Node-Count.mmdb` | 同（node_count 无效） | `59777eae1ee397e2bc57a6394745ccc7af73597b` |
| `source-data/GeoIP2-City-Test.json` | 同 | `a9e07ca3a052051be7dfcf42c2e6f40caba26232` |
| `source-data/GeoLite2-City-Test.json` | 同 | `5ff620eb0d97d97657b5b98b3d14c2abb2eb8416` |
| `source-data/GeoIP2-Country-Test.json` | 同 | `916d3cf24b01384c1e28ac2780f2671a08c4a977` |
| `source-data/GeoLite2-Country-Test.json` | 同 | `3625fdd9c0912f53605a3130248ea013b5082979` |

`source-data/*.json` 是生成对应 mmdb 的输入：每个元素是 `{"<网段>": {记录}}`。
`tests/mmdb_reference.rs` 用它们做期望值：对每个网段取首、中、尾三个地址，逐条断言
`GeoDb::lookup` 的 country / region / city 与源数据一致。

这些是**测试数据**，不是 GeoIP 产品数据；不随 Rivu 的发布物分发。
