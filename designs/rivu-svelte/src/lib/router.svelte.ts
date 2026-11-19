// 极简 hash 路由：原型要能以单个 HTML 文件打开，所以不用 history 路由。
// 正式实现用 SvelteKit 的文件路由，路径保持一致（去掉 #）。

import { tick } from "svelte";

export interface Route {
  /** 去掉 # 与查询串的路径，如 /s/1/settings/general */
  path: string;
  /** 按 / 切开的路径段 */
  seg: string[];
  query: URLSearchParams;
}

function parse(): Route {
  const raw = location.hash.replace(/^#/, "") || "/sites";
  const [path, qs] = raw.split("?");
  return { path, seg: path.split("/").filter(Boolean), query: new URLSearchParams(qs ?? "") };
}

export const route = $state<{ current: Route }>({ current: parse() });

const reduceMotion = matchMedia("(prefers-reduced-motion: reduce)");

// 换页（路径变化）时用 View Transitions 做一次很短的淡入淡出；只改查询参数（筛选、范围）时不做，
// 数据就地刷新。浏览器不支持或要求减少动效时直接切换。
window.addEventListener("hashchange", () => {
  const next = parse();
  const changed = next.path !== route.current.path;
  if (!changed || reduceMotion.matches || !document.startViewTransition) {
    route.current = next;
    return;
  }
  document.startViewTransition(async () => {
    route.current = next;
    await tick();
  });
});

export function navigate(to: string, opts: { replace?: boolean } = {}) {
  if (opts.replace) {
    history.replaceState(null, "", "#" + to);
    route.current = parse();
  } else {
    location.hash = to;
    window.scrollTo(0, 0);
  }
}

/** 只改查询参数（看板的日期范围、筛选等），不产生历史记录。值为 null 或空串时删除该参数。 */
export function setQuery(patch: Record<string, string | null>) {
  const q = new URLSearchParams(route.current.query);
  for (const [k, v] of Object.entries(patch)) {
    if (v === null || v === "") q.delete(k);
    else q.set(k, v);
  }
  const qs = q.toString();
  navigate(route.current.path + (qs ? "?" + qs : ""), { replace: true });
}

/** 登录后回到原来的页面：只接受站内的相对路径。 */
export function safeNext(raw: string | null): string {
  if (!raw || !raw.startsWith("/") || raw.startsWith("//") || raw.startsWith("/\\")) return "/sites";
  return raw;
}

export function href(path: string) {
  return "#" + path;
}
