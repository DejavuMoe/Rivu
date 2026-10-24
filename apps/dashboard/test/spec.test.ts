// 看板状态 ↔ QuerySpec 的换算与结果整理（node --test，Node 24 直接跑 TypeScript）。
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  breakdownSpec,
  decodeFilters,
  encodeFilters,
  readRows,
  readSeries,
  readState,
  readTotals,
  realtimeRange,
  seriesSpec,
  totalsSpec,
  labelDate,
} from "../src/lib/dashboard/spec.ts";
import { alignTo, axisLabels } from "../src/lib/dashboard/labels.ts";
import type { QueryResult } from "../src/lib/api.ts";

const now = new Date("2026-10-24T07:42:17.500Z");
const q = (s: string) => readState(new URLSearchParams(s));

test("缺省状态与非法参数回落", () => {
  assert.deepEqual(q(""), { range: "30d", compare: "previous_period", gran: "day", metric: "visitors", filters: [] });
  const s = q("range=bogus&gran=minute&metric=nope&cmp=weird");
  assert.equal(s.range, "30d");
  assert.equal(s.gran, "day");
  assert.equal(s.metric, "visitors");
  assert.equal(s.compare, "previous_period");
  // 粒度要在范围允许的集合里
  assert.equal(q("range=7d&gran=hour").gran, "hour");
  assert.equal(q("range=7d&gran=month").gran, "day");
  // 实时只有访客与浏览量
  assert.equal(q("range=realtime&metric=bounce_rate").metric, "visitors");
  assert.equal(q("range=realtime&metric=pageviews").metric, "pageviews");
});

test("筛选的编解码：往返、未知维度与操作符丢弃、同维度后者覆盖", () => {
  const fs = [
    { dim: "page", op: "is" as const, values: ["/a;b", "/c|d~e"] },
    { dim: "country", op: "is_not" as const, values: ["CN"] },
  ];
  assert.deepEqual(decodeFilters(encodeFilters(fs)), fs);
  assert.deepEqual(decodeFilters("goal~is~Signup;page~regex~x;page~is~"), []);
  assert.deepEqual(decodeFilters("page~is~%2Fa;page~contains~b"), [{ dim: "page", op: "contains", values: ["b"] }]);
  assert.deepEqual(decodeFilters("page~is~%E0%A4%A"), [{ dim: "page", op: "is", values: ["%E0%A4%A"] }]);
});

test("合计、时间序列与分解的 QuerySpec", () => {
  const s = q("range=7d&f=" + encodeURIComponent("country~is~CN|US"));
  assert.deepEqual(totalsSpec(s, now), {
    metrics: ["visitors", "visits", "pageviews", "views_per_visit", "bounce_rate", "visit_duration"],
    date_range: "7d",
    filters: [["is", "visit:country", ["CN", "US"]]],
    include: { compare: "previous_period" },
  });
  assert.deepEqual(seriesSpec(q("range=12mo&cmp=off&gran=week"), now), {
    metrics: ["visitors", "visits", "pageviews", "views_per_visit", "bounce_rate", "visit_duration"],
    date_range: "12mo",
    dimensions: ["time:week"],
    include: { time_labels: true },
  });
  assert.deepEqual(breakdownSpec(s, "page", 8, now), {
    metrics: ["visitors"],
    date_range: "7d",
    dimensions: ["event:page"],
    pagination: { limit: 8 },
    filters: [["is", "visit:country", ["CN", "US"]]],
  });
});

test("实时：过去 30 分钟的时刻区间，不带对比", () => {
  assert.deepEqual(realtimeRange(now), ["2026-10-24T07:13:00+00:00", "2026-10-24T07:43:00+00:00"]);
  const s = q("range=realtime&cmp=year_over_year");
  const spec = seriesSpec(s, now);
  assert.deepEqual(spec.dimensions, ["time:minute"]);
  assert.deepEqual(spec.metrics, ["visitors", "pageviews"]);
  assert.deepEqual(spec.include, { time_labels: true });
  assert.equal("include" in totalsSpec(s, now), false);
});

const result = (r: Partial<QueryResult>): QueryResult => ({ results: [], meta: {}, query: {}, ...r });

test("合计：有对比与没有数据", () => {
  const t = readTotals(
    result({ results: [{ dimensions: [], metrics: [10, 12], comparison: { dimensions: [], metrics: [5, 6], change: [100, 100] } }] }),
    ["visitors", "pageviews"],
  );
  assert.deepEqual(t, { values: { visitors: 10, pageviews: 12 }, prev: { visitors: 5, pageviews: 6 } });
  assert.deepEqual(readTotals(result({}), ["visitors"]), { values: { visitors: 0 }, prev: null });
});

test("时间序列：按标签补零，对比期用自己的标签", () => {
  const s = readSeries(
    result({
      results: [{ dimensions: ["2026-10-02"], metrics: [3, 4] }],
      comparison_results: [{ dimensions: ["2026-09-30"], metrics: [1, 2] }],
      meta: { time_labels: ["2026-10-01", "2026-10-02"], comparison_time_labels: ["2026-09-29", "2026-09-30"] },
    }),
    ["visitors", "pageviews"],
  );
  assert.deepEqual(s.values, { visitors: [0, 3], pageviews: [0, 4] });
  assert.deepEqual(s.prevValues, { visitors: [0, 1], pageviews: [0, 2] });
  assert.deepEqual(alignTo([1, 2, 3], 2), [1, 2]);
  assert.deepEqual(alignTo([1], 3), [1, null, null]);
});

test("标签按站点墙上时间解析，不做时区换算", () => {
  const d = labelDate("2026-10-23T14:00:00+05:45");
  assert.deepEqual([d.getFullYear(), d.getMonth(), d.getDate(), d.getHours(), d.getMinutes()], [2026, 9, 23, 14, 0]);
  assert.deepEqual(
    axisLabels(["2026-10-24T15:40:00+08:00", "2026-10-24T15:41:00+08:00", "2026-10-24T15:42:00+08:00"], "minute", "zh"),
    ["2 分钟前", "1 分钟前", "现在"],
  );
  assert.deepEqual(axisLabels(["2026-10-24T09:00:00+08:00"], "hour", "en"), ["09:00"]);
});

test("分解行", () => {
  assert.deepEqual(readRows(result({ results: [{ dimensions: ["CN"], metrics: [7] }] })), [{ label: "CN", visitors: 7 }]);
});
