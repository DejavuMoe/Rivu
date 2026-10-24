<!-- 迷你趋势线：站点卡片、指标卡。没有坐标轴，只表达走势；数值由旁边的文字给出。 -->
<script lang="ts">
  import { monotonePath } from "./scale";

  interface Props {
    values: number[];
    width?: number;
    height?: number;
    /** 末端圆点 */
    dot?: boolean;
    muted?: boolean;
    label?: string;
  }
  let { values, width = 120, height = 32, dot = true, muted = false, label }: Props = $props();

  const max = $derived(Math.max(1, ...values));
  const xs = $derived(values.map((_, i) => 2 + (i / Math.max(1, values.length - 1)) * (width - 6)));
  const ys = $derived(values.map((v) => 3 + (1 - v / max) * (height - 6)));
  const line = $derived(monotonePath(xs, ys));
  const area = $derived(line ? `${line}L${xs[xs.length - 1]},${height}L${xs[0]},${height}Z` : "");
</script>

<svg {width} {height} viewBox="0 0 {width} {height}" class:muted role={label ? "img" : undefined} aria-label={label} aria-hidden={label ? undefined : true}>
  {#if !muted && area}<path d={area} class="area" />{/if}
  <path d={line} class="line" />
  {#if dot && values.length}
    <circle cx={xs[xs.length - 1]} cy={ys[ys.length - 1]} r="2.5" class="dot" />
  {/if}
</svg>

<style>
  svg {
    display: block;
    overflow: visible;
  }
  .area {
    fill: var(--chart-area);
  }
  .line {
    fill: none;
    stroke: var(--series-1);
    stroke-width: 1.5;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .muted .line {
    stroke: var(--text-placeholder);
  }
  .dot {
    fill: var(--series-1);
    stroke: var(--bg-app);
    stroke-width: 1.5;
  }
  .muted .dot {
    fill: var(--text-placeholder);
  }
</style>
