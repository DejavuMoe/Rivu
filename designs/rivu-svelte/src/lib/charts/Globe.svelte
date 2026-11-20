<!--
  地球：访客在哪里。国家列表是它的无障碍替代，地球帮助一眼看出分布，也是整个界面里唯一「张扬」的东西。

  - 画法见 globe.ts：点阵陆地、按此刻的太阳分昼夜、有访客的国家按多少变亮、此刻在线的国家有扩散的光。
    形状用平面地图的那一份（world/countries-110m），不请求任何第三方资源；看板按需动态加载这个组件，不进首屏。
  - 颜色只用令牌（--globe-*、--live）；状态不只靠颜色：指到或选中国家时提示国家名和数字。
  - 可以拖着转；键盘：聚焦后左右方向键按访客数从多到少逐个国家读数并转过去，Enter 选中（同点击），Esc 取消。
  - 系统要求减少动效时不自转、光圈不动。
-->
<script lang="ts">
  import { onMount } from "svelte";
  import { createGlobe, type GlobeHandle } from "./globe";

  interface Row {
    /** ISO 3166-1 两字母代码 */
    code: string;
    label: string;
    value: number;
  }

  interface Props {
    /** 时间范围内各国的访客数 */
    rows: Row[];
    /** 此刻各国的在线人数（有就画光点） */
    live?: Row[];
    format: (n: number) => string;
    /** 合计（算占比）；不传则提示里不显示占比 */
    total?: number;
    percent?: (r: number) => string;
    /** 提示里在线人数那一行，如「3 人在线」 */
    liveText?: (n: number) => string;
    /** 屏幕阅读器读到的整体描述 */
    summary: string;
    /** 色阶两端的文字；传了才显示「少 → 多」的色阶 */
    lessText?: string;
    moreText?: string;
    /** contain：整个球；horizon：只露出上半个弧，做背景 */
    fit?: "contain" | "horizon";
    /** 空闲时自转的速度（度 / 秒）；0 不转 */
    spin?: number;
    /** 做背景时不接受操作 */
    interactive?: boolean;
    /** 一开始正对着的经纬度 */
    view?: [number, number];
    onpick?: (code: string) => void;
  }

  let { rows, live = [], format, total, percent, liveText, summary, lessText, moreText, fit = "contain", spin = 4, interactive = true, view, onpick }: Props = $props();

  let host: HTMLDivElement | undefined = $state();
  let canvas: HTMLCanvasElement | undefined = $state();
  let globe: GlobeHandle | null = null;
  let failed = $state(false);

  const byCode = $derived(new Map(rows.map((r) => [r.code, r])));
  const liveBy = $derived(new Map(live.map((r) => [r.code, r])));
  /** 键盘顺序：访客数从多到少 */
  const order = $derived(
    [...rows]
      .filter((r) => r.value > 0)
      .sort((a, b) => b.value - a.value)
      .map((r) => r.code),
  );

  /** 指针指到的国家，和键盘选中的国家 */
  let hover: string | null = $state(null);
  let picked: string | null = $state(null);
  let tip = $state({ x: 0, y: 0, shown: false });
  const active = $derived(picked ?? hover);
  const cur = $derived(active ? (byCode.get(active) ?? liveBy.get(active)) : undefined);
  const curLive = $derived(active ? liveBy.get(active) : undefined);

  onMount(() => {
    if (!host || !canvas) return;
    globe = createGlobe(canvas, host, {
      fit,
      spin,
      interactive,
      view,
      onhover: (code, x, y) => {
        if (picked) return;
        hover = code;
        if (code) tip = { x, y, shown: true };
      },
      onanchor: (x, y, visible) => {
        if (picked) tip = { x, y, shown: visible };
      },
      onpick: (code) => {
        if (byCode.has(code)) onpick?.(code);
      },
    });
    if (!globe) {
      failed = true;
      return;
    }
    globe.setRows(rows);
    globe.setLive(live);
    // 主题切换（手动或跟随系统）后重新取色
    const mo = new MutationObserver(() => globe?.restyle());
    mo.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    const mq = matchMedia("(prefers-color-scheme: dark)");
    const onScheme = () => globe?.restyle();
    mq.addEventListener("change", onScheme);
    return () => {
      mo.disconnect();
      mq.removeEventListener("change", onScheme);
      globe?.destroy();
      globe = null;
    };
  });

  $effect(() => {
    globe?.setRows(rows);
  });
  $effect(() => {
    globe?.setLive(live);
  });

  function onKey(e: KeyboardEvent) {
    if (!order.length) return;
    if (e.key === "ArrowRight" || e.key === "ArrowLeft" || e.key === "ArrowDown" || e.key === "ArrowUp") {
      const d = e.key === "ArrowRight" || e.key === "ArrowDown" ? 1 : -1;
      const i = picked ? order.indexOf(picked) : -1;
      const next = i < 0 ? (d > 0 ? 0 : order.length - 1) : Math.min(order.length - 1, Math.max(0, i + d));
      picked = order[next];
      globe?.setActive(picked);
      e.preventDefault();
    } else if (e.key === "Escape" && picked) {
      release();
      e.stopPropagation();
    } else if ((e.key === "Enter" || e.key === " ") && picked && onpick) {
      onpick(picked);
      e.preventDefault();
    }
  }
  function release() {
    if (!picked) return;
    picked = null;
    tip = { ...tip, shown: false };
    globe?.setActive(null);
  }

  let width = $state(320);
  const flip = $derived(tip.x > width * 0.58);
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div
  class="globe {fit}"
  class:pickable={interactive && !!onpick && !!hover && byCode.has(hover)}
  class:interactive
  bind:this={host}
  bind:clientWidth={width}
  role="img"
  aria-label={summary}
  tabindex={interactive ? 0 : undefined}
  onkeydown={interactive ? onKey : undefined}
  onblur={release}
>
  {#if !failed}<canvas bind:this={canvas} aria-hidden="true"></canvas>{/if}
  {#if interactive && cur && tip.shown}
    <div class="tip" class:flip style:left="{tip.x}px" style:top="{tip.y}px" aria-live={picked ? "polite" : undefined}>
      <span class="tip-title">{cur.label}</span>
      {#if byCode.has(cur.code)}
        <span class="tip-val">
          {format(cur.value)}
          {#if total && percent}<span class="tip-share">{percent(cur.value / total)}</span>{/if}
        </span>
      {/if}
      {#if curLive && liveText}
        <span class="tip-live"><span class="lamp" aria-hidden="true"></span>{liveText(curLive.value)}</span>
      {/if}
    </div>
  {/if}
</div>
{#if lessText && moreText}
  <div class="scale" aria-hidden="true">
    <span>{lessText}</span>
    <span class="steps">{#each [1, 2, 3, 4, 5] as l (l)}<i style:--l={l}></i>{/each}</span>
    <span>{moreText}</span>
  </div>
{/if}

<style>
  .globe {
    position: relative;
    width: 100%;
    aspect-ratio: 1;
    border-radius: var(--r-md);
    touch-action: pan-y;
    user-select: none;
  }
  .globe.horizon {
    aspect-ratio: auto;
    height: 100%;
  }
  canvas {
    position: absolute;
    inset: 0;
    display: block;
  }
  .interactive canvas {
    cursor: grab;
  }
  .interactive canvas:active {
    cursor: grabbing;
  }
  .pickable canvas {
    cursor: pointer;
  }
  .tip {
    position: absolute;
    z-index: 2;
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 96px;
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--r-sm);
    background: var(--bg-app);
    box-shadow: var(--shadow-pop);
    font-size: var(--fs-sm);
    white-space: nowrap;
    pointer-events: none;
    transform: translate(14px, -50%);
    animation: fade-in var(--dur-pop) var(--ease);
  }
  .tip.flip {
    transform: translate(calc(-100% - 14px), -50%);
  }
  .tip-title {
    font-weight: var(--fw-semibold);
  }
  .tip-val {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    font-variant-numeric: tabular-nums;
  }
  .tip-share {
    color: var(--text-muted);
  }
  .tip-live {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    color: var(--live-text);
    font-variant-numeric: tabular-nums;
  }
  .lamp {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--live);
  }
  .scale {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: var(--sp-2);
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .steps {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  /* 与地球上的点同一套分档：越多越亮、越大 */
  .steps i {
    width: calc(3px + var(--l) * 1px);
    height: calc(3px + var(--l) * 1px);
    border-radius: 50%;
    background: var(--globe-data-bright);
    opacity: calc(0.4 + var(--l) * 0.12);
  }
  .steps i:nth-child(-n + 2) {
    background: var(--globe-data);
  }
</style>
