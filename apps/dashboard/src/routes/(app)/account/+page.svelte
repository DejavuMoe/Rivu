<!--
  账号与安全：个人资料（只读）、修改密码、登录会话。主题与语言在侧栏用户菜单里设置。
  后端目前没有改名、改邮箱的接口。改密码成功后服务端撤销全部旧会话并换发新会话（cookie 已更新），会话列表重新加载。
-->
<script lang="ts">
  import { Monitor, Smartphone, TriangleAlert } from "@lucide/svelte";
  import { goto } from "$app/navigation";
  import PageHeader from "#lib/app/PageHeader.svelte";
  import Badge from "#lib/ui/Badge.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Card from "#lib/ui/Card.svelte";
  import Dialog from "#lib/ui/Dialog.svelte";
  import EmptyState from "#lib/ui/EmptyState.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import Table from "#lib/ui/Table.svelte";
  import type { Column } from "#lib/ui/types.ts";
  import { api, ApiError, type SessionInfo } from "#lib/api.ts";
  import { relativeTime } from "#lib/format.ts";
  import { t } from "#lib/i18n.ts";
  import { clearSession, session } from "#lib/session.svelte.ts";
  import { app, toast } from "#lib/state.svelte.ts";
  import "./strings";

  let sessions = $state<SessionInfo[]>([]);
  let status = $state<"loading" | "ready" | "failed">("loading");
  let allOpen = $state(false);
  let busy = $state(false);

  const me = $derived(session.me?.user);
  const device = (s: SessionInfo) => s.device_label || t("ac.unknownDevice");
  const mobile = (s: SessionInfo) => /iphone|ipad|android|mobile/i.test(s.device_label ?? "");
  const minutesAgo = (iso: string) => Math.max(0, Math.floor((Date.now() - new Date(iso).getTime()) / 60000));
  const date = (iso: string) => new Date(iso).toLocaleDateString(app.lang === "zh" ? "zh-CN" : "en-US");

  /** 会话已失效（401）就回登录页；其余错误交给调用方 */
  async function unauthorized(e: unknown): Promise<boolean> {
    if (e instanceof ApiError && e.status === 401) {
      clearSession();
      await goto("/login");
      return true;
    }
    return false;
  }

  async function load() {
    status = "loading";
    try {
      sessions = (await api.sessions()).sessions;
      status = "ready";
    } catch (e) {
      if (!(await unauthorized(e))) status = "failed";
    }
  }
  void load();

  // ---- 修改密码 ----
  let cur = $state("");
  let next = $state("");
  let again = $state("");
  let pwTried = $state(false);
  let pwBusy = $state(false);
  /** 服务端返回的字段错误（输入变了就清掉） */
  let serverErr = $state<{ cur?: string; next?: string }>({});
  const nextLen = $derived([...next].length);
  const pwErr = $derived({
    cur: serverErr.cur ?? (cur ? "" : t("ac.pw.currentRequired")),
    next: serverErr.next ?? (nextLen < 12 ? t("ac.pw.short") : nextLen > 128 ? t("ac.pw.long") : ""),
    again: again === next ? "" : t("ac.pw.mismatch"),
  });

  async function changePassword(e: SubmitEvent) {
    e.preventDefault();
    pwTried = true;
    if (pwErr.cur || pwErr.next || pwErr.again || pwBusy) return;
    pwBusy = true;
    try {
      await api.changePassword({ current: cur, new: next });
    } catch (err) {
      pwBusy = false;
      if (await unauthorized(err)) return;
      if (err instanceof ApiError && err.code === "wrong_password") serverErr = { cur: t("ac.pw.wrong") };
      else if (err instanceof ApiError && err.code === "invalid_new_password") serverErr = { next: nextLen > 128 ? t("ac.pw.long") : t("ac.pw.short") };
      else if (err instanceof ApiError && err.code === "same_password") serverErr = { next: t("ac.pw.same") };
      else if (err instanceof ApiError && err.status === 429) toast(t("ac.pw.rate"), "bad");
      else toast(t("ac.failed"), "bad");
      return;
    }
    pwBusy = false;
    cur = next = again = "";
    pwTried = false;
    serverErr = {};
    toast(t("ac.pw.done"), "good");
    // 其他设备的会话已被服务端撤销，只剩换发的这一个
    void load();
  }

  async function revoke(s: SessionInfo) {
    try {
      await api.revokeSession(s.id);
    } catch (e) {
      if (!(await unauthorized(e))) toast(t("ac.failed"), "bad");
      return;
    }
    if (s.current) {
      clearSession();
      await goto("/login");
      return;
    }
    sessions = sessions.filter((x) => x.id !== s.id);
    toast(t("ac.signedOut", { device: device(s) }));
  }

  async function revokeAll() {
    busy = true;
    try {
      await api.logoutAll();
    } catch (e) {
      busy = false;
      allOpen = false;
      if (!(await unauthorized(e))) toast(t("ac.failed"), "bad");
      return;
    }
    clearSession();
    await goto("/login");
  }

  const cols = $derived<Column[]>([
    { key: "device", label: t("ac.col.device") },
    { key: "used", label: t("ac.col.used"), width: "130px" },
    { key: "expires", label: t("ac.col.expires"), optional: true, width: "130px" },
    { key: "act", label: "", align: "right", width: "80px" },
  ]);
</script>

<div class="page">
  <PageHeader title={t("ac.title")} />

  <Card title={t("ac.profile")}>
    <div class="rows">
      <!-- 还没有修改资料的接口：只读，显示成文字而不是禁用的输入框 -->
      <Field layout="row" label={t("ac.name")}>
        <span class="val">{me?.name || "—"}</span>
      </Field>
      <Field layout="row" label={t("ac.email")}>
        <span class="val">{me?.email}</span>
      </Field>
    </div>
  </Card>

  <form onsubmit={changePassword} novalidate>
    <Card title={t("ac.password")}>
      <div class="rows">
        <Field layout="row" label={t("ac.pw.current")} for="ac-cur" error={pwTried ? pwErr.cur : ""}>
          <Input
            id="ac-cur"
            type="password"
            autocomplete="current-password"
            bind:value={cur}
            oninput={() => (serverErr = { ...serverErr, cur: undefined })}
            invalid={pwTried && !!pwErr.cur}
          />
        </Field>
        <Field layout="row" label={t("ac.pw.new")} for="ac-new" hint={t("ac.pw.newHint")} error={pwTried || nextLen > 128 ? pwErr.next : ""}>
          <Input
            id="ac-new"
            type="password"
            autocomplete="new-password"
            bind:value={next}
            oninput={() => (serverErr = { ...serverErr, next: undefined })}
            invalid={(pwTried || nextLen > 128) && !!pwErr.next}
          />
        </Field>
        <Field layout="row" label={t("ac.pw.confirm")} for="ac-again" error={pwTried ? pwErr.again : ""}>
          <Input id="ac-again" type="password" autocomplete="new-password" bind:value={again} invalid={pwTried && !!pwErr.again} />
        </Field>
      </div>
      {#snippet footer()}
        <span></span>
        <Button type="submit" variant="primary" size="sm" loading={pwBusy}>{t("ac.pw.submit")}</Button>
      {/snippet}
    </Card>
  </form>

  <Card title={t("ac.sessions")} flush>
    {#snippet actions()}
      {#if status === "ready"}
        <Button size="sm" onclick={() => (allOpen = true)}>{t("ac.signOutAll")}</Button>
      {/if}
    {/snippet}
    {#if status === "loading"}
      <div class="sk">
        {#each [0, 1, 2] as i (i)}
          <div class="sk-row"><Skeleton width="24%" height={12} /><Skeleton width="12%" height={12} /><Skeleton width="12%" height={12} /></div>
        {/each}
      </div>
    {:else if status === "failed"}
      <EmptyState compact icon={TriangleAlert} tone="bad" title={t("ac.sessions.error")}>
        {#snippet actions()}<Button size="sm" onclick={load}>{t("common.retry")}</Button>{/snippet}
      </EmptyState>
    {:else}
      <Table columns={cols} rows={sessions} rowKey={(s) => s.id} caption={t("ac.sessions")}>
        {#snippet cell(s, c)}
          {#if c.key === "device"}
            <div class="dev">
              {#if mobile(s)}<Smartphone size={16} strokeWidth={1.75} aria-hidden="true" />{:else}<Monitor size={16} strokeWidth={1.75} aria-hidden="true" />{/if}
              <span>{device(s)}</span>
              {#if s.current}<Badge>{t("ac.current")}</Badge>{/if}
            </div>
          {:else if c.key === "used"}
            <span class="muted">{relativeTime(minutesAgo(s.last_used_at), app.lang)}</span>
          {:else if c.key === "expires"}
            <span class="muted num">{date(s.expires_at)}</span>
          {:else if c.key === "act"}
            <Button variant="ghost" size="sm" aria-label={`${t("ac.signOut")} ${device(s)}`} onclick={() => revoke(s)}>{t("ac.signOut")}</Button>
          {/if}
        {/snippet}
      </Table>
    {/if}
  </Card>
</div>

<Dialog bind:open={allOpen} size="sm" title={t("ac.signOutAll.title")} description={t("ac.signOutAll.body")}>
  <span></span>
  {#snippet footer()}
    <Button variant="ghost" onclick={() => (allOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" loading={busy} onclick={revokeAll}>{t("ac.signOutAll")}</Button>
  {/snippet}
</Dialog>

<style>
  /* 与行布局里标签的 5px 上内边距对齐 */
  .val {
    padding-top: 5px;
  }
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
  .rows {
    display: flex;
    flex-direction: column;
  }
  .rows :global(.field.row .control) {
    max-width: 360px;
  }
  .dev {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .dev :global(svg) {
    color: var(--text-muted);
    flex: none;
  }
  .sk {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    padding: var(--sp-5);
  }
  .sk-row {
    display: flex;
    gap: var(--sp-6);
  }
  @media (max-width: 720px) {
    .page {
      padding: var(--sp-4) var(--sp-4) var(--sp-10);
    }
  }
</style>
