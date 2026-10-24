import { extend } from "#lib/i18n.ts";

extend({
  "ac.title": ["账号与安全", "Account & security"],
  "ac.profile": ["个人资料", "Profile"],
  "ac.name": ["名字", "Name"],
  "ac.email": ["邮箱", "Email"],
  "ac.sessions": ["登录会话", "Sessions"],
  "ac.col.device": ["设备", "Device"],
  "ac.col.used": ["最近使用", "Last used"],
  "ac.col.expires": ["过期时间", "Expires"],
  "ac.current": ["当前", "Current"],
  "ac.unknownDevice": ["未知设备", "Unknown device"],
  "ac.signOut": ["退出", "Sign out"],
  "ac.signOutAll": ["退出所有设备", "Sign out everywhere"],
  "ac.signOutAll.title": ["退出所有设备？", "Sign out everywhere?"],
  "ac.signOutAll.body": ["包括当前设备在内，所有设备都需要重新登录。", "Every device, including this one, will have to sign in again."],
  "ac.signedOut": ["已退出 {device}", "Signed out {device}"],
  "ac.sessions.error": ["会话列表加载失败", "Couldn't load sessions"],
  "ac.failed": ["操作失败，请稍后重试", "Something went wrong. Try again shortly"],
});
