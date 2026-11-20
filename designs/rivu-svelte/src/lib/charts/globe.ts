// 地球：一张 canvas 上的点阵地球。陆地是球面上均匀撒的点（斐波那契点阵），落在国家形状里的才留下；
// 形状就是平面地图用的那一份（world/countries-110m，Natural Earth，公有领域），运行时反查，不另带数据。
// 昼夜按此刻太阳直射点的位置算。有访客的国家按多少分五档变亮；此刻在线的国家有一圈圈扩散的光。
// 可以拖着转（带惯性），空闲时自己慢慢转；系统要求减少动效时不转、光圈不动。
// 颜色全部取自宿主元素上的设计令牌（--globe-*、--live），换主题时调用 restyle()。
import { COUNTRIES, WORLD_HEIGHT, WORLD_WIDTH } from "./world/countries-110m";

const RAD = Math.PI / 180;
const TAU = Math.PI * 2;
type Vec = [number, number, number];
type Rgb = [number, number, number];

const clamp01 = (v: number) => Math.max(0, Math.min(1, v));
const smooth = (a: number, b: number, x: number) => {
  const t = clamp01((x - a) / (b - a));
  return t * t * (3 - 2 * t);
};
/** 单位向量：经度 0 朝屏幕外，x 向东，y 向北 */
const vec = (lon: number, lat: number): Vec => {
  const la = lat * RAD;
  const lo = lon * RAD;
  return [Math.cos(la) * Math.sin(lo), Math.sin(la), Math.cos(la) * Math.cos(lo)];
};

// ---------- 国家形状的反查 ----------
// 形状文件是 Equal Earth 投影后的路径；这里用同一组常数把经纬度投到那张图上（见 scripts/gen-world-map.mjs）。
const A1 = 1.340264;
const A2 = -0.081106;
const A3 = 0.000893;
const A4 = 0.003796;
const M = Math.sqrt(3) / 2;
function equalEarth(lon: number, lat: number): [number, number] {
  const l = lon * RAD;
  const th = Math.asin(M * Math.sin(lat * RAD));
  const t2 = th * th;
  const t6 = t2 * t2 * t2;
  return [(l * Math.cos(th)) / (M * (A1 + 3 * A2 * t2 + t6 * (7 * A3 + 9 * A4 * t2))), th * (A1 + A2 * t2 + t6 * (A3 + A4 * t2))];
}
const X_MAX = equalEarth(180, 0)[0];
const Y_TOP = equalEarth(0, 84)[1];
const SCALE = WORLD_WIDTH / (2 * X_MAX);
const LAT_MIN = -60;
const LAT_MAX = 84;

interface Mask {
  data: Uint8ClampedArray;
  w: number;
  h: number;
  s: number;
}
let maskCache: Mask | null | undefined;
/** 把每个国家用「编号颜色」画到一张离屏画布上，之后按像素查国家 */
function mask(): Mask | null {
  if (maskCache !== undefined) return maskCache;
  const s = 2;
  const cv = document.createElement("canvas");
  cv.width = WORLD_WIDTH * s;
  cv.height = WORLD_HEIGHT * s;
  const ctx = cv.getContext("2d", { willReadFrequently: true });
  if (!ctx || typeof Path2D === "undefined") return (maskCache = null);
  ctx.scale(s, s);
  // 大的先画、小的后画，被包在别国里面的小国不会被盖掉
  const order = COUNTRIES.map((_, i) => i).sort((a, b) => COUNTRIES[b][1].length - COUNTRIES[a][1].length);
  for (const i of order) {
    const k = i + 1;
    // 颜色只是编号：红 = 编号，绿和蓝是校验，抗锯齿混出来的边缘像素对不上校验，当作海
    ctx.fillStyle = `rgb(${k},${255 - k},${(k * 97) % 256})`;
    ctx.fill(new Path2D(COUNTRIES[i][1]));
  }
  const img = ctx.getImageData(0, 0, cv.width, cv.height);
  return (maskCache = { data: img.data, w: cv.width, h: cv.height, s });
}
function countryAt(m: Mask, lon: number, lat: number): number {
  if (lat < LAT_MIN || lat > LAT_MAX) return -1;
  const [ex, ey] = equalEarth(lon, lat);
  const px = Math.round((ex + X_MAX) * SCALE * m.s);
  const py = Math.round((Y_TOP - ey) * SCALE * m.s);
  if (px < 0 || py < 0 || px >= m.w || py >= m.h) return -1;
  const o = (py * m.w + px) * 4;
  const r = m.data[o];
  if (m.data[o + 3] !== 255 || r === 0 || m.data[o + 1] !== 255 - r || m.data[o + 2] !== (r * 97) % 256) return -1;
  return r - 1;
}

/** 形状文件里没有的小地方（1:110m 的图上画不出来）的位置，只用来放光点 */
const SMALL: Record<string, [number, number]> = {
  SG: [103.82, 1.35],
  HK: [114.17, 22.32],
  MO: [113.55, 22.2],
  BH: [50.55, 26.07],
  MT: [14.4, 35.9],
  MU: [57.55, -20.25],
  MV: [73.2, 3.2],
  LI: [9.55, 47.16],
  MC: [7.42, 43.73],
  AD: [1.52, 42.5],
  SM: [12.46, 43.94],
  BB: [-59.54, 13.19],
  CV: [-23.6, 15.1],
  KM: [43.9, -11.9],
  SC: [55.49, -4.68],
  ST: [6.6, 0.25],
  AG: [-61.8, 17.07],
  DM: [-61.37, 15.42],
  GD: [-61.68, 12.12],
  LC: [-60.98, 13.9],
  VC: [-61.2, 13.25],
  KN: [-62.75, 17.33],
  WS: [-172.1, -13.76],
  TO: [-175.2, -21.18],
  KI: [173, 1.42],
  FM: [158.2, 6.92],
  MH: [171.2, 7.1],
  PW: [134.5, 7.5],
  NR: [166.93, -0.53],
  TV: [179.2, -8.52],
};

interface Land {
  x: Float32Array;
  y: Float32Array;
  z: Float32Array;
  /** 每个点属于哪个国家（COUNTRIES 的下标） */
  c: Int16Array;
  count: number;
  /** 各国的中心（国家里所有点的平均方向）；没有点的国家用 SMALL */
  centre: Map<string, Vec>;
}
const landCache = new Map<number, Land>();
function land(total: number): Land | null {
  const hit = landCache.get(total);
  if (hit) return hit;
  const m = mask();
  if (!m) return null;
  const golden = Math.PI * (3 - Math.sqrt(5));
  const xs: number[] = [];
  const ys: number[] = [];
  const zs: number[] = [];
  const cs: number[] = [];
  const sums = new Map<number, Vec>();
  for (let i = 0; i < total; i++) {
    const y = 1 - ((i + 0.5) / total) * 2;
    const r = Math.sqrt(1 - y * y);
    const th = golden * i;
    const x = Math.cos(th) * r;
    const z = Math.sin(th) * r;
    const c = countryAt(m, Math.atan2(x, z) / RAD, Math.asin(y) / RAD);
    if (c < 0) continue;
    xs.push(x);
    ys.push(y);
    zs.push(z);
    cs.push(c);
    const s = sums.get(c);
    if (s) {
      s[0] += x;
      s[1] += y;
      s[2] += z;
    } else sums.set(c, [x, y, z]);
  }
  const centre = new Map<string, Vec>();
  for (const [c, s] of sums) {
    const n = Math.hypot(s[0], s[1], s[2]) || 1;
    centre.set(COUNTRIES[c][0], [s[0] / n, s[1] / n, s[2] / n]);
  }
  for (const [code, [lon, lat]] of Object.entries(SMALL)) if (!centre.has(code)) centre.set(code, vec(lon, lat));
  const out: Land = { x: Float32Array.from(xs), y: Float32Array.from(ys), z: Float32Array.from(zs), c: Int16Array.from(cs), count: xs.length, centre };
  landCache.set(total, out);
  return out;
}

/** 太阳此刻直射的地方（近似到度：日期决定纬度，UTC 时刻决定经度） */
export function subsolar(now: Date): Vec {
  const start = Date.UTC(now.getUTCFullYear(), 0, 0);
  const day = (now.getTime() - start) / 86400000;
  const lat = -23.44 * Math.cos((TAU * (day + 10)) / 365);
  const hours = now.getUTCHours() + now.getUTCMinutes() / 60;
  return vec((12 - hours) * 15, lat);
}

// ---------- 颜色 ----------
function parseColor(probe: CanvasRenderingContext2D, value: string, fallback: Rgb): Rgb {
  if (!value) return fallback;
  probe.fillStyle = "#000";
  probe.fillStyle = value;
  const v = String(probe.fillStyle);
  let m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(v);
  if (m) return [parseInt(m[1], 16), parseInt(m[2], 16), parseInt(m[3], 16)];
  m = /^rgba?\(\s*([\d.]+)[,\s]+([\d.]+)[,\s]+([\d.]+)/i.exec(v);
  return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : fallback;
}
const rgba = (c: Rgb, a: number) => `rgba(${c[0] | 0},${c[1] | 0},${c[2] | 0},${a.toFixed(3)})`;
const mix = (a: Rgb, b: Rgb, t: number): Rgb => [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];

interface Colors {
  seaLit: Rgb;
  sea: Rgb;
  shade: Rgb;
  air: Rgb;
  airAlpha: number;
  land: Rgb;
  landAlpha: number;
  nightAlpha: number;
  data: Rgb;
  bright: Rgb;
  live: Rgb;
  /** 深色主题：光点带光晕 */
  dark: boolean;
}

export interface GlobeRow {
  code: string;
  value: number;
}
export interface GlobeOptions {
  /** contain：整个球在画布里；horizon：只露出上半个弧（登录页背景） */
  fit?: "contain" | "horizon";
  /** 空闲时自转（度 / 秒，向东为正）；0 不转 */
  spin?: number;
  /** 能不能拖、悬停、点 */
  interactive?: boolean;
  /** 一开始正对着的经纬度（有访客数据后会转向访客最多的国家） */
  view?: [number, number];
  /** 指到（或键盘选中）的国家变了；x、y 是它在宿主元素里的位置 */
  onhover?: (code: string | null, x: number, y: number) => void;
  onpick?: (code: string) => void;
  /** 当前高亮国家的位置（每帧），不在正面时 visible 为 false */
  onanchor?: (x: number, y: number, visible: boolean) => void;
}
export interface GlobeHandle {
  /** 时间范围内各国的访客数 */
  setRows(rows: GlobeRow[]): void;
  /** 此刻各国的在线人数 */
  setLive(rows: GlobeRow[]): void;
  /** 高亮一个国家（键盘或列表选中），并转到它；null 取消 */
  setActive(code: string | null, turn?: boolean): void;
  /** 转到一个国家正对着 */
  face(code: string): void;
  restyle(): void;
  destroy(): void;
}

const LEVELS = 5;
const ALPHAS = 8;

export function createGlobe(canvas: HTMLCanvasElement, host: HTMLElement, opts: GlobeOptions = {}): GlobeHandle | null {
  const ctx = canvas.getContext("2d");
  if (!ctx || !mask()) return null;
  const fit = opts.fit ?? "contain";
  const interactive = opts.interactive ?? true;
  const still = typeof matchMedia !== "undefined" && matchMedia("(prefers-reduced-motion: reduce)").matches;
  const spin = still ? 0 : (opts.spin ?? 4);

  let w = 0;
  let h = 0;
  let dpr = 1;
  let cx = 0;
  let cy = 0;
  let R = 0;
  let dots: Land | null = null;
  let lit = new Float32Array(0);
  let litAt = 0;
  let sun: Vec = [0, 0, 1];
  let colors: Colors | null = null;

  // 视角：正对着的经纬度
  let lon = opts.view?.[0] ?? 100;
  let lat = opts.view?.[1] ?? 22;
  let target: [number, number] | null = null;
  let velLon = 0;
  let velLat = 0;
  let drag: { x: number; y: number; lon: number; lat: number; moved: boolean; t: number } | null = null;
  let idleAt = 0;
  let hoverCode: string | null = null;
  let activeCode: string | null = null;
  let pointerIn = false;

  let level = new Uint8Array(COUNTRIES.length);
  let liveRows: { v: Vec; size: number; phase: number }[] = [];
  let lastRows: GlobeRow[] = [];
  let lastLive: GlobeRow[] = [];

  // 每帧：看得见的点在屏幕上的位置，给悬停用
  let vx = new Float32Array(0);
  let vy = new Float32Array(0);
  let vc = new Int16Array(0);
  let vn = 0;
  // 计数排序用
  let key = new Uint8Array(0);
  let rad = new Float32Array(0);
  let order = new Uint32Array(0);
  const counts = new Uint32Array((LEVELS + 2) * ALPHAS + 1);

  let frame = 0;
  let last = 0;
  let visible = true;
  let destroyed = false;
  let dirty = true;

  const indexOf = new Map(COUNTRIES.map(([code], i) => [code, i]));

  function restyle() {
    const s = getComputedStyle(host);
    const v = (name: string) => s.getPropertyValue(name).trim();
    const n = (name: string, d: number) => {
      const x = Number(v(name));
      return Number.isFinite(x) && v(name) !== "" ? x : d;
    };
    const c = (name: string, d: Rgb) => parseColor(ctx!, v(name), d);
    const sea = c("--globe-sea", [233, 238, 248]);
    colors = {
      seaLit: c("--globe-sea-lit", [255, 255, 255]),
      sea,
      shade: c("--globe-sea-shade", [207, 216, 234]),
      air: c("--globe-air", [91, 124, 240]),
      airAlpha: n("--globe-air-alpha", 0.22),
      land: c("--globe-land", [142, 153, 179]),
      landAlpha: n("--globe-land-alpha", 0.55),
      nightAlpha: n("--globe-night-alpha", 0.7),
      data: c("--globe-data", [62, 99, 221]),
      bright: c("--globe-data-bright", [0, 162, 199]),
      live: c("--live", [247, 107, 21]),
      dark: sea[0] + sea[1] + sea[2] < 300,
    };
    dirty = true;
    kick();
  }

  function relight(now: number) {
    if (!dots) return;
    sun = subsolar(new Date());
    if (lit.length !== dots.count) lit = new Float32Array(dots.count);
    for (let i = 0; i < dots.count; i++) lit[i] = smooth(-0.18, 0.22, dots.x[i] * sun[0] + dots.y[i] * sun[1] + dots.z[i] * sun[2]);
    litAt = now;
  }

  function resize() {
    const bw = host.clientWidth;
    const bh = host.clientHeight;
    if (!bw || !bh) return;
    w = bw;
    h = bh;
    dpr = Math.min(2, window.devicePixelRatio || 1);
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;
    if (fit === "horizon") {
      // 地平线在画布高度的六成处，上面留给夜空和大气的光
      R = Math.max(w * 0.66, h * 0.82);
      cx = w / 2;
      cy = R + h * 0.6;
    } else {
      R = (Math.min(w, h) / 2) * 0.86;
      cx = w / 2;
      cy = h / 2;
    }
    // 点距约 3.2px：点数跟着球的大小走，取整到 2000 一档，缩放窗口时不必每次重算
    const spacing = fit === "horizon" ? 6.4 : 3.2;
    const total = Math.max(6000, Math.min(fit === "horizon" ? 110000 : 60000, Math.round((4 * Math.PI * R * R) / (spacing * spacing) / 2000) * 2000));
    const next = land(total);
    if (next && next !== dots) {
      dots = next;
      vx = new Float32Array(dots.count);
      vy = new Float32Array(dots.count);
      vc = new Int16Array(dots.count);
      key = new Uint8Array(dots.count);
      rad = new Float32Array(dots.count);
      order = new Uint32Array(dots.count);
      relight(performance.now());
      applyLive();
    }
    dirty = true;
    kick();
  }

  function applyRows() {
    level = new Uint8Array(COUNTRIES.length);
    let max = 0;
    for (const r of lastRows) if (indexOf.has(r.code)) max = Math.max(max, r.value);
    if (max > 0) {
      for (const r of lastRows) {
        const i = indexOf.get(r.code);
        // 开方后分档：访客数相差很大，线性分档时除了第一名都是最浅一档
        if (i !== undefined && r.value > 0) level[i] = 1 + Math.min(LEVELS - 1, Math.floor(Math.sqrt(r.value / max) * LEVELS));
      }
    }
    dirty = true;
    kick();
  }

  function applyLive() {
    if (!dots) return;
    let max = 0;
    for (const r of lastLive) max = Math.max(max, r.value);
    liveRows = [];
    lastLive.forEach((r, i) => {
      const v = dots!.centre.get(r.code);
      if (v && r.value > 0) liveRows.push({ v, size: Math.sqrt(r.value / max), phase: (i * 0.37) % 1 });
    });
    dirty = true;
    kick();
  }

  const rot = (x: number, y: number, z: number, sl: number, cl: number, sp: number, cp: number): Vec => {
    const x1 = x * cl - z * sl;
    const z1 = x * sl + z * cl;
    return [x1, y * cp - z1 * sp, y * sp + z1 * cp];
  };

  function draw(now: number) {
    const c = colors;
    if (!c || !dots || !w) return;
    const sl = Math.sin(lon * RAD);
    const cl = Math.cos(lon * RAD);
    const sp = Math.sin(lat * RAD);
    const cp = Math.cos(lat * RAD);
    ctx!.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx!.clearRect(0, 0, w, h);

    const s = rot(sun[0], sun[1], sun[2], sl, cl, sp, cp);
    const facing = clamp01(0.5 + s[2] * 0.6);

    // 大气：球外一圈光，向着太阳的一边亮一些
    const air = ctx!.createRadialGradient(cx + s[0] * R * 0.08, cy - s[1] * R * 0.08, R * 0.9, cx, cy, R * 1.24);
    air.addColorStop(0, rgba(c.air, c.airAlpha));
    air.addColorStop(0.35, rgba(c.air, c.airAlpha * 0.45));
    air.addColorStop(1, rgba(c.air, 0));
    ctx!.fillStyle = air;
    ctx!.beginPath();
    ctx!.arc(cx, cy, R * 1.24, 0, TAU);
    ctx!.fill();

    // 海面：底色，向阳处的亮面，背阳处与边缘的暗面
    ctx!.save();
    ctx!.beginPath();
    ctx!.arc(cx, cy, R, 0, TAU);
    ctx!.clip();
    ctx!.fillStyle = rgba(c.sea, 1);
    ctx!.fillRect(cx - R, cy - R, R * 2, R * 2);
    const lx = cx + s[0] * R * 0.62;
    const ly = cy - s[1] * R * 0.62;
    const day = ctx!.createRadialGradient(lx, ly, 0, lx, ly, R * 1.25);
    day.addColorStop(0, rgba(c.seaLit, 0.95 * facing));
    day.addColorStop(0.55, rgba(c.seaLit, 0.35 * facing));
    day.addColorStop(1, rgba(c.seaLit, 0));
    ctx!.fillStyle = day;
    ctx!.fillRect(cx - R, cy - R, R * 2, R * 2);
    const nx = cx - s[0] * R * 0.9;
    const ny = cy + s[1] * R * 0.9;
    const night = ctx!.createRadialGradient(nx, ny, 0, nx, ny, R * 1.5);
    night.addColorStop(0, rgba(c.shade, 0.9 * (1 - facing * 0.5)));
    night.addColorStop(0.6, rgba(c.shade, 0.3 * (1 - facing * 0.5)));
    night.addColorStop(1, rgba(c.shade, 0));
    ctx!.fillStyle = night;
    ctx!.fillRect(cx - R, cy - R, R * 2, R * 2);
    const limb = ctx!.createRadialGradient(cx, cy, R * 0.72, cx, cy, R);
    limb.addColorStop(0, rgba(c.shade, 0));
    limb.addColorStop(1, rgba(c.shade, 0.55));
    ctx!.fillStyle = limb;
    ctx!.fillRect(cx - R, cy - R, R * 2, R * 2);
    ctx!.restore();

    // 陆地的点：先算位置与分组，再按组一次画完
    const base = fit === "horizon" ? 1.7 : 1.02;
    const hi = activeCode ?? hoverCode;
    const hiIndex = hi ? (indexOf.get(hi) ?? -1) : -1;
    counts.fill(0);
    vn = 0;
    const n = dots.count;
    for (let i = 0; i < n; i++) {
      const x1 = dots.x[i] * cl - dots.z[i] * sl;
      const z1 = dots.x[i] * sl + dots.z[i] * cl;
      const z2 = dots.y[i] * sp + z1 * cp;
      if (z2 <= 0.02) continue;
      const y2 = dots.y[i] * cp - z1 * sp;
      const sy = cy - y2 * R;
      // 画布外的不画（地平线模式下大半个球在画布下面）
      if (sy > h + 3 || sy < -3) continue;
      const ci = dots.c[i];
      const lv = ci === hiIndex ? LEVELS + 1 : level[ci];
      const edge = smooth(0.02, 0.32, z2);
      let a: number;
      let r = base * (0.5 + 0.5 * z2);
      if (lv === 0) a = c.landAlpha * edge * (c.nightAlpha + (1 - c.nightAlpha) * lit[i]);
      else if (lv > LEVELS) {
        a = edge;
        r *= 1.5;
      } else {
        const t = (lv - 1) / (LEVELS - 1);
        a = (0.5 + 0.5 * t) * edge;
        r *= 1.12 + 0.3 * t;
      }
      const k = lv * ALPHAS + Math.min(ALPHAS - 1, Math.floor(a * ALPHAS));
      vx[vn] = cx + x1 * R;
      vy[vn] = sy;
      vc[vn] = ci;
      key[vn] = k;
      rad[vn] = r;
      counts[k + 1]++;
      vn++;
    }
    for (let k = 1; k < counts.length; k++) counts[k] += counts[k - 1];
    for (let i = 0; i < vn; i++) order[counts[key[i]]++] = i;
    let at = 0;
    for (let lv = 0; lv <= LEVELS + 1; lv++) {
      const t = lv === 0 ? 0 : (Math.min(lv, LEVELS) - 1) / (LEVELS - 1);
      const col = lv === 0 ? c.land : lv > LEVELS ? (c.dark ? mix(c.bright, [255, 255, 255], 0.35) : mix(c.data, [0, 0, 0], 0.25)) : mix(c.data, c.bright, t);
      for (let ab = 0; ab < ALPHAS; ab++) {
        const end = counts[lv * ALPHAS + ab];
        if (end === at) continue;
        ctx!.fillStyle = rgba(col, (ab + 0.75) / ALPHAS);
        ctx!.beginPath();
        for (; at < end; at++) {
          const i = order[at];
          const r = rad[i];
          if (r < 0.7) ctx!.rect(vx[i] - r, vy[i] - r, r * 2, r * 2);
          else {
            ctx!.moveTo(vx[i] + r, vy[i]);
            ctx!.arc(vx[i], vy[i], r, 0, TAU);
          }
        }
        ctx!.fill();
      }
    }

    // 此刻在线：一圈圈扩散的光
    if (liveRows.length) {
      const period = 2400;
      for (const p of liveRows) {
        const v = rot(p.v[0], p.v[1], p.v[2], sl, cl, sp, cp);
        if (v[2] <= 0.06) continue;
        const px = cx + v[0] * R;
        const py = cy - v[1] * R;
        const edge = smooth(0.06, 0.4, v[2]);
        const core = (2.1 + 2.4 * p.size) * (fit === "horizon" ? 1.3 : 1);
        const reach = core + 9 + 13 * p.size;
        for (let ring = 0; ring < 2; ring++) {
          const t = still ? 0.45 + ring * 0.3 : (now / period + p.phase + ring * 0.5) % 1;
          const rr = core + (reach - core) * t;
          ctx!.strokeStyle = rgba(c.live, 0.55 * (1 - t) * (1 - t) * edge);
          ctx!.lineWidth = 1.2;
          ctx!.beginPath();
          ctx!.arc(px, py, rr, 0, TAU);
          ctx!.stroke();
        }
        const glow = ctx!.createRadialGradient(px, py, 0, px, py, core * 3.2);
        glow.addColorStop(0, rgba(c.live, (c.dark ? 0.55 : 0.32) * edge));
        glow.addColorStop(1, rgba(c.live, 0));
        ctx!.fillStyle = glow;
        ctx!.beginPath();
        ctx!.arc(px, py, core * 3.2, 0, TAU);
        ctx!.fill();
        ctx!.fillStyle = rgba(c.dark ? mix(c.live, [255, 255, 255], 0.35) : c.live, edge);
        ctx!.beginPath();
        ctx!.arc(px, py, core, 0, TAU);
        ctx!.fill();
      }
    }

    if (opts.onanchor && hi) {
      const v0 = dots.centre.get(hi);
      if (v0) {
        const v = rot(v0[0], v0[1], v0[2], sl, cl, sp, cp);
        opts.onanchor(cx + v[0] * R, cy - v[1] * R, v[2] > 0.1);
      }
    }
  }

  function tick(now: number) {
    frame = 0;
    if (destroyed) return;
    const dt = Math.min(0.05, (now - last) / 1000 || 0);
    last = now;
    if (now - litAt > 60000) relight(now);
    let moving = false;
    if (drag) moving = true;
    else if (target) {
      const dl = ((((target[0] - lon) % 360) + 540) % 360) - 180;
      const dp = target[1] - lat;
      const k = still ? 1 : Math.min(1, dt * 5);
      lon += dl * k;
      lat += dp * k;
      if (Math.abs(dl) < 0.05 && Math.abs(dp) < 0.05) target = null;
      moving = true;
    } else if (Math.abs(velLon) > 0.5 || Math.abs(velLat) > 0.5) {
      lon += velLon * dt;
      lat = Math.max(-55, Math.min(62, lat + velLat * dt));
      const drop = Math.exp(-dt * 2.6);
      velLon *= drop;
      velLat *= drop;
      moving = true;
    } else if (spin && !activeCode && !(pointerIn && hoverCode) && now - idleAt > 1600) {
      lon += spin * dt;
      moving = true;
    } else if (spin && !activeCode) moving = true;
    const pulsing = liveRows.length > 0 && !still;
    if (moving || pulsing || dirty) {
      dirty = false;
      draw(now);
    }
    if (visible && (moving || pulsing)) frame = requestAnimationFrame(tick);
  }
  function kick() {
    if (!frame && !destroyed && visible) {
      last = performance.now();
      frame = requestAnimationFrame(tick);
    }
  }

  // ---------- 指针 ----------
  function pick(x: number, y: number, reach: number): string | null {
    let best = -1;
    let dist = reach * reach;
    for (let i = 0; i < vn; i++) {
      const dx = vx[i] - x;
      const dy = vy[i] - y;
      const d = dx * dx + dy * dy;
      if (d < dist) {
        dist = d;
        best = i;
      }
    }
    return best < 0 ? null : COUNTRIES[vc[best]][0];
  }
  const local = (e: PointerEvent) => {
    const b = canvas.getBoundingClientRect();
    return [e.clientX - b.left, e.clientY - b.top] as const;
  };
  function setHover(code: string | null, x: number, y: number) {
    if (code !== hoverCode) {
      hoverCode = code;
      dirty = true;
      kick();
    }
    opts.onhover?.(code, x, y);
  }
  function down(e: PointerEvent) {
    if (e.button !== 0) return;
    const [x, y] = local(e);
    if (Math.hypot(x - cx, y - cy) > R * 1.05) return;
    drag = { x: e.clientX, y: e.clientY, lon, lat, moved: false, t: performance.now() };
    target = null;
    velLon = velLat = 0;
    canvas.setPointerCapture(e.pointerId);
    kick();
  }
  function move(e: PointerEvent) {
    const [x, y] = local(e);
    if (drag) {
      const dx = e.clientX - drag.x;
      const dy = e.clientY - drag.y;
      if (!drag.moved && Math.hypot(dx, dy) < 4) return;
      drag.moved = true;
      const k = 180 / (Math.PI * R);
      const nl = drag.lon - dx * k;
      const np = Math.max(-55, Math.min(62, drag.lat + dy * k));
      const now = performance.now();
      const dt = Math.max(0.008, (now - drag.t) / 1000);
      velLon = velLon * 0.5 + ((nl - lon) / dt) * 0.5;
      velLat = velLat * 0.5 + ((np - lat) / dt) * 0.5;
      drag.t = now;
      lon = nl;
      lat = np;
      dirty = true;
      if (hoverCode) setHover(null, x, y);
      return;
    }
    setHover(pick(x, y, e.pointerType === "touch" ? 18 : 9), x, y);
  }
  function up(e: PointerEvent) {
    if (!drag) return;
    const was = drag;
    drag = null;
    idleAt = performance.now();
    if (canvas.hasPointerCapture(e.pointerId)) canvas.releasePointerCapture(e.pointerId);
    if (!was.moved) {
      velLon = velLat = 0;
      const [x, y] = local(e);
      const code = pick(x, y, e.pointerType === "touch" ? 18 : 9);
      if (code) opts.onpick?.(code);
    } else if (still) velLon = velLat = 0;
    kick();
  }
  function enter() {
    pointerIn = true;
  }
  function leave() {
    pointerIn = false;
    idleAt = performance.now();
    if (!drag) setHover(null, 0, 0);
    kick();
  }
  if (interactive) {
    canvas.addEventListener("pointerdown", down);
    canvas.addEventListener("pointermove", move);
    canvas.addEventListener("pointerup", up);
    canvas.addEventListener("pointercancel", up);
    canvas.addEventListener("pointerenter", enter);
    canvas.addEventListener("pointerleave", leave);
  }

  const ro = new ResizeObserver(resize);
  ro.observe(host);
  const io = new IntersectionObserver(([e]) => {
    visible = e.isIntersecting && !document.hidden;
    if (visible) kick();
  });
  io.observe(host);
  const onVisibility = () => {
    visible = !document.hidden;
    if (visible) kick();
  };
  document.addEventListener("visibilitychange", onVisibility);

  restyle();
  resize();

  function face(code: string) {
    const v = dots?.centre.get(code);
    if (!v) return;
    target = [Math.atan2(v[0], v[2]) / RAD, Math.max(-50, Math.min(58, Math.asin(v[1]) / RAD))];
    velLon = velLat = 0;
    kick();
  }

  return {
    setRows(rows) {
      const first = !lastRows.length;
      lastRows = rows;
      applyRows();
      // 第一次拿到数据时，把访客最多的国家转到偏右一点的位置，转起来它正好经过正面
      if (first && rows.length && dots) {
        const top = [...rows].sort((a, b) => b.value - a.value).find((r) => dots!.centre.has(r.code));
        const v = top && dots.centre.get(top.code);
        if (v) {
          lon = Math.atan2(v[0], v[2]) / RAD - (spin ? 18 : 0);
          lat = Math.max(8, Math.min(34, Math.asin(v[1]) / RAD));
        }
      }
    },
    setLive(rows) {
      lastLive = rows;
      applyLive();
    },
    setActive(code, turn = true) {
      activeCode = code;
      idleAt = performance.now();
      if (code && turn) face(code);
      dirty = true;
      kick();
    },
    face,
    restyle,
    destroy() {
      destroyed = true;
      cancelAnimationFrame(frame);
      ro.disconnect();
      io.disconnect();
      document.removeEventListener("visibilitychange", onVisibility);
      if (interactive) {
        canvas.removeEventListener("pointerdown", down);
        canvas.removeEventListener("pointermove", move);
        canvas.removeEventListener("pointerup", up);
        canvas.removeEventListener("pointercancel", up);
        canvas.removeEventListener("pointerenter", enter);
        canvas.removeEventListener("pointerleave", leave);
      }
    },
  };
}
