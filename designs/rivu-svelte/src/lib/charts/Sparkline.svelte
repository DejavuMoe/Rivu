<!-- 迷你趋势线：站点卡片、指标卡。没有坐标轴，只表达走势；数值由旁边的文字给出。和大图一样是「水流」：颜色从深到亮，下方向下淡出。 -->
<script module lang="ts">
  let seq = 0;
</script>

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

  const uid = ++seq;
  const max = $derived(Math.max(1, ...values));
  const xs = $derived(values.map((_, i) => 2 + (i / Math.max(1, values.length - 1)) * (width - 6)));
  const ys = $derived(values.map((v) => 3 + (1 - v / max) * (height - 6)));
  const line = $derived(monotonePath(xs, ys));
  const area = $derived(line ? `${line}L${xs[xs.length - 1]},${height}L${xs[0]},${height}Z` : "");
</script>

<svg {width} {height} viewBox="0 0 {width} {height}" class:muted role={label ? "img" : undefined} aria-label={label} aria-hidden={label ? undefined : true}>
  <defs>
    <linearGradient id="spark-flow-{uid}" gradientUnits="userSpaceOnUse" x1="0" x2={width} y1="0" y2="0">
      <stop offset="0" class="from" />
      <stop offset="1" class="to" />
    </linearGradient>
    <linearGradient id="spark-fade-{uid}" gradientUnits="userSpaceOnUse" x1="0" x2="0" y1="0" y2={height}>
      <stop offset="0" class="fade-top" />
      <stop offset="1" class="fade-bottom" />
    </linearGradient>
    <mask id="spark-mask-{uid}" maskUnits="userSpaceOnUse" x="0" y="0" {width} {height}>
      <rect {width} {height} fill="url(#spark-fade-{uid})" />
    </mask>
  </defs>
  {#if !muted && area}<path d={area} fill="url(#spark-flow-{uid})" mask="url(#spark-mask-{uid})" />{/if}
  <path d={line} class="line" stroke={muted ? undefined : `url(#spark-flow-${uid})`} />
  {#if dot && values.length}
    <circle cx={xs[xs.length - 1]} cy={ys[ys.length - 1]} r="2.5" class="dot" />
  {/if}
</svg>

<style>
  svg {
    display: block;
    overflow: visible;
  }
  .from {
    stop-color: var(--flow-from);
  }
  .to {
    stop-color: var(--flow-to);
  }
  .fade-top {
    stop-color: var(--mask-on);
    stop-opacity: var(--flow-area-top);
  }
  .fade-bottom {
    stop-color: var(--mask-on);
    stop-opacity: 0;
  }
  .line {
    fill: none;
    stroke-width: 1.75;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .muted .line {
    stroke: var(--text-placeholder);
  }
  .dot {
    fill: var(--flow-to);
    stroke: var(--bg-app);
    stroke-width: 1.5;
  }
  .muted .dot {
    fill: var(--text-placeholder);
  }
</style>
