<!-- 提示条：页面或卡片内的说明、警告、错误。总是带图标和文字，颜色只是辅助。 -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { CircleAlert, CircleCheck, Info, TriangleAlert } from "@lucide/svelte";
  import type { IconType } from "./types";

  interface Props {
    tone?: "info" | "good" | "warn" | "bad" | "neutral";
    title?: string;
    icon?: IconType;
    children?: Snippet;
    actions?: Snippet;
  }
  let { tone = "info", title, icon, children, actions }: Props = $props();

  const icons = { info: Info, good: CircleCheck, warn: TriangleAlert, bad: CircleAlert, neutral: Info };
  const Icon = $derived(icon ?? icons[tone]);
</script>

<div class="callout {tone}" role={tone === "bad" ? "alert" : "status"}>
  <Icon size={16} strokeWidth={1.9} class="ico" aria-hidden="true" />
  <div class="content">
    {#if title}<p class="title">{title}</p>{/if}
    {#if children}<div class="body">{@render children()}</div>{/if}
  </div>
  {#if actions}<div class="actions">{@render actions()}</div>{/if}
</div>

<style>
  .callout {
    --tone-text: var(--accent-text);
    --tone-bg: var(--accent-bg);
    display: flex;
    gap: var(--sp-2);
    align-items: flex-start;
    padding: 10px var(--sp-3);
    border-radius: var(--r-sm);
    background: var(--tone-bg);
    font-size: var(--fs-md);
  }
  .good {
    --tone-text: var(--good-text);
    --tone-bg: var(--good-bg);
  }
  .warn {
    --tone-text: var(--warn-text);
    --tone-bg: var(--warn-bg);
  }
  .bad {
    --tone-text: var(--bad-text);
    --tone-bg: var(--bad-bg);
  }
  .neutral {
    --tone-text: var(--text-muted);
    --tone-bg: var(--bg-subtle);
  }
  .callout :global(.ico) {
    flex: none;
    margin-top: 3px;
    color: var(--tone-text);
  }
  .content {
    flex: 1;
    min-width: 0;
    color: var(--text);
  }
  .title {
    font-weight: var(--fw-medium);
  }
  .actions {
    display: flex;
    gap: var(--sp-2);
    flex: none;
    align-self: center;
  }
</style>
