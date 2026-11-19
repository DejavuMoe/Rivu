<!-- 分解明细：完整表格 + 搜索 + 导出。点行同样是按该值筛选。 -->
<script lang="ts">
  import { Download, Search } from "@lucide/svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import type { BarRow } from "../../lib/charts/types";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import { full, percent } from "../../lib/format";

  interface Props {
    open: boolean;
    title: string;
    dimLabel: string;
    rows: BarRow[];
    total: number;
    onpick: (v: string) => void;
  }
  let { open = $bindable(), title, dimLabel, rows, total, onpick }: Props = $props();

  let q = $state("");
  const shown = $derived(rows.filter((r) => r.label.toLowerCase().includes(q.toLowerCase())));
</script>

<Dialog bind:open {title} size="lg">
  <div class="tools">
    <Input icon={Search} bind:value={q} placeholder={t("dash.searchRows", { dim: dimLabel })} size="sm" />
    <Button size="sm" icon={Download} onclick={() => toast(t("dash.exported"), "good")}>{t("dash.export")}</Button>
  </div>
  <div class="table">
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
          <span class="lbl">{r.label}</span>{#if r.sub}<span class="sub">{r.sub}</span>{/if}
        {:else if c.key === "visitors"}
          {full(r.value, app.lang)}
        {:else}
          <span class="muted">{percent(r.value / total)}</span>
        {/if}
      {/snippet}
    </Table>
  </div>
</Dialog>

<style>
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
  .sub {
    margin-left: 8px;
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
</style>
