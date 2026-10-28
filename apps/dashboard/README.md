# apps/dashboard

管理后台与统计看板：SvelteKit 3（adapter-static 单页应用）+ Svelte 5。视觉与交互规范见 [`DESIGN.md`](DESIGN.md)
（颜色只用 Radix Colors，四个字号，0.5px 线，图表自绘 SVG，不用 Tailwind 与图表库）。

数据全部来自 `/api/internal/*`，契约见 `bin/rivu/src/dash_api.rs` 开头的端点表与
[`docs/architecture/system-design.md`](../../docs/architecture/system-design.md) §6。

## 页面

| 路径 | 内容 |
|---|---|
| `/login`、`/setup`、`/invite/{token}` | 登录、首次初始化、接受邀请 |
| `/sites` | 当前组织的站点与 24 小时访客 |
| `/s/{id}` | 看板：指标、趋势、来源、页面、地区、设备；范围与筛选都在查询串里 |
| `/sites/new` | 添加站点（组织 owner / admin） |
| `/s/{id}/install` | 安装代码与安装验证 |
| `/s/{id}/diagnostics` | 接入诊断（接受与各丢弃原因的计数） |
| `/s/{id}/settings/{section}` | 站点设置：常规、主机名、数据采集、隐私、屏蔽规则、目标、共享、危险操作（按角色显示） |
| `/org/members`、`/org/api-keys` | 成员与邀请、API key |
| `/account` | 密码与登录会话 |
| `/share/{slug}`、`/public/{site_key}` | 共享链接（可带密码）与公开看板，只读 |
## 开发

Linux 工具链，经 WSL 运行器执行（见仓库的开发环境说明）：

```powershell
linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu\apps\dashboard' -Command 'pnpm install && pnpm test && pnpm run check && pnpm build'
```

本地后端：`scripts/dev-backend.sh` 构建并启动 `rivu serve`（127.0.0.1:8080），准备演示账号、两个站点、一个带密码的共享链接，
并发一批模拟访问；然后 `pnpm dev`（5174 端口，`/api` 代理到 8080 并改写 `Origin`，满足 CSRF 检查）。

注意：`svelte-kit sync`（`pnpm run check` 会执行）重写 `.svelte-kit/`，同一目录里正在运行的 dev server 需要重启；
运行器同步源码时会清掉 `.svelte-kit/`，所以 `build` 与 `preview` 要在同一条命令里执行。

## 约定

- `#lib/...` 是 package.json 的子路径导入，必须写扩展名（`#lib/i18n.ts`、`#lib/ui/Button.svelte`）。
- 文案在 `src/lib/i18n.ts`（通用）与各区域的 `strings.ts`（`extend` 注册），每条 `[中文, English]`。
- 纯逻辑放在不依赖 Svelte 的 `.ts` 里，用 `node --test` 测（`test/`）。
- CSP 由构建写进 `index.html` 的 `<meta>`（启动脚本按哈希放行，见 `vite.config.ts`），Caddy 不再给页面另加 CSP。
