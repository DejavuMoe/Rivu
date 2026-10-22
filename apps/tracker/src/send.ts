// 发送（规范 §3.2 `send`）：core 只用 `fetch keepalive`。`sendBeacon` 优先的路径属于 full（`beacon` 模块）。
// 不重试、不排队、不落地；任何失败静默。

export const send = (endpoint: string, body: unknown): void => {
  // 体是字符串，浏览器自动带 `Content-Type: text/plain;charset=UTF-8`，属于 CORS 简单请求，不触发预检。
  // `credentials: 'omit'`：即使端点是站点的第一方反代路径，也不把站点 Cookie 带过去
  // （默认的 `same-origin` 会带）。
  fetch(endpoint, {
    method: 'POST',
    keepalive: true,
    credentials: 'omit',
    body: JSON.stringify(body),
  }).catch(() => {});
};
