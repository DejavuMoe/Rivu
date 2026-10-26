// 发送（规范 §3.2 `send`）：`fetch keepalive`。`sendBeacon` 优先的路径在 full 的 `beacon` 模块（engagement 用）。
// 不重试、不排队、不落地；任何失败静默。

/** full 手动 API 的 `callback`：fetch 成功 `{status}`，网络错误 `{error}`，beacon `{}`，被忽略时无参（规范 §4.8）。 */
export type Callback = (r?: { status?: number; error?: unknown }) => void;

export const send = (endpoint: string, body: unknown, cb?: Callback): void => {
  // 体是字符串，浏览器自动带 `Content-Type: text/plain;charset=UTF-8`，属于 CORS 简单请求，不触发预检。
  // `credentials: 'omit'`：即使端点是站点的第一方反代路径，也不把站点 Cookie 带过去
  // （默认的 `same-origin` 会带）。
  const p = fetch(endpoint, {
    method: 'POST',
    keepalive: true,
    credentials: 'omit',
    body: JSON.stringify(body),
  });
  // core 没有 callback；full 把结果交给 callback（恰好一次；callback 自己抛的异常吞掉，不影响页面）
  if (__FULL__) {
    const call = (r: { status?: number; error?: unknown }) => {
      try {
        if (cb) cb(r);
      } catch {}
    };
    p.then(
      (r) => call({ status: r.status }),
      (e) => call({ error: e }),
    );
  } else p.catch(() => {});
};
