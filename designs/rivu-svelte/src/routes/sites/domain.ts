// 添加站点的域名输入：与服务端的规范化保持一致（服务端为准，这里只做即时预览与初筛）。

/** 把粘贴的网址或域名规范成主机名：小写、去协议、路径、查询、端口、末尾点和一个前导 www.。 */
export function normalizeDomain(input: string): string {
  let s = input.trim().toLowerCase();
  s = s.replace(/^[a-z][a-z0-9+.-]*:\/\//, "");
  s = s.replace(/^[^@/]*@/, "");
  s = s.split(/[/?#]/)[0];
  s = s.replace(/:\d*$/, "");
  s = s.replace(/\.+$/, "");
  s = s.replace(/^www\./, "");
  return s;
}

/** 初筛：每段 1–63 个小写字母、数字、连字符，首尾不是连字符；允许单段（localhost），不含通配。 */
export function isValidDomain(host: string): boolean {
  return host.length > 0 && host.length <= 253 && /^([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)(\.[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?)*$/.test(host);
}
