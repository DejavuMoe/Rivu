// 生成看板世界地图的国家形状：src/lib/charts/world/countries-110m.ts。
// 输入是 Natural Earth 1:110m Admin 0 – Countries 的 GeoJSON（公有领域，来源与校验和见 world/PROVENANCE.md）：
//
//   node scripts/gen-world-map.mjs <ne_110m_admin_0_countries.geojson>
//
// 做的事：Equal Earth 投影 → 放到宽 1000 的整数网格上 → 去掉重复点和投影后小于 1 个单位的岛 → 写成相对坐标的 SVG 路径。
// 国家按 ISO 3166-1 两字母代码（地理库返回的就是它）；没有代码的两块并入所在国家；去掉南极洲。
// 只依赖 Node 本身，生成结果是确定的（同一输入得到逐字节相同的输出）。

import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const input = process.argv[2];
if (!input) {
  console.error("usage: node scripts/gen-world-map.mjs <ne_110m_admin_0_countries.geojson>");
  process.exit(1);
}
const raw = readFileSync(input);
const geo = JSON.parse(raw.toString("utf8"));

// ---------- Equal Earth 投影（Šavrič, Patterson, Jenny 2018） ----------
const A1 = 1.340264, A2 = -0.081106, A3 = 0.000893, A4 = 0.003796;
const M = Math.sqrt(3) / 2;
function equalEarth(lon, lat) {
  const l = (lon * Math.PI) / 180;
  const p = (lat * Math.PI) / 180;
  const th = Math.asin(M * Math.sin(p));
  const t2 = th * th, t6 = t2 * t2 * t2;
  const x = (l * Math.cos(th)) / (M * (A1 + 3 * A2 * t2 + t6 * (7 * A3 + 9 * A4 * t2)));
  const y = th * (A1 + A2 * t2 + t6 * (A3 + A4 * t2));
  return [x, y];
}

// 投影后的范围：经度 ±180、纬度从南纬 60 度（去掉南极洲后的最南端）到北纬 84 度
const [xMax] = equalEarth(180, 0);
const [, yTop] = equalEarth(0, 84);
const [, yBottom] = equalEarth(0, -60);
const W = 1000;
const scale = W / (2 * xMax);
const H = Math.round((yTop - yBottom) * scale);
const px = ([lon, lat]) => {
  const [x, y] = equalEarth(lon, lat);
  return [Math.round((x + xMax) * scale), Math.round((yTop - y) * scale)];
};

// ---------- 国家代码 ----------
const MERGE = { "N. Cyprus": "CY", Somaliland: "SO" };
function codeOf(p) {
  if (MERGE[p.NAME]) return MERGE[p.NAME];
  for (const c of [p.ISO_A2_EH, p.ISO_A2]) if (/^[A-Z]{2}$/.test(c ?? "")) return c;
  throw new Error(`no ISO code for ${p.NAME}`);
}

// ---------- 环 → 路径 ----------
function ringPath(ring) {
  const pts = [];
  for (const c of ring) {
    const q = px(c);
    const last = pts.at(-1);
    if (!last || last[0] !== q[0] || last[1] !== q[1]) pts.push(q);
  }
  if (pts.length > 1 && pts[0][0] === pts.at(-1)[0] && pts[0][1] === pts.at(-1)[1]) pts.pop();
  // 去掉共线的中间点
  const out = [];
  for (const q of pts) {
    if (out.length >= 2) {
      const [a, b] = [out.at(-2), out.at(-1)];
      if ((b[0] - a[0]) * (q[1] - b[1]) === (b[1] - a[1]) * (q[0] - b[0])) out.pop();
    }
    out.push(q);
  }
  if (out.length < 3) return "";
  let area = 0;
  for (let i = 0; i < out.length; i++) {
    const [x1, y1] = out[i];
    const [x2, y2] = out[(i + 1) % out.length];
    area += x1 * y2 - x2 * y1;
  }
  if (Math.abs(area) / 2 < 1) return "";
  const num = (n, first) => (n < 0 ? `${n}` : first ? `${n}` : ` ${n}`);
  let d = `M${out[0][0]} ${out[0][1]}l`;
  for (let i = 1; i < out.length; i++) {
    const dx = out[i][0] - out[i - 1][0];
    const dy = out[i][1] - out[i - 1][1];
    d += num(dx, i === 1) + num(dy, false);
  }
  return d + "z";
}

const paths = new Map();
for (const f of geo.features) {
  const p = f.properties;
  if (p.ISO_A2_EH === "AQ" || p.NAME === "Antarctica") continue;
  const code = codeOf(p);
  const polys = f.geometry.type === "Polygon" ? [f.geometry.coordinates] : f.geometry.coordinates;
  const d = polys.flatMap((poly) => poly.map(ringPath)).join("");
  if (d) paths.set(code, (paths.get(code) ?? "") + d);
}

const rows = [...paths.entries()].sort(([a], [b]) => a.localeCompare(b));
const sha = createHash("sha256").update(raw).digest("hex");
const body = `// 生成文件，不要手改：node scripts/gen-world-map.mjs <ne_110m_admin_0_countries.geojson>
// 来源 Natural Earth 1:110m Admin 0 – Countries（公有领域），见同目录 PROVENANCE.md。输入 sha256 ${sha}
// Equal Earth 投影，宽 ${W}、高 ${H} 的整数网格；每项是 [ISO 3166-1 两字母代码, SVG 路径]。

export const WORLD_WIDTH = ${W};
export const WORLD_HEIGHT = ${H};

export const COUNTRIES: [string, string][] = [
${rows.map(([c, d]) => `  ["${c}", "${d}"],`).join("\n")}
];
`;
const here = dirname(fileURLToPath(import.meta.url));
const out = join(here, "../src/lib/charts/world/countries-110m.ts");
writeFileSync(out, body);
console.log(`${rows.length} countries, ${body.length} bytes -> ${out}`);
