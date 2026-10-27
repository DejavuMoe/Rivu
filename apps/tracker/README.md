# apps/tracker

浏览器采集脚本（TypeScript + esbuild，产出 `core.js` / `full.js`，`t.js` 是 `full.js` 的副本），零运行时依赖。

**状态：`core` 与 `full` 都已实现并通过三内核测试**。体积（gzip -9）：core 约 870 B（预算 1024 B），full 约 2620 B（预算 2816 B，2026-10-27 由 2048 B 上调，见规范附录第 1 条）。

线协议、字段、体积预算和行为规范见 [`docs/architecture/tracker-and-protocol.md`](../../docs/architecture/tracker-and-protocol.md)。

## 布局

| 路径 | 内容 |
|---|---|
| `src/index.ts` | 入口：幂等守卫、配置解析、启动、探测字段 |
| `src/config.ts` `guard.ts` `payload.ts` `send.ts` `pageview.ts` `router.ts` | 规范 §3.2 的模块；`config`/`guard.isLocalHost`/`payload` 是纯函数 |
| `src/full/` | full 独有功能：`config`（full 开关与纯函数）、`beacon`、`engagement`（状态机）、`api`（手动 API 与事件组装）、`capture`（外链、下载、表单、tagged）。core 构建里 `if (__FULL__)` 整体被删除，`pnpm size` 断言 core 不含这里的任何字节 |
| `scripts/build.mjs` `size.mjs` `lib.mjs` | 构建与产物检查（体积预算、回归、模块泄漏、禁用 API、es2018 语法、构建确定性） |
| `test/unit/` | 纯函数单元测试（`node:test`） |
| `test/fixture/server.ts` | 浏览器测试夹具：站点服务器 + 模拟 Rivu 的采集端（记录方法、Content-Type、Cookie 头、请求体） |
| `test/e2e/` | Playwright（chromium / firefox / webkit）行为测试：`core.spec.ts`、`full.spec.ts`（夹具按 `?v=full` 发 full） |
| `test/e2e-rivu/` | 真实浏览器 → 真实运行的 rivu → ClickHouse → `rivu query`：脚本从 rivu 的 `/js/<site_key>.js` 加载，断言系统里的数字与已知动作完全一致（「已知答案探针」雏形）。由 `scripts/e2e-rivu.sh` 编排，配置 `playwright.rivu.config.ts`，不在 `pnpm test` 里 |
| `test/wire.ts` | 线协议 `v=1` 请求体校验器，对照规范 §5.1 与 `crates/ingest/src/protocol.rs` |
| `probe/` | 已知答案探针：`probe.mjs` 定时用无头浏览器制造已知访问并经公开 API 核对数字，`summary.mjs` 判定「连续 N 天全部通过」，`run-local.sh` 本机演练；说明见 `docs/research/known-answer-probe.md` |
| `size-baseline.json` | 体积基线，`pnpm size` 与之对照 |

## 命令

本机的 Node/pnpm 在 Debian WSL 里，通过运行器执行（源码仍在 Windows 仓库里改）：

```powershell
& D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu\apps\tracker' -Command 'pnpm install --frozen-lockfile && pnpm run browsers && pnpm build && pnpm size && pnpm test'
```

| 脚本 | 作用 |
|---|---|
| `pnpm build` | 产出 `dist/core.js`、`dist/full.js`、`dist/t.js`（full 的副本）、`dist/meta.json`。构建号取环境变量 `RIVU_SV`（缺省 1） |
| `pnpm size` | 产物检查，任一项失败退出码非 0；`node scripts/size.mjs --update-baseline` 刷新基线 |
| `pnpm typecheck` | `tsc --noEmit` |
| `pnpm test:unit` | 单元测试 |
| `pnpm test:e2e` | 先构建，再跑 Playwright 三个内核 |
| `pnpm test` | 单元 + 浏览器 |
| `pnpm run browsers` | 安装 Playwright 浏览器（缺系统库时需 `--with-deps`，要 root） |

注意：运行器的 build 模式每次都会用 Windows 源码覆盖镜像并删除镜像里多出的文件（含 `dist/`），
所以 `build` 与依赖 `dist/` 的命令要放在同一次运行器调用里。

## 设计取舍

- **单元测试用 `node:test` 而不是 vitest**：被测的都是不依赖 DOM 的纯函数，Node 24 原生支持直接运行 `.ts`
  （类型擦除），不需要再引入一个测试框架及其依赖树；浏览器行为本来就由 Playwright 覆盖。
  因此源码要遵守「可擦除语法」（`erasableSyntaxOnly`：不用 enum、namespace、参数属性），导入带 `.ts` 扩展名。
- **core 不含手动 API**（规范 §2.5、§4.8）：加载前入队的命令不回放，`window.rivu` 被换成空函数，
  所以 `callback`、`init`、`set` 在 core 里都不生效；它们属于 full。
- **full 的忽略原因**：`guard.ts` 在 full 构建里返回原因字符串（`debug` 时输出），core 构建里返回 1（编译期折叠，不占体积）。
- **发送**：core 只用 `fetch keepalive`（`credentials: 'omit'`，体为字符串，故 `Content-Type: text/plain`，无预检）。
  full 的 engagement 与页面隐藏时的事件 `sendBeacon` 优先，失败回退 fetch。
- **构建号 `sv`**：CI 用 `github.run_number`（单调递增）。「不得小于上一发布」目前没有检查。

## 接入服务端（rivu 已实现）

- `GET /js/<site_key>.js`（collector / all 角色）：响应 = 配置前缀（规范 §2.3，唯一的映射函数是 `bin/rivu/src/script.rs` 的 `tracker_prefix`；`endpoint` 取 `RIVU_PUBLIC_BASE_URL`，不取请求的 `Host`）+ `core.js` 的字节；
  头与缓存按规范 §2.4（`ETag`、`public, max-age=300`、`nosniff`、CORP、CORS `*`，`If-None-Match` 命中 304）；
  未知、已删除、已停用（`collect_enabled = false`）的站点返回 404 + `no-store`。站点开了任一 full 专属能力（`site_settings.tracker` 的 `engagement`、`outbound`、`downloads`、`forms`、`tagged`；新站点默认开 engagement 与 tagged）就发 full，否则发 core。
- `core.js`、`full.js` 与构建号来自 `RIVU_TRACKER_DIR`（默认 `apps/tracker/dist`，相对进程工作目录）里的 `core.js`、`full.js` 和 `meta.json`：rivu 启动时读入并缓存，不在 cargo 构建里跑 pnpm，也不改写 `sv`。
  目录缺失时该路由对已知站点返回 503，`/readyz` 的 `checks.tracker` 报告原因，但不判就绪失败（采集不依赖脚本分发）。
  **生产镜像应把 CI 构建的 `dist/`（`pnpm build`，`RIVU_SV` = `github.run_number`）放进该目录**。
- `POST /api/event`：协议见规范 §5、§6。
- `GET /js/t.js`：`full.js` 的副本（无前缀，配置来自 `data-*`，缓存 1 天）；`GET /js/t.<sv>.js` 是当前构建号的不可变地址。

## 真实浏览器端到端

```powershell
& D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu' -Command 'set -a; . ./.env.example; set +a; bash apps/tracker/scripts/e2e-rivu.sh'
```

脚本依次：`pnpm build` → `cargo build -p rivu` → `rivu migrate` → 为 chromium / firefox / webkit 各 `rivu site add` → 后台启动 `rivu all`（端口 18080 / 19090）→ 等 `/readyz` → Playwright → 停 rivu（trap 保证清理）。
需要 dev 的 PostgreSQL / ClickHouse。页面主机名是 `127.0.0.1`（在站点白名单里）；tracker 默认排除 127.x，所以脚本标签带 `data-local="true"`。