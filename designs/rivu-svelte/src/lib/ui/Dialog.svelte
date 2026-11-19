<!--
  对话框：基于原生 <dialog>（showModal 自带焦点圈定、Esc 关闭、背景不可交互）。
  size：sm 确认类，md 表单，lg 明细表格。footer 片段放按钮，主按钮在最右。
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { X } from "@lucide/svelte";
  import { t } from "../i18n";

  interface Props {
    open: boolean;
    title: string;
    description?: string;
    size?: "sm" | "md" | "lg";
    onclose?: () => void;
    requireConfirmation?: boolean;
    children: Snippet;
    footer?: Snippet;
  }

  let { open = $bindable(), title, description, size = "md", onclose, requireConfirmation = false, children, footer }: Props = $props();

  let dlg: HTMLDialogElement;
  const titleId = `dlg-${Math.random().toString(36).slice(2, 8)}`;

  $effect(() => {
    if (open && !dlg.open) dlg.showModal();
    if (!open && dlg.open) dlg.close();
  });

  function handleClose() {
    open = false;
    onclose?.();
  }
</script>

<dialog
  bind:this={dlg}
  class={size}
  aria-labelledby={titleId}
  onclose={handleClose}
  oncancel={(e) => {
    if (requireConfirmation) e.preventDefault();
  }}
  onclick={(e) => {
    if (!requireConfirmation && e.target === dlg) dlg.close();
  }}
>
  {#if open}
    <div class="inner">
      <header>
        <div>
          <h2 id={titleId}>{title}</h2>
          {#if description}<p>{description}</p>{/if}
        </div>
        {#if !requireConfirmation}
          <button type="button" class="x" aria-label={t("common.close")} onclick={() => dlg.close()}><X size={16} strokeWidth={1.75} /></button>
        {/if}
      </header>
      <div class="body">{@render children()}</div>
      {#if footer}<footer>{@render footer()}</footer>{/if}
    </div>
  {/if}
</dialog>

<style>
  dialog {
    width: min(calc(100vw - 32px), var(--w));
    max-height: min(85vh, 760px);
    padding: 0;
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-lg);
    background: var(--bg-app);
    color: var(--text);
    box-shadow: var(--shadow-pop);
    overflow: hidden;
  }
  dialog[open] {
    animation: dialog-in var(--dur) var(--ease);
  }
  dialog::backdrop {
    background: var(--overlay);
  }
  dialog[open]::backdrop {
    animation: fade-in var(--dur) var(--ease);
  }
  .sm {
    --w: 400px;
  }
  .md {
    --w: 520px;
  }
  .lg {
    --w: 800px;
  }
  .inner {
    display: flex;
    flex-direction: column;
    max-height: inherit;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: var(--sp-4);
    padding: var(--sp-5) var(--sp-4) var(--sp-3) var(--sp-5);
  }
  h2 {
    padding-top: 2px;
    font-size: var(--fs-lg);
  }
  header p {
    margin-top: var(--sp-1);
    color: var(--text-muted);
  }
  .x {
    display: grid;
    place-items: center;
    flex: none;
    width: var(--control-sm);
    height: var(--control-sm);
    border: 0;
    border-radius: var(--r-sm);
    background: none;
    color: var(--text-muted);
  }
  .x:hover {
    background: var(--bg-element);
    color: var(--text);
  }
  .body {
    padding: var(--sp-2) var(--sp-5) var(--sp-5);
    overflow: auto;
  }
  footer {
    display: flex;
    justify-content: flex-end;
    gap: var(--sp-2);
    padding: var(--sp-3) var(--sp-4) var(--sp-4);
  }
</style>
