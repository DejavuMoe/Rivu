// 线协议校验器自身的测试：规范 §5.2 的三个示例必须通过，典型坏请求必须被拒绝。
// 校验器是浏览器测试判断「脚本发出的请求合法」的依据，所以它本身不能出错。

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { validateWire } from '../wire.ts';

const SITE = 'k7qm2xv5atbr';
const base = () => ({ v: 1, s: SITE, n: 'pageview', u: 'https://example.com/' });

test('规范 §5.2 的三个示例合法', () => {
  const ex1 = JSON.parse('{"v":1,"sv":3,"s":"k7qm2xv5atbr","n":"pageview","u":"https://www.Example.com/pricing?utm_source=newsletter&utm_medium=email&token=abc123#plans","r":"https://news.example.org/post/42?id=7"}');
  const ex2 = JSON.parse('{"v":1,"sv":3,"s":"k7qm2xv5atbr","n":"Purchase","u":"https://example.com/checkout/done","p":{"plan":"pro","seats":"5"},"$":{"a":"29.00","c":"USD"}}');
  const ex3 = JSON.parse('{"v":1,"sv":3,"s":"k7qm2xv5atbr","n":"engagement","u":"https://example.com/pricing","sd":72,"e":18450}');
  for (const ex of [ex1, ex2, ex3]) assert.deepEqual(validateWire(ex), []);
});

test('v 只能是整数 1', () => {
  for (const v of [0, 2, '1', 1.5, null, undefined]) assert.notDeepEqual(validateWire({ ...base(), v }), [], String(v));
});

test('s 的格式', () => {
  for (const s of ['short', 'abcdefgh', 'ABCDEFGHIJKL', 'abcdefghij01', 'has space1', 'dots.dots.x', 12345678]) assert.notDeepEqual(validateWire({ ...base(), s }), [], String(s));
  assert.deepEqual(validateWire({ ...base(), s: 'k7qm2xv5atbr' }), []);
});

test('u 必须是 http/https 的合法 URL', () => {
  for (const u of ['not a url', '/relative', 'ftp://example.com/', 'javascript:alert(1)', 1]) assert.notDeepEqual(validateWire({ ...base(), u }), [], String(u));
});

test('n 的长度与控制字符', () => {
  assert.deepEqual(validateWire({ ...base(), n: '名'.repeat(120) }), []);
  assert.notDeepEqual(validateWire({ ...base(), n: '名'.repeat(121) }), []);
  assert.notDeepEqual(validateWire({ ...base(), n: '   ' }), []);
  assert.notDeepEqual(validateWire({ ...base(), n: 'a\nb' }), []);
});

test('r、p、i、h 的类型', () => {
  assert.deepEqual(validateWire({ ...base(), r: null }), []);
  assert.notDeepEqual(validateWire({ ...base(), r: 5 }), []);
  assert.notDeepEqual(validateWire({ ...base(), r: 'x'.repeat(2001) }), []);
  assert.notDeepEqual(validateWire({ ...base(), p: { k: null } }), []);
  assert.deepEqual(validateWire({ ...base(), p: { k: 1, b: false } }), []);
  assert.notDeepEqual(validateWire({ ...base(), i: 'false' }), []);
  assert.notDeepEqual(validateWire({ ...base(), h: '1' }), []);
});

test('engagement 至少要有有效的 sd 或 e', () => {
  const e = { ...base(), n: 'engagement' };
  assert.notDeepEqual(validateWire(e), []);
  assert.deepEqual(validateWire({ ...e, sd: 0 }), []);
  assert.deepEqual(validateWire({ ...e, e: 1 }), []);
  assert.notDeepEqual(validateWire({ ...e, sd: -1, e: 2_592_000_000 }), []);
});

test('未知键、非对象、体积超限都被标出', () => {
  assert.notDeepEqual(validateWire({ ...base(), t: 123 }), []);
  assert.notDeepEqual(validateWire([]), []);
  assert.notDeepEqual(validateWire(null), []);
  assert.notDeepEqual(validateWire(base(), 8193), []);
  assert.deepEqual(validateWire(base(), 8192), []);
});
