<!--
  时间范围控件：[上一周期] [范围菜单] [下一周期]。菜单是预设分组，最后一项「自定义范围…」打开日期对话框。
  站点看板的筛选栏和合并看板共用；前后箭头与按钮文字由调用方按 period.ts 算好传进来（rangeUi）。
-->
<script lang="ts">
  import { CalendarRange, ChevronLeft, ChevronRight } from "@lucide/svelte";
  import Popover from "../../lib/ui/Popover.svelte";
  import MenuItem from "../../lib/ui/MenuItem.svelte";
  import MenuLabel from "../../lib/ui/MenuLabel.svelte";
  import { t } from "../../lib/i18n";
  import type { RangeKey, RangeParam } from "./period";
  import RangeDialog from "./RangeDialog.svelte";

  interface Props {
    /** 当前范围 */
    range: RangeKey;
    /** 菜单里的预设，按组分隔 */
    groups: RangeKey[][];
    /** 范围按钮上的文字：预设名，自定义范围是日期 */
    rangeText: string;
    /** 上一周期 / 下一周期；undefined 表示这个范围没有前后（实时、全部时间），null 表示这一边不能再走（还没开始） */
    prev: RangeParam | null | undefined;
    next: RangeParam | null | undefined;
    /** 自定义对话框的初始值（当前范围的起止日）、上限（站点本地的今天）、站点时区 */
    custom: { from: string; to: string; today: string; timezone: string };
    onrange: (r: RangeParam) => void;
  }
  let { range, groups, rangeText, prev, next, custom, onrange }: Props = $props();

  let rangeOpen = $state(false);
</script>

<div class="range" class:stepper={prev !== undefined}>
  {#if prev !== undefined}
    <button
      type="button"
      class="step"
      aria-label={t("range.prev")}
      title={t("range.prev")}
      disabled={!prev}
      onclick={() => prev && onrange(prev)}><ChevronLeft size={16} strokeWidth={1.75} /></button
    >
  {/if}
  <Popover width={208} label={t("range.label")}>
    {#snippet trigger({ toggle, props })}
      <button type="button" class="ctl" onclick={toggle} {...props}>
        <CalendarRange size={16} strokeWidth={1.75} />
        <span class="num">{rangeText}</span>
      </button>
    {/snippet}
    {#snippet children({ close })}
      {#each groups as group, gi (gi)}
        {#if gi > 0}<MenuLabel separator />{/if}
        {#each group as r (r)}
          <MenuItem
            checked={range === r}
            onclick={() => {
              onrange({ range: r });
              close();
            }}>{t(`range.${r}`)}</MenuItem
          >
        {/each}
      {/each}
      <MenuLabel separator />
      <MenuItem
        checked={range === "custom"}
        onclick={() => {
          close();
          rangeOpen = true;
        }}>{t("range.custom")}</MenuItem
      >
    {/snippet}
  </Popover>
  {#if next !== undefined}
    <button
      type="button"
      class="step"
      aria-label={t("range.next")}
      title={t("range.next")}
      disabled={!next}
      onclick={() => next && onrange(next)}><ChevronRight size={16} strokeWidth={1.75} /></button
    >
  {/if}
</div>

<RangeDialog
  bind:open={rangeOpen}
  from={custom.from}
  to={custom.to}
  today={custom.today}
  timezone={custom.timezone}
  onapply={(from, to) => onrange({ range: "custom", from, to })}
/>

<style>
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
  /* 有前后箭头时，三个按钮拼成一组：外框一条，中间用分隔线 */
  .range {
    display: inline-flex;
    align-items: stretch;
  }
  .stepper .ctl {
    border-radius: 0;
  }
  .step {
    display: grid;
    place-items: center;
    width: var(--control-md);
    height: var(--control-md);
    border: var(--hairline) solid var(--border);
    background: var(--bg-app);
    color: var(--text-muted);
  }
  .step:first-child {
    border-right: 0;
    border-radius: var(--r-sm) 0 0 var(--r-sm);
  }
  .step:last-child {
    border-left: 0;
    border-radius: 0 var(--r-sm) var(--r-sm) 0;
  }
  .step:hover:not(:disabled) {
    background: var(--bg-element);
    color: var(--text);
  }
  .step:disabled {
    color: var(--text-placeholder);
    cursor: not-allowed;
  }
</style>
