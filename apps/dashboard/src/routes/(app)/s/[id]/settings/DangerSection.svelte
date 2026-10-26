<!--
  危险操作：删除某个日期范围的统计数据（后台任务执行，列出删除任务）；删除站点（7 天恢复期）。
  卡片只写后果和一个按钮，日期与名称确认都在对话框里（服务端要求 confirm 与站点名称完全一致）。
-->
<script lang="ts">
  import { goto } from "$app/navigation";
  import Badge from "#lib/ui/Badge.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Card from "#lib/ui/Card.svelte";
  import Dialog from "#lib/ui/Dialog.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import Table from "#lib/ui/Table.svelte";
  import type { Tone } from "#lib/ui/types.ts";
  import { api, type Site, type StatsDeletion } from "#lib/api.ts";
  import { rangeError, ymdInZone } from "#lib/admin/deletion.ts";
  import { localYmd } from "#lib/admin/expiry.ts";
  import { t } from "#lib/i18n.ts";
  import { refreshSites } from "#lib/session.svelte.ts";
  import { toast } from "#lib/state.svelte.ts";
  import { errText } from "./settings.svelte.ts";
  import "./strings";

  let { site }: { site: Site } = $props();

  // ---- 删除统计数据 ----
  let jobs = $state<StatsDeletion[]>([]);
  const active = $derived(jobs.some((j) => j.status === "pending" || j.status === "running"));

  async function loadJobs() {
    try {
      jobs = (await api.statsDeletions(site.id)).deletions;
    } catch {
      /* 列表只是辅助信息，失败时保持原样 */
    }
  }

  $effect(() => {
    void site.id;
    loadJobs();
  });

  // 有排队或执行中的任务时定期刷新状态
  $effect(() => {
    if (!active) return;
    const timer = setInterval(loadJobs, 10_000);
    return () => clearInterval(timer);
  });

  // 站点时区的今天：打开对话框时取（对话框外用不到）
  let today = $state("");
  let from = $state("");
  let to = $state("");
  const rangeErr = $derived(rangeError(from, to, today));
  const rangeOk = $derived(!!from && !!to && !rangeErr);

  let dataOpen = $state(false);
  let typedName = $state("");
  let dataBusy = $state(false);
  const dataNameOk = $derived(typedName === site.name);

  function openData() {
    today = ymdInZone(new Date(), site.timezone);
    typedName = "";
    from = to = "";
    dataOpen = true;
  }

  async function submitData() {
    if (!dataNameOk || !rangeOk || dataBusy) return;
    dataBusy = true;
    try {
      const { deletion } = await api.deleteStats(site.id, { from, to, confirm: site.name });
      jobs = [deletion, ...jobs.filter((j) => j.id !== deletion.id)];
      dataOpen = false;
      toast(t("set.danger.data.queued"), "good");
    } catch (e) {
      toast(errText(e), "bad");
    } finally {
      dataBusy = false;
    }
  }

  const tones: Record<StatsDeletion["status"], Tone> = {
    pending: "warn",
    running: "warn",
    done: "good",
    failed: "bad",
    cancelled: "neutral",
  };

  // ---- 删除站点 ----
  let siteOpen = $state(false);
  let typed = $state("");
  let busy = $state(false);
  const nameOk = $derived(typed === site.name);

  async function remove() {
    if (!nameOk || busy) return;
    busy = true;
    const name = site.name;
    try {
      await api.deleteSite(site.id, name);
      siteOpen = false;
      await refreshSites();
      toast(t("set.danger.site.done", { n: name }));
      await goto("/sites");
    } catch (e) {
      toast(errText(e), "bad");
    } finally {
      busy = false;
    }
  }
</script>

<Card title={t("set.danger.data.title")}>
  <div class="row">
    <p>{t("set.danger.data.body")}</p>
    <Button onclick={openData}>{t("set.danger.data.button")}</Button>
  </div>
</Card>

{#if jobs.length}
  <Card title={t("set.danger.jobs.title")} flush>
    <Table
      caption={t("set.danger.jobs.title")}
      rows={jobs}
      rowKey={(j) => j.id}
      columns={[
        { key: "r", label: t("set.danger.data.range") },
        { key: "s", label: t("set.danger.jobs.status"), width: "120px" },
        { key: "c", label: t("set.danger.jobs.created"), width: "120px", optional: true },
      ]}
    >
      {#snippet cell(j, c)}
        {#if c.key === "r"}<span class="num">{j.from} – {j.to}</span>
        {:else if c.key === "s"}<Badge tone={tones[j.status]}>{t(`set.danger.jobs.${j.status}`)}</Badge>
        {:else}<span class="muted num">{localYmd(new Date(j.created_at))}</span>{/if}
      {/snippet}
    </Table>
  </Card>
{/if}

<Card title={t("set.danger.site.title")}>
  <div class="row">
    <p>{t("set.danger.site.body")}</p>
    <Button
      onclick={() => {
        typed = "";
        siteOpen = true;
      }}>{t("set.danger.site.button")}</Button
    >
  </div>
</Card>

<Dialog bind:open={dataOpen} size="sm" title={t("set.danger.data.dlg.title")}>
  <div class="form">
    <Field
      label={t("set.danger.data.range")}
      hint={t("set.danger.data.rangeHint", { tz: site.timezone })}
      error={rangeErr ? t(`set.danger.data.err.${rangeErr}`) : undefined}
    >
      <div class="dates">
        <Input type="date" bind:value={from} max={today} aria-label={t("set.danger.data.from")} invalid={!!rangeErr} />
        <span class="muted" aria-hidden="true">–</span>
        <Input type="date" bind:value={to} max={today} aria-label={t("set.danger.data.to")} invalid={!!rangeErr} />
      </div>
    </Field>
    {#if rangeOk}<p>{t("set.danger.data.dlg.body", { a: from, b: to })}</p>{/if}
    <Field label={t("set.danger.typeName", { n: site.name })} for="d-data-name">
      <Input id="d-data-name" bind:value={typedName} autocomplete="off" placeholder={site.name} />
    </Field>
  </div>
  {#snippet footer()}
    <Button onclick={() => (dataOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" loading={dataBusy} disabled={!dataNameOk || !rangeOk} onclick={submitData}
      >{t("set.danger.data.confirm")}</Button
    >
  {/snippet}
</Dialog>

<Dialog bind:open={siteOpen} size="sm" title={t("set.danger.site.dlg.title")}>
  <Field label={t("set.danger.typeName", { n: site.name })} for="d-name">
    <Input id="d-name" bind:value={typed} autocomplete="off" placeholder={site.name} />
  </Field>
  {#snippet footer()}
    <Button onclick={() => (siteOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" loading={busy} disabled={!nameOk} onclick={remove}>{t("set.danger.site.confirm")}</Button>
  {/snippet}
</Dialog>

<style>
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-4);
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .dates {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .dates :global(.input) {
    flex: 1;
  }
  @media (max-width: 560px) {
    .row {
      flex-direction: column;
      align-items: flex-start;
    }
  }
</style>
