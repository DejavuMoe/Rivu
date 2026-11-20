<!--
  目标卡片：每个有转化的目标一行（访客、完成次数、转化率），条长按访客数。点行按这个目标筛选。
  滚动深度目标没有完成次数，那一格显示 —。
  查询返回空（站点没有目标，或这个范围里没有转化）时整张卡片不显示；目标的名字随查询结果返回，所以看板的只读访问也能看到。
-->
<script lang="ts">
  import { CircleAlert } from "@lucide/svelte";
  import Card from "../ui/Card.svelte";
  import EmptyState from "../ui/EmptyState.svelte";
  import BarList from "../charts/BarList.svelte";
  import type { BarRow } from "../charts/types";
  import { t } from "../i18n";
  import { app } from "../state.svelte";
  import { compact, percent } from "../format";
  import type { GoalRow } from "./spec";
  import { isAbort } from "./source";

  interface Props {
    load: (signal: AbortSignal) => Promise<GoalRow[]>;
    onpick: (dim: string, value: string) => void;
  }
  let { load, onpick }: Props = $props();

  let rows: GoalRow[] = $state([]);
  let status = $state<"loading" | "ready" | "failed">("loading");

  $effect(() => {
    const ctl = new AbortController();
    load(ctl.signal).then(
      (r) => {
        rows = r;
        status = "ready";
      },
      (e) => {
        if (!isAbort(e)) status = "failed";
      },
    );
    return () => ctl.abort();
  });

  const bars = $derived<BarRow[]>(
    rows.map((r) => ({
      key: r.label,
      label: r.label,
      value: r.visitors,
      extra: [r.events === null ? "—" : compact(r.events, app.lang), percent(r.cr)],
    })),
  );
</script>

{#if status === "failed"}
  <Card title={t("dim.goal")}>
    <EmptyState compact icon={CircleAlert} tone="bad" title={t("dash.error.title")} />
  </Card>
{:else if status === "ready" && rows.length}
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
