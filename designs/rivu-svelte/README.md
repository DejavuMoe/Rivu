# Rivu 后台面板原型（Svelte 5）

已审批的可交互原型，是 `apps/dashboard` 的视觉与交互参照，不是生产代码；数据全部是固定种子的模拟数据。设计规范见 [DESIGN.md](DESIGN.md)。

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

右下角「评审」面板可以切换：角色（所有者 / 管理员 / 编辑者 / 查看者）、数据状态（正常、加载、无数据、出错、排队、超时）、安装检测结果、登录结果、是否需要初始化令牌、语言与主题，并直接跳到各页面。URL 加 `?capture=1` 隐藏面板（截图用）。

## 目录

```
src/styles/tokens.css     设计令牌（颜色、字号、间距、圆角、动效；浅色 + 深色）
src/lib/ui/               通用组件
src/lib/charts/           自绘 SVG 图表（趋势图、条形列表、迷你趋势线）
src/lib/app/              应用外壳（侧栏、页头、命令面板、窄屏导航、评审面板）
src/lib/data/fixtures.ts  模拟数据
src/routes/               各页面
```

`apps/dashboard` 用的是同一套令牌与组件，`routes/*/query.ts` 这类模拟层在那里换成了真实的 `/api/internal/*` 调用。改界面时先改原型，审批后再同步到 `apps/dashboard`。
