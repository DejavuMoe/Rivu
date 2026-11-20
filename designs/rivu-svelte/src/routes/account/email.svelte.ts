// 登录邮箱的模拟状态：会话内保持，刷新回到初始值。真实实现里这些来自服务端。
// 配置了邮件时，改邮箱要先打开发到新邮箱的确认链接（见 routes/auth/EmailChangeConfirmPage.svelte）；没配置时校验密码后立即生效。

import { USER } from "../../lib/data/fixtures";
import { MEMBERS } from "../org/data";

/** 确认邮件一分钟只能请求一次（同周报）。 */
export const EMAIL_WAIT_MS = 60_000;

/** 评审面板「改邮箱」切到待确认时用的新邮箱。 */
export const DEMO_NEW_EMAIL = "lin.xia@northwind.dev";

export const account = $state({
  email: USER.email,
  /** 等待确认的新邮箱；空串表示没有进行中的修改 */
  pending: "",
  /** 上次请求确认邮件的时间，用来算还要等多久 */
  requestedAt: 0,
  /** 上次的确认邮件是否发出去了 */
  delivered: true,
  /** 改过邮箱：其他设备的会话已被服务端撤销，账号页的会话列表只剩当前这一个 */
  othersSignedOut: false,
});

/** 原型里，组织成员用着的邮箱算「已被其他账号使用」。 */
export function emailTaken(email: string): boolean {
  const e = email.trim().toLowerCase();
  return MEMBERS.some((m) => !m.me && m.email.toLowerCase() === e);
}

export function emailValid(email: string): boolean {
  const e = email.trim();
  return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(e) && e.length >= 3 && e.length <= 320;
}
