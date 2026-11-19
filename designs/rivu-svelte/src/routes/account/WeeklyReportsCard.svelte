<!--
  邮件周报：收不收周报是个人偏好，所以放在账号页，一张卡片管所有能看的站点。
  两步：先确认一次邮箱（点确认邮件里的链接，见 routes/auth/WeeklyConfirmPage.svelte），之后每个站点一个开关，立刻生效。
  - 确认邮件一分钟只能请求一次；等待时按钮禁用并显示剩余秒数。
  - 邮箱确认一次对所有站点有效；关掉某个站点不影响确认状态，所以误关了再打开就行。
  - 实例没配置邮件：邮箱还没确认时整张卡片不显示（和「忘记密码」入口一样）；已经确认的仍然显示，提示周报暂停，开关照常可用。
  原型里：评审面板的「周报邮箱」直接切换三种状态，「周报确认邮件」演示发送失败，「邮件」演示没配置邮件；
  也可以走完整流程：发送确认邮件 → 评审面板的「周报确认链接」页点确认 → 回到这里。
-->
<script lang="ts">
  import { TriangleAlert } from "@lucide/svelte";
  import Avatar from "../../lib/ui/Avatar.svelte";
  import Badge from "../../lib/ui/Badge.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Callout from "../../lib/ui/Callout.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import Switch from "../../lib/ui/Switch.svelte";
  import { USER, type Site } from "../../lib/data/fixtures";
  import { SITES } from "../../lib/data/sites.svelte";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import { cfgs } from "../settings/store.svelte";
  import { weekly, WEEKLY_WAIT_MS } from "./weekly.svelte";
  import "../settings/strings";
  import "./strings";

  const mail = $derived(app.review.mail);
  const loading = $derived(app.review.dataState === "loading");
  const failed = $derived(["error", "busy", "timeout"].includes(app.review.dataState));

  let busy = $state<"" | "send" | "cancel">("");

  // 请求间隔的倒计时：只在等待期间每秒走一次
  let now = $state(Date.now());
  const waitLeft = $derived(Math.max(0, Math.ceil((weekly.requestedAt + WEEKLY_WAIT_MS - now) / 1000)));
  $effect(() => {
    if (weekly.requestedAt + WEEKLY_WAIT_MS <= Date.now()) return;
    now = Date.now();
    const id = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(id);
  });

  function send(resend: boolean) {
    if (busy || !mail || waitLeft > 0) return;
    busy = "send";
    setTimeout(() => {
      busy = "";
      weekly.requestedAt = Date.now();
      weekly.delivered = app.review.weeklyMail === "ok";
      // 没发出去就不进入「待确认」：留在原来的状态，页面上留一条提示
      if (!weekly.delivered) return;
      weekly.email = "pending";
      toast(t(resend ? "ac.weekly.resent" : "ac.weekly.sent"), "good");
    }, 280);
  }

  function cancel() {
    if (busy) return;
    busy = "cancel";
    setTimeout(() => {
      busy = "";
      weekly.email = "unconfirmed";
      weekly.delivered = true;
    }, 280);
  }

  function toggle(site: Site, on: boolean) {
    weekly.sites[site.id] = on;
    toast(t(on ? "ac.weekly.on" : "ac.weekly.off", { site: site.name }), on ? "good" : "default");
  }

  const schedule = (site: Site) => t("ac.weekly.schedule", { day: t(`set.day.${cfgs[site.id].weekStart}`), tz: site.timezone });
</script>

<!-- 页脚只在有按钮时出现：邮箱确认之后没有页脚，不留一条空栏 -->
{#snippet foot()}
  <span class="wait" role="timer" aria-live="off">
    {#if waitLeft > 0 && !busy}{t("ac.weekly.wait", { n: waitLeft })}{/if}
  </span>
  <div class="actions">
    {#if weekly.email === "unconfirmed"}
      <Button variant="primary" size="sm" loading={busy === "send"} disabled={waitLeft > 0} onclick={() => send(false)}>{t("ac.weekly.send")}</Button>
    {:else}
      <Button variant="ghost" size="sm" loading={busy === "cancel"} disabled={!!busy} onclick={cancel}>{t("common.cancel")}</Button>
      <Button size="sm" loading={busy === "send"} disabled={!!busy || waitLeft > 0} onclick={() => send(true)}>{t("ac.weekly.resend")}</Button>
    {/if}
  </div>
{/snippet}

{#if mail || weekly.email === "confirmed"}
  <Card title={t("ac.weekly.title")} id="weekly" footer={!loading && !failed && weekly.email !== "confirmed" ? foot : undefined}>
    {#snippet actions()}
      {#if !loading && !failed && weekly.email === "pending"}
        <span role="status"><Badge tone="warn">{t("ac.weekly.pending")}</Badge></span>
      {/if}
    {/snippet}

    {#if loading}
      <div class="sk" aria-busy="true">
        <Skeleton width="36%" height={12} />
        <Skeleton width="52%" height={12} />
      </div>
    {:else if failed}
      <EmptyState compact icon={TriangleAlert} tone="bad" title={t("ac.weekly.error")}>
        {#snippet actions()}<Button size="sm" onclick={() => toast(t("common.retry"))}>{t("common.retry")}</Button>{/snippet}
      </EmptyState>
    {:else}
      <div class="rows">
        <Field layout="row" label={t("ac.weekly.recipient")}>
          <span class="val">{USER.email}</span>
        </Field>
        <Field layout="row" label={t("ac.weekly.content")}>
          <span class="val">{t("ac.weekly.metrics")}</span>
        </Field>
      </div>

      {#if weekly.email !== "confirmed" && !weekly.delivered}
        <div class="note"><Callout tone="bad">{t("ac.weekly.sendFailed")}</Callout></div>
      {/if}
      {#if weekly.email === "pending"}
        <p class="note">{t("ac.weekly.pendingBody", { email: USER.email })}</p>
      {:else if weekly.email === "confirmed"}
        {#if !mail}
          <div class="note"><Callout tone="warn">{t("ac.weekly.paused")}</Callout></div>
        {/if}
        {#if SITES.length === 0}
          <p class="note muted">{t("ac.weekly.noSites")}</p>
        {:else}
          <ul class="sites" aria-label={t("ac.weekly.sites")}>
            {#each SITES as site (site.id)}
              <li>
                <Avatar name={site.name} shape="square" size={24} />
                <div class="who">
                  <span class="name">{site.name}</span>
                  <span class="when">{schedule(site)}</span>
                </div>
                <Switch label={t("ac.weekly.switch", { site: site.name })} checked={!!weekly.sites[site.id]} onchange={(v) => toggle(site, v)} />
              </li>
            {/each}
          </ul>
        {/if}
      {/if}
    {/if}

  </Card>
{/if}

<style>
  /* 与行布局里标签的 5px 上内边距对齐（同账号页的个人资料） */
  .val {
    padding-top: 5px;
    overflow-wrap: anywhere;
  }
  .rows {
    display: flex;
    flex-direction: column;
  }
  .note {
    margin-top: var(--sp-4);
  }
  .muted {
    color: var(--text-muted);
  }
  .sites {
    display: flex;
    flex-direction: column;
    margin: var(--sp-4) 0 0;
    padding: 0;
    list-style: none;
    border-top: var(--hairline) solid var(--border-subtle);
  }
  .sites li {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: 52px;
    padding: var(--sp-2) 0;
    border-bottom: var(--hairline) solid var(--border-subtle);
  }
  .sites li:last-child {
    padding-bottom: 0;
    border-bottom: 0;
  }
  .who {
    display: flex;
    flex: 1;
    flex-direction: column;
    min-width: 0;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: var(--fw-medium);
  }
  .when {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .sk {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .wait {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .actions {
    display: flex;
    gap: var(--sp-2);
  }
</style>
