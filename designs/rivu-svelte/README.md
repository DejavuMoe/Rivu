# Rivu 后台面板原型（Svelte 5）

已审批的可交互原型，是 `apps/dashboard` 的视觉与交互参照，不是生产代码；数据全部是固定种子的模拟数据。设计规范见 [`apps/dashboard/DESIGN.md`](../../apps/dashboard/DESIGN.md)，原型在整个流程里的位置见 [`../README.md`](../README.md)。

2026-11-19 确认、正在实现的部分：「账号 → 邮件周报」（`src/routes/account/WeeklyReportsCard.svelte`）和邮件里的确认链接页（`src/routes/auth/WeeklyConfirmPage.svelte`，路径 `#/weekly-report/confirm/t0k3n`）。

待确认的部分（等所有者确认后再进 `apps/dashboard`）：

- 修改登录邮箱（#37）：`src/routes/account/LoginEmail.svelte`（账号页个人资料的「邮箱」一行和对话框）与确认链接页 `src/routes/auth/EmailChangeConfirmPage.svelte`（`#/email-change/confirm/t0k3n`）。
- 月报（#38）：`src/routes/account/WeeklyReportsCard.svelte` 改成「邮件订阅」卡片，站点 × 周报 / 月报的开关表（文件名沿用）。
- 流量突增通知（#39）：站点设置的「通知」分区 `src/routes/settings/AlertsSection.svelte`（`#/s/1/settings/alerts`，所有者 / 管理员，且「邮件」为已配置），以及「邮件订阅」表里的「流量突增」一列。
- 分群（#40）：`src/routes/dashboard/SegmentMenu.svelte`，`#/s/1` 筛选栏的「分群」；评审面板「看板分群」切换有 / 无，「角色」切到查看者只剩套用。
- 图表批注（#41）：`src/routes/dashboard/NotesDialog.svelte` 与 `lib/charts/TrendChart.svelte` 的 `marks`；`#/s/1` 趋势图下方的标记、图表右上角「添加批注」；评审面板「图表批注」切换有 / 无 / 已满。

## 运行

依赖装在 WSL 构建镜像里（Windows 只放源码）：

```powershell
# 安装依赖并启动开发服务器（http://localhost:5173）
linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu\designs\rivu-svelte' -Command 'pnpm install && pnpm exec vite --host 0.0.0.0 --port 5173 --strictPort'

# 类型检查 + 构建单文件 HTML（dist/index.html，可离线打开）
linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu\designs\rivu-svelte' -Command 'pnpm run check && pnpm run build'
```

开发服务器跑在构建镜像里：在 Windows 改了源码后，再执行一次任意 build 模式命令（如 `-Command 'true'`）把改动同步过去，Vite 会热更新。

## 评审

右下角「评审」面板可以切换：角色（所有者 / 管理员 / 编辑者 / 查看者）、数据状态（正常、加载、无数据、出错、排队、超时）、应用加载（正常 / 失败，失败时整页只有「无法连接到服务器」）、安装检测结果（等待 / 已收到）、登录结果（含断网与失败）、验证码步骤、邮件与两步验证是否可用、初始化（有令牌 / 无令牌 / 已初始化）、邀请页状态与范围、已删除站点、语言与主题，并直接跳到各页面。URL 加 `?capture=1` 隐藏面板（截图用）。

共享链接的密码与初始化令牌输入 `rivu` 才算正确，验证码 `000000` 演示不正确；周报：评审面板的「周报邮箱」直接切换未确认、待确认、已确认，「周报确认邮件」演示确认邮件发送失败，「邮件」设为未配置时未确认的账号看不到周报卡片，确认链接的令牌为 `expired` 时演示链接失效；登录与改密的成败由「登录结果」开关决定。改登录邮箱：「改邮箱」直接切到待确认，「改邮箱链接」切换打开确认链接的浏览器是否已登录，组织成员的邮箱（如 `zhou.qiming@northwind.dev`）演示已被使用，开了两步验证时验证码 `000000` 演示不正确，令牌为 `expired` 时演示链接失效；「确认邮件」（原「周报确认邮件」）演示确认邮件发送失败。组织 `o3` 是只以站点访客身份可见的组织（没有成员与 API 密钥页）。

## 目录

```
src/styles/tokens.css     设计令牌（颜色、字号、间距、圆角、动效；浅色 + 深色）
src/lib/ui/               通用组件
src/lib/charts/           自绘 SVG 图表（趋势图、条形列表、迷你趋势线）
src/lib/app/              应用外壳（侧栏、页头、命令面板、窄屏导航、评审面板）
src/lib/data/fixtures.ts  模拟数据
src/routes/               各页面
```

改完在仓库根目录运行 `node tools/design-lint/check.mjs`：它检查界面规则，并要求 `src/styles/tokens.css`、`radix.css`、`src/lib/ui`、`src/lib/charts` 与 `apps/dashboard` 里的对应文件逐字节相同。

`apps/dashboard` 用的是同一套令牌与组件，`routes/*/query.ts` 这类模拟层在那里换成了真实的 `/api/internal/*` 调用。改界面时先改原型，审批后再同步到 `apps/dashboard`。
