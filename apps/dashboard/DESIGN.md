# Rivu 设计规范

状态：已审批（2026-10-24，原型第二版），`apps/dashboard` 按本规范实现。界面改动先对照第 1 节。

## 1. 硬性规则

1. **颜色只用 Radix Colors**，并按 Radix 的分级用法：1 应用背景 · 2 次级背景 · 3/4/5 控件底（常态 / 悬停 / 按下或选中）· 6 分隔线 · 7 控件边框 · 8 悬停边框与焦点 · 9 实色 · 10 实色悬停 · 11 次要文字 · 12 主要文字。组件只用 `tokens.css` 的语义令牌，不写色阶编号，不写任何裸色值（`#fff`、`rgb()` 只允许出现在 `tokens.css`）。
2. **只有四个字号**：12（辅助信息、表头、坐标轴）· 14（正文、控件、表格）· 16（页面与对话框标题）· 24（指标数值）。最小 12。层级靠字重（400 / 500 / 600）、颜色（`--text` / `--text-muted`）和间距，不靠更多字号。
3. **线和边框一律 0.5px**（`var(--hairline)`）。焦点环是无障碍要求，例外，用 2px。
4. **圆角克制且同心**：控件 6、卡片与浮层 8、对话框 12；嵌套时内层 = 外层 − 间距（浮层 8 / 内边距 4 / 菜单项 4；分段控件 6 / 2 / 4）。贴边的子元素继承外层圆角。
5. **阴影只给浮层**（菜单、对话框、提示）；卡片、按钮、输入框没有阴影。不用渐变、不用发光、不用半透明毛玻璃。
6. **不要装饰性色条**：卡片、导航项、指标卡都不加彩色边线或色条；选中状态用背景（Radix 第 4/5 阶）和字重表达。
7. **动效默认没有**。确实需要时只用 `var(--dur) var(--ease)`（250ms ease-out），目前只有开关滑块在用。悬停、浮层出现、对话框打开都是即时的。
8. **不写中点「·」** 作分隔；需要并列时用间距、换列或换行。
9. **不写冗余说明**：页面标题下不加副标题，卡片标题下不加描述，字段不加显而易见的提示。只有在不说用户就会做错、或会误解数据时才写一句，并放在出错的位置旁边。
10. **不要眉题（eyebrow / overline）**：不在标题上方加小标签。分组靠标题字重与留白。
11. **文案面向这个界面的使用者**：站点所有者与分析人员。准确、简短、不口语化、不造词；按钮用动词；术语以 `docs/ui/capabilities.md` 词表为准。用「你」，不用「您」，不加感叹号，中文与数字之间加空格。
12. **少让用户做判断**：能给默认值就不问；需要选择时放在用户本来就要做决定的地方（如创建对话框里），不在页面上散布开关与「保存」按钮。没实现的功能不展示。

## 2. 令牌（`src/lib/styles/tokens.css`）

| 用途 | 令牌 | Radix |
|---|---|---|
| 页面、卡片 | `--bg-app` | gray 1 |
| 侧栏、表头、次级区块 | `--bg-subtle` | gray 2 |
| 控件底 / 悬停 / 选中 | `--bg-element` / `--bg-hover` / `--bg-active` | gray 3 / 4 / 5 |
| 分隔线 / 控件边框 / 悬停边框 | `--border-subtle` / `--border` / `--border-hover` | gray 6 / 7 / 8 |
| 占位与禁用文字 | `--text-placeholder` | gray 9 |
| 次要 / 主要文字 | `--text-muted` / `--text` | gray 11 / 12 |
| 强调 | `--accent-bg(-hover/-active)` `--accent-border` `--accent-focus` `--accent-solid(-hover)` `--accent-text` | blue 3–5、7、8、9–10、11 |
| 状态 | `--good-*` `--warn-*` `--bad-*` | grass / amber / red 3、11（危险按钮用 red 9） |
| 在线 | `--live` | grass 9 |
| 图表 | `--series-1/2/3` `--series-compare` `--chart-area` `--chart-bar` `--chart-grid` `--chart-axis` | blue 9、tomato 9、violet 9、gray 8、blue a3、gray a3、gray a4、gray a6 |

换强调色只改 `--accent-*` 与 `--series-1` 指向的色阶（例如 blue → iris），并在 `scripts/gen-radix.mjs` 里加上那个色阶。

## 3. 布局与层级

- 应用页：左侧栏 240px（`--bg-subtle`，右侧 0.5px 分隔线）+ 内容区（`--bg-app`）。窄于 860px 时侧栏变为顶栏 + 抽屉。
- 内容区宽度：看板 1200px；设置与表单页 880px。页面外边距 32px（窄屏 16px），区块间距 16px 或 24px。
- 页面标题 16 / 600，没有副标题；标题行右侧放这一页唯一的主操作。
- 卡片：0.5px `--border-subtle`、圆角 8、无阴影、无填充色差。卡片标题 14 / 600。
- 表单：设置页用 `Field layout="row"`（左标签、右控件），一组字段一张卡片；只有一组可保存的字段时才出现「保存」，放在卡片右下。开关类设置即时生效，不需要保存。
- 表格：表头 12 / 500 / `--text-muted`，行高 40，行间 0.5px 分隔线，数字右对齐等宽。

## 4. 组件（`src/lib/ui`、`src/lib/charts`）

`Button`（primary / secondary / ghost / danger）· `Select` · `Popover` + `MenuItem` / `MenuLabel` · `Segmented` · `Tabs` · `Input` / `Field` / `Checkbox` / `Switch` · `Card` · `Table` · `Dialog` · `Callout`（只用于警告与错误）· `EmptyState` · `Badge` · `CopyField` · `Avatar`（中性灰，不按名字上色）· `LiveDot` · `Skeleton` · `toast()` · `TrendChart` / `BarList` / `Sparkline`。

新增组件前先确认现有组件组合不出来；新组件放 `src/lib/ui`，只用令牌。

## 5. 数据可视化

- 主序列 `--series-1`（blue 9），2px 线 + 平涂的 `--chart-area`，不用渐变；对比期 1.5px `--series-compare`。最多 3 个分类色（blue / tomato / violet，已做色觉缺陷校验，深浅两种模式全部通过）。
- 一个纵轴；网格与轴线 0.5px；刻度 0 起、取整；坐标轴文字 12px。
- 进行中的时间桶用虚线，这是虚线唯一的用途。
- 条形列表的条是中性的 `--chart-bar`；文字永远是文字色。
- 悬停：十字线 + 圆点 + 提示框；键盘左右键同样可用。每个图都有表格视图。
- 展示地理数据的地方保留 DB-IP 署名（CC BY 4.0 要求），用 12px 次要文字。

## 6. 状态

每个数据区域都要处理：首次加载（静态占位块，不闪烁）· 空（一句原因 + 下一步）· 出错 · 排队 / 超时 · 无权限（404）· 只读（不显示写操作入口，而不是显示灰掉的按钮）。

## 7. 可访问性

键盘可达；焦点环统一（2px `--accent-focus`）；图标按钮必须有 `aria-label`；状态不只靠颜色；对话框用原生 `<dialog>`。
