// 原型的全局状态：语言、主题、评审开关（角色、数据状态等）。
// 评审开关只存在于原型里，用来在不同状态之间切换给评审者看；正式实现里这些来自服务端。

export type Lang = "zh" | "en";
export type ThemePref = "system" | "light" | "dark";
export type Role = "owner" | "admin" | "editor" | "viewer";
export type DataState = "ready" | "loading" | "empty" | "error" | "busy" | "timeout";

interface Review {
  role: Role;
  dataState: DataState;
  /** 新站点的安装检测结果 */
  install: "waiting" | "received" | "dropped";
  /** 登录页演示的结果 */
  login: "ok" | "invalid" | "rateLimited" | "totp";
  /** 实例是否配置了 SMTP：决定「忘记密码」入口与邀请是否发邮件 */
  mail: boolean;
  /** 账号页的两步验证：未开启、已开启、服务器没配置 RIVU_SECRET_KEY */
  twofa: "off" | "on" | "unavailable";
  setupToken: boolean;
  /** 接受邀请页的状态：加载、无效、新账号、已登录为被邀请人、登录的是别人、有账号但未登录 */
  invite: "loading" | "invalid" | "new" | "existing" | "other" | "signedOut";
  /** 邀请范围：整个组织，或单个站点（访客） */
  inviteScope: "org" | "site";
  /** 站点列表里是否有处于恢复期的已删除站点 */
  deleted: "some" | "none";
}

function load<T>(key: string, fallback: T, storage: "local" | "session" = "local"): T {
  try {
    const raw = (storage === "local" ? localStorage : sessionStorage).getItem(key);
    return raw ? { ...fallback, ...JSON.parse(raw) } : fallback;
  } catch {
    return fallback;
  }
}

function save(key: string, value: unknown, storage: "local" | "session" = "local") {
  try {
    (storage === "local" ? localStorage : sessionStorage).setItem(key, JSON.stringify(value));
  } catch {
    /* 隐私模式等情况下存不了，忽略 */
  }
}

const prefs = load("rivu.proto.prefs", { lang: "zh" as Lang, theme: "system" as ThemePref });

export const app = $state({
  lang: prefs.lang,
  theme: prefs.theme,
  review: load<Review>(
    "rivu.proto.review",
    { role: "owner", dataState: "ready", install: "waiting", login: "ok", mail: true, twofa: "off", setupToken: true, invite: "new", inviteScope: "org", deleted: "some" },
    "session",
  ),
  toasts: [] as { id: number; text: string; tone: "default" | "good" | "bad" }[],
});

export function setLang(lang: Lang) {
  app.lang = lang;
  save("rivu.proto.prefs", { lang: app.lang, theme: app.theme });
}

export function setTheme(theme: ThemePref) {
  app.theme = theme;
  save("rivu.proto.prefs", { lang: app.lang, theme: app.theme });
}

export function setReview(patch: Partial<Review>) {
  Object.assign(app.review, patch);
  save("rivu.proto.review", app.review, "session");
}

let toastSeq = 0;
export function toast(text: string, tone: "default" | "good" | "bad" = "default") {
  const id = ++toastSeq;
  app.toasts.push({ id, text, tone });
  setTimeout(() => {
    const i = app.toasts.findIndex((t) => t.id === id);
    if (i >= 0) app.toasts.splice(i, 1);
  }, 2800);
}

/** 当前角色能否改站点设置（owner / admin）。 */
export function canManage() {
  return app.review.role === "owner" || app.review.role === "admin";
}

/** 当前角色能否写站点内容（目标、分群）与管理共享链接。 */
export function canEdit() {
  return app.review.role !== "viewer";
}
