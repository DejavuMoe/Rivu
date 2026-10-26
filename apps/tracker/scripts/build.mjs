// 构建 dist/<变体>.js（core、full，以及 full 的副本 t.js），并写 dist/meta.json（构建号，供测试读取）。
//   pnpm build            # RIVU_SV 未设置时构建号为 1

import { mkdir, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { DIST, VARIANTS, buildNumber, buildVariant } from './lib.mjs';

await rm(DIST, { recursive: true, force: true });
await mkdir(DIST, { recursive: true });

for (const name of Object.keys(VARIANTS)) {
  const { code } = await buildVariant(name);
  await writeFile(path.join(DIST, `${name}.js`), code);
  console.log(`dist/${name}.js  ${Buffer.byteLength(code)} B`);
  // 通用脚本 t.js 与 full.js 逐字节相同（规范 §2.2）
  if (name === 'full') await writeFile(path.join(DIST, 't.js'), code);
}
await writeFile(path.join(DIST, 'meta.json'), JSON.stringify({ sv: buildNumber() }) + '\n');
