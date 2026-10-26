// beacon（规范 §3.2）：engagement 与页面隐藏时的事件 `sendBeacon` 优先，不可用或返回 false 时回退 `fetch keepalive`
// （决策 7）。字符串体的 beacon 是 `text/plain;charset=UTF-8`，属于简单请求，响应不可读。

import { send } from '../send.ts';

export const beacon = (endpoint: string, body: unknown): void => {
  try {
    if (navigator.sendBeacon(endpoint, JSON.stringify(body))) return;
  } catch {}
  send(endpoint, body);
};
