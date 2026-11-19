/** 把用户粘贴的网址或域名规范成主机名：小写、去协议、路径、查询、端口、末尾点和一个前导 www.（与采集端的规范化一致）。 */
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

export function isValidDomain(host: string): boolean {
  return host.length <= 253 && /^([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z0-9-]{2,}$/.test(host);
}
