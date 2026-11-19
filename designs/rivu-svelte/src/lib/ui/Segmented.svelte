<!-- 分段控件：2–5 个互斥选项，就地切换（粒度、主题、视图）。选项多于 5 个用 Select。选中的底块在选项之间滑动。 -->
<script lang="ts" generics="V extends string">
  import type { Option } from "./types";
  import { trackIndicator } from "./indicator";

  interface Props {
    options: Option<V>[];
    value: V;
    onchange?: (v: V) => void;
    label: string;
    size?: "sm" | "md";
    block?: boolean;
  }

  let { options, value = $bindable(), onchange, label, size = "md", block = false }: Props = $props();

  let box: HTMLDivElement;
  let ind: HTMLSpanElement;
  $effect(() => trackIndicator(box, ind, '[aria-checked="true"]'));

  function onKeydown(e: KeyboardEvent) {
    if (e.key !== "ArrowRight" && e.key !== "ArrowLeft") return;
    const enabled = options.filter((o) => !o.disabled);
    const i = enabled.findIndex((o) => o.value === value);
    const n = enabled[(i + (e.key === "ArrowRight" ? 1 : -1) + enabled.length) % enabled.length];
    value = n.value;
    onchange?.(n.value);
    (e.currentTarget as HTMLElement).querySelector<HTMLElement>(`[data-v="${n.value}"]`)?.focus();
    e.preventDefault();
  }
</script>

<div class="seg {size}" class:block role="radiogroup" aria-label={label} tabindex="-1" onkeydown={onKeydown} bind:this={box}>
  <span class="thumb" bind:this={ind} aria-hidden="true"></span>
  {#each options as o (o.value)}
    <button
      type="button"
      role="radio"
      data-v={o.value}
      aria-checked={o.value === value}
      tabindex={o.value === value ? 0 : -1}
      disabled={o.disabled}
      title={o.hint}
      onclick={() => {
        value = o.value;
        onchange?.(o.value);
      }}
    >
      {#if o.icon}<o.icon size={14} strokeWidth={1.75} aria-hidden="true" />{/if}
      {#if o.label}<span>{o.label}</span>{/if}
    </button>
  {/each}
</div>

<style>
  .seg {
    position: relative;
    display: inline-flex;
    /* 放进纵向 flex 容器（如 Field）时不被拉伸成整行 */
    align-self: flex-start;
    padding: 2px;
    gap: 2px;
    border-radius: var(--r-sm);
    background: var(--bg-element);
  }
  .block {
    display: flex;
    align-self: stretch;
  }
  .block button {
    flex: 1;
  }
  .thumb {
    position: absolute;
    top: 0;
    left: 0;
    border-radius: var(--r-xs);
    background: var(--bg-app);
    box-shadow: 0 0 0 var(--hairline) var(--border);
    transition:
      transform var(--dur) var(--ease-move),
      width var(--dur) var(--ease-move);
  }
  button {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    height: calc(var(--control-md) - 4px);
    padding: 0 10px;
    border: 0;
    border-radius: var(--r-xs);
    background: transparent;
    color: var(--text-muted);
    font-size: var(--fs-md);
    font-weight: var(--fw-medium);
    white-space: nowrap;
    transition: color var(--dur-fast) var(--ease);
  }
  .sm button {
    height: calc(var(--control-sm) - 4px);
    padding: 0 8px;
    font-size: var(--fs-sm);
  }
  button:hover:not(:disabled) {
    color: var(--text);
  }
  button[aria-checked="true"] {
    color: var(--text);
  }
  button:disabled {
    opacity: 0.45;
    cursor: not-allowed;
  }
</style>
