<!--
  属性卡片：标题 + 属性键下拉（范围里出现过的键，事件多的在前）+ 条形列表（前 8 个取值）+ 「查看全部」。
  点击行即按「这个键 = 这个取值」筛选；没有这个键的事件归为 (none)。范围里没有任何属性时整张卡片不显示。
  键列表与取值各自查询；看板状态变了，loadKeys / load 换新函数，卡片随之重新查询。
-->
<script lang="ts">
  import { untrack } from "svelte";
  import { CircleAlert, Inbox, Maximize2 } from "@lucide/svelte";
  import Card from "../ui/Card.svelte";
  import Select from "../ui/Select.svelte";
  import Button from "../ui/Button.svelte";
  import EmptyState from "../ui/EmptyState.svelte";
  import Skeleton from "../ui/Skeleton.svelte";
  import BarList from "../charts/BarList.svelte";
  import type { BarRow } from "../charts/types";
  import { t } from "../i18n";
  import { app } from "../state.svelte";
  import { compact, percent } from "../format";
  import { PROP_PREFIX, type Row } from "./spec";
  import { isAbort } from "./source";
  import { displayValue } from "./values";
  import BreakdownDialog from "./BreakdownDialog.svelte";

  interface Props {
    loadKeys: (signal: AbortSignal) => Promise<string[]>;
    load: (dim: string, limit: number, signal: AbortSignal) => Promise<Row[]>;
    total: number;
    onpick: (dim: string, value: string) => void;
  }
  let { loadKeys, load, total, onpick }: Props = $props();

  let keys: string[] = $state([]);
  let keysFailed = $state(false);
  let key = $state("");
  let full = $state(false);

  $effect(() => {
    const ctl = new AbortController();
    loadKeys(ctl.signal).then(
      (ks) => {
        keys = ks;
        keysFailed = false;
        // 保留用户选的键，除非它在新范围里不存在了
        if (!ks.includes(untrack(() => key))) key = ks[0] ?? "";
      },
      (e) => {
        if (!isAbort(e)) keysFailed = true;
      },
    );
    return () => ctl.abort();
  });

  const dim = $derived(`${PROP_PREFIX}${key}`);
  let rows: Row[] = $state([]);
  let status = $state<"loading" | "ready" | "failed">("loading");
  let loadedDim = "";

  $effect(() => {
    if (!key) return;
    const ctl = new AbortController();
    const d = dim;
    if (d !== loadedDim || untrack(() => status) === "failed") status = "loading";
    load(d, 8, ctl.signal).then(
      (r) => {
        rows = r;
        loadedDim = d;
        status = "ready";
      },
      (e) => {
        if (!isAbort(e)) status = "failed";
      },
    );
    return () => ctl.abort();
  });

  const toBars = (rs: Row[]): BarRow[] =>
    rs.map((r) => ({ key: r.label, label: displayValue(dim, r.label, app.lang), value: r.visitors }));
  const bars = $derived(toBars(rows));
</script>

{#if keysFailed}
  <Card title={t("dash.card.props")}>
    <EmptyState compact icon={CircleAlert} tone="bad" title={t("dash.error.title")} />
  </Card>
{:else if keys.length}
  <Card>
    {#snippet header()}
      <div class="head">
        <h2 class="title">{t("dash.card.props")}</h2>
        <div class="tools">
          <Select
            size="sm"
            variant="ghost"
            align="end"
            label={t("dash.propKey")}
            bind:value={key}
            options={keys.map((k) => ({ value: k, label: k }))}
          />
          <Button
            variant="ghost"
            size="sm"
            icon={Maximize2}
            aria-label={t("common.viewAll")}
            title={t("common.viewAll")}
            onclick={() => (full = true)}
            disabled={status !== "ready" || !rows.length}
          />
        </div>
      </div>
    {/snippet}
    {#if status === "loading"}
      <div class="sk">
        {#each [92, 74, 60, 48, 40, 33, 28, 22] as w, i (i)}<Skeleton width="{w}%" height={28} radius={4} />{/each}
      </div>
    {:else if status === "failed"}
      <EmptyState compact icon={CircleAlert} tone="bad" title={t("dash.error.title")} />
    {:else if !rows.length}
      <EmptyState compact icon={Inbox} title={t("dash.noRows")} />
    {:else}
      <BarList
        rows={bars}
        dimLabel={key}
        metricLabel={t("metric.visitors")}
        format={(n) => compact(n, app.lang)}
        {total}
        percent={(r) => percent(r, 0)}
        pickLabel={t("dash.addAsFilter")}
        onpick={(r) => onpick(dim, r.key)}
      />
    {/if}
  </Card>

  {#if full}
    <BreakdownDialog
      bind:open={full}
      title={key}
      dimLabel={key}
      load={async (signal) => toBars(await load(dim, 1000, signal))}
      {total}
      onpick={(v) => {
        full = false;
        onpick(dim, v);
      }}
    />
  {/if}
{/if}

<style>
  /* 标题左缘与下方条目文字左缘对齐（同 BreakdownCard） */
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2);
    padding: var(--sp-3) var(--sp-3) 0 28px;
  }
  .title {
    font-weight: var(--fw-semibold);
  }
  .tools {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
    min-width: 0;
  }
  .sk {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding-top: 26px;
  }
</style>
