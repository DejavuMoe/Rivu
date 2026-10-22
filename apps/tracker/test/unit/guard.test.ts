// localhost 判定的单元测试（规范 §4.7）。其余排除规则依赖 window/navigator，在浏览器测试里覆盖。

import assert from 'node:assert/strict';
import { test } from 'node:test';
import { isLocalHost } from '../../src/guard.ts';

test('localhost、127.x.x.x、[::1] 与 file: 视为本地', () => {
  for (const h of ['localhost', '127.0.0.1', '127.1.2.3', '[::1]']) {
    assert.equal(isLocalHost(h, 'http:'), true, h);
  }
  assert.equal(isLocalHost('', 'file:'), true);
  assert.equal(isLocalHost('example.com', 'file:'), true);
});

test('普通主机名不是本地', () => {
  for (const h of ['example.com', 'www.example.com', '192.168.1.10', '10.0.0.1', '128.0.0.1', '127.example.com', 'notlocalhost', 'localhost.example.com', 'sub.localhost', '127.0.0', '127.0.0.1.example.com', '::1']) {
    assert.equal(isLocalHost(h, 'https:'), false, h);
  }
});
