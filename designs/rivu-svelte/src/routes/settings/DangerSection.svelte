<!--
  危险操作：删除某个日期范围的统计数据；删除站点（7 天恢复期）。
  卡片只写后果和一个按钮，日期与名称确认都在对话框里。
-->
<script lang="ts">
  import Badge from "../../lib/ui/Badge.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import { t } from "../../lib/i18n";
  import { navigate } from "../../lib/router.svelte";
  import { toast } from "../../lib/state.svelte";
  import { NOW, type Site } from "../../lib/data/fixtures";
  import { deleteSite as removeSite } from "../../lib/data/sites.svelte";
  import { cfgs, nextId, ymd } from "./store.svelte";
  import "./strings";

  let { site, readOnly = false }: { site: Site; readOnly?: boolean } = $props();
  const cfg = $derived(cfgs[site.id]);

  const today = ymd(NOW);
  let from = $state("");
  let to = $state("");
  const rangeErr = $derived.by((): string | null => {
    if (!from || !to) return null;
    if (from > to) return "set.danger.data.errOrder";
    if (to > today) return "set.danger.data.errFuture";
    return null;
  });
  const rangeOk = $derived(!!from && !!to && !rangeErr);

  let dataOpen = $state(false);
  let typedName = $state("");
  const nameOk = $derived(typedName.trim() === cfg.name);

  function submitData() {
    if (!nameOk || !rangeOk) return;
    cfg.jobs.unshift({ id: nextId(), from, to, status: "pending", createdAt: NOW });
    dataOpen = false;
    toast(t("set.danger.data.queued"), "good");
    from = to = "";
  }

  let siteOpen = $state(false);
  let typedDomain = $state("");
  const domainOk = $derived(typedDomain.trim() === site.domain);
  function deleteSite() {
    if (!domainOk) return;
    siteOpen = false;
    removeSite(site.id);
    toast(t("set.danger.site.done", { d: site.domain }));
    navigate("/sites");
  }
</script>

<Card title={t("set.danger.data.title")}>
  <div class="row">
    <p>{t("set.danger.data.body")}</p>
    {#if !readOnly}
      <Button
        onclick={() => {
          typedName = "";
          from = to = "";
          dataOpen = true;
        }}>{t("set.danger.data.button")}</Button
      >
    {/if}
  </div>
</Card>

{#if cfg.jobs.length}
  <Card title={t("set.danger.jobs.title")} flush>
    <Table
      caption={t("set.danger.jobs.title")}
      rows={cfg.jobs}
      rowKey={(j) => j.id}
      columns={[
        { key: "r", label: t("set.danger.data.range") },
        { key: "s", label: t("set.danger.jobs.status"), width: "120px" },
        { key: "c", label: t("set.danger.jobs.created"), width: "120px", optional: true },
      ]}
    >
      {#snippet cell(j, c)}
        {#if c.key === "r"}<span class="num">{j.from} – {j.to}</span>
        {:else if c.key === "s"}<Badge tone="warn">{t(`set.danger.jobs.${j.status}`)}</Badge>
        {:else}<span class="muted num">{ymd(j.createdAt)}</span>{/if}
      {/snippet}
    </Table>
  </Card>
{/if}

<Card title={t("set.danger.site.title")}>
  <div class="row">
    <p>{t("set.danger.site.body")}</p>
    {#if !readOnly}
      <Button
        onclick={() => {
          typedDomain = "";
          siteOpen = true;
        }}>{t("set.danger.site.button")}</Button
      >
    {/if}
  </div>
</Card>

<Dialog bind:open={dataOpen} size="sm" title={t("set.danger.data.dlg.title")}>
  <div class="form">
    <Field label={t("set.danger.data.range")} hint={t("set.danger.data.rangeHint", { tz: cfg.timezone })} error={rangeErr ? t(rangeErr) : undefined}>
      <div class="dates">
        <Input type="date" bind:value={from} max={today} aria-label={t("set.danger.data.from")} invalid={!!rangeErr} />
        <span class="muted" aria-hidden="true">–</span>
        <Input type="date" bind:value={to} max={today} aria-label={t("set.danger.data.to")} invalid={!!rangeErr} />
      </div>
    </Field>
    {#if rangeOk}<p>{t("set.danger.data.dlg.body", { a: from, b: to })}</p>{/if}
    <Field label={t("set.danger.typeName", { n: cfg.name })} for="d-name">
      <Input id="d-name" bind:value={typedName} autocomplete="off" placeholder={cfg.name} />
    </Field>
  </div>
  {#snippet footer()}
    <Button onclick={() => (dataOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" disabled={!nameOk || !rangeOk} onclick={submitData}>{t("set.danger.data.confirm")}</Button>
  {/snippet}
</Dialog>

<Dialog bind:open={siteOpen} size="sm" title={t("set.danger.site.dlg.title")}>
  <Field label={t("set.danger.typeName", { n: site.domain })} for="d-domain">
    <Input id="d-domain" mono bind:value={typedDomain} autocomplete="off" placeholder={site.domain} />
  </Field>
  {#snippet footer()}
    <Button onclick={() => (siteOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" disabled={!domainOk} onclick={deleteSite}>{t("set.danger.site.confirm")}</Button>
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
