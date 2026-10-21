# examples/

本目录放分析 Plausible Analytics 时用到的参考材料。**两个子目录都不入库**（见根目录 `.gitignore`）：

| 目录 | 内容 | 为什么不入库 |
|---|---|---|
| `plausible-analytics/` | Plausible 源码快照（`analytics-master.zip` 及解压结果） | 体积大；服务端是 AGPL-3.0，不把别人的源码放进本仓库；`extra/`（EE）只用于记录行为 |
| `rea-evidence/` | REA 工具对 tracker 和 dashboard 的静态分析原始输出、运行时抓包 | 体积大，且可重新生成 |

设计文档引用的证据（`docs/research/plausible/` 里的 R00–R04 和 `docs/architecture/system-design.md`）已经带了 `路径:行号`，需要核对时再按下面的方法取回材料。

## 重新获取 Plausible 源码

固定的版本：

| 项 | 值 |
|---|---|
| 仓库 | https://github.com/plausible/analytics |
| commit | `b8cc135edb03450b2b57c92d375d3a5fedb0cb55`（master 分支） |
| 源码 zip | https://github.com/plausible/analytics/archive/b8cc135edb03450b2b57c92d375d3a5fedb0cb55.zip |
| 原始 zip 的 SHA-256 | `688FEE691AD77CCC8ADFC4B1DE11A754CAF51875C35AE13D872E5F4C7EBE33E5`（2116 个文件） |

PowerShell：

```powershell
New-Item -ItemType Directory -Force examples/plausible-analytics | Out-Null
Invoke-WebRequest "https://github.com/plausible/analytics/archive/b8cc135edb03450b2b57c92d375d3a5fedb0cb55.zip" -OutFile examples/plausible-analytics/analytics-master.zip
(Get-FileHash examples/plausible-analytics/analytics-master.zip -Algorithm SHA256).Hash   # 应为上表的 SHA-256
Expand-Archive examples/plausible-analytics/analytics-master.zip -DestinationPath examples/plausible-analytics
```

bash：

```bash
mkdir -p examples/plausible-analytics
curl -L -o examples/plausible-analytics/analytics-master.zip \
  https://github.com/plausible/analytics/archive/b8cc135edb03450b2b57c92d375d3a5fedb0cb55.zip
sha256sum examples/plausible-analytics/analytics-master.zip   # 应为上表的 SHA-256（大小写不敏感）
unzip examples/plausible-analytics/analytics-master.zip -d examples/plausible-analytics
```

注意：

- GitHub 不保证同一个 commit 每次生成的 zip 字节完全一致。哈希对不上时，不要直接判定内容不对，改为核对 zip 注释里的 commit id
  （`git archive` 生成的 zip 注释就是 commit hash；PowerShell 里可以用 `7z l -slt` 之类的工具查看，或解压后看目录内容是否与引用的 `路径:行号` 吻合）。
- 解压后的目录名与下载方式有关（`analytics-master/` 或 `analytics-<commit>/`）；文档里引用的路径以 `analytics-master/` 为根。

## 重新生成 REA 证据

`rea-evidence/` 由 REA（npm 包 `rea-agents`，版本 6.7.0，`npx -y rea-agents@latest`）生成：
对 tracker（58 个 JS 文件）和 dashboard（227 个 JS/TS 文件）做静态分析，再用 CDP 观测 plausible.io 的运行时请求。
具体命令、参数和每一步的结果记录在 [`docs/research/plausible/00-rea-analysis.md`](../docs/research/plausible/00-rea-analysis.md)，按那里的步骤重跑即可，
输出放到 `examples/rea-evidence/plausible/`。运行时抓包依赖在线站点，内容可能与当时不同。
