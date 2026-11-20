// 看板的时间范围：预设、自定义日期、上一周期 / 下一周期，以及它们在 URL 里的写法。
// 日期一律是站点本地日（YYYY-MM-DD，含两端）；只有「过去 24 小时」平移出来的范围是时刻区间（YYYY-MM-DDTHH:mm，含起点、不含终点）。
// 换算规则与查询内核一致（system-design.md §6.7）；正式实现把 custom 换成 QuerySpec 的 date_range: [from, to]。
// 纯函数，不依赖 Svelte，正式实现可以直接搬进 apps/dashboard/src/lib/dashboard 并用 node --test 测。

import type { Lang } from "../../lib/state.svelte";

export type PresetKey =
  | "realtime"
  | "day"
  | "yesterday"
  | "24h"
  | "7d"
  | "30d"
  | "month"
  | "last_month"
  | "6mo"
  | "12mo"
  | "year"
  | "all";
export type RangeKey = PresetKey | "custom";

/** URL 里的范围：预设只有 range；自定义是 range=custom&from=…&to=…。 */
export interface RangeParam {
  range: RangeKey;
  from?: string;
  to?: string;
}

export interface Period {
  /** days：整本地日区间，from、to 是两端的本地日零点（含 to 这一天）；instant：时刻区间 [from, to) */
  kind: "days" | "instant";
  from: Date;
  to: Date;
  /** 按自然月对齐时的月数（从某月 1 日到某月最后一天，或「本月」「过去 6 个月」这类预设），否则 0 */
  months: number;
}

const DAY = 864e5;
/** 查询内核支持的日期范围（§6.7）。 */
const MIN_DAY = "1970-01-03";
const MAX_DAY = "2100-12-31";

// ---------- 日期工具（本地日，不受夏令时影响：只用年月日构造） ----------

const pad = (n: number) => String(n).padStart(2, "0");

export function ymd(d: Date): string {
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

function ymdhm(d: Date): string {
  return `${ymd(d)}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** 解析 YYYY-MM-DD；格式不对或日期不存在（2 月 30 日）返回 null。 */
export function parseYmd(s: string | null | undefined): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(s ?? "");
  if (!m) return null;
  const d = new Date(+m[1], +m[2] - 1, +m[3]);
  return d.getMonth() === +m[2] - 1 && d.getDate() === +m[3] ? d : null;
}

function parseYmdhm(s: string | null | undefined): Date | null {
  const m = /^(\d{4}-\d{2}-\d{2})T(\d{2}):(\d{2})$/.exec(s ?? "");
  const day = m && parseYmd(m[1]);
  if (!m || !day || +m[2] > 23 || +m[3] > 59) return null;
  day.setHours(+m[2], +m[3]);
  return day;
}

const startOfDay = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate());
const addDays = (d: Date, n: number) => new Date(d.getFullYear(), d.getMonth(), d.getDate() + n);
const addMonths = (d: Date, n: number) => new Date(d.getFullYear(), d.getMonth() + n, 1);
const lastOfMonth = (d: Date) => new Date(d.getFullYear(), d.getMonth() + 1, 0);
/** 两个本地日之间相差的天数（按日历算，夏令时的 23、25 小时日也是 1 天）。 */
const dayDiff = (a: Date, b: Date) => Math.round((startOfDay(b).getTime() - startOfDay(a).getTime()) / DAY);
const sameDay = (a: Date, b: Date) => dayDiff(a, b) === 0;

/** 整本地日区间的天数（含两端）。 */
export function dayCount(p: Period): number {
  return dayDiff(p.from, p.to) + 1;
}

/** 从 from 所在月到 to 所在月一共几个月（from 必须是 1 日，to 必须是月末），不是整月返回 0。 */
function wholeMonths(from: Date, to: Date): number {
  if (from.getDate() !== 1 || !sameDay(to, lastOfMonth(to))) return 0;
  return (to.getFullYear() - from.getFullYear()) * 12 + to.getMonth() - from.getMonth() + 1;
}

// ---------- 解析 ----------

export const PRESETS: PresetKey[] = ["realtime", "day", "yesterday", "24h", "7d", "30d", "month", "last_month", "6mo", "12mo", "year", "all"];
const PRESET_SET = new Set<string>(PRESETS);

/** 读 URL；不认识或不合法的范围回到默认的「过去 30 天」。 */
export function readRange(q: { get(k: string): string | null }, now: Date): RangeParam {
  const r = q.get("range");
  if (r && PRESET_SET.has(r)) return { range: r as PresetKey };
  if (r === "custom") {
    const from = q.get("from") ?? "";
    const to = q.get("to") ?? "";
    if (validCustom(from, to, now)) return { range: "custom", from, to };
  }
  return { range: "30d" };
}

/** 写 URL：返回 setQuery 要的补丁（默认范围不写）。 */
export function rangeQuery(p: RangeParam): Record<string, string | null> {
  if (p.range !== "custom") return { range: p.range === "30d" ? null : p.range, from: null, to: null };
  return { range: "custom", from: p.from ?? null, to: p.to ?? null };
}

/** 自定义范围合法：同为日期或同为时刻；起点不晚于终点、不晚于今天；在内核支持的日期范围内。 */
export function validCustom(from: string, to: string, now: Date): boolean {
  const today = startOfDay(now);
  if (from.includes("T") || to.includes("T")) {
    const a = parseYmdhm(from);
    const b = parseYmdhm(to);
    return !!a && !!b && a < b && a <= now && ymd(a) >= MIN_DAY && ymd(b) <= MAX_DAY;
  }
  const a = parseYmd(from);
  const b = parseYmd(to);
  return !!a && !!b && a <= b && a <= today && from >= MIN_DAY && to <= MAX_DAY;
}

/** 范围换算成区间。firstDay：站点最早有数据的本地日（「全部时间」的起点）。 */
export function resolve(p: RangeParam, now: Date, firstDay: Date): Period {
  const today = startOfDay(now);
  const days = (from: Date, to: Date, months = 0): Period => ({ kind: "days", from, to, months });
  const monthsBack = (n: number) => days(addMonths(today, -(n - 1)), today, n);
  switch (p.range) {
    case "realtime":
      return { kind: "instant", from: new Date(now.getTime() - 30 * 60_000), to: now, months: 0 };
    case "24h":
      return { kind: "instant", from: new Date(now.getTime() - DAY), to: now, months: 0 };
    case "day":
      return days(today, today);
    case "yesterday":
      return days(addDays(today, -1), addDays(today, -1));
    case "7d":
      return days(addDays(today, -6), today);
    case "30d":
      return days(addDays(today, -29), today);
    case "month":
      return monthsBack(1);
    case "last_month":
      return days(addMonths(today, -1), lastOfMonth(addMonths(today, -1)), 1);
    case "6mo":
      return monthsBack(6);
    case "12mo":
      return monthsBack(12);
    case "year":
      return days(new Date(today.getFullYear(), 0, 1), today, 12);
    case "all":
      return days(startOfDay(firstDay), today);
    case "custom": {
      if (p.from?.includes("T")) return { kind: "instant", from: parseYmdhm(p.from)!, to: parseYmdhm(p.to)!, months: 0 };
      const from = parseYmd(p.from)!;
      const to = parseYmd(p.to)!;
      return days(from, to, wholeMonths(from, to));
    }
  }
}

// ---------- 上一周期 / 下一周期 ----------

/** 能不能平移：实时和全部时间没有前后。 */
export function canStep(r: RangeKey): boolean {
  return r !== "realtime" && r !== "all";
}

/**
 * 按范围自己的长度前后平移一个周期。按月对齐的范围（本月、上个月、过去 6 个月、整月的自定义范围）按自然月平移；
 * 其余整日区间按天数平移；时刻区间按时长平移。平移后的起点晚于现在（还没开始）时返回 null。
 * 结果和某个预设相同时换成预设（今天往前一步是「昨天」，再往后一步回到「今天」）。
 */
export function step(p: RangeParam, dir: -1 | 1, now: Date, firstDay: Date): RangeParam | null {
  if (!canStep(p.range)) return null;
  const cur = resolve(p, now, firstDay);
  const today = startOfDay(now);
  let next: Period;
  if (cur.kind === "instant") {
    const len = cur.to.getTime() - cur.from.getTime();
    next = { kind: "instant", from: new Date(cur.from.getTime() + dir * len), to: new Date(cur.to.getTime() + dir * len), months: 0 };
    if (next.from >= now) return null;
  } else if (cur.months) {
    const from = addMonths(cur.from, dir * cur.months);
    next = { kind: "days", from, to: lastOfMonth(addMonths(from, cur.months - 1)), months: cur.months };
    if (next.from > today) return null;
  } else {
    const n = dayCount(cur);
    next = { kind: "days", from: addDays(cur.from, dir * n), to: addDays(cur.to, dir * n), months: 0 };
    if (next.from > today) return null;
  }
  if (ymd(next.from) < MIN_DAY) return null;
  return canonical(next, now, firstDay);
}

/** 区间换回 URL 的写法：与某个预设相同就用预设（终点晚于今天的按今天比较，因为查询会把终点封顶到现在）。 */
export function canonical(p: Period, now: Date, firstDay: Date): RangeParam {
  if (p.kind === "instant") {
    const len = p.to.getTime() - p.from.getTime();
    if (len === DAY && Math.abs(p.to.getTime() - now.getTime()) < 60_000) return { range: "24h" };
    return { range: "custom", from: ymdhm(p.from), to: ymdhm(p.to) };
  }
  const today = startOfDay(now);
  const capped = p.to > today ? today : p.to;
  for (const key of ["day", "yesterday", "7d", "30d", "month", "last_month", "6mo", "12mo", "year"] as PresetKey[]) {
    const q = resolve({ range: key }, now, firstDay);
    if (sameDay(q.from, p.from) && sameDay(q.to, capped)) return { range: key };
  }
  return { range: "custom", from: ymd(p.from), to: ymd(p.to) };
}

// ---------- 对比期（与内核 §6.7 一致） ----------

const leapDay = (d: Date) => d.getMonth() === 1 && d.getDate() === 29;

/**
 * previous_period：整日区间向前平移 N 个本地日（终点晚于今天时先裁到今天再数）；时刻区间平移时长。
 * year_over_year：起点减一年（2 月 29 日取 28 日），长度不变。
 */
export function comparePeriod(p: Period, mode: "previous_period" | "year_over_year", now: Date): Period {
  const today = startOfDay(now);
  if (p.kind === "instant") {
    if (mode === "previous_period") {
      const len = p.to.getTime() - p.from.getTime();
      return { kind: "instant", from: new Date(p.from.getTime() - len), to: p.from, months: 0 };
    }
    const back = (d: Date) => new Date(d.getFullYear() - 1, d.getMonth(), leapDay(d) ? 28 : d.getDate(), d.getHours(), d.getMinutes());
    return { kind: "instant", from: back(p.from), to: back(p.to), months: 0 };
  }
  const to = p.to > today ? today : p.to;
  const n = dayDiff(p.from, to) + 1;
  if (mode === "previous_period") return { kind: "days", from: addDays(p.from, -n), to: addDays(p.from, -1), months: 0 };
  const from = new Date(p.from.getFullYear() - 1, p.from.getMonth(), leapDay(p.from) ? 28 : p.from.getDate());
  return { kind: "days", from, to: addDays(from, n - 1), months: 0 };
}

// ---------- 显示 ----------

const MONTHS_EN = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTHS_EN_LONG = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];

function dayText(d: Date, lang: Lang, withYear: boolean): string {
  if (lang === "zh") return `${withYear ? `${d.getFullYear()}年` : ""}${d.getMonth() + 1}月${d.getDate()}日`;
  return `${MONTHS_EN[d.getMonth()]} ${d.getDate()}${withYear ? `, ${d.getFullYear()}` : ""}`;
}

function monthText(d: Date, lang: Lang): string {
  return lang === "zh" ? `${d.getFullYear()}年${d.getMonth() + 1}月` : `${MONTHS_EN_LONG[d.getMonth()]} ${d.getFullYear()}`;
}

/**
 * 区间的文字：单日「10月22日」，整月「2026年8月」，整年「2025年」，其余「10月11日 – 10月20日」；
 * 不在今年的日期带年份。时刻区间带时间。
 */
export function periodText(p: Period, lang: Lang, now: Date): string {
  const thisYear = now.getFullYear();
  const dash = " – ";
  if (p.kind === "instant") {
    const t = (d: Date) => `${dayText(d, lang, d.getFullYear() !== thisYear)} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
    return t(p.from) + dash + t(p.to);
  }
  const today = startOfDay(now);
  if (sameDay(p.from, p.to)) return dayText(p.from, lang, p.from.getFullYear() !== thisYear);
  // 整月：终点是月末，或是今天（本月到今天）
  const monthEnd = sameDay(p.to, lastOfMonth(p.to));
  if (p.months === 12 && p.from.getMonth() === 0 && monthEnd && p.from.getFullYear() === p.to.getFullYear())
    return lang === "zh" ? `${p.from.getFullYear()}年` : String(p.from.getFullYear());
  if (p.months === 1 && (monthEnd || sameDay(p.to, today))) return monthText(p.from, lang);
  if (p.months > 1 && monthEnd) return monthText(p.from, lang) + dash + monthText(p.to, lang);
  const fy = p.from.getFullYear();
  const ty = p.to.getFullYear();
  if (fy !== ty) return dayText(p.from, lang, true) + dash + dayText(p.to, lang, true);
  // 同一年：年份只写一次（中文写在前，英文写在后）；今年不写
  if (fy === thisYear) return dayText(p.from, lang, false) + dash + dayText(p.to, lang, false);
  return lang === "zh"
    ? dayText(p.from, lang, true) + dash + dayText(p.to, lang, false)
    : dayText(p.from, lang, false) + dash + dayText(p.to, lang, true);
}

/** 终点封顶到今天的区间（导出文件名、标签用：未来没有数据）。 */
export function capped(p: Period, now: Date): Period {
  const today = startOfDay(now);
  return p.kind === "days" && p.to > today ? { ...p, to: today } : p;
}
