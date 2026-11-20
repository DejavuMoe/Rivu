<!--
  看板筛选栏：一行，作用于下面所有图表（不在单个卡片里放筛选）。
  日期范围（前后两个箭头切到上一周期 / 下一周期，菜单最后是自定义范围）、对比、分群（segment 片段，登录后的看板才有）、
  已加的筛选条件（可点开修改、可移除）、添加筛选。
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { GitCompareArrows, ListFilter, Plus, X } from "@lucide/svelte";
  import Select from "../../lib/ui/Select.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import { t } from "../../lib/i18n";
  import { app } from "../../lib/state.svelte";
  import { displayValue } from "./values";
  import { PROP_PREFIX, RANGES, isPropDim, type CompareKey, type DashState, type Filter } from "./query";
  import type { RangeParam } from "./period";
  import FilterDialog from "./FilterDialog.svelte";
  import RangeControl from "./RangeControl.svelte";

  interface Props {
    dash: DashState;
    onrange: (r: RangeParam) => void;
    /** 范围按钮上的文字：预设名，自定义范围是日期 */
    rangeText: string;
    /** 上一周期 / 下一周期；undefined 表示这个范围没有前后（实时、全部时间），null 表示这一边不能再走（还没开始） */
    prev: RangeParam | null | undefined;
    next: RangeParam | null | undefined;
    /** 自定义对话框的初始值（当前范围的起止日）、上限（站点本地的今天）、站点时区 */
    custom: { from: string; to: string; today: string; timezone: string };
    oncompare: (c: CompareKey) => void;
    onfilters: (fs: Filter[]) => void;
    /** 可以筛选的维度 */
    dims: string[];
    /** 维度的候选值（原始取值） */
    suggest: (dim: string) => string[];
    /** 分群按钮（共享链接与公开看板不传） */
    segment?: Snippet;
  }
  let { dash, onrange, rangeText, prev, next, custom, oncompare, onfilters, dims, suggest, segment }: Props = $props();

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

  // 属性筛选直接显示键名（`prop:plan` → `plan`）
  const dimName = (d: string) => (isPropDim(d) ? d.slice(PROP_PREFIX.length) : t(`fdim.${d}`));
  const opName = (o: string) => t(`op.${o}`);
</script>

<div class="bar">
  <RangeControl range={dash.range} groups={RANGES} {rangeText} {prev} {next} {custom} {onrange} />

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

  {#if segment}{@render segment()}{/if}

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

<!-- 编辑属性筛选时，维度下拉里补上这个属性（新建筛选只列固定维度；属性从属性卡片点选） -->
<FilterDialog
  bind:open={dialogOpen}
  initial={editing === null ? null : dash.filters[editing]}
  dims={editing !== null && isPropDim(dash.filters[editing].dim) ? [...dims, dash.filters[editing].dim] : dims}
  {suggest}
  onsave={save}
/>

<style>
  .bar {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: var(--sp-2);
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
