<!--
  表单字段：标签 + 控件 + 说明 / 错误。layout="row" 用于设置页（左说明、右控件）。
  控件用 children 传入，id 由调用方给控件，同时传给 Field 的 for。
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { t } from "../i18n";

  interface Props {
    label: string;
    for?: string;
    hint?: string;
    error?: string;
    optional?: boolean;
    layout?: "stack" | "row";
    children: Snippet;
    labelExtra?: Snippet;
  }

  let { label, for: htmlFor, hint, error, optional = false, layout = "stack", children, labelExtra }: Props = $props();
</script>

<div class="field {layout}">
  <div class="head">
    <div class="label-row">
      <label for={htmlFor}>{label}</label>
      {#if optional}<span class="opt">{t("common.optional")}</span>{/if}
      {#if labelExtra}<span class="extra">{@render labelExtra()}</span>{/if}
    </div>
    {#if hint && layout === "row"}<p class="hint">{hint}</p>{/if}
  </div>
  <div class="control">
    {@render children()}
    {#if error}
      <p class="error" role="alert">{error}</p>
    {:else if hint && layout === "stack"}
      <p class="hint">{hint}</p>
    {/if}
  </div>
</div>

<style>
  .field.stack {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .field.row {
    display: grid;
    grid-template-columns: minmax(180px, 1fr) minmax(0, 1.5fr);
    align-items: start;
    gap: var(--sp-6);
    padding: var(--sp-4) 0;
    border-top: var(--hairline) solid var(--border-subtle);
  }
  /* 卡片已有内边距，首尾字段不再重复留白 */
  .field.row:first-child {
    border-top: 0;
    padding-top: 0;
  }
  .field.row:last-child {
    padding-bottom: 0;
  }
  /* 行布局里标签与控件的第一行文字对齐（控件高 32，标签行高 21） */
  .row .head {
    padding-top: 5px;
  }
  .label-row {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
  }
  label {
    font-weight: var(--fw-medium);
  }
  .opt {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .extra {
    margin-left: auto;
    font-size: var(--fs-sm);
  }
  .control {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  .hint {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .row .head .hint {
    margin-top: 2px;
  }
  .error {
    color: var(--bad-text);
    font-size: var(--fs-sm);
  }
  @media (max-width: 720px) {
    .field.row {
      grid-template-columns: 1fr;
      gap: var(--sp-2);
    }
    .row .head {
      padding-top: 0;
    }
  }
</style>
