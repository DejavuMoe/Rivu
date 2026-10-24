<!--
  趋势图（折线 + 面积）。看板主图、接入诊断、站点卡片放大图都用它。

  规则（DESIGN.md「数据可视化」）：
  - 只有一个纵轴；不同量纲的指标分开画。
  - 主序列 2px 品牌色 + 约 15% 的渐隐面积；对比期是 1.5px 中性灰线，永远不占分类色。
  - 进行中的最后一个时间桶用虚线（「数据未完整」的约定含义），其余一律实线。
  - 悬停 / 键盘左右键：竖向十字线 + 每个序列一个带 2px 表面色描边的圆点 + 浮动提示。
  - 网格是 1px 实线发丝线；刻度 0 起，取整。
-->
<script lang="ts">
  import { monotonePath, niceTicks } from "./scale";
  import type { Series } from "./types";

  interface Props {
    series: Series[];
    /** 每个点的横轴文字（短）与提示标题（长） */
    xLabels: string[];
    tipLabels?: string[];
    format: (n: number) => string;
    height?: number;
    /** 最后一个点是否属于进行中的时间桶 */
    partialLast?: boolean;
    partialText?: string;
    area?: boolean;
    /** 屏幕阅读器读到的整体描述 */
    summary: string;
    /** 点击某个点（如：按天的图点进某一天） */
    onselect?: (i: number) => void;
    /** 计算差值时用的格式 */
    deltaOf?: (cur: number, prev: number) => { text: string; dir: "up" | "down" | "flat" };
    /** 指标「越低越好」（跳出率）时，下降显示为好 */
    lowerIsBetter?: boolean;
  }

  let {
    series,
    xLabels,
    tipLabels,
    format,
    height = 260,
    partialLast = false,
    partialText = "",
    area = true,
    summary,
    onselect,
    deltaOf,
    lowerIsBetter = false,
  }: Props = $props();

  const pad = { top: 12, right: 12, bottom: 28, left: 48 };

  let width = $state(640);
  let hover: number | null = $state(null);

  const n = $derived(xLabels.length);
  const plotW = $derived(Math.max(10, width - pad.left - pad.right));
  const plotH = $derived(height - pad.top - pad.bottom);
  const max = $derived(Math.max(0, ...series.flatMap((s) => s.values.filter((v): v is number => v !== null))));
  const ticks = $derived(niceTicks(max, plotH < 160 ? 3 : 4));
  const top = $derived(ticks[ticks.length - 1] || 1);

  const x = (i: number) => pad.left + (n <= 1 ? plotW / 2 : (i / (n - 1)) * plotW);
  const y = (v: number) => pad.top + plotH - (v / top) * plotH;

  const xs = $derived(Array.from({ length: n }, (_, i) => x(i)));

  const colored = $derived.by(() => {
    let slot = 0;
    return series.map((s) => ({
      ...s,
      stroke: s.compare ? "var(--series-compare)" : (s.color ?? `var(--series-${++slot})`),
    }));
  });

  function paths(values: (number | null)[]) {
    const ys = values.map((v) => (v === null ? null : y(v)));
    if (!partialLast || n < 2) return { solid: monotonePath(xs, ys), dashed: "" };
    const solid = monotonePath(xs.slice(0, -1), ys.slice(0, -1));
    const a = ys[n - 2];
    const b = ys[n - 1];
    const dashed = a !== null && b !== null ? `M${xs[n - 2]},${a}L${xs[n - 1]},${b}` : "";
    return { solid, dashed };
  }

  const primary = $derived(colored.find((s) => !s.compare));
  const areaPath = $derived.by(() => {
    if (!area || !primary) return "";
    const ys = primary.values.map((v) => (v === null ? null : y(v)));
    const line = monotonePath(xs, ys);
    if (!line) return "";
    const base = pad.top + plotH;
    return `${line}L${xs[n - 1]},${base}L${xs[0]},${base}Z`;
  });

  // 横轴文字：按宽度抽稀，首尾优先
  const xTickIdx = $derived.by(() => {
    const room = Math.max(2, Math.floor(plotW / 76));
    if (n <= room) return Array.from({ length: n }, (_, i) => i);
    const step = Math.ceil((n - 1) / (room - 1));
    const out: number[] = [];
    for (let i = 0; i < n; i += step) out.push(i);
    // 最后一个刻度离末端不足一个步长就让位给末端，避免两行日期挤在一起
    if (n - 1 - out[out.length - 1] < step) out.pop();
    out.push(n - 1);
    return out;
  });

  function onMove(e: PointerEvent) {
    const r = (e.currentTarget as SVGElement).getBoundingClientRect();
    const px = e.clientX - r.left;
    const i = Math.round(((px - pad.left) / plotW) * (n - 1));
    hover = Math.min(n - 1, Math.max(0, i));
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
      const d = e.key === "ArrowRight" ? 1 : -1;
      hover = hover === null ? (d > 0 ? 0 : n - 1) : Math.min(n - 1, Math.max(0, hover + d));
      e.preventDefault();
    } else if (e.key === "Escape") {
      hover = null;
    } else if ((e.key === "Enter" || e.key === " ") && hover !== null && onselect) {
      onselect(hover);
      e.preventDefault();
    }
  }

  const tipLeft = $derived(hover === null ? 0 : xs[hover]);
  const tipFlip = $derived(tipLeft > width * 0.62);
</script>

<div class="trend" bind:clientWidth={width} style:height="{height}px">
  <!-- 图表本身可聚焦：方向键逐点读数，Enter 选中；读屏另有表格视图 -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <svg
    {width}
    {height}
    role="img"
    aria-label={summary}
    tabindex="0"
    onpointermove={onMove}
    onpointerleave={() => (hover = null)}
    onkeydown={onKey}
    onblur={() => (hover = null)}
    onclick={() => hover !== null && onselect?.(hover)}
    class:selectable={!!onselect}
  >
    <!-- 网格与纵轴刻度 -->
    {#each ticks as tk, i (tk)}
      <line
        x1={pad.left}
        x2={pad.left + plotW}
        y1={y(tk)}
        y2={y(tk)}
        class={i === 0 ? "baseline" : "grid"}
      />
      <text x={pad.left - 10} y={y(tk)} class="ytick" dy="0.32em" text-anchor="end">{format(tk)}</text>
    {/each}

    <!-- 横轴文字 -->
    {#each xTickIdx as i (i)}
      <text
        x={xs[i]}
        y={height - 8}
        class="xtick"
        text-anchor={i === 0 && n > 1 ? "start" : i === n - 1 && n > 1 ? "end" : "middle"}>{xLabels[i]}</text
      >
    {/each}

    {#if areaPath}<path d={areaPath} class="area" />{/if}

    <!-- 对比期先画，主序列压在上面 -->
    {#each [...colored].sort((a, b) => Number(b.compare ?? false) - Number(a.compare ?? false)) as s (s.key)}
      {@const p = paths(s.values)}
      <path d={p.solid} class="line" class:compare={s.compare} stroke={s.stroke} />
      {#if p.dashed}<path d={p.dashed} class="line partial" class:compare={s.compare} stroke={s.stroke} />{/if}
    {/each}

    {#if hover !== null}
      <line x1={xs[hover]} x2={xs[hover]} y1={pad.top} y2={pad.top + plotH} class="cross" />
      {#each colored as s (s.key)}
        {@const v = s.values[hover]}
        {#if v !== null && v !== undefined}
          <circle cx={xs[hover]} cy={y(v)} r={s.compare ? 3.5 : 4.5} fill={s.stroke} class="dot" />
        {/if}
      {/each}
    {/if}
  </svg>

  {#if hover !== null}
    {@const main = colored.find((s) => !s.compare)}
    {@const cmp = colored.find((s) => s.compare)}
    <div class="tip" class:flip={tipFlip} style:left="{tipLeft}px" role="status">
      <p class="tip-title">{tipLabels?.[hover] ?? xLabels[hover]}</p>
      {#if partialLast && hover === n - 1 && partialText}<p class="tip-note">{partialText}</p>{/if}
      {#each colored as s (s.key)}
        {@const v = s.values[hover]}
        <div class="tip-row">
          <span class="key" class:compare={s.compare} style:--c={s.stroke}></span>
          <span class="tip-label">{s.labels?.[hover] ?? s.label}</span>
          <span class="tip-val">{v === null || v === undefined ? "—" : format(v)}</span>
        </div>
      {/each}
      {#if main && cmp && deltaOf}
        {@const a = main.values[hover]}
        {@const b = cmp.values[hover]}
        {#if a !== null && b !== null && a !== undefined && b !== undefined}
          {@const d = deltaOf(a, b)}
          <div
            class="tip-delta"
            class:good={(d.dir === "up") !== lowerIsBetter && d.dir !== "flat"}
            class:bad={(d.dir === "down") !== lowerIsBetter && d.dir !== "flat"}
          >
            {d.text}
          </div>
        {/if}
      {/if}
    </div>
  {/if}
</div>

<style>
  .trend {
    position: relative;
    width: 100%;
    user-select: none;
  }
  svg {
    display: block;
    overflow: visible;
    border-radius: var(--r-xs);
  }
  svg:focus-visible {
    box-shadow: var(--focus-ring);
  }
  .selectable {
    cursor: pointer;
  }
  .grid {
    stroke: var(--chart-grid);
    stroke-width: 0.5;
  }
  .baseline {
    stroke: var(--chart-axis);
    stroke-width: 0.5;
  }
  .ytick,
  .xtick {
    fill: var(--text-muted);
    font-size: var(--fs-sm);
    font-variant-numeric: tabular-nums;
  }
  .line {
    fill: none;
    stroke-width: 2;
    stroke-linejoin: round;
    stroke-linecap: round;
  }
  .line.compare {
    stroke-width: 1.5;
  }
  .line.partial {
    stroke-dasharray: 3 4;
  }
  .area {
    fill: var(--chart-area);
    pointer-events: none;
  }
  .cross {
    stroke: var(--border);
    stroke-width: 0.5;
  }
  .dot {
    stroke: var(--bg-app);
    stroke-width: 2;
  }

  .tip {
    position: absolute;
    top: 0;
    z-index: 5;
    min-width: 168px;
    margin-left: 14px;
    padding: 10px 12px;
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-app);
    box-shadow: var(--shadow-pop);
    font-size: var(--fs-md);
    pointer-events: none;
  }
  .tip.flip {
    margin-left: 0;
    transform: translateX(calc(-100% - 14px));
  }
  .tip-title {
    margin-bottom: 4px;
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .tip-note {
    margin: -2px 0 6px;
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .tip-row {
    display: flex;
    align-items: center;
    gap: 8px;
    height: 22px;
  }
  .key {
    width: 10px;
    height: 2px;
    border-radius: 1px;
    background: var(--c);
    flex: none;
  }
  .tip-label {
    flex: 1;
    color: var(--text-muted);
    white-space: nowrap;
  }
  .tip-val {
    font-weight: var(--fw-semibold);
    font-variant-numeric: tabular-nums;
  }
  .tip-delta {
    margin-top: 6px;
    padding-top: 6px;
    border-top: var(--hairline) solid var(--border-subtle);
    color: var(--text-muted);
    font-size: var(--fs-sm);
    font-variant-numeric: tabular-nums;
  }
  .tip-delta.good {
    color: var(--good-text);
  }
  .tip-delta.bad {
    color: var(--bad-text);
  }
</style>
