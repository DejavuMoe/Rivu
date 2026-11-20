<!--
  账号与安全：个人资料（名字只读，登录邮箱可以修改，见 LoginEmail.svelte）、修改密码、两步验证、邮件订阅（周报、月报、流量突增）、登录会话。主题与语言在侧栏用户菜单里设置。
  后端目前没有改名的接口。改密码或改邮箱成功后服务端撤销其他设备的会话，会话列表只剩当前这一个。
  评审面板的「登录结果」演示修改密码时当前密码错误、被限流与其他失败；新密码与当前密码相同由原型直接判断。
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
  import { app, passwordResult, toast } from "../../lib/state.svelte";
  import { navigate } from "../../lib/router.svelte";
  import LoginEmail from "./LoginEmail.svelte";
  import TwoFactorCard from "./TwoFactorCard.svelte";
  import WeeklyReportsCard from "./WeeklyReportsCard.svelte";
  import { account } from "./email.svelte";
  import "./strings";

  interface Session {
    id: string;
    /** 设备标签：客户端没有给时为 null，显示「未知设备」 */
    device: string | null;
    mobile?: boolean;
    usedMinutesAgo: number;
    current?: boolean;
  }

  let sessions = $state<Session[]>([
    { id: "s1", device: "Windows Chrome", usedMinutesAgo: 0, current: true },
    { id: "s2", device: "iPhone Safari", mobile: true, usedMinutesAgo: 2 * 60 },
    { id: "s3", device: "Linux Firefox", usedMinutesAgo: 3 * 24 * 60 },
    { id: "s4", device: "Windows Edge", usedMinutesAgo: 9 * 24 * 60 },
    { id: "s5", device: null, usedMinutesAgo: 12 * 24 * 60 },
  ].filter((s) => s.current || !account.othersSignedOut));
  // 改邮箱生效后其他设备已退出（没配置邮件时在这个页面上立即生效）
  $effect(() => {
    if (account.othersSignedOut) sessions = sessions.filter((s) => s.current);
  });
  const loading = $derived(app.review.dataState === "loading");
  const failed = $derived(["error", "busy", "timeout"].includes(app.review.dataState));
  const device = (s: Session) => s.device || t("ac.unknownDevice");
  const date = (s: Session) =>
    new Date(NOW.getTime() - s.usedMinutesAgo * 60000 + 14 * 864e5).toLocaleDateString(app.lang === "zh" ? "zh-CN" : "en-US");

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
  function changePassword(e: SubmitEvent) {
    e.preventDefault();
    pwTried = true;
    if (pwErr.cur || pwErr.next || pwErr.again || pwBusy) return;
    pwBusy = true;
    setTimeout(() => {
      pwBusy = false;
      const r = passwordResult();
      if (r === "wrong") serverErr = { cur: t("ac.pw.wrong") };
      else if (next === cur) serverErr = { next: t("ac.pw.same") };
      else if (r === "rate") toast(t("ac.pw.rate"), "bad");
      else if (r === "failed") toast(t("ac.failed"), "bad");
      else {
        cur = next = again = "";
        pwTried = false;
        serverErr = {};
        toast(t("ac.pw.done"), "good");
        // 其他设备的会话已被服务端撤销，只剩换发的这一个
        sessions = sessions.filter((s) => s.current);
      }
    }, 650);
  }

  // ---- 会话 ----
  let allOpen = $state(false);
  let busy = $state(false);
  function revoke(s: Session) {
    if (s.current) {
      navigate("/login");
      return;
    }
    sessions = sessions.filter((x) => x.id !== s.id);
    toast(t("ac.signedOut", { device: device(s) }));
  }
  function revokeAll() {
    busy = true;
    setTimeout(() => {
      busy = false;
      navigate("/login");
    }, 500);
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
      <!-- 还没有改名的接口：只读，显示成文字而不是禁用的输入框 -->
      <Field layout="row" label={t("ac.name")}>
        <span class="val">{USER.name || "—"}</span>
      </Field>
      <Field layout="row" label={t("ac.email")}>
        <LoginEmail />
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

  <TwoFactorCard />

  <WeeklyReportsCard />

  <Card title={t("ac.sessions")} flush>
    {#snippet actions()}
      {#if !loading && !failed}
        <Button size="sm" onclick={() => (allOpen = true)}>{t("ac.signOutAll")}</Button>
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
              <span>{device(s)}</span>
              {#if s.current}<Badge>{t("ac.current")}</Badge>{/if}
            </div>
          {:else if c.key === "used"}
            <span class="muted">{relativeTime(s.usedMinutesAgo, app.lang)}</span>
          {:else if c.key === "expires"}
            <span class="muted num">{date(s)}</span>
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
