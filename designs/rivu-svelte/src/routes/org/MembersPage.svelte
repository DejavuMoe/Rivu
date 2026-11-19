<!--
  组织成员：成员表、待接受的邀请、站点访客。
  所有者和管理员可改角色、移除、邀请、撤销；编辑者与查看者只读，看不到任何操作入口。
  规则：管理员不能修改所有者、不能指派所有者；至少保留一位所有者；邀请不能创建所有者。
-->
<script lang="ts">
  import { Trash2, TriangleAlert, UserPlus, X } from "@lucide/svelte";
  import OrgHeader from "../../lib/app/OrgHeader.svelte";
  import Avatar from "../../lib/ui/Avatar.svelte";
  import Badge from "../../lib/ui/Badge.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Callout from "../../lib/ui/Callout.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import CopyField from "../../lib/ui/CopyField.svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Segmented from "../../lib/ui/Segmented.svelte";
  import Select from "../../lib/ui/Select.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import type { Column } from "../../lib/ui/types";
  import { ORGS } from "../../lib/data/fixtures";
  import { SITES } from "../../lib/data/sites.svelte";
  import { relativeTime } from "../../lib/format";
  import { t } from "../../lib/i18n";
  import { app, canManage, toast, type Role } from "../../lib/state.svelte";
  import { GUESTS, INVITATIONS, MEMBERS, type Guest, type Invitation, type Member } from "./data";
  import "./strings";

  let { orgId }: { orgId: string } = $props();

  let members = $state<Member[]>(structuredClone(MEMBERS));
  let invites = $state<Invitation[]>(structuredClone(INVITATIONS));
  let guests = $state<Guest[]>(structuredClone(GUESTS));

  const org = $derived(ORGS.find((o) => o.id === orgId));
  const sites = $derived(SITES.filter((s) => s.org === orgId));
  const siteName = (id: number) => SITES.find((s) => s.id === id)?.name ?? String(id);
  const ds = $derived(app.review.dataState);
  const loading = $derived(ds === "loading");
  const failed = $derived(ds === "error" || ds === "busy" || ds === "timeout");

  const manage = $derived(canManage());
  const actorOwner = $derived(app.review.role === "owner");
  const roleOf = (m: Member): Role => (m.me ? app.review.role : m.role);

  const orgMembers = $derived(members.filter((m) => m.org === orgId));
  const orgInvites = $derived(invites.filter((i) => i.org === orgId));
  const orgGuests = $derived(guests.filter((g) => g.org === orgId));
  const ownerCount = $derived(orgMembers.filter((m) => roleOf(m) === "owner").length);

  /** 这一行能否被当前登录者修改或移除。 */
  function editable(m: Member) {
    if (!manage || m.me) return false;
    const r = roleOf(m);
    if (r === "owner") return actorOwner && ownerCount > 1;
    return true;
  }
  function lockReason(m: Member): string {
    if (roleOf(m) === "owner") return actorOwner ? t("mb.lastOwner") : t("mb.adminNoOwner");
    return "";
  }
  const assignable = $derived(
    (actorOwner ? (["owner", "admin", "editor", "viewer"] as Role[]) : (["admin", "editor", "viewer"] as Role[])).map((r) => ({
      value: r,
      label: t(`role.${r}`),
    })),
  );

  function changeRole(m: Member, r: Role) {
    m.role = r;
    toast(t("mb.roleChanged", { name: m.name, role: t(`role.${r}`) }), "good");
  }

  // ---- 确认对话框：移除成员、撤销邀请、移除站点访客共用 ----
  let confirmOpen = $state(false);
  let confirming = $state<{ title: string; body?: string; label: string; run: () => void } | null>(null);
  function ask(c: NonNullable<typeof confirming>) {
    confirming = c;
    confirmOpen = true;
  }
  function askRemove(m: Member) {
    ask({
      title: t("mb.remove.title", { name: m.name }),
      body: t("mb.remove.body", { name: m.name, org: org?.name ?? "" }),
      label: t("mb.remove"),
      run: () => {
        members = members.filter((x) => x.id !== m.id);
        toast(t("mb.removed", { name: m.name }));
      },
    });
  }
  // ---- 邀请 ----
  let inviteOpen = $state(false);
  let scope = $state<"org" | "site">("org");
  let email = $state("");
  let inviteRole = $state<"admin" | "editor" | "viewer">("viewer");
  let guestRole = $state<"editor" | "viewer">("viewer");
  let inviteSite = $state("");
  let tried = $state(false);
  /** 创建之后：显示链接（只显示这一次）；配置了邮件时邀请邮件已发出 */
  let phase = $state<"form" | "link">("form");
  let sentTo = $state("");
  let link = $state("");

  const emailOk = $derived(/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim()));
  const dupEmail = $derived.by(() => {
    const e = email.trim().toLowerCase();
    return orgMembers.some((m) => m.email.toLowerCase() === e) || orgInvites.some((i) => i.email.toLowerCase() === e);
  });
  const emailError = $derived(!emailOk ? t("inv.email.invalid") : dupEmail ? t("inv.email.dup") : "");

  function openInvite() {
    email = "";
    scope = "org";
    inviteRole = "viewer";
    guestRole = "viewer";
    inviteSite = String(sites[0]?.id ?? "");
    tried = false;
    phase = "form";
    inviteOpen = true;
  }
  function sendInvite(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    if (emailError) return;
    const addr = email.trim();
    if (scope === "org") {
      invites.push({ id: `i${Date.now()}`, org: orgId, email: addr, role: inviteRole, expiresInDays: 7 });
    } else {
      guests.push({ id: `g${Date.now()}`, org: orgId, email: addr, siteId: Number(inviteSite), role: guestRole });
    }
    sentTo = addr;
    link = `https://rivu.northwind.dev/invite/${crypto.randomUUID().replaceAll("-", "")}`;
    phase = "link";
    toast(t("inv.sent", { email: addr }), "good");
  }

  function askRevoke(i: Invitation) {
    ask({
      title: t("mb.revoke.title", { email: i.email }),
      label: t("mb.revoke.confirm"),
      run: () => {
        invites = invites.filter((x) => x.id !== i.id);
        toast(t("mb.revoked", { email: i.email }));
      },
    });
  }
  function askRemoveGuest(g: Guest) {
    ask({
      title: t("mb.guest.remove.title", { email: g.email }),
      body: t("mb.guest.remove.body", { site: siteName(g.siteId) }),
      label: t("mb.remove"),
      run: () => {
        guests = guests.filter((x) => x.id !== g.id);
        toast(t("mb.guestRemoved", { email: g.email, site: siteName(g.siteId) }));
      },
    });
  }
  // ---- 表格列 ----
  const memberCols = $derived<Column[]>([
    { key: "member", label: t("mb.col.member") },
    { key: "role", label: t("mb.col.role"), width: "132px" },
    { key: "active", label: t("mb.col.active"), optional: true, width: "120px" },
    ...(manage ? [{ key: "act", label: "", align: "right" as const, width: "56px" }] : []),
  ]);
  const inviteCols = $derived<Column[]>([
    { key: "email", label: t("mb.col.email") },
    { key: "role", label: t("mb.col.role"), width: "160px" },
    { key: "expires", label: t("mb.col.expires"), optional: true, width: "120px" },
    ...(manage ? [{ key: "act", label: "", align: "right" as const, width: "56px" }] : []),
  ]);
  const guestCols = $derived<Column[]>([
    { key: "email", label: t("mb.col.email") },
    { key: "site", label: t("mb.col.site"), width: "200px", optional: true },
    { key: "role", label: t("mb.col.role"), width: "96px" },
    ...(manage ? [{ key: "act", label: "", align: "right" as const, width: "56px" }] : []),
  ]);
</script>

{#snippet skeletonRows()}
  <div class="sk">
    {#each [0, 1, 2, 3] as i (i)}
      <div class="sk-row"><Skeleton width="28px" height={28} radius={14} /><Skeleton width="30%" height={12} /><Skeleton width="14%" height={12} /></div>
    {/each}
  </div>
{/snippet}

<div class="page">
  <OrgHeader {orgId}>
    {#snippet actions()}
      {#if manage}<Button variant="primary" icon={UserPlus} onclick={openInvite}>{t("mb.invite")}</Button>{/if}
    {/snippet}
  </OrgHeader>

  {#if failed}
    <Card>
      <EmptyState icon={TriangleAlert} tone="bad" title={t("mb.error.title")}>
        {#snippet actions()}<Button onclick={() => toast(t("common.retry"))}>{t("common.retry")}</Button>{/snippet}
      </EmptyState>
    </Card>
  {:else}
    <Card flush>
      {#if loading}
        {@render skeletonRows()}
      {:else}
        <Table fit columns={memberCols} rows={orgMembers} rowKey={(m) => m.id} caption={t("mb.title")}>
          {#snippet cell(m, c)}
            {#if c.key === "member"}
              <div class="who">
                <Avatar name={m.name} size={28} />
                <div class="who-text">
                  <span class="who-name">{m.name}{#if m.me}<span class="you">{t("mb.you")}</span>{/if}</span>
                  <span class="who-mail">{m.email}</span>
                </div>
              </div>
            {:else if c.key === "role"}
              {#if editable(m)}
                <Select
                  size="sm"
                  width={168}
                  label={t("mb.roleFor", { name: m.name })}
                  value={roleOf(m)}
                  options={assignable}
                  onchange={(r: Role) => changeRole(m, r)}
                />
              {:else}
                <span title={manage && !m.me ? lockReason(m) : undefined}>
                  <Badge>{t(`role.${roleOf(m)}`)}</Badge>
                </span>
              {/if}
            {:else if c.key === "active"}
              <span class="muted">{relativeTime(m.activeMinutesAgo, app.lang)}</span>
            {:else if c.key === "act"}
              {#if editable(m)}
                <Button variant="ghost" size="sm" icon={Trash2} aria-label={`${t("mb.remove")} ${m.name}`} onclick={() => askRemove(m)} />
              {/if}
            {/if}
          {/snippet}
        </Table>
      {/if}
    </Card>

    {#if !loading && orgInvites.length > 0}
      <Card title={t("mb.pending")} flush>
        <Table fit columns={inviteCols} rows={orgInvites} rowKey={(i) => i.id} caption={t("mb.pending")}>
          {#snippet cell(i, c)}
            {#if c.key === "email"}
              {i.email}
            {:else if c.key === "role"}
              <Badge>{t(`role.${i.role}`)}</Badge>
            {:else if c.key === "expires"}
              <span class="muted">{t("mb.expiresIn", { n: i.expiresInDays })}</span>
            {:else if c.key === "act"}
              <Button variant="ghost" size="sm" icon={X} aria-label={`${t("mb.revoke")} ${i.email}`} onclick={() => askRevoke(i)} />
            {/if}
          {/snippet}
        </Table>
      </Card>
    {/if}

    {#if !loading && orgGuests.length > 0}
      <Card title={t("mb.guests")} flush>
        <Table fit columns={guestCols} rows={orgGuests} rowKey={(g) => g.id} caption={t("mb.guests")}>
          {#snippet cell(g, c)}
            {#if c.key === "email"}
              <div class="guest">
                <span class="guest-mail">{g.email}</span>
                <!-- 窄屏站点列隐藏，站点名放到邮箱下面 -->
                <span class="guest-site">{siteName(g.siteId)}</span>
              </div>
            {:else if c.key === "site"}
              {siteName(g.siteId)}
            {:else if c.key === "role"}
              <Badge>{t(`role.${g.role}`)}</Badge>
            {:else if c.key === "act"}
              <Button variant="ghost" size="sm" icon={Trash2} aria-label={`${t("mb.remove")} ${g.email}`} onclick={() => askRemoveGuest(g)} />
            {/if}
          {/snippet}
        </Table>
      </Card>
    {/if}
  {/if}
</div>

<Dialog bind:open={confirmOpen} size="sm" title={confirming?.title ?? ""} description={confirming?.body}>
  {#snippet children()}<span class="sr-only">{confirming?.title}</span>{/snippet}
  {#snippet footer()}
    <Button variant="ghost" onclick={() => (confirmOpen = false)}>{t("common.cancel")}</Button>
    <Button
      variant="danger"
      onclick={() => {
        confirming?.run();
        confirmOpen = false;
      }}>{confirming?.label}</Button
    >
  {/snippet}
</Dialog>

<Dialog bind:open={inviteOpen} title={phase === "form" ? t("inv.title") : t("inv.created")}>
  {#if phase === "link"}
    <div class="form">
      {#if app.review.mail}
        <Callout tone="good">{t("inv.mailed", { email: sentTo })}</Callout>
      {:else}
        <Callout tone="warn">{t("inv.once")}</Callout>
      {/if}
      <CopyField label={t("inv.link")} value={link} />
    </div>
  {:else}
  <form id="invite-form" class="form" onsubmit={sendInvite} novalidate>
    <Field label={t("inv.scope")}>
      <Segmented
        label={t("inv.scope")}
        value={scope}
        onchange={(v: "org" | "site") => (scope = v)}
        options={[
          { value: "org", label: t("inv.scope.org") },
          { value: "site", label: t("inv.scope.site") },
        ]}
      />
    </Field>
    <Field label={t("inv.email")} for="inv-email" error={tried ? emailError : ""}>
      <Input id="inv-email" type="email" autocomplete="off" bind:value={email} invalid={tried && !!emailError} />
    </Field>
    {#if scope === "site"}
      <Field label={t("inv.site")}>
        <Select block label={t("inv.site")} bind:value={inviteSite} options={sites.map((s) => ({ value: String(s.id), label: s.name, hint: s.domain }))} width={380} />
      </Field>
      <Field label={t("inv.role")}>
        <Select
          block
          label={t("inv.role")}
          bind:value={guestRole}
          width={380}
          options={[
            { value: "editor", label: t("role.editor") },
            { value: "viewer", label: t("role.viewer") },
          ]}
        />
      </Field>
    {:else}
      <Field label={t("inv.role")}>
        <Select
          block
          label={t("inv.role")}
          bind:value={inviteRole}
          width={380}
          options={[
            { value: "admin", label: t("role.admin") },
            { value: "editor", label: t("role.editor") },
            { value: "viewer", label: t("role.viewer") },
          ]}
        />
      </Field>
    {/if}
  </form>
  {/if}
  {#snippet footer()}
    {#if phase === "link"}
      <Button variant="primary" onclick={() => (inviteOpen = false)}>{t("common.done")}</Button>
    {:else}
      <Button variant="ghost" onclick={() => (inviteOpen = false)}>{t("common.cancel")}</Button>
      <Button type="submit" form="invite-form" variant="primary">{t("inv.send")}</Button>
    {/if}
  {/snippet}
</Dialog>

<style>
  .page {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    width: 100%;
    max-width: var(--content-max);
    margin: 0 auto;
    padding: var(--sp-6) var(--sp-8) var(--sp-12);
  }
  /* 与其他页面同一左缘，整页（含页头）限宽：动作按钮挨着正文 */
  .page > :global(*) {
    max-width: 880px;
  }
  .who {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
  }
  .who-text {
    line-height: 1.3;
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .who-name {
    font-weight: var(--fw-medium);
  }
  .who-mail {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .guest {
    display: flex;
    flex-direction: column;
    min-width: 0;
    line-height: 1.3;
  }
  .guest-mail {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .guest-site {
    display: none;
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  @media (max-width: 720px) {
    .guest-site {
      display: block;
    }
  }
  .you {
    margin-left: var(--sp-2);
    color: var(--text-muted);
    font-size: var(--fs-sm);
    font-weight: var(--fw-regular);
  }
  .sk {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    padding: var(--sp-5);
  }
  .sk-row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  @media (max-width: 720px) {
    .page {
      padding: var(--sp-4) var(--sp-4) var(--sp-10);
    }
  }
</style>

