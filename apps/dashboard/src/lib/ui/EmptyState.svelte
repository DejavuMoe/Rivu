<!-- 空状态 / 错误状态：图标 + 标题 + 一句说明 + 最多两个动作。compact 用在卡片内。 -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { IconType } from "./types";

  interface Props {
    icon?: IconType;
    title: string;
    body?: string;
    tone?: "neutral" | "bad" | "warn";
    compact?: boolean;
    actions?: Snippet;
  }
  let { icon: Icon, title, body, tone = "neutral", compact = false, actions }: Props = $props();
</script>

<div class="empty {tone}" class:compact role={tone === "neutral" ? undefined : "alert"}>
  {#if Icon}
    <div class="icon"><Icon size={24} strokeWidth={1.5} aria-hidden="true" /></div>
  {/if}
  <p class="title">{title}</p>
  {#if body}<p class="body">{body}</p>{/if}
  {#if actions}<div class="actions">{@render actions()}</div>{/if}
</div>

<style>
  .empty {
    display: flex;
    flex-direction: column;
    align-items: center;
    text-align: center;
    padding: var(--sp-12) var(--sp-6);
  }
  .compact {
    padding: var(--sp-8) var(--sp-4);
  }
  .icon {
    margin-bottom: var(--sp-3);
    color: var(--text-placeholder);
  }
  .bad .icon {
    color: var(--bad-text);
  }
  .warn .icon {
    color: var(--warn-text);
  }
  .title {
    font-weight: var(--fw-semibold);
  }
  .body {
    max-width: 400px;
    margin-top: var(--sp-1);
    color: var(--text-muted);
  }
  .actions {
    display: flex;
    gap: var(--sp-2);
    margin-top: var(--sp-5);
  }
</style>
