<!--
  分解明细：完整表格（至多 1000 行）+ 搜索 + 导出 CSV。点行同样是按该值筛选。
  extras：访客之后的附加列（页面的滚动深度、停留时间），值在 BarRow.extra；notes 是表格下面的口径说明。
  窄屏放不下几列数字，附加列收进名称下面的一行小字。
-->
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
    extras?: string[];
    notes?: string[];
  }
  let { open = $bindable(), title, dimLabel, load, total, onpick, extras = [], notes = [] }: Props = $props();

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
          { key: "visitors", label: t("metric.visitors"), align: "right", width: extras.length ? "96px" : "120px" },
          ...extras.map((h, i) => ({ key: `x${i}`, label: h, align: "right" as const, width: "112px", optional: true })),
          { key: "share", label: t("metric.percentage"), align: "right", width: "96px", optional: true },
        ]}
      >
        {#snippet cell(r, c)}
          {#if c.key === "label"}
            <button class="lbl" type="button">{r.label}</button>
            {#if extras.length}
              <span class="mini">
                {#each extras as h, i (i)}<span>{h} <span class="num">{r.extra?.[i] ?? "—"}</span></span>{/each}
              </span>
            {/if}
          {:else if c.key === "visitors"}
            {full(r.value, app.lang)}
          {:else if c.key.startsWith("x")}
            {@const v = r.extra?.[Number(c.key.slice(1))] ?? "—"}
            <span class:muted={v === "—"}>{v}</span>
          {:else}
            <span class="muted">{percent(total ? r.value / total : NaN)}</span>
          {/if}
        {/snippet}
      </Table>
    {/if}
  </div>
  {#if notes.length && status === "ready"}
    <div class="notes">
      {#each notes as n, i (i)}<p>{n}</p>{/each}
    </div>
  {/if}
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
    min-width: 0;
  }
  .mini {
    display: none;
  }
  @media (max-width: 720px) {
    .mini {
      display: flex;
      flex-wrap: wrap;
      column-gap: var(--sp-3);
      padding-bottom: var(--sp-2);
      color: var(--text-muted);
      font-size: var(--fs-sm);
    }
    .lbl {
      padding-top: var(--sp-2);
    }
  }
  .table {
    max-height: 56vh;
    overflow: auto;
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-sm);
  }
  .notes {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    margin-top: var(--sp-3);
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .sk {
    display: flex;
    flex-direction: column;
    gap: 12px;
    padding: 16px 20px;
  }
</style>
