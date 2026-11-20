// 从 @radix-ui/colors 生成 src/styles/radix.css：只取界面用到的色阶，浅色写在 :root，
// 深色写两遍（跟随系统 + 手动切换；手动切换的选择器不限根元素，页面里的一小块也可以单独用深色）。改用别的色阶时改 SCALES 再运行：node scripts/gen-radix.mjs
import * as radix from "@radix-ui/colors";
import { writeFileSync } from "node:fs";

// 中性色（slate）、强调色与水流的深色端（indigo）、水流的亮色端（cyan）、状态色、「此刻」的灯光色（orange / amber）、图表第 2/3 系列色
const SCALES = ["slate", "indigo", "cyan", "red", "grass", "amber", "orange", "tomato", "violet"];

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

[data-theme="dark"] {
${block("Dark")}
}
`;

writeFileSync(new URL("../src/styles/radix.css", import.meta.url), css);
console.log("wrote src/styles/radix.css");
