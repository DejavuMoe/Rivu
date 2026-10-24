<!-- 复选框（带文字）。原生 input，外观自绘。 -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { Check } from "@lucide/svelte";

  interface Props {
    checked: boolean;
    disabled?: boolean;
    hint?: string;
    onchange?: (v: boolean) => void;
    children: Snippet;
  }
  let { checked = $bindable(), disabled = false, hint, onchange, children }: Props = $props();
</script>

<label class="cb" class:disabled>
  <input type="checkbox" bind:checked {disabled} onchange={() => onchange?.(checked)} />
  <span class="box" aria-hidden="true"><Check size={12} strokeWidth={3} /></span>
  <span class="text">
    <span>{@render children()}</span>
    {#if hint}<span class="hint">{hint}</span>{/if}
  </span>
</label>

<style>
  .cb {
    display: inline-flex;
    align-items: flex-start;
    gap: var(--sp-2);
    cursor: pointer;
  }
  .disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  input {
    position: absolute;
    opacity: 0;
    width: 1px;
    height: 1px;
  }
  /* 16px 方框与 14px 文字（行高 21）的第一行居中对齐 */
  .box {
    display: grid;
    place-items: center;
    flex: none;
    width: 16px;
    height: 16px;
    margin-top: 2.5px;
    border: var(--hairline) solid var(--border-hover);
    border-radius: var(--r-xs);
    background: var(--bg-app);
    color: transparent;
  }
  input:checked + .box {
    background: var(--accent-solid);
    border-color: var(--accent-solid);
    color: var(--accent-contrast);
  }
  input:focus-visible + .box {
    box-shadow: var(--focus-ring);
  }
  .text {
    display: flex;
    flex-direction: column;
  }
  .hint {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
</style>
