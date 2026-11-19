// 周报订阅卡片的纯逻辑（node --test）。
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  cardVisible,
  classifySendError,
  classifyToggleError,
  retryAfterSeconds,
  shouldRefreshOnReturn,
  waitLeft,
  WEEKLY_WAIT_SECONDS,
} from "../src/lib/account/weekly.ts";

test("waitLeft：没在等、刚开始、向上取整、到点", () => {
  assert.equal(waitLeft(0, 1_000_000), 0);
  assert.equal(waitLeft(1000, 1000), 60);
  assert.equal(waitLeft(1000, 1000 + 59_800), 1);
  assert.equal(waitLeft(1000, 1000 + 60_000), 0);
  assert.equal(waitLeft(1000, 1000 + 90_000), 0);
  assert.equal(waitLeft(1000, 1000, 5), 5);
});

test("retryAfterSeconds：采用服务端的值，缺失或不合理时按一分钟，有上限", () => {
  assert.equal(retryAfterSeconds(17), 17);
  assert.equal(retryAfterSeconds(16.2), 17);
  assert.equal(retryAfterSeconds(null), WEEKLY_WAIT_SECONDS);
  assert.equal(retryAfterSeconds(0), WEEKLY_WAIT_SECONDS);
  assert.equal(retryAfterSeconds(Number.NaN), WEEKLY_WAIT_SECONDS);
  assert.equal(retryAfterSeconds(999_999), 3600);
});

test("cardVisible：没数据时显示；没配置邮件且未确认时隐藏；已确认始终显示", () => {
  assert.equal(cardVisible(null), true);
  assert.equal(cardVisible({ available: true, email_status: "unconfirmed" }), true);
  assert.equal(cardVisible({ available: true, email_status: "pending" }), true);
  assert.equal(cardVisible({ available: false, email_status: "unconfirmed" }), false);
  assert.equal(cardVisible({ available: false, email_status: "pending" }), false);
  assert.equal(cardVisible({ available: false, email_status: "confirmed" }), true);
});

test("classifySendError：429 用 Retry-After，502 mail_failed，503 mail_unavailable，其余一般失败", () => {
  assert.deepEqual(classifySendError({ status: 429, code: "http_429", retryAfter: 23 }), { kind: "wait", seconds: 23 });
  assert.deepEqual(classifySendError({ status: 429, code: "rate_limited", retryAfter: null }), { kind: "wait", seconds: 60 });
  assert.deepEqual(classifySendError({ status: 502, code: "mail_failed", retryAfter: null }), { kind: "mail_failed" });
  assert.deepEqual(classifySendError({ status: 503, code: "mail_unavailable", retryAfter: null }), { kind: "unavailable" });
  assert.deepEqual(classifySendError({ status: 502, code: "http_502", retryAfter: null }), { kind: "error" });
  assert.deepEqual(classifySendError({ status: 0, code: "network", retryAfter: null }), { kind: "error" });
  assert.deepEqual(classifySendError({ status: 500, code: "internal", retryAfter: null }), { kind: "error" });
});

test("classifyToggleError：409 email_unconfirmed、404、其余", () => {
  assert.equal(classifyToggleError({ status: 409, code: "email_unconfirmed" }), "unconfirmed");
  assert.equal(classifyToggleError({ status: 409, code: "other" }), "error");
  assert.equal(classifyToggleError({ status: 404, code: "not_found" }), "gone");
  assert.equal(classifyToggleError({ status: 0, code: "network" }), "error");
  assert.equal(classifyToggleError({ status: 500, code: "internal" }), "error");
});

test("shouldRefreshOnReturn：只在待确认且页面可见时重取", () => {
  assert.equal(shouldRefreshOnReturn("pending", true), true);
  assert.equal(shouldRefreshOnReturn("pending", false), false);
  assert.equal(shouldRefreshOnReturn("unconfirmed", true), false);
  assert.equal(shouldRefreshOnReturn("confirmed", true), false);
  assert.equal(shouldRefreshOnReturn(undefined, true), false);
});
