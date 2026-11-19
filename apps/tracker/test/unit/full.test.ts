// full 专属纯函数的单元测试（规范 §2.2、§4.6、§4.8）。

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { cleanProps, DOWNLOADS, extOf, parseTag, resolveFull } from '../../src/full/config.ts';

test('默认值：engagement 与 tagged 开，其余关，没有下载表', () => {
  assert.deepEqual(resolveFull(undefined, undefined), {
    engagement: true,
    tagged: true,
    outbound: false,
    forms: false,
    manual: false,
    debug: false,
    downloads: undefined,
  });
});

test('前缀（rivu.c）优先于 data-*，data-* 只认字面量 true / false', () => {
  const c = resolveFull({ engagement: false, outbound: true }, { engagement: 'true', outbound: 'false', forms: 'true', tagged: 'false', debug: 'yes' });
  assert.equal(c.engagement, false);
  assert.equal(c.outbound, true);
  assert.equal(c.forms, true);
  assert.equal(c.tagged, false);
  assert.equal(c.debug, false, '"yes" 不是 true');
});

test('下载：true 用默认 28 项；字符串是逗号分隔的列表（转小写）；false 关闭；前缀里的数组原样', () => {
  assert.equal(DOWNLOADS.length, 28);
  assert.deepEqual(resolveFull({ downloads: true }).downloads, DOWNLOADS);
  assert.deepEqual(resolveFull(undefined, { downloads: 'true' }).downloads, DOWNLOADS);
  assert.deepEqual(resolveFull(undefined, { downloads: 'PDF, zip ,Key' }).downloads, ['pdf', 'zip', 'key']);
  assert.equal(resolveFull(undefined, { downloads: 'false' }).downloads, false);
  assert.equal(resolveFull({ downloads: false }).downloads, false);
  assert.deepEqual(resolveFull({ downloads: ['pdf', 'zip'] }).downloads, ['pdf', 'zip']);
  // 前缀里的值压过 data-*
  assert.deepEqual(resolveFull({ downloads: ['csv'] }, { downloads: 'true' }).downloads, ['csv']);
});

test('属性只留 string / number / boolean，转成字符串；没有就返回 undefined', () => {
  assert.deepEqual(cleanProps({ a: 'x', b: 2, c: true, d: null, e: {}, f: [1], g: undefined, h: () => 1 }), { a: 'x', b: '2', c: 'true' });
  assert.equal(cleanProps({ d: null }), undefined);
  assert.equal(cleanProps(undefined), undefined);
  assert.equal(cleanProps(null), undefined);
});

test('下载扩展名取 pathname 最后一段的最后一个点，转小写', () => {
  assert.equal(extOf('/files/Report.PDF'), 'pdf');
  assert.equal(extOf('/a.b/c.tar.gz'), 'gz');
  assert.equal(extOf('/a.b/readme'), '');
  assert.equal(extOf('/dir/'), '');
  assert.equal(extOf('/.hidden'), 'hidden');
  assert.equal(extOf(''), '');
});

test('tagged 标记：事件名（= 或 --）、属性（+ 换空格），忽略旧收入标记', () => {
  assert.deepEqual(parseTag(['btn', 'rivu-event-name=Signup', 'rivu-event-plan=pro+plan', 'rivu-event-seats--5']), [
    'Signup',
    { plan: 'pro plan', seats: '5' },
  ]);
  assert.deepEqual(parseTag(['rivu-event-name--Sign+Up']), ['Sign Up']);
  assert.deepEqual(parseTag(['rivu-event-name=Buy', 'rivu-revenue-amount=29.90', 'rivu-revenue-currency=EUR']), ['Buy']);
  assert.deepEqual(parseTag(['plain', 'rivu-event-', 'rivu-other-name=x']), []);
});
