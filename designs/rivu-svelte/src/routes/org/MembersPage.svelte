<!--
  组织成员：成员表、待接受的邀请、站点访客。
  所有者和管理员可改角色、移除、邀请、撤销；编辑者与查看者只读，看不到任何操作入口。
  界面按规则隐藏做不到的操作（管理员不能动所有者、至少保留一位所有者、不能动自己、邀请不能创建所有者）。
  创建邀请后给出一次性链接，由邀请人自己转发；实例配置了邮件（评审面板的「邮件」）时同时发出邀请邮件。
  邮箱已经是成员或已有待接受的邀请，要提交之后才会提示（生产里由服务端判定）。
  只以站点访客身份看到的组织：没有成员页，也没有任何操作入口。
-->
<script lang="ts">
  import { Trash2, TriangleAlert, UserPlus, Users, X } from "@lucide/svelte";
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
  import { href } from "../../lib/router.svelte";
  import { app, canManage, toast, type Role } from "../../lib/state.svelte";
  import { GUESTS, INVITATIONS, MEMBERS, type Guest, type Invitation, type Member } from "./data";
  import "./strings";

  let { orgId }: { orgId: string } = $props();

  let members = $state<Member[]>(structuredClone(MEMBERS));
  let invites = $state<Invitation[]>(structuredClone(INVITATIONS));
  let guests = $state<Guest[]>(structuredClone(GUESTS));
  let busy = $state(false);

  const org = $derived(ORGS.find((o) => o.id === orgId));
  const sites = $derived(SITES.filter((s) => s.org === orgId));
  const siteName = (id: number) => SITES.find((s) => s.id === id)?.name ?? String(id);
  const ds = $derived(app.review.dataState);
  const loading = $derived(ds === "loading");
  const failed = $derived(ds === "error" || ds === "busy" || ds === "timeout");
  /** 用户在这个组织里没有角色（只以站点访客身份可见）：没有成员页 */
  const notMember = $derived(!org?.role);

  // 加载完成之前、加载失败、没有成员页时，都没有任何操作入口
  const manage = $derived(!notMember && !loading && !failed && canManage());
  const actorOwner = $derived(app.review.role === "owner");
  const roleOf = (m: Member): Role => (m.me ? app.review.role : m.role);
  /** 没填名字的成员用邮箱代替 */
  const name = (m: { name: string | null; email: string }) => m.name ?? m.email;

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
    toast(t("mb.roleChanged", { name: name(m), role: t(`role.${r}`) }), "good");
  }

  // ---- 确认对话框：移除成员、撤销邀请、移除站点访客共用 ----
  let confirmOpen = $state(false);
  let confirming = $state<{ title: string; body?: string; label: string; run: () => string } | null>(null);
  function ask(c: NonNullable<typeof confirming>) {
    confirming = c;
    confirmOpen = true;
  }
  function confirmRun() {
    const c = confirming;
    if (!c || busy) return;
    busy = true;
    setTimeout(() => {
      toast(c.run());
      busy = false;
      confirmOpen = false;
    }, 400);
  }
  function askRemove(m: Member) {
    ask({
      title: t("mb.remove.title", { name: name(m) }),
      body: t("mb.remove.body", { name: name(m), org: org?.name ?? "" }),
      label: t("mb.remove"),
      run: () => {
        members = members.filter((x) => x.id !== m.id);
        return t("mb.removed", { name: name(m) });
      },
    });
  }
  function askRevoke(i: Invitation) {
    ask({
      title: t("mb.revoke.title", { email: i.email }),
      label: t("mb.revoke.confirm"),
      run: () => {
        invites = invites.filter((x) => x.id !== i.id);
        return t("mb.revoked", { email: i.email });
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
        return t("mb.guestRemoved", { email: g.email, site: siteName(g.siteId) });
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
  /** 提交后才知道的错误（邮箱已是成员或已有邀请）；改动邮箱时清除 */
  let serverEmail = $state("");
  /** 创建之后：显示链接（只显示这一次）；配置了邮件时邀请邮件已发出 */
  let phase = $state<"form" | "link">("form");
  let sentTo = $state("");
  let link = $state("");

  const emailOk = $derived(/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim()) && email.trim().length <= 320);
  const emailError = $derived(serverEmail || (emailOk ? "" : t("inv.email.invalid")));

  function openInvite() {
    email = "";
    scope = "org";
    inviteRole = "viewer";
    guestRole = "viewer";
    inviteSite = String(sites[0]?.id ?? "");
    tried = false;
    serverEmail = "";
    link = "";
    phase = "form";
    inviteOpen = true;
  }
  function sendInvite(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    if (!emailOk || busy) return;
    const addr = email.trim();
    const lower = addr.toLowerCase();
    const id = `i${Date.now()}`;
    const item: Invitation =
      scope === "org"
        ? { id, org: orgId, email: addr, role: inviteRole, expiresInDays: 7 }
        : { id, org: orgId, email: addr, role: guestRole, siteId: Number(inviteSite), expiresInDays: 7 };
    busy = true;
    serverEmail = "";
    setTimeout(() => {
      busy = false;
      if (orgMembers.some((m) => m.email.toLowerCase() === lower) || orgInvites.some((i) => i.email.toLowerCase() === lower)) {
        serverEmail = t("inv.email.dup");
        return;
      }
      invites.push(item);
      sentTo = addr;
      link = `https://rivu.northwind.dev/invite/${crypto.randomUUID().replaceAll("-", "")}`;
      phase = "link";
      toast(t("inv.createdToast", { email: addr }), "good");
    }, 600);
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
  const active = (m: Member) => (m.activeMinutesAgo === null ? "—" : relativeTime(m.activeMinutesAgo, app.lang));
</script>

<div class="page">
  <OrgHeader {orgId}>
    {#snippet actions()}
      {#if manage}<Button variant="primary" icon={UserPlus} onclick={openInvite}>{t("mb.invite")}</Button>{/if}
    {/snippet}
  </OrgHeader>

  {#if notMember}
    <Card>
      <EmptyState icon={Users} title={t("mb.noOrg")}>
        {#snippet actions()}<Button href={href("/sites")}>{t("nav.allSites")}</Button>{/snippet}
      </EmptyState>
    </Card>
  {:else if failed}
    <Card>
      <EmptyState icon={TriangleAlert} tone="bad" title={t("mb.error.title")}>
        {#snippet actions()}<Button onclick={() => toast(t("common.retry"))}>{t("common.retry")}</Button>{/snippet}
      </EmptyState>
    </Card>
  {:else if loading}
    <Card flush>
      <div class="sk" aria-busy="true">
        {#each [0, 1, 2, 3] as i (i)}
          <div class="sk-row"><Skeleton width="28px" height={28} radius={14} /><Skeleton width="30%" height={12} /><Skeleton width="14%" height={12} /></div>
        {/each}
      </div>
    </Card>
  {:else}
    <Card flush>
      <Table fit columns={memberCols} rows={orgMembers} rowKey={(m) => m.id} caption={t("mb.title")}>
        {#snippet cell(m, c)}
          {#if c.key === "member"}
            <div class="who">
              <Avatar name={name(m)} size={28} />
              <div class="who-text">
                <span class="who-name">{name(m)}{#if m.me}<span class="you">{t("mb.you")}</span>{/if}</span>
                {#if m.name}<span class="who-mail">{m.email}</span>{/if}
              </div>
            </div>
          {:else if c.key === "role"}
            {#if editable(m)}
              <Select
                size="sm"
                width={168}
                label={t("mb.roleFor", { name: name(m) })}
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
            <span class="muted">{active(m)}</span>
          {:else if c.key === "act"}
            {#if editable(m)}
              <Button variant="ghost" size="sm" icon={Trash2} aria-label={`${t("mb.remove")} ${name(m)}`} onclick={() => askRemove(m)} />
            {/if}
          {/if}
        {/snippet}
      </Table>
    </Card>

    {#if orgInvites.length > 0}
      <Card title={t("mb.pending")} flush>
        <Table fit columns={inviteCols} rows={orgInvites} rowKey={(i) => i.id} caption={t("mb.pending")}>
          {#snippet cell(i, c)}
            {#if c.key === "email"}
              <div class="who-text">
                <span>{i.email}</span>
                {#if i.siteId !== undefined}<span class="who-mail">{siteName(i.siteId)}</span>{/if}
              </div>
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

    {#if orgGuests.length > 0}
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
  <span class="sr-only">{confirming?.title}</span>
  {#snippet footer()}
    <Button variant="ghost" onclick={() => (confirmOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" loading={busy} onclick={confirmRun}>{confirming?.label}</Button>
  {/snippet}
</Dialog>

<Dialog bind:open={inviteOpen} title={phase === "form" ? t("inv.title") : t("inv.created")} onclose={() => (link = "")}>
  {#if phase === "form"}
    <form id="invite-form" class="form" onsubmit={sendInvite} novalidate>
      <Field label={t("inv.scope")}>
        <Segmented
          label={t("inv.scope")}
          value={scope}
          onchange={(v: "org" | "site") => (scope = v)}
          options={[
            { value: "org", label: t("inv.scope.org") },
            { value: "site", label: t("inv.scope.site"), disabled: sites.length === 0 },
          ]}
        />
      </Field>
      <Field label={t("inv.email")} for="inv-email" error={tried ? emailError : serverEmail}>
        <Input
          id="inv-email"
          type="email"
          autocomplete="off"
          bind:value={email}
          oninput={() => (serverEmail = "")}
          invalid={(tried && !!emailError) || !!serverEmail}
        />
      </Field>
      {#if scope === "site"}
        <Field label={t("inv.site")}>
          <Select block label={t("inv.site")} bind:value={inviteSite} options={sites.map((s) => ({ value: String(s.id), label: s.name }))} width={380} />
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
  {:else}
    <div class="form">
      {#if app.review.mail}
        <Callout tone="good">{t("inv.mailed", { email: sentTo })}</Callout>
      {:else}
        <Callout tone="warn">{t("inv.once")}</Callout>
      {/if}
      <CopyField label={t("inv.link")} value={link} />
    </div>
  {/if}
  {#snippet footer()}
    {#if phase === "form"}
      <Button variant="ghost" onclick={() => (inviteOpen = false)}>{t("common.cancel")}</Button>
      <Button type="submit" form="invite-form" variant="primary" loading={busy}>{t("inv.create")}</Button>
    {:else}
      <Button variant="primary" onclick={() => (inviteOpen = false)}>{t("common.done")}</Button>
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
