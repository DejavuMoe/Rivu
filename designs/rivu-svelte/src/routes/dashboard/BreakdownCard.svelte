<!--
  分解卡片：标题 + 维度标签页 + 条形列表（前 8 项）+ 「查看全部」。
  点击行即按该值筛选；国家行前面是两字母代码徽标（不用 emoji 国旗：Windows 上不显示）。
  取值是服务端的原始取值，显示名在这里翻译（values.ts）。
  details：某些维度在「查看全部」的明细表里多几列（页面的滚动深度、停留时间），卡片本身保持一列访客数。
  visual：某些维度在列表旁边多一个图（国家的世界地图）；卡片窄于 720px 时图在列表上面。
-->
<script module lang="ts">
  /** 明细表的附加列 */
  export interface Details {
    /** 有附加列的维度 */
    dims: string[];
    headers: string[];
    /** 一行的附加列（已格式化）；整行没有数据时返回 null */
    values: (label: string) => string[] | null;
    /** 表格下面的说明（附加列的口径） */
    notes: string[];
    /** 所有行都没有附加列的数据时的说明 */
    missing: string;
  }
</script>

<script lang="ts">
  import type { Snippet } from "svelte";
  import { CircleAlert, Inbox, Maximize2 } from "@lucide/svelte";
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
  import { simulateFetch, type Row } from "./query";
  import { displayValue, valueCode } from "./values";
  import BreakdownDialog from "./BreakdownDialog.svelte";

  interface Props {
    title: string;
    dims: string[];
    rowsOf: (dim: string) => Row[];
    total: number;
    onpick: (dim: string, value: string) => void;
    loading?: boolean;
    /** 这张卡片的查询失败（出错、排队、超时都显示成同一个错误） */
    failed?: boolean;
    footer?: Snippet;
    details?: Details;
    /** 列表旁边的图（拿到这个维度的全部行）与它出现的维度 */
    visual?: Snippet<[BarRow[]]>;
    visualDims?: string[];
    /** 补空行到这个行数，让并排的卡片等高；单独一行的卡片传 0 */
    minRows?: number;
  }
  let { title, dims, rowsOf, total, onpick, loading = false, failed = false, footer, details, visual, visualDims = [], minRows = 8 }: Props = $props();

  let dim = $state("");
  $effect.pre(() => {
    if (!dims.includes(dim)) dim = dims[0];
  });
  let full = $state(false);

  const rows = $derived(rowsOf(dim));
  const toBars = (d: string, rs: Row[]): BarRow[] =>
    rs.map((r) => ({
      key: r.label,
      label: displayValue(d, r.label, app.lang),
      // 城市行：城市名后面跟国家名，行首是国家代码
      code: valueCode(d, r.label) ?? r.country,
      sub: r.country ? displayValue("country", r.country, app.lang) : undefined,
      value: r.visitors,
    }));
  const bars = $derived(toBars(dim, rows));

  /** 明细表：这个维度有附加列时，每行带上附加列；没有数据的显示「—」 */
  const extras = $derived(details?.dims.includes(dim) ? details : undefined);
  function withExtras(rs: BarRow[]): BarRow[] {
    if (!extras) return rs;
    return rs.map((r) => ({ ...r, extra: extras.values(r.key) ?? extras.headers.map(() => "—") }));
  }
  const allMissing = (rs: BarRow[]) => !!extras && rs.every((r) => extras.values(r.key) === null);
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
        disabled={loading || failed || !rows.length}
      />
    </div>
  {/snippet}
  {#if loading}
    <div class="sk">
      {#each [92, 74, 60, 48, 40, 33, 28, 22] as w, i (i)}<Skeleton width="{w}%" height={28} radius={4} />{/each}
    </div>
  {:else if failed}
    <EmptyState compact icon={CircleAlert} tone="bad" title={t("dash.error.title")} />
  {:else if !rows.length}
    <EmptyState compact icon={Inbox} title={t("dash.noRows")} />
  {:else if visual && visualDims.includes(dim)}
    <div class="wrap">
      <div class="split">
        <div class="vis">{@render visual(bars)}</div>
        <div class="lst">{@render list()}</div>
      </div>
    </div>
  {:else}
    {@render list()}
  {/if}
</Card>

{#snippet list()}
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
        {#if r.code}<span class="code">{r.code}</span>{/if}
      {/snippet}
    </BarList>
{/snippet}

{#if full}
  <BreakdownDialog
    bind:open={full}
    title={t(`dim.${dim}`)}
    dimLabel={t(`fdim.${dim}`)}
    load={(signal) => simulateFetch(() => withExtras(toBars(dim, rowsOf(dim))), signal)}
    extras={extras?.headers}
    notes={extras ? (allMissing(bars) ? [extras.missing] : extras.notes) : undefined}
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
  /* 图和列表：宽卡片里左右排（图占 7 份），窄了上下排 */
  .wrap {
    container-type: inline-size;
  }
  .split {
    display: grid;
    grid-template-columns: minmax(0, 7fr) minmax(0, 5fr);
    align-items: start;
    gap: var(--sp-6);
  }
  .vis {
    padding-top: var(--sp-6);
  }
  @container (max-width: 720px) {
    .split {
      grid-template-columns: 1fr;
      gap: var(--sp-4);
    }
    .vis {
      padding-top: var(--sp-4);
    }
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
