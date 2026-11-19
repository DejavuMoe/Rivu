<!--
  目标卡片：每个有转化的目标一行（访客、完成次数、转化率），条长按访客数。点行按这个目标筛选。
  没有转化（站点没有目标，或这个范围里没有转化）时整张卡片不显示，加载中也不显示；查询失败时是带标题的错误卡片。
-->
<script lang="ts">
  import { CircleAlert } from "@lucide/svelte";
  import Card from "../../lib/ui/Card.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import BarList from "../../lib/charts/BarList.svelte";
  import type { BarRow } from "../../lib/charts/types";
  import { t } from "../../lib/i18n";
  import { app } from "../../lib/state.svelte";
  import { compact, percent } from "../../lib/format";
  import type { GoalRow } from "./query";

  interface Props {
    rows: GoalRow[];
    onpick: (dim: string, value: string) => void;
    loading?: boolean;
    /** 查询失败（出错、排队、超时都显示成同一个错误） */
    failed?: boolean;
  }
  let { rows, onpick, loading = false, failed = false }: Props = $props();

  const bars = $derived<BarRow[]>(
    rows.map((r) => ({
      key: r.label,
      label: r.label,
      value: r.visitors,
      extra: [compact(r.events, app.lang), percent(r.cr)],
    })),
  );
</script>

{#if failed}
  <Card title={t("dim.goal")}>
    <EmptyState compact icon={CircleAlert} tone="bad" title={t("dash.error.title")} />
  </Card>
{:else if !loading && rows.length}
  <Card title={t("dim.goal")}>
    <BarList
      rows={bars}
      dimLabel={t("fdim.goal")}
      metricLabel={t("metric.uniques")}
      extras={[t("metric.conversions"), t("metric.conversion_rate")]}
      format={(n) => compact(n, app.lang)}
      pickLabel={t("dash.addAsFilter")}
      onpick={(r) => onpick("goal", r.key)}
    />
  </Card>
{/if}
