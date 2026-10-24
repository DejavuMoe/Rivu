// 从 @radix-ui/colors 生成 src/lib/styles/radix.css：只取界面用到的色阶，浅色写在 :root，
// 深色写两遍（跟随系统 + 手动切换）。改用别的色阶时改 SCALES 再运行：node scripts/gen-radix.mjs
import * as radix from "@radix-ui/colors";
import { writeFileSync } from "node:fs";

// 中性色、强调色、状态色、图表第 2/3 系列色
const SCALES = ["gray", "blue", "red", "grass", "amber", "tomato", "violet"];

function block(suffix) {
  const lines = [];
  for (const name of SCALES) {
    for (const alpha of [false, true]) {
      const key = `${name}${suffix}${alpha ? "A" : ""}`;
      const scale = radix[key];
      if (!scale) throw new Error(`missing scale ${key}`);
      for (let i = 1; i <= 12; i++) {
        const v = scale[`${name}${alpha ? "A" : ""}${i}`];
        lines.push(`  --${name}-${alpha ? "a" : ""}${i}: ${v};`);
      }
    }
  }
  return lines.join("\n");
}

const css = `/* 由 scripts/gen-radix.mjs 从 @radix-ui/colors 生成，不要手改。 */

:root {
${block("")}
}

@media (prefers-color-scheme: dark) {
  :root:where(:not([data-theme="light"])) {
${block("Dark").replace(/^/gm, "  ")}
  }
}

:root[data-theme="dark"] {
${block("Dark")}
}
`;

writeFileSync(new URL("../src/lib/styles/radix.css", import.meta.url), css);
console.log("wrote src/lib/styles/radix.css");
