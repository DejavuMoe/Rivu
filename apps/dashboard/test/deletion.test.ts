// 数据删除的纯逻辑：站点时区的今天、日期范围校验、距永久删除的天数（node --test）。
import { test } from "node:test";
import assert from "node:assert/strict";
import { daysLeft, MAX_RANGE_DAYS, rangeError, ymdInZone } from "../src/lib/admin/deletion.ts";

test("ymdInZone：按站点时区取日期，无效时区按 UTC", () => {
  const at = new Date("2026-10-26T20:30:00Z");
  assert.equal(ymdInZone(at, "UTC"), "2026-10-26");
  assert.equal(ymdInZone(at, "Asia/Shanghai"), "2026-10-27");
  assert.equal(ymdInZone(at, "America/Los_Angeles"), "2026-10-26");
  assert.equal(ymdInZone(at, "Not/AZone"), "2026-10-26");
});

test("rangeError：顺序、未来、格式、跨度", () => {
  const today = "2026-10-26";
  assert.equal(rangeError("", "", today), null);
  assert.equal(rangeError("2026-10-01", "", today), null);
  assert.equal(rangeError("2026-10-01", "2026-10-26", today), null);
  assert.equal(rangeError("2026-10-05", "2026-10-05", today), null);
  assert.equal(rangeError("2026-10-05", "2026-10-04", today), "order");
  assert.equal(rangeError("2026-10-01", "2026-10-27", today), "future");
  assert.equal(rangeError("2026-02-30", "2026-03-01", today), "invalid");
  assert.equal(rangeError("2026-1-01", "2026-03-01", today), "invalid");
  // 731 天（含两端）可以，732 天不行
  assert.equal(MAX_RANGE_DAYS, 731);
  assert.equal(rangeError("2024-10-26", "2026-10-26", today), null);
  assert.equal(rangeError("2024-10-25", "2026-10-26", today), "tooLong");
});

test("daysLeft：按本地日历日，当天与已过为 0", () => {
  const now = new Date(2026, 9, 26, 23, 0);
  assert.equal(daysLeft(new Date(2026, 10, 2, 1, 0).toISOString(), now), 7);
  assert.equal(daysLeft(new Date(2026, 9, 27, 0, 30).toISOString(), now), 1);
  assert.equal(daysLeft(new Date(2026, 9, 26, 23, 59).toISOString(), now), 0);
  assert.equal(daysLeft(new Date(2026, 9, 20).toISOString(), now), 0);
  assert.equal(daysLeft("bogus", now), 0);
});
