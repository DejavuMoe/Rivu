<!--
  账号与安全：个人资料、修改密码、两步验证、登录会话。主题与语言在侧栏用户菜单里设置。
-->
<script lang="ts">
  import { Monitor, Smartphone, TriangleAlert } from "@lucide/svelte";
  import PageHeader from "../../lib/app/PageHeader.svelte";
  import Badge from "../../lib/ui/Badge.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import type { Column } from "../../lib/ui/types";
  import { NOW, USER } from "../../lib/data/fixtures";
  import { relativeTime } from "../../lib/format";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import { isoDate } from "../org/data";
  import TwoFactorCard from "./TwoFactorCard.svelte";
  import "./strings";

  interface Session {
    id: string;
    device: string;
    mobile?: boolean;
    usedMinutesAgo: number;
    current?: boolean;
  }

  let sessions = $state<Session[]>([
    { id: "s1", device: "Windows Chrome", usedMinutesAgo: 0, current: true },
    { id: "s2", device: "iPhone Safari", mobile: true, usedMinutesAgo: 2 * 60 },
    { id: "s3", device: "Linux Firefox", usedMinutesAgo: 3 * 24 * 60 },
    { id: "s4", device: "Windows Edge", usedMinutesAgo: 9 * 24 * 60 },
  ]);
  const loading = $derived(app.review.dataState === "loading");
  const failed = $derived(["error", "busy", "timeout"].includes(app.review.dataState));
  const expires = (s: Session) => isoDate(new Date(NOW.getTime() - s.usedMinutesAgo * 60000 + 14 * 864e5));

  // ---- 个人资料 ----
  let name = $state(USER.name);
  let savedName = $state(USER.name);
  function saveProfile(e: SubmitEvent) {
    e.preventDefault();
    savedName = name.trim();
    toast(t("ac.saved"), "good");
  }

  // ---- 修改密码 ----
  let cur = $state("");
  let next = $state("");
  let again = $state("");
  let pwTried = $state(false);
  let pwBusy = $state(false);
  const nextLen = $derived([...next].length);
  const pwErr = $derived({
    cur: cur ? "" : t("ac.pw.currentRequired"),
    next: nextLen < 12 ? t("ac.pw.short") : nextLen > 128 ? t("ac.pw.long") : "",
    again: again === next ? "" : t("ac.pw.mismatch"),
  });
  function changePassword(e: SubmitEvent) {
    e.preventDefault();
    pwTried = true;
    if (pwErr.cur || pwErr.next || pwErr.again || pwBusy) return;
    pwBusy = true;
    setTimeout(() => {
      pwBusy = false;
      cur = next = again = "";
      pwTried = false;
      toast(t("ac.pw.done"), "good");
    }, 650);
  }

  // ---- 会话 ----
  let othersOpen = $state(false);
  const others = $derived(sessions.filter((s) => !s.current));
  function signOut(s: Session) {
    sessions = sessions.filter((x) => x.id !== s.id);
    toast(t("ac.signedOut", { device: s.device }));
  }
  function signOutOthers() {
    sessions = sessions.filter((s) => s.current);
    othersOpen = false;
    toast(t("ac.signedOutOthers"));
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

  <form onsubmit={saveProfile}>
    <Card title={t("ac.profile")}>
      <div class="rows">
        <Field layout="row" label={t("ac.name")} for="ac-name">
          <Input id="ac-name" autocomplete="name" bind:value={name} />
        </Field>
        <Field layout="row" label={t("ac.email")} for="ac-email">
          <Input id="ac-email" type="email" value={USER.email} disabled />
        </Field>
      </div>
      {#snippet footer()}
        <span></span>
        <Button type="submit" variant="primary" size="sm" disabled={!name.trim() || name.trim() === savedName}>{t("common.save")}</Button>
      {/snippet}
    </Card>
  </form>

  <form onsubmit={changePassword} novalidate>
    <Card title={t("ac.password")}>
      <div class="rows">
        <Field layout="row" label={t("ac.pw.current")} for="ac-cur" error={pwTried ? pwErr.cur : ""}>
          <Input id="ac-cur" type="password" autocomplete="current-password" bind:value={cur} invalid={pwTried && !!pwErr.cur} />
        </Field>
        <Field layout="row" label={t("ac.pw.new")} for="ac-new" hint={t("ac.pw.newHint")} error={pwTried || nextLen > 128 ? pwErr.next : ""}>
          <Input id="ac-new" type="password" autocomplete="new-password" bind:value={next} invalid={(pwTried || nextLen > 128) && !!pwErr.next} />
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

  <TwoFactorCard />

  <Card title={t("ac.sessions")} flush>
    {#snippet actions()}
      {#if others.length > 0 && !loading && !failed}
        <Button size="sm" onclick={() => (othersOpen = true)}>{t("ac.signOutOthers")}</Button>
      {/if}
    {/snippet}
    {#if loading}
      <div class="sk">
        {#each [0, 1, 2] as i (i)}
          <div class="sk-row"><Skeleton width="24%" height={12} /><Skeleton width="12%" height={12} /><Skeleton width="12%" height={12} /></div>
        {/each}
      </div>
    {:else if failed}
      <EmptyState compact icon={TriangleAlert} tone="bad" title={t("ac.sessions.error")}>
        {#snippet actions()}<Button size="sm" onclick={() => toast(t("common.retry"))}>{t("common.retry")}</Button>{/snippet}
      </EmptyState>
    {:else}
      <Table columns={cols} rows={sessions} rowKey={(s) => s.id} caption={t("ac.sessions")}>
        {#snippet cell(s, c)}
          {#if c.key === "device"}
            <div class="dev">
              {#if s.mobile}<Smartphone size={16} strokeWidth={1.75} aria-hidden="true" />{:else}<Monitor size={16} strokeWidth={1.75} aria-hidden="true" />{/if}
              <span>{s.device}</span>
              {#if s.current}<Badge>{t("ac.current")}</Badge>{/if}
            </div>
          {:else if c.key === "used"}
            <span class="muted">{relativeTime(s.usedMinutesAgo, app.lang)}</span>
          {:else if c.key === "expires"}
            <span class="muted num">{expires(s)}</span>
          {:else if c.key === "act" && !s.current}
            <Button variant="ghost" size="sm" aria-label={`${t("ac.signOut")} ${s.device}`} onclick={() => signOut(s)}>{t("ac.signOut")}</Button>
          {/if}
        {/snippet}
      </Table>
    {/if}
  </Card>
</div>

<Dialog bind:open={othersOpen} size="sm" title={t("ac.signOutOthers.title")} description={t("ac.signOutOthers.body")}>
  <span class="sr-only">{others.map((s) => s.device).join(", ")}</span>
  {#snippet footer()}
    <Button variant="ghost" onclick={() => (othersOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" onclick={signOutOthers}>{t("ac.signOutOthers")}</Button>
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
