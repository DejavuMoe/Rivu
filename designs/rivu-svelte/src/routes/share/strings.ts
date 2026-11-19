// 共享看板的文案（SharedPage 使用）。
import { extend } from "../../lib/i18n";

extend({
  "share.brand": ["共享看板", "Shared dashboard"],
  "share.brandPublic": ["公开看板", "Public dashboard"],
  "share.theme": ["切换主题", "Switch theme"],

  "share.expired.title": ["这个共享链接已过期", "This shared link has expired"],
  "share.notFound.title": ["找不到这个看板", "Dashboard not found"],

  "share.lock.title": ["输入密码查看这个看板", "Enter the password to view this dashboard"],
  "share.lock.label": ["密码", "Password"],
  "share.lock.submit": ["查看看板", "View dashboard"],
  "share.lock.wrong": ["密码不对，请再试一次", "Wrong password. Try again"],
  "share.lock.tooMany": ["尝试次数过多，请在 {s} 秒后再试", "Too many attempts. Try again in {s} seconds"],
  "share.lock.left": ["还可以尝试 {n} 次", "{n} attempts left"],
});
