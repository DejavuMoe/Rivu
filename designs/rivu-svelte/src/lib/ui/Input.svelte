<!-- 单行输入框。可带前置图标、后置片段（单位、按钮）。invalid 时红框，错误文字由 Field 负责。 -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLInputAttributes } from "svelte/elements";
  import type { IconType } from "./types";

  interface Props extends Omit<HTMLInputAttributes, "size"> {
    value?: string | number | null;
    icon?: IconType;
    suffix?: Snippet;
    size?: "sm" | "md" | "lg";
    invalid?: boolean;
    mono?: boolean;
    el?: HTMLInputElement;
  }

  let {
    value = $bindable(),
    icon: Icon,
    suffix,
    size = "md",
    invalid = false,
    mono = false,
    el = $bindable(),
    class: cls = "",
    ...rest
  }: Props = $props();
</script>

<div class="input {size} {cls}" class:invalid class:mono>
  {#if Icon}<Icon size={15} strokeWidth={1.75} class="ico" aria-hidden="true" />{/if}
  <input bind:this={el} bind:value aria-invalid={invalid || undefined} {...rest} />
  {#if suffix}<div class="suffix">{@render suffix()}</div>{/if}
</div>

<style>
  .input {
    display: flex;
    align-items: center;
    gap: 8px;
    height: var(--control-md);
    padding: 0 10px;
    border: var(--hairline) solid var(--border);
    border-radius: var(--r-sm);
    background: var(--bg-app);
  }
  .sm {
    height: var(--control-sm);
  }
  .lg {
    height: var(--control-lg);
  }
  .input:hover {
    border-color: var(--border-hover);
  }
  .input:focus-within {
    border-color: var(--accent-focus);
    box-shadow: var(--focus-ring);
  }
  .invalid,
  .invalid:hover {
    border-color: var(--bad-border);
  }
  input {
    flex: 1;
    min-width: 0;
    height: 100%;
    padding: 0;
    border: 0;
    outline: 0;
    background: transparent;
    box-shadow: none !important;
  }
  input::placeholder {
    color: var(--text-placeholder);
  }
  input:disabled {
    color: var(--text-muted);
    cursor: not-allowed;
  }
  .mono input {
    font-family: var(--font-mono);
  }
  .input :global(.ico) {
    color: var(--text-muted);
    flex: none;
  }
  .suffix {
    display: flex;
    align-items: center;
    gap: 4px;
    margin-right: -6px;
    color: var(--text-muted);
  }
</style>
