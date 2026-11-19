# Rivu 设计规范

界面规则只有一份，在 [`apps/dashboard/DESIGN.md`](../../apps/dashboard/DESIGN.md)。原型和生产按同一份规则实现，这里不再另存副本，避免两份各改各的。

原型里路径不同的地方只有一处：令牌在 `src/styles/tokens.css`（生产在 `src/lib/styles/tokens.css`），内容必须逐字节相同，由 `node tools/design-lint/check.mjs` 检查。
