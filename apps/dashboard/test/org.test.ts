// 成员与 API 密钥页的纯逻辑（node --test）。
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  assignableRoles,
  canEditMember,
  curlExample,
  daysLeft,
  invitableRoles,
  keyExpiry,
  keyScope,
  minutesSince,
} from "../src/lib/admin/org.ts";

const NOW = Date.parse("2026-10-26T08:00:00Z");
const inDays = (d: number, extraMs = 0) => new Date(NOW + d * 864e5 + extraMs).toISOString();

test("assignableRoles / invitableRoles", () => {
  assert.deepEqual(assignableRoles("owner"), ["owner", "admin", "editor", "viewer"]);
  assert.deepEqual(assignableRoles("admin"), ["admin", "editor", "viewer"]);
  assert.deepEqual(assignableRoles("editor"), []);
  assert.deepEqual(assignableRoles("viewer"), []);
  assert.deepEqual(invitableRoles("owner"), ["admin", "editor", "viewer"]);
  assert.deepEqual(invitableRoles("viewer"), []);
});

test("canEditMember：自己、权限、所有者与最后一位所有者", () => {
  const own = { canManage: true, actorRole: "owner" as const, ownerCount: 2 };
  assert.equal(canEditMember({ me: true, role: "owner" }, own), false);
  assert.equal(canEditMember({ me: false, role: "viewer" }, own), true);
  assert.equal(canEditMember({ me: false, role: "owner" }, own), true);
  assert.equal(canEditMember({ me: false, role: "owner" }, { ...own, ownerCount: 1 }), false);
  const adm = { canManage: true, actorRole: "admin" as const, ownerCount: 2 };
  assert.equal(canEditMember({ me: false, role: "owner" }, adm), false);
  assert.equal(canEditMember({ me: false, role: "admin" }, adm), true);
  assert.equal(canEditMember({ me: false, role: "editor" }, adm), true);
  const ro = { canManage: false, actorRole: "editor" as const, ownerCount: 2 };
  assert.equal(canEditMember({ me: false, role: "viewer" }, ro), false);
});

test("keyScope：全部、单站点、多站点", () => {
  assert.deepEqual(keyScope({ scopes: ["stats:read:*"], sites: [] }), { kind: "all" });
  assert.deepEqual(keyScope({ scopes: ["stats:read:k1"], sites: [{ name: "Docs" }] }), { kind: "site", name: "Docs" });
  assert.deepEqual(keyScope({ scopes: ["stats:read:k1", "stats:read:k2"], sites: [{ name: "A" }, { name: "B" }] }), { kind: "sites", n: 2 });
  // 站点已删除、名称解析不到：只给数量
  assert.deepEqual(keyScope({ scopes: ["stats:read:k1"], sites: [] }), { kind: "sites", n: 1 });
});

test("keyExpiry：永不、已过期、7 天内、之后", () => {
  assert.deepEqual(keyExpiry(null, NOW), { kind: "never" });
  assert.deepEqual(keyExpiry(inDays(0), NOW), { kind: "expired" });
  assert.deepEqual(keyExpiry(inDays(-3), NOW), { kind: "expired" });
  assert.deepEqual(keyExpiry(inDays(0, 1000), NOW), { kind: "soon", days: 1 });
  assert.deepEqual(keyExpiry(inDays(7), NOW), { kind: "soon", days: 7 });
  assert.deepEqual(keyExpiry(inDays(7, 1000), NOW), { kind: "date", at: new Date(NOW + 7 * 864e5 + 1000) });
  assert.equal(keyExpiry(inDays(90), NOW).kind, "date");
});

test("minutesSince / daysLeft", () => {
  assert.equal(minutesSince(null, NOW), null);
  assert.equal(minutesSince("bad", NOW), null);
  assert.equal(minutesSince(new Date(NOW - 125 * 60000).toISOString(), NOW), 125);
  assert.equal(minutesSince(new Date(NOW + 60000).toISOString(), NOW), 0);
  assert.equal(daysLeft(inDays(7), NOW), 7);
  assert.equal(daysLeft(inDays(0, 1000), NOW), 1);
});

test("curlExample", () => {
  const s = curlExample("https://stats.example.com", "rivu_abc", "k7qm2xv9atbr");
  assert.ok(s.startsWith("curl -X POST https://stats.example.com/api/v1/query"));
  assert.ok(s.includes('Bearer rivu_abc"'));
  assert.ok(s.includes('"site":"k7qm2xv9atbr"'));
});
