# 原型

界面改动先在这里做出来、看过、确认，再动 `apps/dashboard`。这个目录里的东西都不是生产代码，数据全是编造的。

## 有哪些

| 目录 | 是什么 | 状态 |
|---|---|---|
| [`rivu-svelte/`](rivu-svelte/) | **主原型**。整个看板和管理后台，可以点、可以切换角色和各种状态。用的令牌和共享组件与生产逐字节相同 | 已确认。「账号 → 邮件周报」和确认链接页于 2026-11-19 确认，已按它实现 |
| [`weekly-report/`](weekly-report/) | 周报相关的邮件（`email.html`）：周报邮件、确认邮件，以及现行的退订页 | 待确认 |

### 账号页与邮件：待确认的部分

- 修改登录邮箱（#37）：待确认。`rivu-svelte` 的「账号 → 个人资料 → 邮箱 → 修改」，以及发到新邮箱的确认链接页（`#/email-change/confirm/t0k3n`，失效：`#/email-change/confirm/expired`）。评审面板：「邮件」「两步验证」「改邮箱」「改邮箱链接」「确认邮件」「登录结果」。
- 月报（#38）：待确认。原来的「邮件周报」卡片改成「账号 → 邮件订阅」：一行一个站点，周报、月报各一列开关，共用同一个确认过的邮箱。评审面板：「周报邮箱」切到已确认，「邮件」设为未配置看暂停提示。
- 流量突增通知（#39）：待确认。站点设置新增「通知」分区（`#/s/1/settings/alerts`），只有所有者和管理员看得到，邮件未配置时不显示；账号页「邮件订阅」表多一列「流量突增」，站点没设提醒人数时显示「未设置」（能改设置的人看到「去设置」）。评审面板：「角色」「邮件」「周报邮箱」。

### 看板与站点列表：待确认的部分

- 分群（#40，把一组筛选条件存下来，一键套用）：待确认。`rivu-svelte` 看板（`#/s/1`）筛选栏的「分群」按钮，共享链接创建对话框里可选分群。评审面板：「看板分群」「角色」。
- 图表批注（#41，在趋势图的某一天记一句话）：待确认。`rivu-svelte` 看板趋势图下方的批注标记和图表右上角「添加批注」。评审面板：「图表批注」「角色」。
- 合并看板（#42，把组织里你能读的站点加在一起看）：待确认。`rivu-svelte` 的 `#/sites/combined`，站点列表页头的「合并看板」标签与命令面板。评审面板：「数据状态」。

### 周报为什么分两处

- **订阅界面在主原型里，已经实现**：`rivu-svelte` 的「账号 → 邮件周报」，以及邮件链接打开的确认页（`#/weekly-report/confirm/t0k3n`）。邮箱确认一次，之后每个站点一个开关。生产里对应 `apps/dashboard` 的账号页和 `(auth)/weekly-report/confirm/[token]` 页。
- **邮件单独放在 `weekly-report/email.html`**：邮件不能用应用的样式表，只能单独做。里面有周报邮件、确认邮件（邮件里是一个确认链接，点开后再点按钮才算确认）和现行退订页的样子。
- **早先单独做的站点设置页已经删除**：它要粘贴 43 个字符的确认码，那套做法已经不存在，现在 `weekly-report/` 只剩邮件。

## 怎么看

主原型构建出来是一个可以直接双击打开的 HTML 文件：

```powershell
linux-task.ps1 -Mode build -Project 'D:\Forgejo\Rivu\designs\rivu-svelte' -Command 'pnpm install --frozen-lockfile && pnpm run check && pnpm run build'
```

产物在构建目录的 `dist/index.html`。开发服务器和评审面板的用法见 [`rivu-svelte/README.md`](rivu-svelte/README.md)。

`weekly-report/email.html` 直接用浏览器打开；它的检查脚本是同目录的 `verify.cjs`。

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
