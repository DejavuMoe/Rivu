# Rivu

Rivu 是开源、自托管的网站分析系统（Plausible 的替代品）：一个 Rust 二进制搭配 PostgreSQL 与 ClickHouse，单实例部署就是全功能版本。

**部署**：见 [`docs/install.md`](docs/install.md)（Docker 镜像 + `deploy/compose.yml`，前面放 nginx 或 Caddy）。**进度与计划**：见 [`docs/roadmap.md`](docs/roadmap.md)。**全部文档**：见 [`docs/README.md`](docs/README.md)。

技术栈：Rust（Axum + Tokio）、ClickHouse、PostgreSQL；看板 SvelteKit + Svelte 5；采集脚本 TypeScript + esbuild。许可证：[MIT](LICENSE)。

## 功能

| 部分 | 内容 |
|---|---|
| 采集脚本（`apps/tracker`） | `core`（pageview、SPA、hash 路由、bfcache、预渲染）与 `full`（另有 engagement、手动 API、外链 / 下载 / 表单 / 标记元素的自动捕获）。不读写 Cookie，不写浏览器存储。唯一读取的键是用于排除自身访问的 `localStorage.rivu_ignore` |
| 接入（`POST /api/event`） | 协议解析、站点快照（增量刷新 + `LISTEN/NOTIFY`）、hostname 白名单、按站点限流、垃圾引荐、屏蔽规则（IP / CIDR / 国家 / 页面 / hostname）、机器人、GeoIP、UA、来源与渠道、每日盐 + 访客密钥的访客 ID、会话分配、批量写入（带去重 token 的重试）、`ingest_counters` |
| 会话 | 进程内分片的 SessionAssigner；关停时写快照，启动时从快照或 ClickHouse 预热 |
| 查询 | 统一查询内核 `rivu-query`（`QuerySpec`：指标、维度、过滤树、日期范围、时间序列、对比期、目标）；公开 API `POST /api/v1/query`（API key、按组织限流）与 `GET /api/v1/query/schema.json`；命令行 `rivu query` |
| 看板（`apps/dashboard`） | 登录与首次初始化、站点列表、统计看板、安装与安装验证、站点设置（主机名、采集、脚本功能、隐私、屏蔽规则、目标、共享、危险操作）、接入诊断、成员与邀请、API key、账号与登录会话、共享链接与公开看板 |
| 数据迁移与导出 | 从 Plausible 导入历史汇总数据、将统计视图导出为 CSV ZIP，支持看板和 CLI |
| 邮件 | 配置 SMTP 后可发成员邀请、密码重置和站点周报。周报目前只能通过登录后的订阅接口开通，看板里的入口（账号页）还在原型阶段，见 [`docs/roadmap.md`](docs/roadmap.md) |
| 后台任务 | 盐轮换、GeoIP 刷新、站点与日期范围的统计数据删除 |
| 运维 | 单个二进制：`rivu serve` 运行实例，`rivu migrate` 执行迁移；PG 与 ClickHouse 迁移共用版本序列，带冻结校验与 schema 快照；内部端口 `/healthz` `/readyz` `/metrics` |

开发进度见 [`docs/roadmap.md`](docs/roadmap.md)。

## 目录结构

```
rivu/
├── apps/
│   ├── tracker/        # 采集脚本（core / full）、Playwright 测试、已知答案探针
│   └── dashboard/      # 看板（SvelteKit 单页应用）
├── bin/rivu/           # 单个二进制：角色启动、公共 / 内部端口、管理命令、迁移执行器
├── crates/
│   ├── domain/ ingest/ session/ writer/      # 领域类型、接入管线、会话分配、批量写入
│   ├── ua/ geo/ source/                      # UA 与机器人、GeoIP、来源与渠道
│   ├── query/ reference/                     # 查询内核、独立参考实现（差分测试用）
│   ├── auth/ ratelimit/                      # 鉴权、限流
│   ├── storage-pg/ storage-ch/ jobs/ common/ # 两个库的访问、后台任务、配置与遥测
├── tools/              # loadgen、reconcile、reident、replay（压测与验收工具）、design-lint（界面规则检查）
├── migrations/         # postgres/ 与 clickhouse/ 的初始迁移、checksums.lock
├── schema/             # 迁移后的 schema 快照（测试比对）
├── deploy/             # 生产 compose.yml 与 .env.example、nginx / Caddy 示例、开发用 compose.dev.yml
├── designs/            # 界面原型（先改原型，确认后再改 apps/dashboard）
├── docs/               # 文档地图见 docs/README.md：进度、产品约束、系统设计、界面设计、调研与验收报告
├── fuzz/               # cargo-fuzz 目标（不在工作区内）
└── .env.example        # 开发环境变量
```

## 开发快速开始

需要 Rust（版本由 `rust-toolchain.toml` 决定）、Docker 和 Docker Compose v2、Node.js 24+、pnpm 12.5.1。下面的命令都在仓库根目录执行。

### 1. 启动依赖

```
docker compose -f deploy/compose.dev.yml up -d
```

等 `docker compose -f deploy/compose.dev.yml ps` 里的服务都是 `healthy`。端口只绑 `127.0.0.1`；要改端口，复制 `deploy/.env.dev.example` 为 `deploy/.env` 修改，并同步 `.env.example` 里的连接串。

### 2. 加载环境变量

`rivu` 不读 `.env` 文件，要先把 `.env.example` 加载进当前 shell。

PowerShell：

```powershell
Get-Content .env.example | Where-Object { $_ -match '^\s*[^#\s][^=]*=' } |
  ForEach-Object { $k, $v = $_ -split '=', 2; Set-Item "env:$($k.Trim())" $v.Trim() }
```

bash / zsh：

```bash
set -a; source .env.example; set +a
```

二进制按 `<NAME>` 或 `<NAME>_FILE` 读取配置，文件内容会去掉首尾空白。两者同时给出非空值会报错。生产 Compose 的配置方式见[安装说明](docs/install.md)。

### 3. 构建看板与采集脚本

```
pnpm --dir apps/tracker install --frozen-lockfile
pnpm --dir apps/tracker build
pnpm --dir apps/dashboard install --frozen-lockfile
pnpm --dir apps/dashboard build
```

从仓库根目录运行 `rivu serve` 时，会读取 `apps/tracker/dist` 与 `apps/dashboard/build`。

### 4. 迁移与运行

```
cargo run -p rivu -- migrate          # 应用全部迁移；再执行一次输出「没有待执行的迁移」
cargo run -p rivu -- migrate status   # 列出每个版本的状态
```

用 CLI 创建首个用户与组织。将至少 12 个字符的密码写入仅自己可读的文件，用 `--password-file` 读取。

```
cargo run -p rivu -- setup --email you@example.com --org-name Acme --password-file pw.txt
```

在 bash 或 zsh 中也可以重定向标准输入，让 CLI 读取第一行。

```bash
cargo run -p rivu -- setup --email you@example.com --org-name Acme --password-stdin < pw.txt
```

初始化后启动服务，打开 `http://127.0.0.1:8080`。

```
cargo run -p rivu -- serve
```

看板的开发方式见 [`apps/dashboard/README.md`](apps/dashboard/README.md)。

另开一个终端发一个 pageview 并查询（先 `cargo run -p rivu -- site add --name demo --hostname example.test`，记下打印的 `site_key`）：

```bash
curl -i -X POST http://127.0.0.1:8080/api/event -H "Content-Type: text/plain" \
  -A "Mozilla/5.0 (X11; Linux x86_64; rv:126.0) Gecko/20100101 Firefox/126.0" \
  -d '{"v":1,"s":"<site_key>","n":"pageview","u":"https://example.test/"}'      # 202
# 写缓冲每秒 flush 一次；没有 User-Agent（空 UA）和机器人 UA 的事件会被丢弃
echo '{"site":"<site_key>","metrics":["visitors","pageviews"],"dimensions":["event:page"],"date_range":"day"}' \
  | cargo run -q -p rivu -- query
```

`rivu query` 与公开 API 执行同一个 QuerySpec、输出同样的 QueryResult。公开 API 需要 API key（`rivu apikey create --org <slug> --user <email> --name demo`，明文只显示一次）：

```bash
curl -i -X POST http://127.0.0.1:8080/api/v1/query -H "Authorization: Bearer <key>" --data-binary @q.json
curl -i http://127.0.0.1:8080/api/v1/query/schema.json     # 无需 key
```

错误体都是 `{"error":"<代码>","message":"..."}`；400 另有 `errors:[{"path":"filters[0][2]","message":"..."}]`；站点不存在与无权访问都是同一个 404。

内部端口 `127.0.0.1:9090`：`/healthz`、`/metrics`，以及逐项 JSON 的 `/readyz`（PG 与 ClickHouse 可连、schema 版本达到编译期最高版本，以及站点快照与盐已加载）。

### 5. 测试

```
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test        # 需要数据库的集成测试在缺少 RIVU_TEST_* 变量时跳过并打印原因
```

加载 `.env.example`（带 `RIVU_TEST_*`）后，`cargo test` 连真实的 PostgreSQL 和 ClickHouse 跑 `bin/rivu/tests/` 的集成测试，其中：

- `e2e_lite.rs`：事件经管线、会话分配、批量写入进入 ClickHouse（覆盖夏令时切换日与跨午夜会话），各日期范围的指标与维度分组和 `rivu-reference` 的真值逐项比较，并对账 `ingest_counters`、验证去重重放。
- `query_differential.rs`：随机生成数百个 `QuerySpec`（`RIVU_DIFF_SEED`、`RIVU_DIFF_CASES`），逐个与参考实现比较，失败时打印最小化后的请求。
- `public_api.rs`、`internal_api.rs`：公开 API 与看板端点的端到端测试（真实端口、鉴权、限流、CSRF、共享链接）。
- `schema_snapshot.rs`：迁移后的 schema 与 `schema/` 快照一致（`RIVU_SCHEMA_UPDATE=1` 重新生成）。

SQL 与 JSON Schema 的 insta 快照在 `crates/query/src/snapshots/`（`INSTA_UPDATE=always cargo test -p rivu-query` 更新）。

采集脚本的单元测试、Playwright（chromium / firefox / webkit）与体积预算见 [`apps/tracker/README.md`](apps/tracker/README.md)。`apps/tracker/scripts/e2e-rivu.sh` 用真实浏览器对真实运行的 `rivu serve` 做一组已知动作，再用 `rivu query` 断言数字完全一致。

### 6. 清理

```
docker compose -f deploy/compose.dev.yml down       # 停止容器，保留数据卷
docker compose -f deploy/compose.dev.yml down -v    # 同时删除数据卷
```

`rivu --version` 输出版本和 git commit；commit 由构建环境通过 `RIVU_GIT_COMMIT` 注入，没有注入时显示 `unknown`。

## 文档索引

按问题找文档，从 [`docs/README.md`](docs/README.md) 开始。常用的几份：

- [`docs/roadmap.md`](docs/roadmap.md)：进度，离发布还差什么。
- [`docs/product/constraints.md`](docs/product/constraints.md)：做什么不做什么。
- [`apps/dashboard/DESIGN.md`](apps/dashboard/DESIGN.md) 与 [`designs/README.md`](designs/README.md)：界面规则与原型。

架构（`docs/architecture/`）：

- [`system-design.md`](docs/architecture/system-design.md)：系统设计（接入、会话、ClickHouse 数据模型、查询引擎、容量、路线图、刻意偏离清单）。与其他文档冲突时以它为准。
- [`platform-and-ops.md`](docs/architecture/platform-and-ops.md)：进程与部署、PostgreSQL 模型、权限、后台任务、配置、数据库账号、迁移、可观测性、备份。
- [`tracker-and-protocol.md`](docs/architecture/tracker-and-protocol.md)：采集脚本与线协议。

其他：[`docs/privacy.md`](docs/privacy.md)（隐私说明）、[`docs/brief-p1.md`](docs/brief-p1.md)（P1 任务与验收）、[`docs/research/`](docs/research/)（Plausible 源码分析、富化研究、ClickHouse 核实，以及对账、回放、压测、重识别等验收报告）。参考材料的获取方法见 [`examples/README.md`](examples/README.md)。
