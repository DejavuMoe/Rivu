<!-- 分解明细：完整表格（至多 1000 行）+ 搜索 + 导出 CSV。点行同样是按该值筛选。 -->
<script lang="ts">
  import { CircleAlert, Download, Search } from "@lucide/svelte";
  import Dialog from "../ui/Dialog.svelte";
  import Input from "../ui/Input.svelte";
  import Button from "../ui/Button.svelte";
  import Table from "../ui/Table.svelte";
  import Skeleton from "../ui/Skeleton.svelte";
  import EmptyState from "../ui/EmptyState.svelte";
  import type { BarRow } from "../charts/types";
  import { t } from "../i18n";
  import { app } from "../state.svelte";
  import { full, percent } from "../format";
  import { isAbort } from "./source";

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

  function csvCell(s: string) {
    // 以 = + - @ 开头的单元格在表格软件里会被当成公式
    const safe = /^[=+\-@\t\r]/.test(s) ? `'${s}` : s;
    return /[",\n\r]/.test(safe) ? `"${safe.replaceAll('"', '""')}"` : safe;
  }

  function exportCsv() {
    const lines = [[dimLabel, t("metric.visitors")].map(csvCell).join(",")];
    for (const r of shown) lines.push([csvCell(r.label), String(r.value)].join(","));
    const url = URL.createObjectURL(new Blob(["﻿" + lines.join("\r\n")], { type: "text/csv;charset=utf-8" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = `${title}.csv`;
    a.click();
    URL.revokeObjectURL(url);
  }
</script>

<Dialog bind:open {title} size="lg">
  <div class="tools">
    <Input icon={Search} bind:value={q} placeholder={t("dash.searchRows", { dim: dimLabel })} size="sm" />
    <Button size="sm" icon={Download} disabled={status !== "ready" || !shown.length} onclick={exportCsv}>{t("dash.export")}</Button>
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
