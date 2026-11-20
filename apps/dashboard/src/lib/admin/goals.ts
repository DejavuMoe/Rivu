// 目标表单的初筛（服务端校验为准，见 bin/rivu/src/goals_admin.rs）：事件名、页面路径、滚动阈值、重复与重名。
import type { Goal, GoalKind } from "../api.ts";

export const EVENT_NAME_MAX = 120;
export const PAGE_PATH_MAX_BYTES = 250;

/** 页面路径的规范化：trim，连续的 * 合并成一个（与服务端相同）。 */
export function normalizePagePath(raw: string): string {
  return raw.trim().replace(/\*{2,}/g, "*");
}

export function eventNameError(raw: string): string | null {
  const s = raw.trim();
  if (!s) return null;
  if ([...s].length > EVENT_NAME_MAX) return "set.goals.err.eventLen";
  if (s === "engagement") return "set.goals.err.engagement";
  return null;
}

export function pagePathError(raw: string): string | null {
  const s = normalizePagePath(raw);
  if (!s) return null;
  if (!s.startsWith("/") || /[\s\p{Cc}]/u.test(s)) return "set.goals.err.page";
  if (new TextEncoder().encode(s).length > PAGE_PATH_MAX_BYTES) return "set.goals.err.page";
  return null;
}

/** 滚动深度可选的阈值（%），默认 75。 */
export const SCROLL_DEPTHS = [25, 50, 75, 90, 100];
/** 滚动深度目标不限定页面时的路径：所有页面（与服务端相同）。 */
export const SCROLL_ALL_PAGES = "/*";

/** 滚动深度目标没有显示名时的名字（与服务端相同）。 */
export function scrollLabel(threshold: number): string {
  return `≥ ${threshold}%`;
}

/** 目标值：事件名、页面路径，滚动深度目标是 `≥ N%`（路径另行显示）。 */
export function goalTarget(g: Goal): string {
  if (g.kind === "scroll") return scrollLabel(g.scroll_threshold ?? 0);
  return g.event_name ?? g.page_path ?? "";
}

/** 同类同值的目标已经存在；滚动深度目标比较路径与阈值。 */
export function isDuplicateGoal(goals: Goal[], kind: GoalKind, value: string, threshold: number | null = null): boolean {
  if (!value) return false;
  return goals.some(
    (g) => g.kind === kind && (g.event_name ?? g.page_path ?? "") === value && (kind !== "scroll" || g.scroll_threshold === threshold),
  );
}

/** 新目标的名字（显示名，否则目标值）已被别的目标占用：看板按名字区分目标，所以名字不能重。 */
export function labelTaken(goals: Goal[], label: string): boolean {
  return !!label && goals.some((g) => g.label === label);
}
