// 数据删除的纯逻辑：按站点时区的「今天」、日期范围校验（与服务端规则一致）、已删除站点距永久删除的天数。

/** 某个时刻在 IANA 时区里的日期 YYYY-MM-DD；时区无效时按 UTC。 */
export function ymdInZone(at: Date, timeZone: string): string {
  const fmt = (tz: string) =>
    new Intl.DateTimeFormat("en-CA", { timeZone: tz, year: "numeric", month: "2-digit", day: "2-digit" }).format(at);
  try {
    return fmt(timeZone);
  } catch {
    return fmt("UTC");
  }
}

/** 一次最多删除的天数（含两端），与服务端相同（原始数据保留 731 天）。 */
export const MAX_RANGE_DAYS = 731;

/** 严格的 YYYY-MM-DD 且是真实存在的日期，返回 UTC 零点的毫秒数；否则 null。 */
function dayMs(ymd: string): number | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(ymd);
  if (!m) return null;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  const ms = Date.UTC(y, mo - 1, d);
  const back = new Date(ms);
  return back.getUTCFullYear() === y && back.getUTCMonth() === mo - 1 && back.getUTCDate() === d ? ms : null;
}

export type RangeProblem = "invalid" | "order" | "future" | "tooLong";

/** 校验日期范围（站点本地日期）；任一端为空时不报错（还没填完）。`today` 是站点时区的今天。 */
export function rangeError(from: string, to: string, today: string): RangeProblem | null {
  if (!from || !to) return null;
  const a = dayMs(from);
  const b = dayMs(to);
  if (a === null || b === null) return "invalid";
  if (a > b) return "order";
  if (to > today) return "future";
  if ((b - a) / 86_400_000 + 1 > MAX_RANGE_DAYS) return "tooLong";
  return null;
}

/** 距永久删除还剩几天：按浏览器本地日历日相减，同一天（或已过、无效）为 0，显示「今天永久删除」。 */
export function daysLeft(purgeAt: string, now: Date): number {
  const p = new Date(purgeAt);
  if (Number.isNaN(p.getTime())) return 0;
  const a = Date.UTC(now.getFullYear(), now.getMonth(), now.getDate());
  const b = Date.UTC(p.getFullYear(), p.getMonth(), p.getDate());
  return Math.max(0, Math.round((b - a) / 86_400_000));
}
