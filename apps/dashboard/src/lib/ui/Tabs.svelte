<!--
  标签页（下划线式）：卡片内维度切换、设置页分区。选中项下方是 2px 品牌色「水流线」。
  只负责切换按钮；内容由调用方按 value 渲染。
-->
<script lang="ts" generics="V extends string">
  import type { Option } from "./types";

  interface Props {
    options: Option<V>[];
    value: V;
    onchange?: (v: V) => void;
    label: string;
    size?: "sm" | "md";
  }
  let { options, value = $bindable(), onchange, label, size = "md" }: Props = $props();

  function onKeydown(e: KeyboardEvent) {
    if (e.key !== "ArrowRight" && e.key !== "ArrowLeft") return;
    const i = options.findIndex((o) => o.value === value);
    const n = options[(i + (e.key === "ArrowRight" ? 1 : -1) + options.length) % options.length];
    value = n.value;
    onchange?.(n.value);
    (e.currentTarget as HTMLElement).querySelector<HTMLElement>(`[data-v="${n.value}"]`)?.focus();
    e.preventDefault();
  }
</script>

<div class="tabs {size}" role="tablist" aria-label={label} tabindex="-1" onkeydown={onKeydown}>
  {#each options as o (o.value)}
    <button
      type="button"
      role="tab"
      data-v={o.value}
      aria-selected={o.value === value}
      tabindex={o.value === value ? 0 : -1}
      onclick={() => {
        value = o.value;
        onchange?.(o.value);
      }}
    >
      {#if o.icon}<o.icon size={14} strokeWidth={1.75} aria-hidden="true" />{/if}
      {o.label}
      {#if o.hint}<span class="count">{o.hint}</span>{/if}
    </button>
  {/each}
</div>

<style>
  .tabs {
    display: flex;
    gap: 2px;
    min-width: 0;
    overflow-x: auto;
    scrollbar-width: none;
  }
  button {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: var(--control-sm);
    padding: 0 10px;
    border: 0;
    border-radius: var(--r-sm);
    background: none;
    color: var(--text-muted);
    font-size: var(--fs-md);
    font-weight: var(--fw-medium);
    white-space: nowrap;
  }
  .md button {
    height: var(--control-md);
  }
  button:hover {
    background: var(--bg-element);
    color: var(--text);
  }
  button[aria-selected="true"] {
    background: var(--bg-hover);
    color: var(--text);
  }
  .count {
    color: var(--text-muted);
    font-size: var(--fs-sm);
    font-variant-numeric: tabular-nums;
  }
</style>
