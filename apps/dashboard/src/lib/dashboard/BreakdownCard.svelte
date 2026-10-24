<!--
  分解卡片：标题 + 维度标签页 + 条形列表（前 8 项）+ 「查看全部」。
  每个标签页自己查询（只查当前标签页）；看板状态变了，load 换一个新函数，卡片随之重新查询。
  点击行即按该值筛选；国家行前面是两字母代码徽标（不用 emoji 国旗：Windows 上不显示）。
-->
<script lang="ts">
  import { untrack, type Snippet } from "svelte";
  import { CircleAlert, Inbox, Maximize2 } from "@lucide/svelte";
  import Card from "../ui/Card.svelte";
  import Tabs from "../ui/Tabs.svelte";
  import Button from "../ui/Button.svelte";
  import EmptyState from "../ui/EmptyState.svelte";
  import Skeleton from "../ui/Skeleton.svelte";
  import BarList from "../charts/BarList.svelte";
  import type { BarRow } from "../charts/types";
  import { t } from "../i18n";
  import { app } from "../state.svelte";
  import { compact, percent } from "../format";
  import type { Row } from "./spec";
  import { isAbort } from "./source";
  import { displayValue, valueCode } from "./values";
  import BreakdownDialog from "./BreakdownDialog.svelte";

  interface Props {
    title: string;
    dims: string[];
    load: (dim: string, limit: number, signal: AbortSignal) => Promise<Row[]>;
    total: number;
    onpick: (dim: string, value: string) => void;
    footer?: Snippet;
    /** 补空行到这个行数，让并排的卡片等高；单独一行的卡片传 0 */
    minRows?: number;
  }
  let { title, dims, load, total, onpick, footer, minRows = 8 }: Props = $props();

  let dim = $state("");
  $effect.pre(() => {
    if (!dims.includes(dim)) dim = dims[0];
  });
  let full = $state(false);

  let rows: Row[] = $state([]);
  let status = $state<"loading" | "ready" | "failed">("loading");
  // 换标签页时显示骨架；同一标签页只是数据刷新（实时视图每 30 秒）时保留旧数据直到新数据到达
  let loadedDim = "";

  $effect(() => {
    const ctl = new AbortController();
    const d = dim;
    if (d !== loadedDim || untrack(() => status) === "failed") status = "loading";
    load(d, 8, ctl.signal).then(
      (r) => {
        rows = r;
        loadedDim = d;
        status = "ready";
      },
      (e) => {
        if (!isAbort(e)) status = "failed";
      },
    );
    return () => ctl.abort();
  });

  const toBars = (d: string, rs: Row[]): BarRow[] =>
    rs.map((r) => ({ key: r.label, label: displayValue(d, r.label, app.lang), code: valueCode(d, r.label), value: r.visitors }));
  const bars = $derived(toBars(dim, rows));
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
        disabled={status !== "ready" || !rows.length}
      />
    </div>
  {/snippet}
  {#if status === "loading"}
    <div class="sk">
      {#each [92, 74, 60, 48, 40, 33, 28, 22] as w, i (i)}<Skeleton width="{w}%" height={28} radius={4} />{/each}
    </div>
  {:else if status === "failed"}
    <EmptyState compact icon={CircleAlert} tone="bad" title={t("dash.error.title")} />
  {:else if !rows.length}
    <EmptyState compact icon={Inbox} title={t("dash.noRows")} />
  {:else}
    <BarList
      rows={bars}
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
        {#if r.code}<span class="code">{r.code}</span>{/if}
      {/snippet}
    </BarList>
  {/if}
</Card>

{#if full}
  <BreakdownDialog
    bind:open={full}
    title={t(`dim.${dim}`)}
    dimLabel={t(`fdim.${dim}`)}
    load={async (signal) => toBars(dim, await load(dim, 1000, signal))}
    {total}
    onpick={(v) => {
      full = false;
      onpick(dim, v);
    }}
  />
{/if}

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
