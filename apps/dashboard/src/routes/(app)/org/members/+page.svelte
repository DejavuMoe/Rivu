<!--
  组织成员：成员表、待接受的邀请、站点访客。
  所有者和管理员可改角色、移除、邀请、撤销；编辑者与查看者只读，看不到任何操作入口。
  界面按规则隐藏做不到的操作（管理员不能动所有者、至少保留一位所有者、不能动自己），服务端拒绝时仍以提示显示原因。
  没有发邮件：创建邀请后给出一次性链接，由邀请人自己转发。
-->
<script lang="ts">
  import { Trash2, TriangleAlert, UserPlus, Users, X } from "@lucide/svelte";
  import { untrack } from "svelte";
  import OrgHeader from "#lib/app/OrgHeader.svelte";
  import Avatar from "#lib/ui/Avatar.svelte";
  import Badge from "#lib/ui/Badge.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Callout from "#lib/ui/Callout.svelte";
  import Card from "#lib/ui/Card.svelte";
  import CopyField from "#lib/ui/CopyField.svelte";
  import Dialog from "#lib/ui/Dialog.svelte";
  import EmptyState from "#lib/ui/EmptyState.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import Segmented from "#lib/ui/Segmented.svelte";
  import Select from "#lib/ui/Select.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import Table from "#lib/ui/Table.svelte";
  import type { Column } from "#lib/ui/types.ts";
  import { assignableRoles, canEditMember, daysLeft, invitableRoles, minutesSince } from "#lib/admin/org.ts";
  import { api, ApiError, type InviteBody, type MembersBody, type OrgInvitation, type OrgMember, type Role, type SiteGuest } from "#lib/api.ts";
  import { relativeTime } from "#lib/format.ts";
  import { t } from "#lib/i18n.ts";
  import { currentOrg, orgSites, session } from "#lib/session.svelte.ts";
  import { app, toast } from "#lib/state.svelte.ts";
  import { orgErrText } from "../errors.ts";
  import "../strings";

  let data = $state<MembersBody | null>(null);
  let status = $state<"loading" | "ready" | "failed">("loading");
  /** 每次加载递增：下拉在请求失败后回到真实值 */
  let gen = $state(0);
  let seq = 0;
  let busy = $state(false);

  const orgId = $derived(session.orgId);
  const org = $derived(currentOrg());
  /** 只以站点访客身份看到这个组织（或没有组织）：没有成员页 */
  const notMember = $derived(orgId === null || !org?.role);
  const manage = $derived(data?.can_manage ?? false);
  const actor = $derived<Role>(data?.my_role ?? "viewer");
  const members = $derived(data?.members ?? []);
  const invites = $derived(data?.invitations ?? []);
  const guests = $derived(data?.guests ?? []);
  const ownerCount = $derived(members.filter((m) => m.role === "owner").length);
  const sites = $derived(orgSites());
  let now = $state(Date.now());

  async function load(id: number | null, silent = false) {
    const mine = ++seq;
    if (id === null) {
      status = "failed";
      return;
    }
    if (!silent) status = "loading";
    try {
      const body = await api.members(id);
      if (mine !== seq) return;
      data = body;
      now = Date.now();
      gen++;
      status = "ready";
    } catch (e) {
      if (mine !== seq) return;
      if (silent) toast(orgErrText(e), "bad");
      else status = "failed";
    }
  }

  // 切换组织时重新加载；进入页面时加载一次
  $effect(() => {
    const id = notMember ? null : orgId;
    untrack(() => {
      data = null;
      void load(id);
    });
  });

  const name = (m: { name: string | null; email: string }) => m.name ?? m.email;
  const ctx = $derived({ canManage: manage, actorRole: actor, ownerCount });
  const editable = (m: OrgMember) => canEditMember(m, ctx);
  const lockReason = (m: OrgMember) => (m.role === "owner" ? (actor === "owner" ? t("mb.lastOwner") : t("mb.adminNoOwner")) : "");
  const assignable = $derived(assignableRoles(actor).map((r) => ({ value: r, label: t(`role.${r}`) })));

  async function changeRole(m: OrgMember, r: Role) {
    if (orgId === null || r === m.role) return;
    try {
      await api.setMemberRole(orgId, m.user_id, r);
      toast(t("mb.roleChanged", { name: name(m), role: t(`role.${r}`) }), "good");
    } catch (e) {
      toast(orgErrText(e), "bad");
    }
    await load(orgId, true);
  }

  // ---- 确认对话框：移除成员、撤销邀请、移除站点访客共用 ----
  let confirmOpen = $state(false);
  let confirming = $state<{ title: string; body?: string; label: string; run: (org: number) => Promise<string> } | null>(null);
  function ask(c: NonNullable<typeof confirming>) {
    confirming = c;
    confirmOpen = true;
  }
  async function confirmRun() {
    if (!confirming || orgId === null || busy) return;
    busy = true;
    try {
      toast(await confirming.run(orgId));
    } catch (e) {
      toast(orgErrText(e), "bad");
    } finally {
      busy = false;
      confirmOpen = false;
    }
    await load(orgId, true);
  }
  function askRemove(m: OrgMember) {
    ask({
      title: t("mb.remove.title", { name: name(m) }),
      body: t("mb.remove.body", { name: name(m), org: org?.name ?? "" }),
      label: t("mb.remove"),
      run: async (id) => {
        await api.removeMember(id, m.user_id);
        return t("mb.removed", { name: name(m) });
      },
    });
  }
  function askRevoke(i: OrgInvitation) {
    ask({
      title: t("mb.revoke.title", { email: i.email }),
      label: t("mb.revoke.confirm"),
      run: async (id) => {
        await api.revokeInvitation(id, i.id);
        return t("mb.revoked", { email: i.email });
      },
    });
  }
  function askRemoveGuest(g: SiteGuest) {
    ask({
      title: t("mb.guest.remove.title", { email: g.email }),
      body: t("mb.guest.remove.body", { site: g.site.name }),
      label: t("mb.remove"),
      run: async (id) => {
        await api.removeGuest(id, g.id);
        return t("mb.guestRemoved", { email: g.email, site: g.site.name });
      },
    });
  }

  // ---- 邀请 ----
  type Phase = "form" | "done";
  let inviteOpen = $state(false);
  let phase = $state<Phase>("form");
  let scope = $state<"org" | "site">("org");
  let email = $state("");
  let inviteRole = $state<Exclude<Role, "owner">>("viewer");
  let guestRole = $state<"editor" | "viewer">("viewer");
  let inviteSite = $state("");
  let tried = $state(false);
  let serverEmail = $state("");
  let link = $state("");
  /** 服务端配置了 SMTP 时邀请邮件已发出（发给 `mailedTo`） */
  let mailedTo = $state("");

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

  async function createInvite(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    if (!emailOk || busy || orgId === null) return;
    const addr = email.trim();
    const body: InviteBody =
      scope === "org" ? { email: addr, role: inviteRole } : { email: addr, site_id: Number(inviteSite), site_role: guestRole };
    busy = true;
    serverEmail = "";
    try {
      const res = await api.invite(orgId, body);
      link = res.url;
      mailedTo = res.email_sent ? res.invitation.email : "";
      phase = "done";
      toast(t("inv.createdToast", { email: addr }), "good");
      void load(orgId, true);
    } catch (err) {
      if (err instanceof ApiError && err.status === 409) serverEmail = t("inv.email.dup");
      else if (err instanceof ApiError && err.status === 400 && /email/i.test(err.message)) serverEmail = err.message;
      else toast(orgErrText(err), "bad");
    } finally {
      busy = false;
    }
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
  const active = (m: OrgMember) => {
    const min = minutesSince(m.last_active_at, now);
    return min === null ? "—" : relativeTime(min, app.lang);
  };
</script>

<div class="page">
  <OrgHeader>
    {#snippet actions()}
      {#if manage}<Button variant="primary" icon={UserPlus} onclick={openInvite}>{t("mb.invite")}</Button>{/if}
    {/snippet}
  </OrgHeader>

  {#if notMember}
    <Card>
      <EmptyState icon={Users} title={t("mb.noOrg")}>
        {#snippet actions()}<Button href="/sites">{t("nav.allSites")}</Button>{/snippet}
      </EmptyState>
    </Card>
  {:else if status === "failed"}
    <Card>
      <EmptyState icon={TriangleAlert} tone="bad" title={t("mb.error.title")}>
        {#snippet actions()}<Button onclick={() => load(orgId)}>{t("common.retry")}</Button>{/snippet}
      </EmptyState>
    </Card>
  {:else if status === "loading" || !data}
    <Card flush>
      <div class="sk" aria-busy="true">
        {#each [0, 1, 2, 3] as i (i)}
          <div class="sk-row"><Skeleton width="28px" height={28} radius={14} /><Skeleton width="30%" height={12} /><Skeleton width="14%" height={12} /></div>
        {/each}
      </div>
    </Card>
  {:else}
    <Card flush>
      {#key gen}
        <Table fit columns={memberCols} rows={members} rowKey={(m) => m.user_id} caption={t("mb.title")}>
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
                  value={m.role}
                  options={assignable}
                  onchange={(r: Role) => changeRole(m, r)}
                />
              {:else}
                <span title={manage && !m.me ? lockReason(m) : undefined}>
                  <Badge>{t(`role.${m.role}`)}</Badge>
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
      {/key}
    </Card>

    {#if invites.length > 0}
      <Card title={t("mb.pending")} flush>
        <Table fit columns={inviteCols} rows={invites} rowKey={(i) => i.id} caption={t("mb.pending")}>
          {#snippet cell(i, c)}
            {#if c.key === "email"}
              <div class="who-text">
                <span>{i.email}</span>
                {#if i.site}<span class="who-mail">{i.site.name}</span>{/if}
              </div>
            {:else if c.key === "role"}
              {@const role = i.site ? i.site_role : i.role}
              {#if role}<Badge>{t(`role.${role}`)}</Badge>{/if}
            {:else if c.key === "expires"}
              <span class="muted">{t("mb.expiresIn", { n: daysLeft(i.expires_at, now) })}</span>
            {:else if c.key === "act"}
              <Button variant="ghost" size="sm" icon={X} aria-label={`${t("mb.revoke")} ${i.email}`} onclick={() => askRevoke(i)} />
            {/if}
          {/snippet}
        </Table>
      </Card>
    {/if}

    {#if guests.length > 0}
      <Card title={t("mb.guests")} flush>
        <Table fit columns={guestCols} rows={guests} rowKey={(g) => g.id} caption={t("mb.guests")}>
          {#snippet cell(g, c)}
            {#if c.key === "email"}
              <div class="guest">
                <span class="guest-mail">{g.email}</span>
                <!-- 窄屏站点列隐藏，站点名放到邮箱下面 -->
                <span class="guest-site">{g.site.name}</span>
              </div>
            {:else if c.key === "site"}
              {g.site.name}
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
    <form id="invite-form" class="form" onsubmit={createInvite} novalidate>
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
            options={invitableRoles(actor).map((r) => ({ value: r, label: t(`role.${r}`) }))}
          />
        </Field>
      {/if}
    </form>
  {:else}
    <div class="form">
      {#if mailedTo}
        <Callout tone="good">{t("inv.mailed", { email: mailedTo })}</Callout>
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
</style>
