# 原型

界面改动先在这里做出来、看过、确认，再动 `apps/dashboard`。这个目录里的东西都不是生产代码，数据全是编造的。

## 有哪些

| 目录 | 是什么 | 状态 |
|---|---|---|
| [`rivu-svelte/`](rivu-svelte/) | **主原型**。整个看板和管理后台，可以点、可以切换角色和各种状态。用的令牌和共享组件与生产逐字节相同 | 已确认。「账号 → 邮件周报」和确认链接页于 2026-11-19 确认，按它实现 |
| [`weekly-report/`](weekly-report/) | 周报的邮件（`email.html`）；另有一个旧流程的独立设置页（`index.html` + `verify.cjs`） | 邮件待确认；独立设置页已被主原型取代 |

### 周报为什么有两处

- **订阅入口看主原型**：`rivu-svelte` 的「账号 → 邮件周报」，以及邮件链接打开的确认页（`#/weekly-report/confirm/t0k3n`）。这是 2026-11-19 定下的做法：邮箱确认一次，之后每个站点一个开关。实现 `apps/dashboard` 时照它做。
- **邮件看 `weekly-report/email.html`**：邮件不能用应用的样式表，只能单独做。里面的周报邮件仍然有效；确认邮件还是旧的确认码版本，实现时要改成带确认链接的版本。
- **`weekly-report/index.html` 是旧流程**：它是早先单独做的站点设置页，要粘贴 43 个字符的确认码。它和 `verify.cjs` 里对应的验证如实反映了后台**现在**的接口，所以在后台改完之前留着做对照；后台改完后删除，并把界面契约指到主原型。

## 怎么看
## 怎么看

主原型构建出来是一个可以直接双击打开的 HTML 文件：

```powershell
linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu\designs\rivu-svelte' -Command 'pnpm install --frozen-lockfile && pnpm run check && pnpm run build'
```

产物在构建目录的 `dist/index.html`。开发服务器和评审面板的用法见 [`rivu-svelte/README.md`](rivu-svelte/README.md)。

`weekly-report/` 的两个 HTML 文件直接用浏览器打开。

## 流程

1. **改原型**。只动 `designs/` 和 `docs/ui/`。
2. **看**。浅色和深色、桌面和手机宽度、中文和英文、每种角色、加载 / 没数据 / 出错各看一遍。
3. **确认**。你明确说「按这一版实现」才算确认；「不错，但按钮小一点」是再改一轮。
4. **实现**。在 `apps/dashboard` 里实现，共享组件或令牌有改动时两边一起改。
5. **检查**。`node tools/design-lint/check.mjs`，再跑 `apps/dashboard` 的类型检查和测试。

规则与约束：

- 界面规则：[`apps/dashboard/DESIGN.md`](../apps/dashboard/DESIGN.md)。
- 产品约束（做什么不做什么、文案边界）：[`docs/product/constraints.md`](../docs/product/constraints.md)。
- 设计的现状和改进清单：[`docs/ui/`](../docs/ui/README.md)。

这套流程由仓库里的 `prototype-first-ui` 技能（`.agents/skills/prototype-first-ui`）支持。带记录的原型目录（目前是 `weekly-report/`）里，`_d_meta.json` 记录确认状态，`ui-contract.json` 记录原型的每个界面对应生产的哪个路由、接口和测试，`content-inventory.json` 记录界面上出现的每一条文字及其来源。

## 原型里不能有的

- 真实站点的统计、真实邮箱、密钥、确认码。
- 生产代码不能引用这个目录里的任何文件；原型里的模拟数据、评审面板也不能带进生产。
