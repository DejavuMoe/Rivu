<!--
  下拉选择（listbox）。外观与 Button secondary 一致；选项可带图标和提示。
  prefix 是按钮里值前面的灰色说明（如「对比」）。
-->
<script lang="ts" generics="V extends string">
  import { ChevronDown } from "@lucide/svelte";
  import Popover from "./Popover.svelte";
  import MenuItem from "./MenuItem.svelte";
  import type { IconType, Option } from "./types";

  interface Props {
    options: Option<V>[];
    value: V;
    onchange?: (v: V) => void;
    label: string;
    prefix?: string;
    icon?: IconType;
    size?: "sm" | "md";
    align?: "start" | "end";
    width?: number;
    variant?: "secondary" | "ghost";
    block?: boolean;
  }

  let {
    options,
    value = $bindable(),
    onchange,
    label,
    prefix,
    icon: Icon,
    size = "md",
    align = "start",
    width = 220,
    variant = "secondary",
    block = false,
  }: Props = $props();

  const current = $derived(options.find((o) => o.value === value));
</script>

<Popover role="listbox" {label} {align} {width}>
  {#snippet trigger({ toggle, props })}
    <button type="button" class="select {size} {variant}" class:block onclick={toggle} aria-label={label} {...props}>
      {#if Icon}<Icon size={15} strokeWidth={1.75} aria-hidden="true" class="ico" />{/if}
      {#if prefix}<span class="prefix">{prefix}</span>{/if}
      <span class="value">{current?.label ?? "—"}</span>
      <ChevronDown size={14} strokeWidth={1.75} aria-hidden="true" class="chev" />
    </button>
  {/snippet}
  {#snippet children({ close })}
    {#each options as o (o.value)}
      <MenuItem
        role="option"
        icon={o.icon}
        hint={o.hint}
        disabled={o.disabled}
        checked={o.value === value}
        onclick={() => {
          value = o.value;
          onchange?.(o.value);
          close();
        }}>{o.label}</MenuItem
      >
    {/each}
  {/snippet}
</Popover>

<style>
  .select {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: var(--control-md);
    padding: 0 8px 0 10px;
    border: var(--hairline) solid var(--border);
    border-radius: var(--r-sm);
    background: var(--bg-app);
    font-size: var(--fs-md);
    font-weight: var(--fw-medium);
    white-space: nowrap;
  }
  .select:hover,
  .select[aria-expanded="true"] {
    background: var(--bg-element);
    border-color: var(--border-hover);
  }
  .ghost {
    border-color: transparent;
    background: transparent;
    color: var(--text-muted);
  }
  .ghost:hover,
  .ghost[aria-expanded="true"] {
    background: var(--bg-element);
    color: var(--text);
  }
  .sm {
    height: var(--control-sm);
    font-size: var(--fs-md);
  }
  .block {
    width: 100%;
  }
  .block .value {
    flex: 1;
    text-align: left;
  }
  .prefix {
    color: var(--text-muted);
    font-weight: var(--fw-regular);
  }
  .select :global(.ico),
  .select :global(.chev) {
    color: var(--text-muted);
    flex: none;
  }
</style>
