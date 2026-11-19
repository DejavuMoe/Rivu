<!-- 分解明细：完整表格（至多 1000 行）+ 搜索 + 导出 CSV。点行同样是按该值筛选。 -->
<script lang="ts">
  import { CircleAlert, Download, Search } from "@lucide/svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import type { BarRow } from "../../lib/charts/types";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import { full, percent } from "../../lib/format";
  import { isAbort } from "./query";

  interface Props {
    open: boolean;
    title: string;
    dimLabel: string;
    load: (signal: AbortSignal) => Promise<BarRow[]>;
    total: number;
    onpick: (v: string) => void;
  }
  let { open = $bindable(), title, dimLabel, load, total, onpick }: Props = $props();

  let rows: BarRow[] = $state([]);
  let status = $state<"loading" | "ready" | "failed">("loading");
  let q = $state("");

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

  const shown = $derived(rows.filter((r) => r.label.toLowerCase().includes(q.toLowerCase())));
</script>

<Dialog bind:open {title} size="lg">
  <div class="tools">
    <Input icon={Search} bind:value={q} placeholder={t("dash.searchRows", { dim: dimLabel })} size="sm" />
    <!-- 原型里不生成文件，只提示已导出；正式实现是浏览器直接下载当前表格的 CSV -->
    <Button size="sm" icon={Download} disabled={status !== "ready" || !shown.length} onclick={() => toast(t("dash.exported"), "good")}
      >{t("dash.export")}</Button
    >
  </div>
  <div class="table">
    {#if status === "loading"}
      <div class="sk">{#each [0, 1, 2, 3, 4, 5] as i (i)}<Skeleton height={20} radius={4} />{/each}</div>
    {:else if status === "failed"}
      <EmptyState compact icon={CircleAlert} tone="bad" title={t("dash.error.title")} />
    {:else}
      <Table
        sticky
        caption={title}
        rows={shown}
        rowKey={(r) => r.key}
        onrowclick={(r) => onpick(r.key)}
        columns={[
          { key: "label", label: dimLabel },
          { key: "visitors", label: t("metric.visitors"), align: "right", width: "120px" },
          { key: "share", label: t("metric.percentage"), align: "right", width: "96px", optional: true },
        ]}
      >
        {#snippet cell(r, c)}
          {#if c.key === "label"}
            <button class="lbl" type="button">{r.label}</button>
          {:else if c.key === "visitors"}
            {full(r.value, app.lang)}
          {:else}
            <span class="muted">{percent(total ? r.value / total : NaN)}</span>
          {/if}
        {/snippet}
      </Table>
    {/if}
  </div>
</Dialog>

<style>
  .lbl {
    padding: 0;
    border: 0;
    background: none;
    color: inherit;
    text-align: start;
  }
  .tools {
    display: flex;
    gap: 8px;
    margin-bottom: 12px;
  }
  .tools :global(.input) {
    flex: 1;
  }
  .table {
    max-height: 56vh;
    overflow: auto;
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-sm);
  }
  .sk {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px 20px;
  }
</style>
