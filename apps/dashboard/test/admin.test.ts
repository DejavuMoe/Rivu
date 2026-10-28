// 站点管理的纯逻辑：域名规范化、主机名匹配、共享链接有效期、设置分区可见性（node --test）。
import { test } from "node:test";
import assert from "node:assert/strict";
import { isValidDomain, normalizeDomain } from "../src/lib/admin/domain.ts";
import { checkPattern, hostExamples, hostMatches, normalizeHost } from "../src/lib/admin/hostnames.ts";
import { computeExpiry, endOfLocalDay, localYmd } from "../src/lib/admin/expiry.ts";
import { canShareLinks, sectionReadOnly, sectionVisible, visibleSections } from "../src/lib/admin/sections.ts";

test("normalizeDomain：协议、路径、端口、www、大小写、末尾点", () => {
  assert.equal(normalizeDomain("  HTTPS://WWW.Example.COM:8443/a/b?x=1#h "), "example.com");
  assert.equal(normalizeDomain("user:pw@blog.example.com/"), "blog.example.com");
  assert.equal(normalizeDomain("example.com."), "example.com");
  assert.equal(normalizeDomain("www.www.example.com"), "www.example.com");
  assert.equal(normalizeDomain(""), "");
});

test("isValidDomain", () => {
  assert.equal(isValidDomain("example.com"), true);
  assert.equal(isValidDomain("a-b.c1.example.co.uk"), true);
  assert.equal(isValidDomain("localhost"), true);
  assert.equal(isValidDomain(""), false);
  assert.equal(isValidDomain("-bad.com"), false);
  assert.equal(isValidDomain("bad-.com"), false);
  assert.equal(isValidDomain("a..com"), false);
  assert.equal(isValidDomain("*.example.com"), false);
  assert.equal(isValidDomain("exa mple.com"), false);
  assert.equal(isValidDomain(`${"a".repeat(64)}.com`), false);
});

test("hostMatches：精确、通配、www 与端口", () => {
  assert.equal(normalizeHost("WWW.Example.com.:8443"), "example.com");
  assert.equal(hostMatches("example.com", "www.example.com"), true);
  assert.equal(hostMatches("example.com", "shop.example.com"), false);
  assert.equal(hostMatches("*.example.com", "a.b.example.com"), true);
  assert.equal(hostMatches("*.example.com", "example.com"), false);
  assert.equal(hostMatches("*.example.com", "www.example.com"), false);
});

test("checkPattern", () => {
  assert.equal(checkPattern("", []), "empty");
  assert.equal(checkPattern("https://a.com", []), "scheme");
  assert.equal(checkPattern("a.com:80", []), "scheme");
  assert.equal(checkPattern("a.*.com", []), "wildcard");
  assert.equal(checkPattern("**.a.com", []), "wildcard");
  assert.equal(checkPattern("a_b.com", []), "chars");
  assert.equal(checkPattern("www.a.com", []), "www");
  assert.equal(checkPattern("*.www.a.com", []), "www");
  assert.equal(checkPattern("a.com", ["a.com"]), "dup");
  assert.equal(checkPattern("*.a.com", ["a.com"]), null);
});

test("hostExamples 标出匹配结果", () => {
  const ex = hostExamples("*.a.com");
  assert.deepEqual(
    ex.map((e) => [e.host, e.match]),
    [
      ["blog.a.com", true],
      ["a.b.a.com", true],
      ["a.com", false],
      ["www.a.com", false],
    ],
  );
  assert.deepEqual(hostExamples("a.com").map((e) => e.match), [true, true, true, false]);
});

test("endOfLocalDay 与 localYmd", () => {
  const d = endOfLocalDay("2026-10-24");
  assert.ok(d);
  assert.equal(localYmd(d), "2026-10-24");
  assert.equal(d.getHours(), 23);
  assert.equal(d.getMinutes(), 59);
  assert.equal(endOfLocalDay("2026-02-31"), null);
  assert.equal(endOfLocalDay("26-1-1"), null);
});

test("computeExpiry", () => {
  const now = new Date(2026, 9, 24, 15, 30);
  assert.deepEqual(computeExpiry("never", "", now), { at: null, error: null });

  const seven = computeExpiry("7", "", now);
  assert.equal(seven.error, null);
  assert.equal(localYmd(new Date(seven.at!)), "2026-10-31");
  assert.equal(new Date(seven.at!).getHours(), 23);

  const thirty = computeExpiry("30", "", now);
  assert.equal(localYmd(new Date(thirty.at!)), "2026-11-23");

  const custom = computeExpiry("custom", "2026-12-01", now);
  assert.equal(localYmd(new Date(custom.at!)), "2026-12-01");

  // 今天的末尾仍在未来，可以选；昨天不行
  assert.equal(computeExpiry("custom", "2026-10-24", now).error, null);
  assert.deepEqual(computeExpiry("custom", "2026-10-23", now), { at: null, error: "past" });
  assert.deepEqual(computeExpiry("custom", "", now), { at: null, error: "invalid" });
});

test("设置分区可见性", () => {
  const owner = { role: "owner", access: "member" } as const;
  const editor = { role: "editor", access: "member" } as const;
  const viewer = { role: "viewer", access: "member" } as const;
  const guestEditor = { role: "editor", access: "guest" } as const;
  assert.deepEqual(visibleSections(owner), ["general", "hostnames", "collection", "privacy", "shield", "goals", "sharing", "imports", "danger"]);
  assert.deepEqual(visibleSections(editor), ["shield", "goals", "sharing"]);
  assert.deepEqual(visibleSections(viewer), []);
  assert.deepEqual(visibleSections(guestEditor), ["shield", "goals"]);
  assert.equal(sectionReadOnly("shield", editor), true);
  assert.equal(sectionReadOnly("shield", owner), false);
  // 目标：editor 起（含站点访客 editor）可写
  assert.equal(sectionReadOnly("goals", editor), false);
  assert.equal(sectionReadOnly("goals", guestEditor), false);
  assert.equal(sectionVisible("goals", { role: "viewer", access: "guest" }), false);
  assert.equal(canShareLinks(guestEditor), false);
  assert.equal(canShareLinks(editor), true);
  assert.equal(sectionVisible("general", { role: "admin", access: "guest" }), false);
});
