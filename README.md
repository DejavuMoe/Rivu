# Rivu

Rivu 是开源、自托管的网站分析系统（Plausible 的替代品）：一个 Rust 二进制，Lite 模式一个进程搭配 ClickHouse 和 PostgreSQL 就能跑，需要不丢数据和横向扩展时再切到基于 NATS JetStream 的 Reliable 模式。

技术栈：Rust（Axum + Tokio）、ClickHouse、PostgreSQL、可选 NATS JetStream。许可证：[MIT](LICENSE)。

## 当前阶段

**P0 基础**：工程骨架已就绪，业务功能还没有。

| 已有 | 还没有（后续阶段） |
|---|---|
| Cargo workspace 和单个二进制 `rivu`（`collector \| worker \| api \| migrate \| all`） | `POST /api/event` 的接入管线、会话分配、批量写入（P1） |
| 配置加载与集中校验（`RIVU_*`，支持 `*_FILE`） | 查询引擎和 `/api/*`（P1/P2） |
| `rivu migrate`：PG 与 ClickHouse 迁移按版本号交织执行 | tracker 脚本（P1）、管理后台（等待 `designs/` 原型审批） |
| 内部端口 `/healthz` `/readyz` `/metrics` | JetStream、worker、日汇总层（P2/P3） |
| 初始 schema：PG 业务表，ClickHouse `events` / `sessions` / `mv_sessions` / `ingest_counters` | |
| `deploy/compose.dev.yml`、CI 工作流 | |

公共端口上的 `POST /api/event` 和 `/api/*` 现在都返回 `501`（标注 P1 实现）。路线图见 [`docs/architecture/system-design.md` §9](docs/architecture/system-design.md)。

## 目录结构

```
rivu/
├── apps/
│   ├── tracker/        # 浏览器采集脚本（P1，目前只有 README）
│   └── dashboard/      # 管理后台（等待原型审批，目前只有 README）
├── bin/rivu/           # 单个二进制：子命令、角色启动、内部/公共端口、迁移执行器
├── crates/
│   ├── common/         # 配置、遥测、构建信息、迁移文件模型（有代码）
│   ├── storage-pg/     # sqlx：连接池、迁移（有代码）
│   ├── storage-ch/     # clickhouse crate 封装、迁移、语句拆分（有代码）
│   ├── domain/ ingest/ session/ writer/ bus/ query/ jobs/   # 占位，P1 起实现
├── migrations/
│   ├── postgres/       # <14 位时间戳>_<name>.sql
│   └── clickhouse/     # 同上；按行尾 `;` 拆成多条语句执行
├── deploy/
│   ├── compose.dev.yml           # 开发依赖：ClickHouse、PostgreSQL、NATS（profile reliable）
│   ├── .env.dev.example          # 端口覆盖示例
│   ├── clickhouse/users.d/       # 开发用的四个 ClickHouse 账号
│   └── postgres/init/            # 开发用的 rivu_app 角色
├── docs/{architecture,research}/ # 设计与调研
├── examples/           # 参考材料（Plausible 源码、REA 证据）：不入库，见 examples/README.md
├── .forgejo/workflows/ci.yml
└── .env.example        # 开发环境变量默认值
```

## 开发快速开始

前置：Rust（版本由 `rust-toolchain.toml` 决定）、Docker。下面的命令都在仓库根目录执行。

### 1. 启动依赖

PowerShell 和 bash 相同：

```
docker compose -f deploy/compose.dev.yml up -d
# 需要 NATS（Reliable 联调）时：
docker compose -f deploy/compose.dev.yml --profile reliable up -d
```

等 `docker compose -f deploy/compose.dev.yml ps` 里所有服务都是 `healthy`。端口都只绑 `127.0.0.1`；要改端口，复制 `deploy/.env.dev.example` 为 `deploy/.env` 修改，并同步 `.env.example` 里的连接串。

### 2. 加载环境变量

`rivu` 本身不读 `.env` 文件，要先把 `.env.example` 加载进当前 shell。

PowerShell：

```powershell
Get-Content .env.example | Where-Object { $_ -match '^\s*[^#\s][^=]*=' } |
  ForEach-Object { $k, $v = $_ -split '=', 2; Set-Item "env:$($k.Trim())" $v.Trim() }
```

bash / zsh：

```bash
set -a; source .env.example; set +a
```

要用自己的值，复制成 `.env`（已被 `.gitignore` 忽略）再加载那个文件。每个变量都支持 `<NAME>_FILE` 形式（从文件读取，去掉首尾空白），同时设置两者会报错；密码类变量在生产里应该走这种形式（Docker secrets）。

### 3. 迁移

```
cargo run -p rivu -- migrate          # 第一次：全部应用
cargo run -p rivu -- migrate          # 第二次：输出「没有待执行的迁移」
cargo run -p rivu -- migrate status   # 列出每个版本的状态
```

### 4. 运行

```
cargo run -p rivu -- all
```

另开一个终端检查（只用 curl，不需要加载环境变量；停止服务用 Ctrl-C）：

PowerShell（`curl` 是 `Invoke-WebRequest` 的别名，用 `curl.exe` 调真正的 curl）：

```powershell
curl.exe -i http://127.0.0.1:9090/healthz
curl.exe -i http://127.0.0.1:9090/readyz
curl.exe -i http://127.0.0.1:9090/metrics
curl.exe -i -X POST http://127.0.0.1:8080/api/event -d '{}'    # 501
```

bash：

```bash
curl -i http://127.0.0.1:9090/healthz
curl -i http://127.0.0.1:9090/readyz
curl -i http://127.0.0.1:9090/metrics
curl -i -X POST http://127.0.0.1:8080/api/event -d '{}'        # 501
```

`/readyz` 返回逐项 JSON：PostgreSQL `SELECT 1`、ClickHouse `SELECT 1`、两个库的 schema 版本是否达到编译期最高版本；全部通过 200，否则 503。

### 5. 测试

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test                      # 集成测试在缺少 RIVU_TEST_* 变量时跳过并打印原因
```

加载 `.env.example` 后（里面带了 `RIVU_TEST_*`），`cargo test` 会连真实的 PostgreSQL 和 ClickHouse 跑 `bin/rivu/tests/clickhouse_sessions.rs`：
迁移后写入一个会话的 3 个事件，校验 `mv_sessions` 的聚合，并用同一个 `insert_deduplication_token` 重放，确认 `events` 和 `sessions` 都不重复计数。

### 6. 清理

```
docker compose -f deploy/compose.dev.yml down       # 停止容器，保留数据卷
docker compose -f deploy/compose.dev.yml down -v    # 同时删除 rivu-pg-data / rivu-ch-data / rivu-nats-data
```

### 构建信息

`rivu --version` 输出版本和 git commit。commit 由构建环境通过环境变量 `RIVU_GIT_COMMIT` 注入（CI 里已设置）；没有注入时显示 `unknown`。构建脚本不会去调用 git。

## 文档索引

架构（`docs/architecture/`）：

- [`system-design.md`](docs/architecture/system-design.md)：系统设计 v2（接入、会话、ClickHouse 数据模型、查询引擎、路线图）。与其他文档冲突时以它为准。
- [`platform-and-ops.md`](docs/architecture/platform-and-ops.md)：进程与部署、PostgreSQL 模型、权限、后台任务、配置、数据库账号、迁移、可观测性、备份。
- [`tracker-and-protocol.md`](docs/architecture/tracker-and-protocol.md)：采集脚本与线协议。
- [`archive/`](docs/architecture/archive/)：v1 架构设计存档。

调研（`docs/research/`）：

- [`clickhouse-verification.md`](docs/research/clickhouse-verification.md)：ClickHouse 语义核实与版本陷阱（`async_insert`、MV 去重等）。
- [`plausible/`](docs/research/plausible/)：对 Plausible 的 REA 分析与源码通读，编号 00–04（REA、接入、会话与存储、统计查询、平台与运维）。

参考材料的获取方法见 [`examples/README.md`](examples/README.md)。
