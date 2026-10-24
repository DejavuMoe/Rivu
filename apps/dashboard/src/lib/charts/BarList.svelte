<!--
  条形列表：看板分解卡片的主体（来源、页面、地区、设备……）。
  每行是一个按钮：底下一条品牌色 wash 条表示占比（相对最大值），右侧是数值与占比。
  点击行 = 按这个值筛选。条只用 wash（约 10%），不用饱和色块；文字永远用文字色。
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { Filter } from "@lucide/svelte";
  import type { BarRow } from "./types";

  interface Props {
    rows: BarRow[];
    /** 表头：维度名、指标名 */
    dimLabel: string;
    metricLabel: string;
    format: (n: number) => string;
    /** 合计（算占比）；不传则不显示占比列 */
    total?: number;
    percent?: (r: number) => string;
    onpick?: (row: BarRow) => void;
    pickLabel?: string;
    leading?: Snippet<[BarRow]>;
    /** 行数不足时补空行，保持卡片等高 */
    minRows?: number;
    deltaOf?: (cur: number, prev: number) => { text: string; dir: "up" | "down" | "flat" };
  }

  let {
    rows,
    dimLabel,
    metricLabel,
    format,
    total,
    percent,
    onpick,
    pickLabel = "",
    leading,
    minRows = 0,
    deltaOf,
  }: Props = $props();

  const max = $derived(Math.max(1, ...rows.map((r) => r.value)));
</script>

<div class="barlist">
  <div class="head" aria-hidden="true">
    <span>{dimLabel}</span>
    <span class="r">{metricLabel}</span>
  </div>
  <div class="rows" role="group" aria-label={dimLabel}>
    {#each rows as r (r.key)}
      <button
        type="button"
        class="row"
        aria-label="{r.label}：{format(r.value)}{total && percent ? `（${percent(r.value / total)}）` : ''}"
        onclick={() => onpick?.(r)}
        disabled={!onpick}
        title={r.sub ? `${r.label}（${r.sub}）` : r.label}
      >
        <span class="bar" style:width="{(r.value / max) * 100}%" aria-hidden="true"></span>
        <span class="label">
          {#if leading}{@render leading(r)}{/if}
          <span class="text">{r.label}</span>
          {#if r.sub}<span class="sub">{r.sub}</span>{/if}
          {#if onpick}<span class="pick" aria-label={pickLabel}><Filter size={12} strokeWidth={2} /></span>{/if}
        </span>
        <span class="vals" aria-hidden="true">
          {#if deltaOf && r.prev !== undefined}
            {@const d = deltaOf(r.value, r.prev)}
            <span class="delta {d.dir}">{d.text}</span>
          {/if}
          {#if total && percent}<span class="pct">{percent(r.value / total)}</span>{/if}
          <span class="val">{format(r.value)}</span>
        </span>
      </button>
    {/each}
    {#each Array(Math.max(0, minRows - rows.length)) as _, i (i)}
      <div class="row filler" aria-hidden="true"></div>
    {/each}
  </div>
</div>

<style>
  .barlist {
    display: flex;
    flex-direction: column;
  }
  .head {
    display: flex;
    justify-content: space-between;
    padding: 0 8px 6px;
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .r {
    text-align: right;
  }
  .row {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    width: 100%;
    height: 32px;
    margin-bottom: 2px;
    padding: 0 8px;
    border: 0;
    border-radius: var(--r-xs);
    background: none;
    font-size: var(--fs-md);
    text-align: left;
    isolation: isolate;
  }
  .row:disabled {
    cursor: default;
    color: inherit;
  }
  .filler {
    pointer-events: none;
  }
  .bar {
    position: absolute;
    inset: 0 auto 0 0;
    z-index: -1;
    min-width: 2px;
    border-radius: var(--r-xs);
    background: var(--chart-bar);
  }
  .row:not(:disabled):hover .bar,
  .row:focus-visible .bar {
    background: var(--chart-bar-hover);
  }
  .label {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text);
  }
  .sub {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .pick {
    display: none;
    flex: none;
    color: var(--text-muted);
  }
  .row:hover .pick,
  .row:focus-visible .pick {
    display: inline-flex;
  }
  .vals {
    display: flex;
    align-items: baseline;
    gap: 10px;
    flex: none;
    font-variant-numeric: tabular-nums;
  }
  .val {
    min-width: 44px;
    text-align: right;
    font-weight: var(--fw-medium);
  }
  .pct {
    min-width: 40px;
    text-align: right;
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .delta {
    font-size: var(--fs-sm);
    color: var(--text-muted);
  }
  .delta.up {
    color: var(--good-text);
  }
  .delta.down {
    color: var(--bad-text);
  }
</style>
