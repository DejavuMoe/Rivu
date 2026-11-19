<!--
  分解卡片：标题 + 维度标签页 + 条形列表（前 8 项）+ 「查看全部」。
  点击行即按该值筛选；国家行前面是两字母代码徽标（不用 emoji 国旗：Windows 上不显示）。
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { Maximize2 } from "@lucide/svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Tabs from "../../lib/ui/Tabs.svelte";
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
  import { Inbox } from "@lucide/svelte";

  interface Props {
    title: string;
    dims: string[];
    rowsOf: (dim: string) => Row[];
    total: number;
    onpick: (dim: string, value: string) => void;
    loading?: boolean;
    footer?: Snippet;
    /** 维度取值的显示名（设备类型等枚举） */
    display?: (dim: string, label: string) => string;
    /** 补空行到这个行数，让并排的卡片等高；单独一行的卡片传 0 */
    minRows?: number;
  }
  let { title, dims, rowsOf, total, onpick, loading = false, footer, display = (_d, l) => l, minRows = 8 }: Props = $props();

  let dim = $state("");
  $effect.pre(() => {
    if (!dims.includes(dim)) dim = dims[0];
  });
  let full = $state(false);

  const rows = $derived(rowsOf(dim));
  const bars: BarRow[] = $derived(
    rows.map((r) => ({ key: r.label, label: display(dim, r.label), sub: r.sub, code: r.code, value: r.visitors })),
  );
</script>

<Card {footer}>
  {#snippet header()}
    <div class="head">
      {#if dims.length > 1}
        <h2 class="sr-only">{title}</h2>
        <Tabs size="sm" label={title} bind:value={dim} options={dims.map((d) => ({ value: d, label: t(`dim.${d}`) }))} />
      {:else}
        <h2 class="title">{title}</h2>
      {/if}
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
      dimLabel={t(`fdim.${dim}`)}
      metricLabel={t("metric.visitors")}
      format={(n) => compact(n, app.lang)}
      {total}
      percent={(r) => percent(r, 0)}
      pickLabel={t("dash.addAsFilter")}
      onpick={(r) => onpick(dim, r.key)}
    >
      {#snippet leading(r)}
        {#if r.code}<span class="code">{r.code.slice(0, 2)}</span>{/if}
      {/snippet}
    </BarList>
  {/if}
</Card>

<BreakdownDialog
  bind:open={full}
  title={t(`dim.${dim}`)}
  dimLabel={t(`fdim.${dim}`)}
  rows={bars}
  {total}
  onpick={(v) => {
    full = false;
    onpick(dim, v);
  }}
/>

<style>
  /* 标签文字左缘（18 + 选项内边距 10）与下方条目文字左缘（卡片内边距 20 + 行内边距 8）对齐 */
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
    padding: var(--sp-3) var(--sp-3) 0 18px;
  }
  /* 单一维度时直接显示标题，左缘与条目文字对齐 */
  .title {
    padding-left: 10px;
    font-weight: var(--fw-semibold);
  }
  .sk {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding-top: 26px;
  }
  .code {
    display: inline-grid;
    place-items: center;
    flex: none;
    width: 24px;
    height: 18px;
    border-radius: var(--r-xs);
    background: var(--bg-element);
    color: var(--text-muted);
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
  }
</style>
