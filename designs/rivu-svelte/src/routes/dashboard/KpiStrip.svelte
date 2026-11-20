<!--
  指标条：指标并排，点击切换主图的指标。数值用展示字体排，变化量是它下面的一个小色块。
  选中项的名字前面有一小段「水流」色（与趋势线同色，它就是图例），底边有同色的线。数值第一次出现时从 0 数上来。
  实时范围下第一格是「当前在线」，只展示、不能选作主图指标。
-->
<script lang="ts">
  import { ArrowDownRight, ArrowUpRight } from "@lucide/svelte";
  import { t } from "../../lib/i18n";
  import { app } from "../../lib/state.svelte";
  import { delta, compact } from "../../lib/format";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import CountUp from "../../lib/ui/CountUp.svelte";
  import LiveDot from "../../lib/ui/LiveDot.svelte";
  import type { Kpi, MetricKey } from "./query";
  import { fmtMetric, lowerIsBetter } from "./metrics";

  interface Props {
    kpis: Kpi[];
    selected: MetricKey;
    onselect: (k: MetricKey) => void;
    loading?: boolean;
    /** 指标名与说明（默认取词表；合并看板把访客写成「访客之和」） */
    labelOf?: (k: MetricKey) => string;
    defOf?: (k: MetricKey) => string;
  }
  let {
    kpis,
    selected,
    onselect,
    loading = false,
    labelOf = (k: MetricKey) => t(`metric.${k}`),
    defOf = (k: MetricKey) => t(`metric.${k}.def`),
  }: Props = $props();

  /** 这次查询有没有对比期 */
  const compared = $derived(kpis.some((k) => k.prev !== null));

  /** 数的时候取整的指标（人数、次数、秒数）；比率和均值保留小数 */
  const whole = (k: MetricKey) => k !== "views_per_visit" && k !== "bounce_rate";
</script>

<div class="strip" style:--n={kpis.length} style:--n-narrow={Math.min(kpis.length, 3)} role="tablist" aria-label={t("common.metrics")}>
  {#each kpis as k (k.key)}
    {#if k.key === "current"}
      <div class="kpi">
        <span class="label">{t("metric.current")}</span>
<span class="value live"><LiveDot size={10} /><CountUp value={k.value} format={(n) => compact(Math.round(n), app.lang)} units /></span>
      </div>
    {:else}
      {@const key = k.key}
      {@const d = k.prev === null ? null : delta(k.value, k.prev, key === "bounce_rate" ? app.lang : undefined)}
      {@const good = d && d.dir !== "flat" && (d.dir === "up") !== lowerIsBetter(key)}
      <button type="button" role="tab" class="kpi" aria-selected={selected === key} onclick={() => onselect(key)}>
        <span class="label" title={defOf(key)}><span class="key" aria-hidden="true"></span>{labelOf(key)}</span>
        {#if loading}
          <Skeleton width="88px" height={32} />
          <Skeleton width="56px" height={18} radius={9} />
        {:else}
          <span class="value" title={fmtMetric(key, k.value, app.lang, "full")}>
            <CountUp value={k.value} format={(n) => fmtMetric(key, whole(key) ? Math.round(n) : n, app.lang)} units />
          </span>
          {#if d && d.dir !== "flat"}
            <span class="delta" class:good class:bad={!good}>
              {#if d.dir === "up"}<ArrowUpRight size={12} strokeWidth={2.25} aria-hidden="true" />{:else}<ArrowDownRight size={12} strokeWidth={2.25} aria-hidden="true" />{/if}
              {d.text}
            </span>
          {:else if compared}
            <!-- 有对比时，没有变化的指标也占住这一行，各个数字才在一条线上 -->
            <span class="delta none" aria-hidden="true">–</span>
          {/if}
        {/if}
      </button>
    {/if}
  {/each}
</div>

<style>
  .strip {
    display: grid;
    grid-template-columns: repeat(var(--n), minmax(0, 1fr));
    border-bottom: var(--hairline) solid var(--border-subtle);
  }
  .kpi {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 6px;
    min-width: 0;
    padding: var(--sp-5) var(--sp-5) var(--sp-4);
    border: 0;
    background: none;
    text-align: left;
    transition: background-color var(--dur-fast) var(--ease);
  }
  /* 指标之间的分隔线不顶到头：短一截，指标条读起来是一排数字而不是一排格子 */
  .kpi + .kpi::before {
    content: "";
    position: absolute;
    left: 0;
    top: var(--sp-5);
    bottom: var(--sp-4);
    border-left: var(--hairline) solid var(--border-subtle);
  }
  /* 选中项底边的线：与趋势线同色 */
  button.kpi::after {
    content: "";
    position: absolute;
    left: var(--sp-5);
    right: var(--sp-5);
    bottom: -0.5px;
    height: 2px;
    border-radius: 1px;
    background: var(--flow-line);
    transform: scaleX(0);
    transform-origin: left;
    transition: transform var(--dur) var(--ease-move);
  }
  button.kpi[aria-selected="true"]::after {
    transform: scaleX(1);
  }
  button.kpi:hover {
    background: var(--bg-subtle);
  }
  .label {
    display: inline-flex;
    align-items: center;
    overflow: hidden;
    max-width: 100%;
    color: var(--text-muted);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .key {
    flex: none;
    width: 0;
    height: 3px;
    border-radius: 2px;
    background: var(--flow-line);
    transition:
      width var(--dur) var(--ease-move),
      margin var(--dur) var(--ease-move);
  }
  [aria-selected="true"] .key {
    width: 12px;
    margin-right: 6px;
  }
  [aria-selected="true"] .label {
    color: var(--text);
    font-weight: var(--fw-medium);
  }
  .value {
    font-family: var(--font-display);
    font-size: var(--fs-2xl);
    font-weight: var(--fw-semibold);
    line-height: var(--lh-display);
    letter-spacing: var(--track-display);
    white-space: nowrap;
  }
  .value.live {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-3);
  }
  .delta {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    height: 20px;
    padding: 0 7px 0 5px;
    border-radius: var(--r-full);
    font-size: var(--fs-sm);
    font-weight: var(--fw-medium);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .good {
    background: var(--good-bg);
    color: var(--good-text);
  }
  .bad {
    background: var(--bad-bg);
    color: var(--bad-text);
  }
  .delta.none {
    padding: 0;
    color: var(--text-placeholder);
  }
  @media (max-width: 1100px) {
    .strip {
      grid-template-columns: repeat(var(--n-narrow), minmax(0, 1fr));
    }
    .kpi:nth-child(4)::before {
      display: none;
    }
    .kpi:nth-child(n + 4) {
      border-top: var(--hairline) solid var(--border-subtle);
    }
  }
  @media (max-width: 560px) {
    .strip {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .kpi {
      padding: var(--sp-4);
    }
    .kpi:nth-child(odd)::before {
      display: none;
    }
    .kpi:nth-child(4)::before {
      display: block;
    }
    .kpi:nth-child(n + 3) {
      border-top: var(--hairline) solid var(--border-subtle);
    }
  }
</style>
