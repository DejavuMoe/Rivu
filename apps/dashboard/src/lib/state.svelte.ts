// 界面偏好（语言、主题）与轻提示。偏好只存在本机浏览器里，读写失败（隐私模式等）时用默认值。

export type Lang = "zh" | "en";
export type ThemePref = "system" | "light" | "dark";

const PREFS_KEY = "rivu.prefs";

function defaultLang(): Lang {
  return navigator.language.toLowerCase().startsWith("zh") ? "zh" : "en";
}

function loadPrefs(): { lang: Lang; theme: ThemePref } {
  const fallback = { lang: defaultLang(), theme: "system" as ThemePref };
  try {
    const raw = localStorage.getItem(PREFS_KEY);
    if (!raw) return fallback;
    const p = JSON.parse(raw);
    return {
      lang: p.lang === "zh" || p.lang === "en" ? p.lang : fallback.lang,
      theme: ["system", "light", "dark"].includes(p.theme) ? p.theme : fallback.theme,
    };
  } catch {
    return fallback;
  }
}

const prefs = loadPrefs();

export const app = $state({
  lang: prefs.lang,
  theme: prefs.theme,
  toasts: [] as { id: number; text: string; tone: "default" | "good" | "bad" }[],
});

function savePrefs() {
  try {
    localStorage.setItem(PREFS_KEY, JSON.stringify({ lang: app.lang, theme: app.theme }));
  } catch {
    /* 存不了就只在本次会话里生效 */
  }
}

export function setLang(lang: Lang) {
  app.lang = lang;
  savePrefs();
}

export function setTheme(theme: ThemePref) {
  app.theme = theme;
  savePrefs();
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
