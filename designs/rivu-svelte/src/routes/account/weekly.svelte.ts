// 邮件订阅（周报、月报）的模拟状态：会话内保持，刷新回到初始值。
// 邮箱确认是账号级的，只做一次，各种邮件共用；确认之后每个站点每种邮件一个开关。真实实现里这些来自服务端。

export type WeeklyEmail = "unconfirmed" | "pending" | "confirmed";

/** 确认邮件一分钟只能请求一次。 */
export const WEEKLY_WAIT_MS = 60_000;

export const weekly = $state({
  email: "unconfirmed" as WeeklyEmail,
  /** 上次请求确认邮件的时间，用来算还要等多久 */
  requestedAt: 0,
  /** 上次的确认邮件是否发出去了：SMTP 拒收时服务端回错误，状态不变，页面留一条提示 */
  delivered: true,
  /** 站点 id → 是否订阅周报 */
  sites: {} as Record<number, boolean>,
  /** 站点 id → 是否订阅月报（与周报互不影响） */
  monthly: {} as Record<number, boolean>,
});
