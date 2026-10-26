// 汇总探针结果（probe.mjs 写的 JSON 行）：总轮数、通过率、最长连续通过的时长、可见延迟分位数、失败列表。
// 用法：node probe/summary.mjs probe.jsonl [--days 7]
// 「连续 7 天全部通过」：最后一次失败之后连续通过的时长 ≥ 7 天，且这段时间里相邻两轮的间隔不超过 2 个计划间隔（中途没有停跑）。

import { readFileSync } from 'node:fs';
import { parseArgs } from 'node:util';

const { values, positionals } = parseArgs({
  allowPositionals: true,
  options: { days: { type: 'string', default: '7' }, 'max-gap': { type: 'string', default: '1200' } },
});
const file = positionals[0] ?? 'probe.jsonl';
const rows = readFileSync(file, 'utf8')
  .split('\n')
  .filter((l) => l.trim())
  .map((l) => JSON.parse(l))
  .sort((a, b) => a.at.localeCompare(b.at));
if (rows.length === 0) {
  console.log('没有记录');
  process.exit(1);
}
const maxGapMs = Number(values['max-gap']) * 1000;
const pass = rows.filter((r) => r.ok);
const lat = pass.map((r) => r.visible_ms).sort((a, b) => a - b);
const q = (p) => (lat.length ? lat[Math.min(lat.length - 1, Math.floor(p * lat.length))] : null);

// 末尾连续通过的一段（遇到失败或中断就断开）
let streakStart = rows.length;
for (let i = rows.length - 1; i >= 0; i--) {
  if (!rows[i].ok) break;
  if (i < rows.length - 1 && Date.parse(rows[i + 1].at) - Date.parse(rows[i].at) > maxGapMs) break;
  streakStart = i;
}
const streakMs = streakStart < rows.length ? Date.parse(rows[rows.length - 1].at) - Date.parse(rows[streakStart].at) : 0;
const needMs = Number(values.days) * 86_400_000;

console.log(`记录：${rows.length} 轮（${rows[0].at} 至 ${rows[rows.length - 1].at}）`);
console.log(`通过：${pass.length}，失败：${rows.length - pass.length}（通过率 ${((100 * pass.length) / rows.length).toFixed(2)}%）`);
console.log(`可见延迟（关页到数字完全出现）：p50 ${q(0.5)} ms，p95 ${q(0.95)} ms，max ${lat[lat.length - 1] ?? null} ms`);
console.log(`末尾连续通过：${streakStart < rows.length ? rows.length - streakStart : 0} 轮，${(streakMs / 3_600_000).toFixed(1)} 小时`);
for (const r of rows.filter((x) => !x.ok).slice(-10)) console.log(`  失败 ${r.at} ${r.rid}：${r.error} ${JSON.stringify(r.got ?? {})}`);
const ok = streakMs >= needMs;
console.log(ok ? `结论：连续 ${values.days} 天全部通过` : `结论：还没有连续 ${values.days} 天全部通过`);
process.exit(ok ? 0 : 1);
