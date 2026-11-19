// 周报订阅卡片的纯逻辑（node --test）：卡片可见性、发送间隔倒计时、接口错误到界面状态的映射。

export type WeeklyEmailStatus = "unconfirmed" | "pending" | "confirmed";

/** 确认邮件每个账号每分钟最多一封。 */
export const WEEKLY_WAIT_SECONDS = 60;

/** 服务端不给 Retry-After 时的兜底，也是上限的参照：等待最长不超过一小时。 */
const MAX_WAIT_SECONDS = 3600;

interface ErrorLike {
  status: number;
  code: string;
  retryAfter: number | null;
}

/**
 * 还要等几秒才能再请求确认邮件。`requestedAt` 是开始等待的时刻（毫秒，0 表示没有在等）。
 * 向上取整：剩 0.2 秒也显示 1，到点才变 0。
 */
export function waitLeft(requestedAt: number, now: number, waitSeconds = WEEKLY_WAIT_SECONDS): number {
  if (!requestedAt) return 0;
  return Math.max(0, Math.ceil((requestedAt + waitSeconds * 1000 - now) / 1000));
}

/** 429 的等待秒数：优先用 Retry-After，缺失或不合理时按一分钟。 */
export function retryAfterSeconds(retryAfter: number | null): number {
  if (retryAfter === null || !Number.isFinite(retryAfter) || retryAfter <= 0) return WEEKLY_WAIT_SECONDS;
  return Math.min(Math.ceil(retryAfter), MAX_WAIT_SECONDS);
}

/**
 * 卡片是否显示。还没拿到数据（加载中或失败）时显示，由卡片自己画骨架与重试；
 * 拿到之后：实例没配置邮件且邮箱未确认时整张卡片不显示（和「忘记密码」入口一样），已确认的仍显示。
 */
export function cardVisible(d: { available: boolean; email_status: WeeklyEmailStatus } | null): boolean {
  return d === null || d.available || d.email_status === "confirmed";
}

export type SendOutcome =
  | { kind: "wait"; seconds: number }
  | { kind: "mail_failed" }
  | { kind: "unavailable" }
  | { kind: "error" };

/** 请求确认邮件失败后界面怎么走（401 由调用方先处理）。 */
export function classifySendError(e: ErrorLike): SendOutcome {
  if (e.status === 429) return { kind: "wait", seconds: retryAfterSeconds(e.retryAfter) };
  if (e.status === 502 && e.code === "mail_failed") return { kind: "mail_failed" };
  if (e.status === 503 && e.code === "mail_unavailable") return { kind: "unavailable" };
  return { kind: "error" };
}

export type ToggleOutcome = "unconfirmed" | "gone" | "error";

/** 开关请求失败后界面怎么走：邮箱已不是确认状态、站点没了或没权限，其余当作一般失败。 */
export function classifyToggleError(e: Pick<ErrorLike, "status" | "code">): ToggleOutcome {
  if (e.status === 409 && e.code === "email_unconfirmed") return "unconfirmed";
  if (e.status === 404) return "gone";
  return "error";
}

/** 待确认时用户多半在别的标签页或设备上点链接：回到这个页面时要重新取一次状态。 */
export function shouldRefreshOnReturn(status: WeeklyEmailStatus | undefined, visible: boolean): boolean {
  return status === "pending" && visible;
}
