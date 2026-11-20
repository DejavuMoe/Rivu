// 目标表单的初筛（node --test）。
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  eventNameError,
  goalTarget,
  isDuplicateGoal,
  labelTaken,
  normalizePagePath,
  pagePathError,
  scrollLabel,
} from "../src/lib/admin/goals.ts";
import type { Goal } from "../src/lib/api.ts";

const goal = (g: Partial<Goal>): Goal => ({
  id: 1,
  kind: "event",
  event_name: null,
  page_path: null,
  scroll_threshold: null,
  display_name: null,
  label: "",
  created_at: "2026-10-26T00:00:00.000Z",
  ...g,
});

test("事件名：空不报错、最长 120 个字符、engagement 不行", () => {
  assert.equal(eventNameError("   "), null);
  assert.equal(eventNameError(" Signup "), null);
  assert.equal(eventNameError("x".repeat(120)), null);
  assert.equal(eventNameError("x".repeat(121)), "set.goals.err.eventLen");
  assert.equal(eventNameError("engagement"), "set.goals.err.engagement");
});

test("页面路径：以 / 开头、没有空白、最长 250 字节，连续的 * 合并", () => {
  assert.equal(normalizePagePath(" /blog/**/x "), "/blog/*/x");
  assert.equal(pagePathError(""), null);
  assert.equal(pagePathError("/checkout/*/done"), null);
  assert.equal(pagePathError("pricing"), "set.goals.err.page");
  assert.equal(pagePathError("/a b"), "set.goals.err.page");
  assert.equal(pagePathError("/" + "a".repeat(249)), null);
  assert.equal(pagePathError("/" + "a".repeat(250)), "set.goals.err.page");
  // 按字节：多字节字符更早到上限
  assert.equal(pagePathError("/" + "页".repeat(83)), null);
  assert.equal(pagePathError("/" + "页".repeat(84)), "set.goals.err.page");
});

test("重复与重名", () => {
  const goals = [
    goal({ id: 1, kind: "event", event_name: "Signup", label: "Signup" }),
    goal({ id: 2, kind: "page", page_path: "/pricing", display_name: "Pricing", label: "Pricing" }),
  ];
  assert.equal(goalTarget(goals[1]), "/pricing");
  assert.equal(isDuplicateGoal(goals, "event", "Signup"), true);
  assert.equal(isDuplicateGoal(goals, "page", "Signup"), false);
  assert.equal(isDuplicateGoal(goals, "page", "/pricing"), true);
  assert.equal(labelTaken(goals, "Pricing"), true);
  assert.equal(labelTaken(goals, "/pricing"), false);
  assert.equal(labelTaken(goals, ""), false);
});

test("滚动深度：名字是 ≥ N%，同路径不同阈值不算重复", () => {
  const goals = [
    goal({ id: 3, kind: "scroll", page_path: "/blog/*", scroll_threshold: 75, label: "≥ 75%" }),
    goal({ id: 4, kind: "page", page_path: "/blog/*", label: "/blog/*" }),
  ];
  assert.equal(scrollLabel(75), "≥ 75%");
  assert.equal(goalTarget(goals[0]), "≥ 75%");
  assert.equal(isDuplicateGoal(goals, "scroll", "/blog/*", 75), true);
  assert.equal(isDuplicateGoal(goals, "scroll", "/blog/*", 90), false);
  assert.equal(isDuplicateGoal(goals, "scroll", "/*", 75), false);
  assert.equal(isDuplicateGoal(goals, "page", "/blog/*"), true);
  // 另一个路径的同阈值目标没有显示名时名字撞了
  assert.equal(labelTaken(goals, scrollLabel(75)), true);
});
