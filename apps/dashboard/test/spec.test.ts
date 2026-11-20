// 看板状态 ↔ QuerySpec 的换算与结果整理（node --test，Node 24 直接跑 TypeScript）。
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  breakdownSpec,
  decodeFilters,
  encodeFilters,
  goalsSpec,
  opsFor,
  propertyKeysSpec,
  specDim,
  readGoals,
  readRows,
  readSeries,
  readState,
  readTotals,
  realtimeRange,
  seriesSpec,
  totalsSpec,
  labelDate,
} from "../src/lib/dashboard/spec.ts";
import { alignTo, axisLabels, tipLabels } from "../src/lib/dashboard/labels.ts";
import { compact, delta } from "../src/lib/format.ts";
import type { QueryResult } from "../src/lib/api.ts";

const now = new Date("2026-10-24T07:42:17.500Z");

test("rate comparisons show percentage points, including a zero baseline", () => {
  assert.deepEqual(delta(0.5, 0.4, "zh"), { text: "+10 个百分点", dir: "up" });
  assert.deepEqual(delta(0, 0.4, "en"), { text: "−40 pp", dir: "down" });
  assert.deepEqual(delta(0.5, 0, "en"), { text: "+50 pp", dir: "up" });
  assert.equal(delta(50, 40).text, "+25%");
});
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
  assert.deepEqual(decodeFilters("hostnamex~is~a;page~regex~x;page~is~"), []);
  // 目标只认等于 / 不等于
  assert.deepEqual(decodeFilters("goal~is~Signup"), [{ dim: "goal", op: "is", values: ["Signup"] }]);
  assert.deepEqual(decodeFilters("goal~contains~Sign"), []);
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

test("紧凑数字保留整数尾零与原有小数精度", () => {
  assert.deepEqual(
    [100_000, 110_000, 100_000_000, 15_500, 1_250_000, 1_200_000, 99_999].map((n) => compact(n, "en")),
    ["100K", "110K", "100M", "15.5K", "1.25M", "1.2M", "100K"],
  );
  assert.deepEqual(
    [1_000_000, 12_500, 123_000, 125_000_000, 9_999].map((n) => compact(n, "zh")),
    ["100 万", "1.25 万", "12.3 万", "1.25 亿", "9,999"],
  );
});

test("柏林秋季回拨：分钟前用偏移计算，提示保留站点时间", () => {
  const labels = ["2026-10-25T02:59:00+02:00", "2026-10-25T02:00:00+01:00"];
  assert.deepEqual(axisLabels(labels, "minute", "en"), ["-1m", "now"]);
  assert.deepEqual(tipLabels(labels, "minute", "en"), ["02:59", "02:00"]);
});

test("柏林春季跳时：分钟前用偏移计算，提示保留站点时间", () => {
  const labels = ["2026-03-29T01:59:00+01:00", "2026-03-29T03:00:00+02:00"];
  assert.deepEqual(axisLabels(labels, "minute", "zh"), ["1 分钟前", "现在"]);
  assert.deepEqual(tipLabels(labels, "minute", "zh"), ["01:59", "03:00"]);
  assert.deepEqual(tipLabels(["2026-03-29T02:30:00+08:00"], "minute", "en"), ["02:30"]);
});

test("目标卡片的 QuerySpec、目标筛选与结果", () => {
  const s = q("range=7d&f=" + encodeURIComponent("goal~is~Signup;country~is~CN"));
  assert.deepEqual(goalsSpec(s, now), {
    metrics: ["visitors", "events", "conversion_rate"],
    date_range: "7d",
    dimensions: ["event:goal"],
    pagination: { limit: 100 },
    filters: [
      ["is", "event:goal", ["Signup"]],
      ["is", "visit:country", ["CN"]],
    ],
  });
  assert.deepEqual(totalsSpec(s, now).filters, [
    ["is", "event:goal", ["Signup"]],
    ["is", "visit:country", ["CN"]],
  ]);
  assert.deepEqual(goalsSpec(q("range=realtime"), now).date_range, realtimeRange(now));
  assert.deepEqual(opsFor("goal"), ["is", "is_not"]);
  assert.deepEqual(opsFor("page"), ["is", "is_not", "contains", "contains_not"]);
  assert.deepEqual(readGoals(result({ results: [{ dimensions: ["Signup"], metrics: [3, 5, 0.25] }] })), [
    { label: "Signup", visitors: 3, events: 5, cr: 0.25 },
  ]);
});

test("不包含：除目标与城市外的维度都能选，URL 往返后保留", () => {
  for (const dim of ["page", "entry_page", "source", "utm_campaign", "country", "browser", "prop:plan"]) {
    assert.ok(opsFor(dim).includes("contains_not"), dim);
  }
  // 目标按名字、城市按 GeoNames id，服务端只支持等于 / 不等于
  assert.deepEqual(opsFor("goal"), ["is", "is_not"]);
  assert.deepEqual(opsFor("city"), ["is", "is_not"]);
  const fs = [
    { dim: "page", op: "contains_not" as const, values: ["/admin", "a~b|c"] },
    { dim: "prop:plan", op: "contains_not" as const, values: ["trial"] },
  ];
  const enc = encodeFilters(fs);
  assert.deepEqual(decodeFilters(enc), fs);
  const s = q(`range=7d&f=${encodeURIComponent(enc)}`);
  assert.deepEqual(totalsSpec(s, now).filters, [
    ["contains_not", "event:page", ["/admin", "a~b|c"]],
    ["contains_not", "event:props:plan", ["trial"]],
  ]);
  // 目标与城市上的包含 / 不包含在解码时丢弃
  assert.deepEqual(decodeFilters("goal~contains_not~Sign;city~contains_not~26;city~contains~26"), []);
  assert.deepEqual(decodeFilters("city~is_not~2694762"), [{ dim: "city", op: "is_not", values: ["2694762"] }]);
});

test("分解行", () => {
  assert.deepEqual(readRows(result({ results: [{ dimensions: ["CN"], metrics: [7] }] })), [{ label: "CN", visitors: 7 }]);
});

test("属性：维度换算、URL 往返（键里有分隔符）、键列表的 QuerySpec", () => {
  assert.equal(specDim("prop:plan"), "event:props:plan");
  assert.equal(specDim("country"), "visit:country");
  const fs = [{ dim: "prop:a~b;c", op: "is" as const, values: ["x|y", "(none)"] }];
  const enc = encodeFilters(fs);
  assert.deepEqual(decodeFilters(enc), fs);
  // 空键、未知维度丢弃
  assert.deepEqual(decodeFilters("prop:~is~x;bogus~is~x"), []);
  assert.deepEqual(opsFor("prop:plan"), ["is", "is_not", "contains", "contains_not"]);
  const s = q(`range=7d&f=${encodeURIComponent(enc)}`);
  const b = breakdownSpec(s, "prop:plan", 8, now);
  assert.deepEqual(b.dimensions, ["event:props:plan"]);
  // 属性卡片去掉 (none)；已经按这个键筛选时不加
  assert.deepEqual((b.filters as unknown[][]).at(-1), ["is_not", "event:props:plan", ["(none)"]]);
  const own = q(`range=7d&f=${encodeURIComponent(encodeFilters([{ dim: "prop:plan", op: "is", values: ["pro"] }]))}`);
  assert.deepEqual(breakdownSpec(own, "prop:plan", 8, now).filters, [["is", "event:props:plan", ["pro"]]]);
  assert.deepEqual(propertyKeysSpec(s, now), {
    metrics: ["events"],
    date_range: "7d",
    filters: [["is", "event:props:a~b;c", ["x|y", "(none)"]]],
  });
});
