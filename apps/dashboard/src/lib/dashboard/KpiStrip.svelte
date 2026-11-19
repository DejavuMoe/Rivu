<!--
  指标条：指标并排，点击切换主图的指标。选中项用浅底色与主要文字色表示。
  实时范围下第一格是「当前在线」，只展示、不能选作主图指标。
-->
<script lang="ts">
  import { ArrowDownRight, ArrowUpRight } from "@lucide/svelte";
  import { t } from "../i18n";
  import { app } from "../state.svelte";
  import { delta, compact } from "../format";
  import Skeleton from "../ui/Skeleton.svelte";
  import LiveDot from "../ui/LiveDot.svelte";
  import type { Kpi, MetricKey } from "./spec";
  import { fmtMetric, lowerIsBetter } from "./metrics";

  interface Props {
    kpis: Kpi[];
    selected: MetricKey;
    onselect: (k: MetricKey) => void;
    loading?: boolean;
  }
  let { kpis, selected, onselect, loading = false }: Props = $props();
</script>

<div class="strip" style:--n={kpis.length} style:--n-narrow={Math.min(kpis.length, 3)} role="tablist" aria-label={t("common.metrics")}>
  {#each kpis as k (k.key)}
    {#if k.key === "current"}
      <div class="kpi">
        <span class="label">{t("metric.current")}</span>
        <span class="value live"><LiveDot />{compact(k.value, app.lang)}</span>
      </div>
    {:else}
      {@const key = k.key}
      {@const d = k.prev === null ? null : delta(k.value, k.prev, key === "bounce_rate" ? app.lang : undefined)}
      {@const good = d && d.dir !== "flat" && (d.dir === "up") !== lowerIsBetter(key)}
      <button type="button" role="tab" class="kpi" aria-selected={selected === key} onclick={() => onselect(key)}>
        <span class="label">{t(`metric.${key}`)}</span>
        {#if loading}
          <Skeleton width="72px" height={28} />
        {:else}
          <span class="row">
            <span class="value" title={fmtMetric(key, k.value, app.lang, "full")}>{fmtMetric(key, k.value, app.lang)}</span>
            {#if d && d.dir !== "flat"}
              <span class="delta" class:good class:bad={!good}>
                {#if d.dir === "up"}<ArrowUpRight size={12} strokeWidth={2} aria-hidden="true" />{:else}<ArrowDownRight size={12} strokeWidth={2} aria-hidden="true" />{/if}
                {d.text}
              </span>
            {/if}
          </span>
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
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--sp-1);
    min-width: 0;
    padding: var(--sp-4) var(--sp-5);
    border: 0;
    border-left: var(--hairline) solid var(--border-subtle);
    background: none;
    text-align: left;
  }
  .kpi:first-child {
    border-left: 0;
  }
  button.kpi:hover {
    background: var(--bg-subtle);
  }
  .kpi[aria-selected="true"] {
    background: var(--bg-element);
  }
  .label {
    overflow: hidden;
    max-width: 100%;
    color: var(--text-muted);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  [aria-selected="true"] .label {
    color: var(--text);
    font-weight: var(--fw-medium);
  }
  .row {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-width: 0;
  }
  .value {
    font-size: var(--fs-xl);
    font-weight: var(--fw-semibold);
    line-height: var(--lh-tight);
    white-space: nowrap;
  }
  .value.live {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .delta {
    display: inline-flex;
    align-items: center;
    gap: 1px;
    font-size: var(--fs-sm);
    font-weight: var(--fw-medium);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  .good {
    color: var(--good-text);
  }
  .bad {
    color: var(--bad-text);
  }
  @media (max-width: 1100px) {
    .strip {
      grid-template-columns: repeat(var(--n-narrow), minmax(0, 1fr));
    }
    .kpi:nth-child(4) {
      border-left: 0;
    }
    .kpi:nth-child(n + 4) {
      border-top: var(--hairline) solid var(--border-subtle);
    }
  }
  @media (max-width: 560px) {
    .strip {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
    .kpi:nth-child(odd) {
      border-left: 0;
    }
    .kpi:nth-child(4) {
      border-left: var(--hairline) solid var(--border-subtle);
    }
    .kpi:nth-child(n + 3) {
      border-top: var(--hairline) solid var(--border-subtle);
    }
  }
</style>
