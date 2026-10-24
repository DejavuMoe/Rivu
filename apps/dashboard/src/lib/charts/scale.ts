// 图表的数学：刻度、单调三次插值路径。纯函数，不依赖 DOM。

/** 「好看」的刻度：0 起，约 count 个间隔，步长取 1/2/2.5/5 × 10^n。 */
export function niceTicks(max: number, count = 4): number[] {
  if (!(max > 0)) return [0, 1];
  const raw = max / count;
  const mag = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? 10 * mag;
  const top = Math.ceil(max / step) * step;
  const ticks: number[] = [];
  for (let v = 0; v <= top + step / 2; v += step) ticks.push(+v.toFixed(10));
  return ticks;
}

/**
 * 单调三次插值（Fritsch–Carlson）：曲线经过每个点、不会冲过极值，
 * 比直线柔和，又不像普通样条那样在突变处画出不存在的「下凹」。
 * null 断开路径。
 */
export function monotonePath(xs: number[], ys: (number | null)[]): string {
  let d = "";
  let seg: [number, number][] = [];
  const flush = () => {
    if (seg.length) d += segmentPath(seg);
    seg = [];
  };
  for (let i = 0; i < xs.length; i++) {
    const y = ys[i];
    if (y === null || !Number.isFinite(y)) flush();
    else seg.push([xs[i], y]);
  }
  flush();
  return d;
}

function segmentPath(p: [number, number][]): string {
  const n = p.length;
  if (n === 1) return `M${p[0][0]},${p[0][1]}`;
  if (n === 2) return `M${p[0][0]},${p[0][1]}L${p[1][0]},${p[1][1]}`;
  const dx: number[] = [];
  const m: number[] = [];
  for (let i = 0; i < n - 1; i++) {
    dx.push(p[i + 1][0] - p[i][0]);
    m.push((p[i + 1][1] - p[i][1]) / dx[i]);
  }
  const t: number[] = [m[0]];
  for (let i = 1; i < n - 1; i++) t.push(m[i - 1] * m[i] <= 0 ? 0 : (m[i - 1] + m[i]) / 2);
  t.push(m[n - 2]);
  for (let i = 0; i < n - 1; i++) {
    if (m[i] === 0) {
      t[i] = t[i + 1] = 0;
      continue;
    }
    const a = t[i] / m[i];
    const b = t[i + 1] / m[i];
    const s = a * a + b * b;
    if (s > 9) {
      const k = 3 / Math.sqrt(s);
      t[i] = k * a * m[i];
      t[i + 1] = k * b * m[i];
    }
  }
  let d = `M${p[0][0]},${p[0][1]}`;
  for (let i = 0; i < n - 1; i++) {
    const h = dx[i] / 3;
    d += `C${p[i][0] + h},${p[i][1] + h * t[i]} ${p[i + 1][0] - h},${p[i + 1][1] - h * t[i + 1]} ${p[i + 1][0]},${p[i + 1][1]}`;
  }
  return d;
}
