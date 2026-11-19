<!--
  邮件周报：收不收周报是个人偏好，所以放在账号页，一张卡片管所有能看的站点。
  两步：先确认一次邮箱（点确认邮件里的链接，见 (auth)/weekly-report/confirm/[token]），之后每个站点一个开关，立刻生效。
  - 确认邮件每个账号一分钟只能请求一次；等待时按钮禁用并显示剩余秒数（429 时用 Retry-After）。
  - 待确认时用户多半在别的标签页或设备上点链接：回到这个页面时重新取一次状态，站点列表自己出现。
  - 开关立即生效：先改界面，请求失败再改回并提示；邮箱已不是确认状态（409）则重新取状态。
  - 实例没配置邮件：邮箱还没确认时整张卡片不显示；已经确认的仍然显示，提示周报暂停，开关照常可用。
  - 确认邮件发送失败（502）：邮箱状态以服务端为准（重新取），页面留一条提示。
  逻辑（可见性、倒计时、错误映射）在 lib/account/weekly.ts。
-->
<script lang="ts">
  import { TriangleAlert } from "@lucide/svelte";
  import Avatar from "#lib/ui/Avatar.svelte";
  import Badge from "#lib/ui/Badge.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Callout from "#lib/ui/Callout.svelte";
  import Card from "#lib/ui/Card.svelte";
  import EmptyState from "#lib/ui/EmptyState.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import Switch from "#lib/ui/Switch.svelte";
  import { api, ApiError, type WeeklyReport, type WeeklyReportSite } from "#lib/api.ts";
  import {
    cardVisible,
    classifySendError,
    classifyToggleError,
    shouldRefreshOnReturn,
    waitLeft,
    WEEKLY_WAIT_SECONDS,
  } from "#lib/account/weekly.ts";
  import { t } from "#lib/i18n.ts";
  import { app, toast } from "#lib/state.svelte.ts";
  import "./strings";

  let { unauthorized }: { unauthorized: (e: unknown) => Promise<boolean> } = $props();

  let data = $state<WeeklyReport | null>(null);
  let status = $state<"loading" | "ready" | "failed">("loading");
  let busy = $state<"" | "send" | "cancel">("");
  /** 上一次请求确认邮件被邮件服务拒绝（502）：页面留一条提示，下一次成功的操作后清掉 */
  let sendFailed = $state(false);
  /** 正在提交开关的站点：同一站点的请求不叠加 */
  let toggling = $state<Record<number, boolean>>({});

  /** 首次加载与「重试」：显示骨架 */
  async function load() {
    status = "loading";
    try {
      data = await api.weeklyReport();
      status = "ready";
    } catch (e) {
      if (!(await unauthorized(e))) status = "failed";
    }
  }
  void load();

  let refreshing = false;
  /** 静默重取：不闪骨架；失败就保留现有内容 */
  async function refresh() {
    if (refreshing) return;
    refreshing = true;
    try {
      const fresh = await api.weeklyReport();
      // 正在提交的开关以界面上的值为准：这次读取可能早于它的请求落库
      for (const s of fresh.sites) {
        if (toggling[s.id]) s.subscribed = data?.sites.find((x) => x.id === s.id)?.subscribed ?? s.subscribed;
      }
      data = fresh;
      status = "ready";
    } catch (e) {
      await unauthorized(e);
    } finally {
      refreshing = false;
    }
  }

  // 窗口重新获得焦点或页面重新可见：待确认时重取
  function onReturn() {
    if (shouldRefreshOnReturn(data?.email_status, document.visibilityState === "visible")) void refresh();
  }

  // 请求间隔的倒计时：只在等待期间每秒走一次
  let waitStart = $state(0);
  let waitSeconds = $state(WEEKLY_WAIT_SECONDS);
  let now = $state(Date.now());
  const left = $derived(waitLeft(waitStart, now, waitSeconds));
  const waiting = $derived(left > 0);
  $effect(() => {
    if (!waiting) return;
    const id = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(id);
  });

  function startWait(seconds: number) {
    waitStart = Date.now();
    waitSeconds = seconds;
    now = waitStart;
  }

  async function send(resend: boolean) {
    if (busy || left > 0) return;
    busy = "send";
    try {
      data = await api.requestWeeklyConfirmation(app.lang);
      sendFailed = false;
      // 邮箱已经在别的标签页或设备上确认了：服务端不发邮件，直接显示站点列表
      if (data.email_status === "confirmed") return;
      startWait(WEEKLY_WAIT_SECONDS);
      toast(t(resend ? "ac.weekly.resent" : "ac.weekly.sent"), "good");
    } catch (e) {
      if (await unauthorized(e)) return;
      const out = e instanceof ApiError ? classifySendError(e) : ({ kind: "error" } as const);
      if (out.kind === "wait") startWait(out.seconds);
      else if (out.kind === "mail_failed") {
        // 邮箱状态与请求前相同，以服务端为准；这次请求仍计入每分钟一次的限制
        sendFailed = true;
        startWait(WEEKLY_WAIT_SECONDS);
        await refresh();
      } else if (out.kind === "unavailable") await refresh();
      else toast(t("ac.failed"), "bad");
    } finally {
      busy = "";
    }
  }

  async function cancel() {
    if (busy) return;
    busy = "cancel";
    try {
      await api.cancelWeeklyConfirmation();
      sendFailed = false;
      await refresh();
    } catch (e) {
      if (!(await unauthorized(e))) toast(t("ac.failed"), "bad");
    } finally {
      busy = "";
    }
  }

  function setSubscribed(id: number, on: boolean) {
    const site = data?.sites.find((x) => x.id === id);
    if (site) site.subscribed = on;
  }

  async function toggle(site: WeeklyReportSite, on: boolean) {
    // Switch 已经把自己的状态改成了 on：同步到数据（界面先变），请求失败再改回
    setSubscribed(site.id, on);
    toggling[site.id] = true;
    try {
      if (on) await api.subscribeWeekly(site.id, app.lang);
      else await api.unsubscribeWeekly(site.id);
      toast(t(on ? "ac.weekly.on" : "ac.weekly.off", { site: site.name }), on ? "good" : "default");
    } catch (e) {
      setSubscribed(site.id, !on);
      if (await unauthorized(e)) return;
      const out = e instanceof ApiError ? classifyToggleError(e) : "error";
      toast(t("ac.failed"), "bad");
      if (out !== "error") await refresh();
    } finally {
      toggling[site.id] = false;
    }
  }

  const schedule = (site: WeeklyReportSite) =>
    t("ac.weekly.schedule", { day: t(`ac.weekly.day.${site.week_starts_on}`), tz: site.timezone });

  const visible = $derived(cardVisible(data));
  const emailStatus = $derived(data?.email_status ?? "unconfirmed");
</script>

<svelte:window onfocus={onReturn} />
<svelte:document onvisibilitychange={onReturn} />

<!-- 页脚只在有按钮时出现：邮箱确认之后没有页脚，不留一条空栏 -->
{#snippet foot()}
  <span class="wait" role="timer" aria-live="off">
    {#if left > 0 && !busy}{t("ac.weekly.wait", { n: left })}{/if}
  </span>
  <div class="actions">
    {#if emailStatus === "unconfirmed"}
      <Button variant="primary" size="sm" loading={busy === "send"} disabled={left > 0} onclick={() => send(false)}>{t("ac.weekly.send")}</Button>
    {:else}
      <Button variant="ghost" size="sm" loading={busy === "cancel"} disabled={!!busy} onclick={cancel}>{t("common.cancel")}</Button>
      <Button size="sm" loading={busy === "send"} disabled={!!busy || left > 0} onclick={() => send(true)}>{t("ac.weekly.resend")}</Button>
    {/if}
  </div>
{/snippet}

{#if visible}
  <Card title={t("ac.weekly.title")} id="weekly" footer={status === "ready" && emailStatus !== "confirmed" ? foot : undefined}>
    {#snippet actions()}
      {#if status === "ready" && emailStatus === "pending"}
        <span role="status"><Badge tone="warn">{t("ac.weekly.pending")}</Badge></span>
      {/if}
    {/snippet}

    {#if status === "loading"}
      <div class="sk" aria-busy="true">
        <Skeleton width="36%" height={12} />
        <Skeleton width="52%" height={12} />
      </div>
    {:else if status === "failed" || !data}
      <EmptyState compact icon={TriangleAlert} tone="bad" title={t("ac.weekly.error")}>
        {#snippet actions()}<Button size="sm" onclick={load}>{t("common.retry")}</Button>{/snippet}
      </EmptyState>
    {:else}
      <div class="rows">
        <Field layout="row" label={t("ac.weekly.recipient")}>
          <span class="val">{data.email}</span>
        </Field>
        <Field layout="row" label={t("ac.weekly.content")}>
          <span class="val">{t("ac.weekly.metrics")}</span>
        </Field>
      </div>

      {#if emailStatus !== "confirmed" && sendFailed}
        <div class="note"><Callout tone="bad">{t("ac.weekly.sendFailed")}</Callout></div>
      {/if}
      {#if emailStatus === "pending"}
        <p class="note">{t("ac.weekly.pendingBody", { email: data.email })}</p>
      {:else if emailStatus === "confirmed"}
        {#if !data.available}
          <div class="note"><Callout tone="warn">{t("ac.weekly.paused")}</Callout></div>
        {/if}
        {#if data.sites.length === 0}
          <p class="note muted">{t("ac.weekly.noSites")}</p>
        {:else}
          <ul class="sites" aria-label={t("ac.weekly.sites")}>
            {#each data.sites as site (site.id)}
              <li>
                <Avatar name={site.name} shape="square" size={24} />
                <div class="who">
                  <span class="name">{site.name}</span>
                  <span class="when">{schedule(site)}</span>
                </div>
                <Switch
                  label={t("ac.weekly.switch", { site: site.name })}
                  checked={site.subscribed}
                  disabled={!!toggling[site.id]}
                  onchange={(v) => toggle(site, v)}
                />
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
