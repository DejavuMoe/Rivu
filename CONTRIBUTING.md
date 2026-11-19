# 参与 Rivu

欢迎提交问题与改动。开始之前先读 [`README.md`](README.md) 的开发快速开始，以及与改动相关的设计文档（`docs/architecture/`）。

## 提交问题

- 缺陷：写清版本（`rivu --version`）、部署方式、复现步骤、期望与实际结果。日志里不要带访客 IP、API key 或密码。
- 安全问题不要公开提交，按 [`SECURITY.md`](SECURITY.md) 私下报告。
- 想加功能：先开一个问题讨论，说明要解决的场景。`docs/roadmap.md` 列出了已经计划的工作。

## 提交改动

1. 一个改动只做一件事；涉及行为或口径的改动同时更新对应的设计文档。
2. 本地通过：
   ```bash
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo test                                  # 加载 .env.example 后会连 dev 的 PostgreSQL / ClickHouse
   cd apps/tracker && pnpm test && pnpm size    # 改了采集脚本时
   cd apps/dashboard && pnpm test && pnpm run check   # 改了看板时
   ```
3. 改数据库结构：新增一个迁移文件，不修改已登记在 `migrations/checksums.lock` 里的迁移（v0.1.0 发布前例外）；
   用 `RIVU_SCHEMA_UPDATE=1 cargo test -p rivu --test schema_snapshot` 更新 `schema/` 快照。
4. 改界面：先对照 `apps/dashboard/DESIGN.md` 的硬性规则；较大的界面改动先在 `designs/` 的原型里做并确认。
5. 提交说明用英文，格式 `type(scope): summary`（`feat`、`fix`、`docs`、`refactor`、`test`、`chore`）。
6. 每次 Git 提交都必须在提交说明末尾添加以下固定的协作者记录，并且只保留这一条 `Co-Authored-By`。

   ```text
   Co-Authored-By: DeepSeek v4.1 Flash Codex (1M context) <noreply@openai.com>
   ```

## 许可

提交的代码按 [MIT](LICENSE) 许可发布。不要复制 Plausible 服务端（AGPL-3.0）或其他不兼容许可的代码；引入数据文件时在对应目录写明来源与许可（参考 `crates/ua/data/uap-core/PROVENANCE.md`）。
