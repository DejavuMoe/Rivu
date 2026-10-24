<!--
  看板筛选栏：一行，作用于下面所有图表（不在单个卡片里放筛选）。
  日期范围、对比、已加的筛选条件（可点开修改、可移除）、添加筛选。
-->
<script lang="ts">
  import { CalendarRange, GitCompareArrows, ListFilter, Plus, X } from "@lucide/svelte";
  import Popover from "../ui/Popover.svelte";
  import MenuItem from "../ui/MenuItem.svelte";
  import MenuLabel from "../ui/MenuLabel.svelte";
  import Select from "../ui/Select.svelte";
  import Button from "../ui/Button.svelte";
  import { t } from "../i18n";
  import { app } from "../state.svelte";
  import { displayValue } from "./values";
  import { RANGES, type CompareKey, type DashState, type Filter, type RangeKey } from "./spec";
  import FilterDialog from "./FilterDialog.svelte";

  interface Props {
    dash: DashState;
    onrange: (r: RangeKey) => void;
    oncompare: (c: CompareKey) => void;
    onfilters: (fs: Filter[]) => void;
    /** 可以筛选的维度 */
    dims: string[];
    suggest: (dim: string, signal: AbortSignal) => Promise<string[]>;
  }
  let { dash, onrange, oncompare, onfilters, dims, suggest }: Props = $props();

  let dialogOpen = $state(false);
  let editing: number | null = $state(null);

  function openNew() {
    editing = null;
    dialogOpen = true;
  }
  function openEdit(i: number) {
    editing = i;
    dialogOpen = true;
  }
  function save(f: Filter) {
    const next = [...dash.filters];
    if (editing === null) {
      const same = next.findIndex((x) => x.dim === f.dim);
      if (same >= 0) next[same] = f;
      else next.push(f);
    } else next[editing] = f;
    onfilters(next);
  }

  const dimName = (d: string) => t(`fdim.${d}`);
  const opName = (o: string) => t(`op.${o}`);
</script>

<div class="bar">
  <Popover width={200} label={t("range.label")}>
    {#snippet trigger({ toggle, props })}
      <button type="button" class="ctl" onclick={toggle} {...props}>
        <CalendarRange size={16} strokeWidth={1.75} />
        <span>{t(`range.${dash.range}`)}</span>
      </button>
    {/snippet}
    {#snippet children({ close })}
      {#each RANGES as group, gi (gi)}
        {#if gi > 0}<MenuLabel separator />{/if}
        {#each group as r (r)}
          <MenuItem
            checked={dash.range === r}
            onclick={() => {
              onrange(r);
              close();
            }}>{t(`range.${r}`)}</MenuItem
          >
        {/each}
      {/each}
    {/snippet}
  </Popover>

  {#if dash.range !== "realtime"}
    <Select
      label={t("compare.label")}
      icon={GitCompareArrows}
      value={dash.compare}
      onchange={oncompare}
      options={[
        { value: "off", label: t("compare.off") },
        { value: "previous_period", label: t("compare.previous_period") },
        { value: "year_over_year", label: t("compare.year_over_year") },
      ]}
    />
  {/if}

  {#each dash.filters as f, i (f.dim)}
    <span class="chip">
      <button type="button" class="chip-main" onclick={() => openEdit(i)}>
        <span class="chip-dim">{dimName(f.dim)}</span>
        <span class="chip-op">{opName(f.op)}</span>
        <span class="chip-val">{f.values.map((v) => displayValue(f.dim, v, app.lang)).join(t("filter.or"))}</span>
      </button>
      <button
        type="button"
        class="chip-x"
        aria-label={t("common.remove")}
        onclick={() => onfilters(dash.filters.filter((_, j) => j !== i))}><X size={14} strokeWidth={1.75} /></button
      >
    </span>
  {/each}

  <Button variant="ghost" icon={dash.filters.length ? Plus : ListFilter} onclick={openNew} aria-label={t("filter.add")}>
    {dash.filters.length ? "" : t("filter.add")}
  </Button>

  {#if dash.filters.length > 1}
    <button type="button" class="clear" onclick={() => onfilters([])}>{t("filter.clearAll")}</button>
  {/if}
</div>

<FilterDialog bind:open={dialogOpen} initial={editing === null ? null : dash.filters[editing]} {dims} {suggest} onsave={save} />

<style>
  .bar {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2);
  }
  .ctl {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: var(--control-md);
    padding: 0 12px 0 10px;
    border: var(--hairline) solid var(--border);
    border-radius: var(--r-sm);
    background: var(--bg-app);
    font-weight: var(--fw-medium);
  }
  .ctl :global(svg) {
    color: var(--text-muted);
  }
  .ctl:hover,
  .ctl[aria-expanded="true"] {
    background: var(--bg-element);
    border-color: var(--border-hover);
  }
  .chip {
    display: inline-flex;
    align-items: stretch;
    height: var(--control-md);
    border-radius: var(--r-sm);
    background: var(--bg-element);
  }
  .chip-main {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    max-width: 320px;
    padding: 0 4px 0 10px;
    border: 0;
    border-radius: var(--r-sm) 0 0 var(--r-sm);
    background: none;
  }
  .chip-main:hover,
  .chip-x:hover {
    background: var(--bg-hover);
  }
  .chip-dim,
  .chip-op {
    color: var(--text-muted);
  }
  .chip-val {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: var(--fw-medium);
  }
  .chip-x {
    display: grid;
    place-items: center;
    width: 28px;
    border: 0;
    border-radius: 0 var(--r-sm) var(--r-sm) 0;
    background: none;
    color: var(--text-muted);
  }
  .chip-x:hover {
    color: var(--text);
  }
  .clear {
    height: var(--control-md);
    padding: 0 var(--sp-2);
    border: 0;
    border-radius: var(--r-sm);
    background: none;
    color: var(--text-muted);
  }
  .clear:hover {
    background: var(--bg-element);
    color: var(--text);
  }
</style>
