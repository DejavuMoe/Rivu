// 导航小工具：看板的范围、筛选等状态都在查询串里，改它们不产生历史记录、不滚动、不丢焦点。

import { goto } from "$app/navigation";
import { page } from "$app/state";

/** 只改查询参数。值为 null 或空串时删除该参数。 */
export function setQuery(patch: Record<string, string | null>) {
  const url = new URL(page.url.href);
  for (const [k, v] of Object.entries(patch)) {
    if (v === null || v === "") url.searchParams.delete(k);
    else url.searchParams.set(k, v);
  }
  void goto(url, { replace: true, reset: false });
}

/** 登录后回到原来的页面：只接受站内的相对路径。 */
export function safeNext(raw: string | null): string {
  if (!raw || !raw.startsWith("/") || raw.startsWith("//") || raw.startsWith("/\\")) return "/sites";
  return raw;
}
