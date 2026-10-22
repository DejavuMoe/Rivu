// 产物检查（规范 §3.3、§11.3）。任一项失败则退出码非 0。
//   pnpm size                       # 先 pnpm build
//   node scripts/size.mjs --update-baseline   # 刷新 size-baseline.json（仅在有意改变体积时）
//
// 检查项：体积预算（gzip -9）、体积回归、模块泄漏、禁用 API、语法（es2018）、构建确定性。
// 注：gzip 用 Node zlib level 9，等价于 `gzip -9 -n`（头 10 B + 尾 8 B，不含文件名）；
// 命令行 `gzip -9` 默认会写入文件名，会多出几个字节。

import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import zlib from 'node:zlib';
import { parse } from 'acorn';
import { DIST, ROOT, VARIANTS, buildVariant } from './lib.mjs';

const BASELINE = path.join(ROOT, 'size-baseline.json');
const WARN_GROWTH = 16; // 规范 §3.3：增长 > 16 B 预警
const FAIL_GROWTH = 64; // 增长 > 64 B 需 label 批准（CI 里以 RIVU_SIZE_APPROVED=1 表示）

const gzip = (b) => zlib.gzipSync(b, { level: 9 }).length;
const brotli = (b) =>
  zlib.brotliCompressSync(b, {
    params: {
      [zlib.constants.BROTLI_PARAM_QUALITY]: 11,
      [zlib.constants.BROTLI_PARAM_SIZE_HINT]: b.length,
    },
  }).length;
const sha = (s) => createHash('sha256').update(s).digest('hex').slice(0, 16);

const updateBaseline = process.argv.includes('--update-baseline');
const failures = [];
const warnings = [];
const fail = (m) => failures.push(m);

let baseline = {};
try {
  baseline = JSON.parse(await readFile(BASELINE, 'utf8'));
} catch {
  /* 没有基线：只报告，不比较 */
}

const rows = [];
const next = {};
for (const [name, v] of Object.entries(VARIANTS)) {
  let code;
  try {
    code = await readFile(path.join(DIST, `${name}.js`), 'utf8');
  } catch {
    fail(`dist/${name}.js 不存在，先运行 pnpm build`);
    continue;
  }
  const buf = Buffer.from(code);
  const sizes = { raw: buf.length, gzip: gzip(buf), brotli: brotli(buf) };
  next[name] = sizes;

  // 1. 体积预算：gzip -9 超标即失败；brotli 与原始大小只报告。
  if (sizes.gzip > v.budget) fail(`${name} gzip ${sizes.gzip} B 超过预算 ${v.budget} B`);

  // 2. 体积回归：与基线对照。
  const base = baseline[name];
  let delta = '无基线';
  if (base) {
    const d = sizes.gzip - base.gzip;
    delta = (d > 0 ? '+' : '') + d + ' B';
    if (d > FAIL_GROWTH && process.env.RIVU_SIZE_APPROVED !== '1') {
      fail(`${name} gzip 相比基线增长 ${d} B（> ${FAIL_GROWTH} B，需 label 批准：RIVU_SIZE_APPROVED=1）`);
    } else if (d > WARN_GROWTH) {
      warnings.push(`${name} gzip 相比基线增长 ${d} B（> ${WARN_GROWTH} B）`);
    }
  }
  rows.push({ 变体: name, 原始: sizes.raw, 'gzip -9': sizes.gzip, 'brotli -q11': sizes.brotli, 预算: v.budget, 基线差: delta });

  // 3. 模块泄漏：core 的产物里不得有 src/full/ 的任何字节。
  const built = await buildVariant(name);
  const out = Object.values(built.metafile.outputs)[0];
  for (const [input, info] of Object.entries(out.inputs)) {
    if (!v.full && input.includes('src/full/') && info.bytesInOutput > 0) {
      fail(`${name} 含有 full 专属模块 ${input}（${info.bytesInOutput} B）`);
    }
  }

  // 4. 禁用 API（规范 §3.3）。
  const forbidden = [
    [/cookie/i, 'cookie'],
    [/sessionStorage/, 'sessionStorage'],
    [/indexedDB/i, 'indexedDB'],
    [/setItem/, 'setItem'],
    [/XMLHttpRequest/, 'XMLHttpRequest'],
    [/\beval\b/, 'eval'],
    [/new Function/, 'new Function'],
    [/console\.log/, 'console.log'],
  ];
  for (const [re, label] of forbidden) {
    if (re.test(code)) fail(`${name} 命中禁用 API：${label}`);
  }
  // localStorage 只允许出现 1 次，且紧接 rivu_ignore（只读自助排除开关）。
  const ls = code.match(/localStorage/g) ?? [];
  if (ls.length !== 1 || !/localStorage\.rivu_ignore/.test(code)) {
    fail(`${name} 的 localStorage 必须恰好出现 1 次且紧接 .rivu_ignore（实际 ${ls.length} 次）`);
  }

  // 5. 语法：能被 ecmaVersion 2018 的解析器完整解析，说明没有更新的语法残留。
  try {
    parse(code, { ecmaVersion: 2018, sourceType: 'script' });
  } catch (e) {
    fail(`${name} 不是合法的 es2018 脚本：${e.message}`);
  }

  // 6. 确定性：同一提交再构建一次，哈希一致，且与 dist 里的文件一致。
  const again = await buildVariant(name);
  if (sha(again.code) !== sha(built.code) || sha(code) !== sha(built.code)) {
    fail(`${name} 构建不确定：dist=${sha(code)} 重建=${sha(built.code)}/${sha(again.code)}`);
  }
}

console.table(rows);
for (const w of warnings) console.warn(`警告：${w}`);

if (updateBaseline) {
  await writeFile(BASELINE, JSON.stringify(next, null, 2) + '\n');
  console.log('已写入 size-baseline.json');
}

if (failures.length) {
  for (const f of failures) console.error(`失败：${f}`);
  process.exit(1);
}
console.log('产物检查通过');
