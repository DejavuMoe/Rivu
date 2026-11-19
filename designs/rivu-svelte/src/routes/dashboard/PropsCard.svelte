<!--
  属性卡片：标题 + 属性键下拉（范围里出现过的键，事件多的在前）+ 条形列表（前 8 个取值）+ 「查看全部」。
  点击行即按「这个键 = 这个取值」筛选；没有这个键的事件归为 (none)。范围里没有任何属性时整张卡片不显示。
-->
<script lang="ts">
  import { Inbox, Maximize2 } from "@lucide/svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Select from "../../lib/ui/Select.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import BarList from "../../lib/charts/BarList.svelte";
  import type { BarRow } from "../../lib/charts/types";
  import { t } from "../../lib/i18n";
  import { app } from "../../lib/state.svelte";
  import { compact, percent } from "../../lib/format";
  import type { Row } from "../../lib/data/fixtures";
  import BreakdownDialog from "./BreakdownDialog.svelte";

  interface Props {
    keys: string[];
    rowsOf: (dim: string) => Row[];
    total: number;
    onpick: (dim: string, value: string) => void;
    loading?: boolean;
    minRows?: number;
  }
  let { keys, rowsOf, total, onpick, loading = false, minRows = 8 }: Props = $props();

  let key = $state("");
  $effect.pre(() => {
    if (!keys.includes(key)) key = keys[0] ?? "";
  });
  let full = $state(false);

  const dim = $derived(`prop:${key}`);
  const rows = $derived(key ? rowsOf(dim) : []);
  const bars: BarRow[] = $derived(rows.map((r) => ({ key: r.label, label: r.label, value: r.visitors })));
</script>

{#if keys.length}
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
    {#if loading}
      <div class="sk">
        {#each [92, 74, 60, 48, 40, 33, 28, 22] as w, i (i)}<Skeleton width="{w}%" height={28} radius={4} />{/each}
      </div>
    {:else if !rows.length}
      <EmptyState compact icon={Inbox} title={t("dash.noRows")} />
    {:else}
      <BarList
        rows={bars.slice(0, 8)}
        {minRows}
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

  <BreakdownDialog
    bind:open={full}
    title={key}
    dimLabel={key}
    rows={bars}
    {total}
    onpick={(v) => {
      full = false;
      onpick(dim, v);
    }}
  />
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
  .sk {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding-top: 26px;
  }
</style>
