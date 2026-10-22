// 构建与体积检查共用的配置。规范 §3.1：
//   esbuild --bundle --minify --format=iife --target=es2018 --legal-comments=none
// 除 `__FULL__` 与 `__SV__` 外不得新增编译期开关。

import { build } from 'esbuild';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
export const DIST = path.join(ROOT, 'dist');

/**
 * 已实现的变体、各自的编译期常量与 gzip -9 体积预算（字节，规范 §1.1 / §3.2）。
 * full 尚未实现（src/full/index.ts 只是挂载点），实现后在这里加一行：
 *   full: { full: true, budget: 2048 }
 * 并补 `t.js` 与 `full.js` 逐字节相同的检查（规范 §11.3）。
 */
export const VARIANTS = {
  core: { full: false, budget: 1024 },
};

/** 构建号：CI 传 `RIVU_SV`（如 `${{ github.run_number }}`，单调递增）；本地默认 1。 */
export function buildNumber() {
  const raw = process.env.RIVU_SV ?? '1';
  const n = Number(raw);
  // 线协议：sv 为 0–999999 的整数，非法值服务端按 0 处理（规范 §5.1）。
  if (!Number.isInteger(n) || n < 0 || n > 999_999) {
    throw new Error(`RIVU_SV 必须是 0–999999 的整数，得到 ${JSON.stringify(raw)}`);
  }
  return n;
}

/** 在内存里构建一个变体，返回产物文本与 metafile。 */
export async function buildVariant(name) {
  const v = VARIANTS[name];
  if (!v) throw new Error(`未知变体 ${name}`);
  const result = await build({
    entryPoints: [path.join(ROOT, 'src/index.ts')],
    outfile: path.join(DIST, `${name}.js`),
    bundle: true,
    minify: true,
    format: 'iife',
    target: 'es2018',
    legalComments: 'none',
    define: { __FULL__: String(v.full), __SV__: String(buildNumber()) },
    write: false,
    metafile: true,
    logLevel: 'warning',
  });
  const file = result.outputFiles.find((f) => f.path.endsWith(`${name}.js`));
  if (!file) throw new Error('esbuild 没有产出 JS');
  return { code: file.text, metafile: result.metafile };
}
