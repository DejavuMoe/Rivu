// 目标表单的初筛（服务端校验为准，见 bin/rivu/src/goals_admin.rs）：事件名、页面路径、重复与重名。
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

/** 目标值：事件名或页面路径。 */
export function goalTarget(g: Goal): string {
  return g.event_name ?? g.page_path ?? "";
}

/** 同类同值的目标已经存在。 */
export function isDuplicateGoal(goals: Goal[], kind: GoalKind, value: string): boolean {
  return !!value && goals.some((g) => g.kind === kind && goalTarget(g) === value);
}

/** 新目标的名字（显示名，否则目标值）已被别的目标占用：看板按名字区分目标，所以名字不能重。 */
export function labelTaken(goals: Goal[], label: string): boolean {
  return !!label && goals.some((g) => g.label === label);
}
