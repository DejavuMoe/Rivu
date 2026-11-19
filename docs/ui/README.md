# 界面设计

Rivu 的界面按分层的设计系统来组织。这一页说明每一层在仓库的哪里、谁说了算、靠什么保证不走样。

## 五层各在哪

| 层 | 管什么 | 唯一出处 | 靠什么保证 |
|---|---|---|---|
| 1 令牌 | 颜色、字号、字重、间距、圆角、动效时长。颜色分两级：Radix 色阶（原始值）和语义令牌（`--text`、`--bg-subtle` 这类），组件只用语义令牌，深浅色只在色阶那一级切换 | `apps/dashboard/src/lib/styles/tokens.css`、`radix.css` | 检查脚本：组件里出现裸值就失败 |
| 2 样式 | 线宽、圆角的嵌套关系、阴影给谁、不用什么效果 | [`DESIGN.md`](../../apps/dashboard/DESIGN.md) 第 1 节 | 检查脚本 |
| 3 组件 | 按钮、输入框、卡片、对话框、表格、图表等 | `apps/dashboard/src/lib/ui`、`src/lib/charts`；清单在 `DESIGN.md` 第 4 节 | 检查脚本：原型和生产的组件逐字节相同 |
| 4 范式 | 完成一件事的最小界面单位，例如「设置分区」「列表与新建」 | 还没有成文，见走查的 B5 | 评审 |
| 5 决策 | 什么时候用什么 | `DESIGN.md` 第 1 节（12 条硬性规则）和第 6 节（状态）；「什么时候用什么」的草案在走查里，待确认 | 评审 |

产品层面的约束（给谁用、做什么不做什么、文案边界）在 [`../product/constraints.md`](../product/constraints.md)，它在这五层之上。

## 这个目录里的文件

| 文件 | 内容 |
|---|---|
| [`design-review-2026-11.md`](design-review-2026-11.md) | 最近一次设计走查：现状、数字、改进清单、决策表草案 |
| [`capabilities.md`](capabilities.md) | 功能与术语的依据表。2026-10-22 的快照，写于看板实现之前；术语词表仍然有效，「已有界面」部分已过时，以代码为准 |

## 改界面的步骤

1. 先看 [`../product/constraints.md`](../product/constraints.md) 和 `DESIGN.md` 第 1 节。
2. 在主原型里改，见 [`../../designs/README.md`](../../designs/README.md)。
3. 确认之后在 `apps/dashboard` 实现。
4. 运行 `node tools/design-lint/check.mjs`。

只是把界面改回已确认的样子（修一个错位、补一个漏掉的状态），不需要先做原型。

## 给 AI 助手的说明

- 新界面先找已有页面仿写：设置分区照 `apps/dashboard/src/routes/(app)/s/[id]/settings/GeneralSection.svelte`，列表与新建照同目录的 `GoalsSection.svelte`，分步确认照 `routes/(app)/account/TwoFactorCard.svelte`。
- 新增组件之前，先确认 `src/lib/ui` 里的组件组合不出来。
- 写完跑检查脚本；它失败时修代码，不要加例外注释绕过去，除非能写出一句站得住的原因。
- 改了样式要看渲染结果：浅色和深色、桌面和手机宽度都看。只通过类型检查不算看过。
