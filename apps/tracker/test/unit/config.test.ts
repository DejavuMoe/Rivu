// 配置解析的单元测试（规范 §2.2、§4.1 步 2）。用 node:test，原因见 package.json 旁的 README。

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { resolveConfig } from '../../src/config.ts';

test('没有任何来源时 site 缺失，且不抛异常', () => {
  const c = resolveConfig(undefined, undefined, undefined);
  assert.equal(c.site, undefined);
  assert.equal(c.endpoint, undefined);
  assert.equal(c.hash, false);
  assert.equal(c.local, false);
});

test('rivu.c（站点脚本前缀）原样生效', () => {
  const c = resolveConfig({ site: 'k7qm2xv5atbr', endpoint: 'https://stats.example.com/api/event', hash: true, local: true });
  assert.deepEqual(c, { site: 'k7qm2xv5atbr', endpoint: 'https://stats.example.com/api/event', hash: true, local: true });
});

test('data-* 配置（t.js）：data-site / data-api / data-hash / data-local', () => {
  const c = resolveConfig(undefined, { site: 'abcdefgh', api: 'https://a.example/api/event', hash: 'true', local: 'true' }, 'https://cdn.example.net/rivu/t.js');
  assert.deepEqual(c, { site: 'abcdefgh', endpoint: 'https://a.example/api/event', hash: true, local: true });
});

test('布尔型 data-* 只有字面量 "true" 才为真', () => {
  for (const v of ['', 'false', '1', 'TRUE', 'yes']) {
    const c = resolveConfig(undefined, { site: 'abcdefgh', hash: v, local: v }, 'https://x.example/t.js');
    assert.equal(c.hash, false, `hash=${JSON.stringify(v)}`);
    assert.equal(c.local, false, `local=${JSON.stringify(v)}`);
  }
});

test('优先级：rivu.c 压过 data-*，data-* 补全 rivu.c 没给的键', () => {
  const c = resolveConfig({ site: 'fromPrefix1' }, { site: 'fromData001', hash: 'true' }, 'https://x.example/js/a.js');
  assert.equal(c.site, 'fromPrefix1');
  assert.equal(c.hash, true);
});

test('缺省端点取脚本 src 的 origin + /api/event（丢弃路径与 query）', () => {
  const c = resolveConfig(undefined, { site: 'abcdefgh' }, 'https://stats.example.com:8443/js/k7qm2xv5atbr.js?x=1');
  assert.equal(c.endpoint, 'https://stats.example.com:8443/api/event');
});

test('显式端点（data-api 或 rivu.c.endpoint）不被 src 覆盖', () => {
  assert.equal(resolveConfig(undefined, { site: 'abcdefgh', api: '/proxy/e' }, 'https://x.example/t.js').endpoint, '/proxy/e');
  assert.equal(resolveConfig({ site: 'abcdefgh', endpoint: '/p/e' }, undefined, 'https://x.example/t.js').endpoint, '/p/e');
});
