// 请求体组装的单元测试（规范 §5.1、§4.2 的 `r` 规则）。

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { buildPayload } from '../../src/payload.ts';
import { CORE_KEYS, validateWire } from '../wire.ts';

const SITE = 'k7qm2xv5atbr';

test('最小 pageview：v/sv/s/n/u，键序固定，不含 r 与 h', () => {
  const p = buildPayload(SITE, 3, 'pageview', 'https://example.com/pricing');
  assert.deepEqual(p, { v: 1, sv: 3, s: SITE, n: 'pageview', u: 'https://example.com/pricing' });
  assert.equal(JSON.stringify(p), '{"v":1,"sv":3,"s":"k7qm2xv5atbr","n":"pageview","u":"https://example.com/pricing"}');
});

test('r：非空才发；空串视为缺失，不发该键', () => {
  assert.equal(buildPayload(SITE, 1, 'pageview', 'https://e.com/', 'https://ref.example.org/x').r, 'https://ref.example.org/x');
  assert.equal('r' in buildPayload(SITE, 1, 'pageview', 'https://e.com/', ''), false);
  assert.equal('r' in buildPayload(SITE, 1, 'pageview', 'https://e.com/', undefined), false);
});

test('h：hash 模式才带 h:1', () => {
  assert.equal(buildPayload(SITE, 1, 'pageview', 'https://e.com/#/a', '', true).h, 1);
  assert.equal('h' in buildPayload(SITE, 1, 'pageview', 'https://e.com/#/a', '', false), false);
  assert.equal('h' in buildPayload(SITE, 1, 'pageview', 'https://e.com/#/a'), false);
});

test('不含任何时间、屏幕、语言、时区字段，键集合不超出 core 的发送集合', () => {
  const p = buildPayload(SITE, 1, 'pageview', 'https://e.com/', 'https://r.example/', true);
  for (const k of Object.keys(p)) assert.ok(CORE_KEYS.includes(k), `意外的键 ${k}`);
});

test('产出的请求体通过线协议校验', () => {
  const p = buildPayload(SITE, 999_999, 'pageview', 'https://www.Example.com/pricing?utm_source=x#plans', 'https://news.example.org/post/42?id=7', true);
  assert.deepEqual(validateWire(p, JSON.stringify(p).length), []);
});
