<!--
  邮件订阅（周报、月报）：收不收是个人偏好，所以放在账号页，一张卡片管所有能看的站点。
  两步：先确认一次邮箱（点确认邮件里的链接，见 routes/auth/WeeklyConfirmPage.svelte），之后每个站点每种邮件一个开关，立刻生效。
  - 站点 × 邮件种类排成一张表：一行一个站点，一列一种邮件，站点多了也只是多几行；窄屏去掉站点头像，给名字留地方。
  - 周报在每周第一天（站点设置的每周起始日）09:00 后发上一周的，月报在每月 1 日 09:00 后发上个月的，都按站点时区。
  文件名沿用已确认的周报卡片，生产里对应同名文件。
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
  import type { Site } from "../../lib/data/fixtures";
  import { account } from "./email.svelte";
  import { SITES } from "../../lib/data/sites.svelte";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import { weekly, WEEKLY_WAIT_MS } from "./weekly.svelte";
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

  type Kind = "weekly" | "monthly";
  const KINDS: Kind[] = ["weekly", "monthly"];
  const subs = (kind: Kind) => (kind === "weekly" ? weekly.sites : weekly.monthly);

  function toggle(kind: Kind, site: Site, on: boolean) {
    subs(kind)[site.id] = on;
    toast(t(`ac.${kind}.${on ? "on" : "off"}`, { site: site.name }), on ? "good" : "default");
  }
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
          <span class="val">{account.email}</span>
        </Field>
        {#each KINDS as kind (kind)}
          <Field layout="row" label={t(`ac.${kind}.name`)}>
            <span class="val">{t(`ac.${kind}.about`)}</span>
          </Field>
        {/each}
      </div>
      <p class="note muted">{t("ac.weekly.perSite")}</p>

      {#if weekly.email !== "confirmed" && !weekly.delivered}
        <div class="note"><Callout tone="bad">{t("ac.weekly.sendFailed")}</Callout></div>
      {/if}
      {#if weekly.email === "pending"}
        <p class="note">{t("ac.weekly.pendingBody", { email: account.email })}</p>
      {:else if weekly.email === "confirmed"}
        {#if !mail}
          <div class="note"><Callout tone="warn">{t("ac.weekly.paused")}</Callout></div>
        {/if}
        {#if SITES.length === 0}
          <p class="note muted">{t("ac.weekly.noSites")}</p>
        {:else}
          <div class="scroll">
            <table class="matrix">
              <caption class="sr-only">{t("ac.weekly.sites")}</caption>
              <thead>
                <tr>
                  <th scope="col" class="site"><span class="sr-only">{t("ac.weekly.site")}</span></th>
                  {#each KINDS as kind (kind)}<th scope="col">{t(`ac.${kind}.col`)}</th>{/each}
                </tr>
              </thead>
              <tbody>
                {#each SITES as site (site.id)}
                  <tr>
                    <th scope="row" class="site">
                      <span class="who">
                        <span class="av"><Avatar name={site.name} shape="square" size={24} /></span>
                        <span class="name">{site.name}</span>
                      </span>
                    </th>
                    {#each KINDS as kind (kind)}
                      <td>
                        <Switch label={t(`ac.${kind}.switch`, { site: site.name })} checked={!!subs(kind)[site.id]} onchange={(v) => toggle(kind, site, v)} />
                      </td>
                    {/each}
                  </tr>
                {/each}
              </tbody>
            </table>
          </div>
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
  .scroll {
    margin-top: var(--sp-4);
    overflow-x: auto;
  }
  .matrix {
    width: 100%;
    border-collapse: collapse;
  }
  .matrix th,
  .matrix td {
    padding: var(--sp-2) 0;
    border-bottom: var(--hairline) solid var(--border-subtle);
    font-weight: var(--fw-regular);
    text-align: center;
  }
  .matrix thead th {
    width: 88px;
    padding-top: 0;
    color: var(--text-muted);
    font-size: var(--fs-sm);
    font-weight: var(--fw-medium);
  }
  .matrix thead th.site {
    width: auto;
  }
  .matrix tbody tr:last-child > * {
    padding-bottom: 0;
    border-bottom: 0;
  }
  .matrix td {
    height: 52px;
  }
  .matrix td :global(.switch) {
    vertical-align: middle;
  }
  /* 名字列占剩下的宽度，过长时省略 */
  .matrix th.site {
    max-width: 0;
    text-align: left;
  }
  .who {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
  }
  .av {
    display: flex;
    flex: none;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: var(--fw-medium);
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
  @media (max-width: 480px) {
    .matrix thead th {
      width: 64px;
    }
    .matrix thead th.site {
      width: auto;
    }
    .av {
      display: none;
    }
  }
</style>
