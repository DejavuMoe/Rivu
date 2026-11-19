<!--
  只读值 + 复制按钮：站点标识、安装代码、API 密钥（只显示一次）。
  multiline 用于代码片段；secret 时默认打码，可切换显示。
-->
<script lang="ts">
  import { Check, Copy, Eye, EyeOff } from "@lucide/svelte";
  import { t } from "../i18n";

  interface Props {
    value: string;
    label: string;
    multiline?: boolean;
    secret?: boolean;
  }
  let { value, label, multiline = false, secret = false }: Props = $props();

  let copied = $state(false);
  let revealed = $state(false);
  const shown = $derived(secret && !revealed ? value.slice(0, 12) + "•".repeat(24) : value);

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
    } catch {
      /* 原型里剪贴板不可用时照样给反馈 */
    }
    copied = true;
    setTimeout(() => (copied = false), 1600);
  }
</script>

<div class="copy" class:multiline>
  {#if multiline}
    <pre aria-label={label}><code>{shown}</code></pre>
  {:else}
    <code class="one" aria-label={label}>{shown}</code>
  {/if}
  <div class="btns">
    {#if secret}
      <button type="button" aria-label={revealed ? t("common.hide") : t("common.show")} onclick={() => (revealed = !revealed)}>
        {#if revealed}<EyeOff size={14} strokeWidth={1.75} />{:else}<Eye size={14} strokeWidth={1.75} />{/if}
      </button>
    {/if}
    <button type="button" class:ok={copied} onclick={copy} aria-label={t("common.copy")}>
      {#if copied}<Check size={14} strokeWidth={2} />{t("common.copied")}{:else}<Copy size={14} strokeWidth={1.75} />{t("common.copy")}{/if}
    </button>
  </div>
  <span class="sr-only" aria-live="polite">{copied ? t("common.copied") : ""}</span>
</div>

<style>
  .copy {
    position: relative;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: var(--control-md);
    padding: 0 4px 0 10px;
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-sm);
    background: var(--bg-subtle);
  }
  .multiline {
    display: block;
    padding: 0;
  }
  .one {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  pre {
    margin: 0;
    padding: var(--sp-3) 96px var(--sp-3) var(--sp-3);
    overflow-x: auto;
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
    line-height: 1.7;
  }
  .btns {
    display: flex;
    gap: 2px;
  }
  /* 浮在代码块右上角：外框圆角 6 − 间距 4 = 按钮圆角 2，取 r-xs 视觉更稳 */
  .multiline .btns {
    position: absolute;
    top: 4px;
    right: 4px;
  }
  button {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 24px;
    padding: 0 6px;
    border: 0;
    border-radius: var(--r-xs);
    background: transparent;
    color: var(--text-muted);
    font-size: var(--fs-sm);
    font-weight: var(--fw-medium);
  }
  button:hover {
    background: var(--bg-hover);
    color: var(--text);
  }
  .ok {
    color: var(--good-text);
  }
  /* 窄屏：复制按钮移到代码下方，不再盖住代码 */
  @media (max-width: 560px) {
    pre {
      padding-right: var(--sp-3);
    }
    .multiline .btns {
      position: static;
      justify-content: flex-end;
      padding: 0 4px 4px;
    }
  }
</style>
