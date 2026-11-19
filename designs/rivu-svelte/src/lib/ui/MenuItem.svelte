<!--
  菜单项。checked 不为 undefined 时是单选项（role=menuitemradio），右侧显示对勾。
  也可以作为 listbox 的 option（role="option"）。
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { Check } from "@lucide/svelte";
  import type { IconType } from "./types";

  interface Props {
    icon?: IconType;
    checked?: boolean;
    danger?: boolean;
    disabled?: boolean;
    shortcut?: string;
    hint?: string;
    href?: string;
    role?: "menuitem" | "menuitemradio" | "option";
    onclick?: () => void;
    children: Snippet;
    leading?: Snippet;
  }

  let {
    icon: Icon,
    checked,
    danger = false,
    disabled = false,
    shortcut,
    hint,
    href,
    role,
    onclick,
    children,
    leading,
  }: Props = $props();

  const r = $derived(role ?? (checked === undefined ? "menuitem" : "menuitemradio"));
</script>

<svelte:element
  this={href ? "a" : "button"}
  {href}
  type={href ? undefined : "button"}
  class="item"
  class:danger
  role={r}
  aria-checked={r === "menuitem" ? undefined : !!checked}
  aria-selected={r === "option" ? !!checked : undefined}
  aria-disabled={disabled || undefined}
  tabindex="-1"
  onclick={disabled ? undefined : onclick}
>
  {#if leading}{@render leading()}{:else if Icon}<Icon size={15} strokeWidth={1.75} class="lead" aria-hidden="true" />{/if}
  <span class="text">
    <span class="main">{@render children()}</span>
    {#if hint}<span class="hint">{hint}</span>{/if}
  </span>
  {#if shortcut}<kbd>{shortcut}</kbd>{/if}
  {#if checked}<Check size={15} strokeWidth={2} class="check" aria-hidden="true" />{/if}
</svelte:element>

<style>
  .item {
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    min-height: 32px;
    padding: 6px 8px;
    border: 0;
    border-radius: var(--r-xs);
    background: none;
    color: var(--text);
    font-size: var(--fs-md);
    text-align: left;
    cursor: pointer;
  }
  .item:hover,
  .item:focus-visible {
    background: var(--bg-hover);
    box-shadow: none;
  }
  .item[aria-disabled="true"] {
    color: var(--text-placeholder);
    cursor: not-allowed;
    background: none;
  }
  .danger {
    color: var(--bad-text);
  }
  .item :global(.lead) {
    color: var(--text-muted);
    flex: none;
  }
  .danger :global(.lead) {
    color: currentColor;
  }
  .text {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
  }
  .main {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .hint {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  kbd {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .item :global(.check) {
    color: var(--text);
    flex: none;
  }
</style>
