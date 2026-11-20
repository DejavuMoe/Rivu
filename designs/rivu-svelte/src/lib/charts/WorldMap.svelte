<!--
  世界地图（分级着色）：看板位置卡片里国家列表旁边的图。列表是它的无障碍替代，地图只是帮助一眼看出分布。

  规则（DESIGN.md「数据可视化」）：
  - 形状打包在 world/countries-110m.ts（Natural Earth 1:110m，公有领域，见 world/PROVENANCE.md），不请求任何第三方资源；
    这个组件连同形状数据由看板按需动态加载，不进首屏。
  - 颜色只用令牌：没有数据 --bg-element，有数据按访客数分 5 级，从 --accent-bg-hover 到 --accent-solid（浅色、深色都由令牌切换）；
    国家之间 0.5px 的 --bg-app 分隔。图下有「少 → 多」的色阶，状态不只靠颜色：悬停 / 聚焦时提示国家和访客数。
  - 键盘：地图可聚焦，左右方向键按访客数从多到少逐个国家读数，Enter 选中（同点击），Esc 取消。
-->
<script lang="ts">
  import { COUNTRIES, WORLD_HEIGHT, WORLD_WIDTH } from "./world/countries-110m";

  interface Row {
    /** ISO 3166-1 两字母代码 */
    code: string;
    label: string;
    value: number;
  }

  interface Props {
    rows: Row[];
    format: (n: number) => string;
    /** 合计（算占比）；不传则提示里不显示占比 */
    total?: number;
    percent?: (r: number) => string;
    /** 屏幕阅读器读到的整体描述 */
    summary: string;
    /** 色阶两端的文字 */
    lessText: string;
    moreText: string;
    onpick?: (code: string) => void;
  }

  let { rows, format, total, percent, summary, lessText, moreText, onpick }: Props = $props();

  const LEVELS = 5;
  const byCode = $derived(new Map(rows.map((r) => [r.code, r])));
  const max = $derived(Math.max(1, ...rows.map((r) => r.value)));
  /** 1–5 级：按访客数相对最大值的平方根分级，小国家也看得出来 */
  const level = (v: number) => Math.min(LEVELS, Math.max(1, Math.ceil(Math.sqrt(v / max) * LEVELS)));
  /** 键盘顺序：访客数从多到少，只含地图上有形状的国家 */
  const order = $derived(
    rows
      .filter((r) => r.value > 0 && COUNTRIES.some(([c]) => c === r.code))
      .sort((a, b) => b.value - a.value)
      .map((r) => r.code),
  );

  let width = $state(600);
  let svg: SVGSVGElement | undefined = $state();
  let active: string | null = $state(null);
  let tip = $state({ x: 0, y: 0 });

  const k = $derived(width / WORLD_WIDTH);

  function onMove(e: PointerEvent) {
    const code = (e.target as Element).getAttribute?.("data-code");
    if (!code || !byCode.has(code)) {
      active = null;
      return;
    }
    const r = svg!.getBoundingClientRect();
    active = code;
    tip = { x: e.clientX - r.left, y: e.clientY - r.top };
  }

  /** 键盘选中时把提示放在国家形状的中心 */
  function focusCountry(code: string) {
    active = code;
    const el = svg?.querySelector<SVGPathElement>(`path[data-code="${code}"]`);
    if (!el) return;
    const b = el.getBBox();
    tip = { x: (b.x + b.width / 2) * k, y: (b.y + b.height / 2) * k };
  }

  function onKey(e: KeyboardEvent) {
    if (!order.length) return;
    if (e.key === "ArrowRight" || e.key === "ArrowLeft" || e.key === "ArrowDown" || e.key === "ArrowUp") {
      const d = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : -1;
      const i = active ? order.indexOf(active) : -1;
      const next = i < 0 ? (d > 0 ? 0 : order.length - 1) : Math.min(order.length - 1, Math.max(0, i + d));
      focusCountry(order[next]);
      e.preventDefault();
    } else if (e.key === "Escape") {
      active = null;
    } else if ((e.key === "Enter" || e.key === " ") && active && onpick) {
      onpick(active);
      e.preventDefault();
    }
  }

  const cur = $derived(active ? byCode.get(active) : undefined);
  const flip = $derived(tip.x > width * 0.6);
</script>

<div class="map" bind:clientWidth={width}>
  <!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
  <svg
    bind:this={svg}
    viewBox="0 0 {WORLD_WIDTH} {WORLD_HEIGHT}"
    role="img"
    aria-label={summary}
    tabindex="0"
    onpointermove={onMove}
    onpointerleave={() => (active = null)}
    onkeydown={onKey}
    onblur={() => (active = null)}
    onclick={() => active && onpick?.(active)}
    class:pickable={!!onpick}
  >
    {#each COUNTRIES as [code, d] (code)}
      {@const r = byCode.get(code)}
      <path {d} data-code={code} class="c l{r && r.value > 0 ? level(r.value) : 0}" />
    {/each}
    <!-- 选中的国家最后再画一次，描边不被邻国盖住 -->
    {#if active}
      {@const hit = COUNTRIES.find(([c]) => c === active)}
      {#if hit}<path d={hit[1]} class="hi" />{/if}
    {/if}
  </svg>
  {#if cur}
    <div class="tip" class:flip style:left="{tip.x}px" style:top="{tip.y}px" role="status">
      <p class="tip-title">{cur.label}</p>
      <p class="tip-val">
        {format(cur.value)}
        {#if total && percent}<span class="tip-share">{percent(cur.value / total)}</span>{/if}
      </p>
    </div>
  {/if}
  <div class="scale" aria-hidden="true">
    <span>{lessText}</span>
    {#each [1, 2, 3, 4, 5] as l (l)}<span class="sw l{l}"></span>{/each}
    <span>{moreText}</span>
  </div>
</div>

<style>
  .map {
    position: relative;
    width: 100%;
    min-width: 0;
  }
  /* 宽度跟着容器走（不写固定宽度，免得窄屏时把卡片撑宽） */
  svg {
    display: block;
    width: 100%;
    height: auto;
    aspect-ratio: 1000 / 442;
    border-radius: var(--r-xs);
  }
  svg:focus-visible {
    outline: none;
    box-shadow: var(--focus-ring);
  }
  .pickable .c.l1,
  .pickable .c.l2,
  .pickable .c.l3,
  .pickable .c.l4,
  .pickable .c.l5 {
    cursor: pointer;
  }
  .c {
    stroke: var(--bg-app);
    stroke-width: 0.5;
    vector-effect: non-scaling-stroke;
    stroke-linejoin: round;
  }
  .hi {
    fill: none;
    stroke: var(--text);
    stroke-width: 1.5;
    vector-effect: non-scaling-stroke;
    stroke-linejoin: round;
    pointer-events: none;
  }
  .l0 {
    fill: var(--bg-element);
  }
  .l1 {
    fill: var(--accent-bg-hover);
  }
  .l2 {
    fill: var(--accent-bg-active);
  }
  .l3 {
    fill: var(--accent-border);
  }
  .l4 {
    fill: var(--accent-focus);
  }
  .l5 {
    fill: var(--accent-solid);
  }
  .scale {
    display: flex;
    align-items: center;
    gap: 2px;
    margin-top: var(--sp-2);
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .scale span:first-child {
    margin-right: var(--sp-1);
  }
  .scale span:last-child {
    margin-left: var(--sp-1);
  }
  .sw {
    width: 16px;
    height: 8px;
    border-radius: 2px;
  }
  .sw.l1 {
    background: var(--accent-bg-hover);
  }
  .sw.l2 {
    background: var(--accent-bg-active);
  }
  .sw.l3 {
    background: var(--accent-border);
  }
  .sw.l4 {
    background: var(--accent-focus);
  }
  .sw.l5 {
    background: var(--accent-solid);
  }
  .tip {
    position: absolute;
    z-index: 5;
    min-width: 128px;
    margin: 12px 0 0 14px;
    padding: 8px 12px;
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
    margin-bottom: 2px;
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .tip-val {
    font-weight: var(--fw-semibold);
    font-variant-numeric: tabular-nums;
  }
  .tip-share {
    margin-left: var(--sp-2);
    color: var(--text-muted);
    font-weight: var(--fw-regular);
  }
</style>
