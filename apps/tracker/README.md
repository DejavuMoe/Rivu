# apps/tracker

浏览器采集脚本（TypeScript + esbuild，产出 `core.js` / `full.js`），零运行时依赖。

**状态：`core` 已实现并通过三内核测试；`full` 尚未实现**（`src/full/index.ts` 只是挂载点）。

线协议、字段、体积预算和行为规范见 [`docs/architecture/tracker-and-protocol.md`](../../docs/architecture/tracker-and-protocol.md)。

## 布局

| 路径 | 内容 |
|---|---|
| `src/index.ts` | 入口：幂等守卫、配置解析、启动、探测字段 |
| `src/config.ts` `guard.ts` `payload.ts` `send.ts` `pageview.ts` `router.ts` | 规范 §3.2 的模块；`config`/`guard.isLocalHost`/`payload` 是纯函数 |
| `src/full/` | full 独有功能的挂载点。core 构建里 `if (__FULL__)` 整体被删除，`pnpm size` 断言 core 不含这里的任何字节 |
| `scripts/build.mjs` `size.mjs` `lib.mjs` | 构建与产物检查（体积预算、回归、模块泄漏、禁用 API、es2018 语法、构建确定性） |
| `test/unit/` | 纯函数单元测试（`node:test`） |
| `test/fixture/server.ts` | 浏览器测试夹具：站点服务器 + 模拟 Rivu 的采集端（记录方法、Content-Type、Cookie 头、请求体） |
| `test/e2e/` | Playwright（chromium / firefox / webkit）行为测试 |
| `test/wire.ts` | 线协议 `v=1` 请求体校验器，对照规范 §5.1 与 `crates/ingest/src/protocol.rs` |
| `size-baseline.json` | 体积基线，`pnpm size` 与之对照 |

## 命令

本机的 Node/pnpm 在 Debian WSL 里，通过运行器执行（源码仍在 Windows 仓库里改）：

```powershell
& D:\WSL\Scripts\linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu\apps\tracker' -Command 'pnpm install --frozen-lockfile && pnpm run browsers && pnpm build && pnpm size && pnpm test'
```

| 脚本 | 作用 |
|---|---|
| `pnpm build` | 产出 `dist/core.js`、`dist/meta.json`。构建号取环境变量 `RIVU_SV`（缺省 1） |
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
- **发送**：core 只用 `fetch keepalive`（`credentials: 'omit'`，体为字符串，故 `Content-Type: text/plain`，无预检）。
  `sendBeacon` 优先属于 full 的 engagement。
- **构建号 `sv`**：CI 用 `github.run_number`（单调递增）。「不得小于上一发布」目前没有检查。

## 接入服务端（本任务范围之外）

服务端需要提供：

- `GET /js/<site_key>.js`：响应 = 配置前缀（规范 §2.3，由服务端的唯一映射函数生成）+ `dist/core.js` 的字节；
  `test/fixture/server.ts` 的 `buildPrefix` 是同形态的参考。站点启用了 full 专属能力时改发 full。
- `GET /t.js`：`full.js` 的副本（full 实现之后）。
- `POST /api/event`：协议见规范 §5、§6。
- 构建产物 `dist/core.js` 由 CI 构建并作为服务端的嵌入资源（`include_bytes!` 或随镜像分发），构建号 `sv` 需与产物一致。
