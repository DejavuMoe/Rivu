// 主机名白名单：匹配与校验，与采集端一致（见 tracker-and-protocol.md）；服务端校验为准，这里给即时反馈与匹配示例。

const HOST_RE = /^(\*\.)?([a-z0-9-]+\.)*[a-z0-9-]+$/;

/** 匹配前的规范化：小写、去端口、去末尾点、去一个前导 www.。 */
export function normalizeHost(raw: string): string {
  let h = raw.trim().toLowerCase();
  h = h.replace(/:\d+$/, "").replace(/\.$/, "");
  if (h.startsWith("www.")) h = h.slice(4);
  return h;
}

export function hostMatches(pattern: string, rawHost: string): boolean {
  const host = normalizeHost(rawHost);
  if (pattern.startsWith("*.")) {
    const base = pattern.slice(2);
    return host.length > base.length + 1 && host.endsWith("." + base);
  }
  return host === pattern;
}

export type HostError = "empty" | "scheme" | "wildcard" | "chars" | "www" | "dup" | null;

export function checkPattern(pattern: string, existing: string[]): HostError {
  if (!pattern) return "empty";
  if (/[/:?#@\s]/.test(pattern)) return "scheme";
  if (pattern.includes("*") && !/^\*\.[^*]+$/.test(pattern)) return "wildcard";
  if (!HOST_RE.test(pattern)) return "chars";
  if (pattern.startsWith("www.") || pattern.startsWith("*.www.")) return "www";
  if (existing.includes(pattern)) return "dup";
  return null;
}

/** 添加对话框里的示例主机：每个示例标出会不会匹配。 */
export function hostExamples(pattern: string): { host: string; match: boolean }[] {
  const wild = pattern.startsWith("*.");
  const base = wild ? pattern.slice(2) : pattern;
  const hosts = wild ? [`blog.${base}`, `a.b.${base}`, base, `www.${base}`] : [base, `www.${base}`, `${base}:8443`, `shop.${base}`];
  return hosts.map((host) => ({ host, match: hostMatches(pattern, host) }));
}
