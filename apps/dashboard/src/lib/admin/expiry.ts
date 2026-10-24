// 共享链接有效期：选项换算成 expires_at（所选日期当天本地时间的末尾，RFC3339）。

export type ExpiryKind = "never" | "7" | "30" | "custom";

/** 本地日期 YYYY-MM-DD。 */
export function localYmd(d: Date): string {
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** 某个本地日期（YYYY-MM-DD）当天 23:59:59 的时刻；格式不对返回 null。 */
export function endOfLocalDay(ymd: string): Date | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(ymd);
  if (!m) return null;
  const [y, mo, d] = [Number(m[1]), Number(m[2]), Number(m[3])];
  const date = new Date(y, mo - 1, d, 23, 59, 59);
  // 排除 2 月 31 日这类被 Date 自动进位的输入
  return date.getFullYear() === y && date.getMonth() === mo - 1 && date.getDate() === d ? date : null;
}

export interface Expiry {
  /** null：永久有效（或输入无效，此时 error 非空） */
  at: string | null;
  error: "invalid" | "past" | null;
}

export function computeExpiry(kind: ExpiryKind, custom: string, now: Date): Expiry {
  if (kind === "never") return { at: null, error: null };
  let day: Date | null;
  if (kind === "custom") day = endOfLocalDay(custom);
  else {
    const t = new Date(now.getFullYear(), now.getMonth(), now.getDate() + Number(kind));
    day = endOfLocalDay(localYmd(t));
  }
  if (!day) return { at: null, error: "invalid" };
  if (day.getTime() <= now.getTime()) return { at: null, error: "past" };
  return { at: day.toISOString(), error: null };
}
