// 线协议 `v=1` 请求体的校验器（浏览器测试与单元测试共用）。
//
// 规则逐条对照 `docs/architecture/tracker-and-protocol.md` §5.1，以及服务端权威实现
// `crates/ingest/src/protocol.rs`（`parse_event`）。这里只做「客户端发出的东西必须被服务端接受」所需的检查，
// 不做规范化（hostname 白名单、query 白名单等是服务端的事）。返回错误描述列表，空数组表示合法。

const SITE_KEY = /^[A-Za-z0-9_-]{8,32}$/;
const MAX_BODY_BYTES = 8192;
const hasControl = (s: string) => /[\u0000-\u001f\u007f-\u009f]/.test(s);
const isInt = (v: unknown): v is number => typeof v === 'number' && Number.isInteger(v);

/** 协议里出现过的全部键（未知键服务端忽略，但客户端不应发）。 */
export const PROTOCOL_KEYS = ['v', 'sv', 's', 'n', 'u', 'r', 'p', 'i', 'h', 'sd', 'e', '$'];

export function validateWire(body: unknown, rawBytes?: number): string[] {
  const errs: string[] = [];
  if (rawBytes !== undefined && rawBytes > MAX_BODY_BYTES) errs.push(`请求体 ${rawBytes} B 超过 ${MAX_BODY_BYTES} B`);
  if (body === null || typeof body !== 'object' || Array.isArray(body)) return [...errs, '请求体必须是 JSON 对象'];
  const b = body as Record<string, unknown>;

  for (const k of Object.keys(b)) if (!PROTOCOL_KEYS.includes(k)) errs.push(`未知键 ${k}`);

  if (b.v !== 1) errs.push(`v 必须是整数 1，得到 ${JSON.stringify(b.v)}`);
  // sv 永不 400，但客户端应发 0–999999 的整数。
  if ('sv' in b && !(isInt(b.sv) && b.sv >= 0 && b.sv <= 999_999)) errs.push(`sv 必须是 0–999999 的整数`);

  if (typeof b.s !== 'string' || !SITE_KEY.test(b.s)) errs.push('s 必须匹配 ^[A-Za-z0-9_-]{8,32}$');

  const n = b.n;
  if (typeof n !== 'string') errs.push('n 必须是字符串');
  else {
    const t = n.trim();
    if (!t) errs.push('n 不能为空');
    if ([...t].length > 120) errs.push('n 超过 120 个字符');
    if (hasControl(t)) errs.push('n 含控制字符');
  }

  if (typeof b.u !== 'string') errs.push('u 必须是字符串');
  else {
    try {
      const u = new URL(b.u);
      if (u.protocol !== 'http:' && u.protocol !== 'https:') errs.push('u 的 scheme 只能是 http/https');
    } catch {
      errs.push('u 必须是合法 URL');
    }
  }

  if ('r' in b && b.r !== null) {
    if (typeof b.r !== 'string') errs.push('r 必须是字符串');
    else if ([...b.r].length > 2000) errs.push('r 超过 2000 个字符');
  }

  if ('p' in b && b.p !== null) {
    const p = b.p;
    if (typeof p !== 'object' || Array.isArray(p) || p === null) errs.push('p 必须是对象');
    else {
      const entries = Object.entries(p as Record<string, unknown>);
      if (entries.length > 30) errs.push('p 超过 30 个键');
      for (const [k, v] of entries) {
        if (Buffer.byteLength(k.trim()) > 300) errs.push(`p 的键 ${k.slice(0, 20)}… 超过 300 B`);
        if (!['string', 'number', 'boolean'].includes(typeof v)) errs.push(`p.${k} 必须是 string/number/boolean`);
        else if (Buffer.byteLength(String(v)) > 2000) errs.push(`p.${k} 超过 2000 B`);
      }
    }
  }

  if ('i' in b && b.i !== null && typeof b.i !== 'boolean') errs.push('i 必须是布尔');
  if ('h' in b && b.h !== null && !isInt(b.h)) errs.push('h 必须是整数');

  if (b.n === 'engagement') {
    const sdOk = isInt(b.sd) && b.sd >= 0;
    const eOk = isInt(b.e) && b.e >= 0 && b.e < 2_592_000_000;
    if (!sdOk && !eOk) errs.push('engagement 必须至少带有效的 sd 或 e');
  }
  return errs;
}

/** core 实际会发的键（只有 pageview）；用来断言脚本没有多发字段。 */
export const CORE_KEYS = ['v', 'sv', 's', 'n', 'u', 'r', 'h'];
