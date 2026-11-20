// 原型的全局状态：语言、主题、当前组织、评审开关（角色、数据状态等）。
// 评审开关只存在于原型里，用来在不同状态之间切换给评审者看；正式实现里这些来自服务端。

import { ORGS, type Site } from "./data/fixtures";
import { SITES } from "./data/sites.svelte";
import { route } from "./router.svelte";

export type Lang = "zh" | "en";
export type ThemePref = "system" | "light" | "dark";
export type Role = "owner" | "admin" | "editor" | "viewer";
export type DataState = "ready" | "loading" | "empty" | "error" | "busy" | "timeout";

interface Review {
  /** 当前用户在自己所属的所有组织里的角色（只以站点访客身份可见的组织不受它影响） */
  role: Role;
  dataState: DataState;
  /** 应用外壳：会话与站点列表是否加载成功；失败时整页只有「无法连接到服务器」和重试，没有侧栏 */
  shell: "ready" | "failed";
  /** 新站点的安装检测结果 */
  install: "waiting" | "received";
  /** 登录与需要校验密码的操作（登录、找回密码、修改密码、两步验证对话框）演示的结果 */
  login: "ok" | "invalid" | "rateLimited" | "totp" | "network" | "failed";
  /** 登录第二步（验证码）演示的结果；ok 时验证码 000000 演示「不正确」 */
  loginCode: "ok" | "expired" | "unavailable" | "rateLimited" | "failed";
  /** 实例是否配置了 SMTP：决定「忘记密码」入口与邀请是否发邮件 */
  mail: boolean;
  /** 账号页的两步验证：未开启、已开启、服务器没配置 RIVU_SECRET_KEY */
  twofa: "off" | "on" | "unavailable";
  /** 初始化页：服务端配置了初始化令牌（输入 rivu 才对）、没有配置、实例已经初始化过 */
  setup: "token" | "open" | "done";
  /** 接受邀请页的状态：加载、无效、出错、新账号、已登录为被邀请人、登录的是别人、有账号但未登录 */
  invite: "loading" | "invalid" | "failed" | "new" | "existing" | "other" | "signedOut";
  /** 邀请范围：整个组织，或单个站点（访客） */
  inviteScope: "org" | "site";
  /** 站点列表里是否有处于恢复期的已删除站点 */
  deleted: "some" | "none";
  /** 确认邮件（周报邮箱、修改登录邮箱）：正常发出，或 SMTP 拒收（服务端回错误，页面提示重新发送） */
  weeklyMail: "ok" | "fails";
  /** 修改登录邮箱的确认链接在哪里打开：登录着这个账号的浏览器，或没登录的（如手机的邮件 App） */
  emailLink: "signedIn" | "signedOut";
  /** 看板的分群：站点 1、2 有现成的分群，或者所有站点都还没有 */
  segments: "some" | "none";
  /** 图表批注：站点 1、2 有几条，或者都没有，或者已到每站 500 条的上限 */
  notes: "some" | "none" | "full";
  /** 实例加载的地理库：site 按站点给几种（见 settings/store 的 geoCapability），city 城市级（MaxMind GeoLite2 City 或同结构的自定义库），country 国家级（默认的 DB-IP Lite） */
  geoDb: "site" | "city" | "country";
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
  /** 当前组织（侧栏切换）；打开站点页时跟着站点所属的组织走 */
  orgId: "o1",
  review: load<Review>(
    "rivu.proto.review",
    {
      role: "owner",
      dataState: "ready",
      shell: "ready",
      install: "waiting",
      login: "ok",
      loginCode: "ok",
      mail: true,
      twofa: "off",
      setup: "token",
      invite: "new",
      inviteScope: "org",
      deleted: "some",
      weeklyMail: "ok",
      emailLink: "signedIn",
      segments: "some",
      notes: "some",
      geoDb: "site",
    },
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

/** 评审开关「登录结果」对需要校验密码的操作（修改密码、两步验证对话框）的映射。 */
export function passwordResult(): "ok" | "wrong" | "rate" | "failed" {
  const r = app.review.login;
  return r === "invalid" ? "wrong" : r === "rateLimited" ? "rate" : r === "network" || r === "failed" ? "failed" : "ok";
}

// ---------- 权限 ----------
// 界面上显示与否的判断，规则与 apps/dashboard 的 session.svelte.ts 一致：权限看站点，不看全局。
// 不传站点时取页面上下文：路由是 /s/:id/… 时用该站点，否则用当前组织。

/** 路由指向的站点（/s/:id/…）。 */
function routeSite(): Site | undefined {
  const seg = route.current.seg;
  return seg[0] === "s" ? SITES.find((s) => s.id === Number(seg[1])) : undefined;
}

/** 当前用户在组织里的角色：成员取评审面板的「角色」，只以站点访客身份可见的组织为 null。 */
function memberRole(orgId: string): Role | null {
  return ORGS.find((o) => o.id === orgId)?.role ? app.review.role : null;
}

/** 在站点上的有效角色：组织角色，没有就取站点访客角色；两者都没有（没有可见站点）为 null。 */
export function siteRole(site: Site | undefined = routeSite()): Role | null {
  return site ? (memberRole(site.org) ?? site.guestRole ?? null) : memberRole(app.orgId);
}

/** 站点的访问方式，形状同生产的站点（role + access），设置分区的可见性按它判断。 */
export function siteAccess(site: Site | undefined = routeSite()): { role: Role | null; access: "member" | "guest" } {
  const org = site ? site.org : app.orgId;
  return { role: siteRole(site), access: memberRole(org) ? "member" : "guest" };
}

/** 能否写站点内容（目标、分群）与管理共享链接、看接入诊断：editor 及以上（含站点访客 editor）。 */
export function canEdit(site?: Site): boolean {
  const r = siteRole(site);
  return r !== null && r !== "viewer";
}

/** 能否改站点设置：owner / admin。 */
export function canManage(site?: Site): boolean {
  const r = siteRole(site);
  return r === "owner" || r === "admin";
}

/** 能否在组织里添加站点：该组织的 owner / admin（只以站点访客身份可见的组织不行）。 */
export function canAddSite(orgId: string = app.orgId): boolean {
  const r = memberRole(orgId);
  return r === "owner" || r === "admin";
}
