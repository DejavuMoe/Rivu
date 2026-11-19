<!--
  属性卡片：标题 + 属性键下拉（范围里出现过的键，事件多的在前）+ 条形列表（前 8 个取值）+ 「查看全部」。
  点击行即按「这个键 = 这个取值」筛选；没有这个键的事件归为 (none)。范围里没有任何属性时整张卡片不显示。
  加载中也不显示（键列表还没回来）；查询失败时是带标题的错误卡片。
-->
<script lang="ts">
  import { CircleAlert, Inbox, Maximize2 } from "@lucide/svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Select from "../../lib/ui/Select.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import BarList from "../../lib/charts/BarList.svelte";
  import type { BarRow } from "../../lib/charts/types";
  import { t } from "../../lib/i18n";
  import { app } from "../../lib/state.svelte";
  import { compact, percent } from "../../lib/format";
  import { PROP_PREFIX, simulateFetch, type Row } from "./query";
  import { displayValue } from "./values";
  import BreakdownDialog from "./BreakdownDialog.svelte";

  interface Props {
    keys: string[];
    rowsOf: (dim: string) => Row[];
    total: number;
    onpick: (dim: string, value: string) => void;
    loading?: boolean;
    /** 查询失败（出错、排队、超时都显示成同一个错误） */
    failed?: boolean;
  }
  let { keys, rowsOf, total, onpick, loading = false, failed = false }: Props = $props();

  let key = $state("");
  $effect.pre(() => {
    if (!keys.includes(key)) key = keys[0] ?? "";
  });
  let full = $state(false);

  const dim = $derived(`${PROP_PREFIX}${key}`);
  const rows = $derived(key ? rowsOf(dim) : []);
  const toBars = (rs: Row[]): BarRow[] =>
    rs.map((r) => ({ key: r.label, label: displayValue(dim, r.label, app.lang), value: r.visitors }));
  const bars = $derived(toBars(rows));
</script>

{#if failed}
  <Card title={t("dash.card.props")}>
    <EmptyState compact icon={CircleAlert} tone="bad" title={t("dash.error.title")} />
  </Card>
{:else if keys.length && !loading}
  <Card>
    {#snippet header()}
      <div class="head">
        <h2 class="title">{t("dash.card.props")}</h2>
        <div class="tools">
          <Select
            size="sm"
            variant="ghost"
            align="end"
            label={t("dash.propKey")}
            bind:value={key}
            options={keys.map((k) => ({ value: k, label: k }))}
          />
          <Button
            variant="ghost"
            size="sm"
            icon={Maximize2}
            aria-label={t("common.viewAll")}
            title={t("common.viewAll")}
            onclick={() => (full = true)}
            disabled={!rows.length}
          />
        </div>
      </div>
    {/snippet}
    {#if !rows.length}
      <EmptyState compact icon={Inbox} title={t("dash.noRows")} />
    {:else}
      <BarList
        rows={bars.slice(0, 8)}
        dimLabel={key}
        metricLabel={t("metric.visitors")}
        format={(n) => compact(n, app.lang)}
        {total}
        percent={(r) => percent(r, 0)}
        pickLabel={t("dash.addAsFilter")}
        onpick={(r) => onpick(dim, r.key)}
      />
    {/if}
  </Card>

  {#if full}
    <BreakdownDialog
      bind:open={full}
      title={key}
      dimLabel={key}
      load={(signal) => simulateFetch(() => toBars(rowsOf(dim)), signal)}
      {total}
      onpick={(v) => {
        full = false;
        onpick(dim, v);
      }}
    />
  {/if}
{/if}

<style>
  /* 标题左缘与下方条目文字左缘对齐（同 BreakdownCard） */
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
    padding: var(--sp-3) var(--sp-3) 0 28px;
  }
  .title {
    font-weight: var(--fw-semibold);
  }
  .tools {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    min-width: 0;
  }
</style>
