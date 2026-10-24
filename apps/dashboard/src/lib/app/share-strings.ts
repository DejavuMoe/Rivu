// 共享看板与公开看板的文案。
import { extend } from "../i18n";

extend({
  "share.brand": ["共享看板", "Shared dashboard"],
  "share.brandPublic": ["公开看板", "Public dashboard"],
  "share.theme": ["切换主题", "Switch theme"],

  "share.notFound.title": ["找不到这个看板", "Dashboard not found"],

  "share.lock.title": ["输入密码查看这个看板", "Enter the password to view this dashboard"],
  "share.lock.label": ["密码", "Password"],
  "share.lock.submit": ["查看看板", "View dashboard"],
  "share.lock.wrong": ["密码不对，请再试一次", "Wrong password. Try again"],
  "share.lock.tooMany": ["尝试次数过多，请在 {s} 秒后再试", "Too many attempts. Try again in {s} seconds"],
  "share.lock.failed": ["验证失败，请稍后重试", "Couldn't verify. Try again shortly"],
});
