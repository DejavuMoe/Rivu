// 组装线协议请求体（规范 §5.1）。core 只发 pageview，所以只有 v/sv/s/n/u/r/h；
// full 再按需补 p（属性）、i（非互动）、sd / e（engagement）。
// 时间戳由服务端生成，客户端不发任何时间字段；也不发屏幕、语言、时区等指纹字段。

export interface Wire {
  v: 1;
  sv: number;
  s: string;
  n: string;
  u: string;
  r?: string;
  h?: 1;
  p?: Record<string, string>;
  i?: false;
  sd?: number;
  e?: number;
}

/**
 * @param ref  `document.referrer`；空串视为缺失，不发该键
 * @param hash hash 路由模式：所有事件带 `h:1`
 */
export const buildPayload = (
  site: string,
  sv: number,
  name: string,
  url: string,
  ref?: string,
  hash?: boolean,
): Wire => {
  const p: Wire = { v: 1, sv, s: site, n: name, u: url };
  if (ref) p.r = ref;
  if (hash) p.h = 1;
  return p;
};
