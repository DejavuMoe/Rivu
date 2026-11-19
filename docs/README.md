# 文档地图

每类问题只有一个地方回答。先按你要找的东西选一行。

| 你想知道 | 看哪里 |
|---|---|
| 现在做到哪了，离发布还差什么 | [`roadmap.md`](roadmap.md) |
| 做什么、不做什么，为什么 | [`product/constraints.md`](product/constraints.md) |
| 怎么安装、升级、备份 | [`install.md`](install.md) |
| 收集了访客的什么，怎么保护隐私 | [`privacy.md`](privacy.md) |
| 界面长什么样、有什么规则 | [`ui/README.md`](ui/README.md)，规则本身在 [`../apps/dashboard/DESIGN.md`](../apps/dashboard/DESIGN.md) |
| 原型在哪，怎么看，怎么确认 | [`../designs/README.md`](../designs/README.md) |
| 系统是怎么实现的 | [`architecture/`](architecture/) |
| 某个结论的测试依据 | [`research/`](research/) |
| 怎么搭开发环境、怎么提交改动 | [`../README.md`](../README.md)、[`../CONTRIBUTING.md`](../CONTRIBUTING.md) |

## 四类文档，各管一件事

| 类别 | 文件 | 什么时候改 |
|---|---|---|
| **进度** | `roadmap.md` | 一件事开始、做完、或者移出某个版本时 |
| **约束** | `product/constraints.md`（产品）、`apps/dashboard/DESIGN.md`（界面） | 取舍变了的时候。改约束需要项目所有者确认 |
| **设计** | `architecture/`（系统）、`ui/`（界面） | 行为或口径变了，和代码在同一个改动里更新 |
| **原型** | `designs/` | 界面改动动手之前 |

约束决定设计，设计决定原型和代码，进度只记录结果。几份文档说法不一致时，按这个顺序以前者为准。

## 目录

```
docs/
├── README.md               这一页
├── roadmap.md              进度
├── product/
│   └── constraints.md      产品约束
├── install.md              安装与运维
├── privacy.md              隐私说明
├── architecture/           系统设计
│   ├── system-design.md        总体设计，与其他文档冲突时以它为准
│   ├── platform-and-ops.md     部署、权限、后台任务、配置、监控、备份
│   └── tracker-and-protocol.md 采集脚本与上报协议
├── ui/                     界面设计
│   ├── README.md               五层设计系统各在哪
│   ├── design-review-2026-11.md 最近一次走查与改进清单
│   └── capabilities.md         功能与术语依据表（2026-10 快照）
├── research/               调研与验收报告，写完不再改
└── brief-p1.md             第一阶段的任务书（历史文档，代码注释里还在引用）
```

仓库里其他位置的文档：`apps/dashboard/DESIGN.md`（界面规则）、`apps/*/README.md`（各部分的开发说明）、`designs/README.md`（原型）、`CHANGELOG.md`（每个版本改了什么）。

## 写文档的约定

- 写给第一次看的人。专有名词第一次出现时，用一句话加一个例子说明它是什么。
- 一个事实只写在一个地方，其他地方链接过去。
- 调研和验收报告是当时的记录，写完不再改；结论变了就写一份新的。
- 文档里的数字要能在代码、测试或报告里找到出处。
