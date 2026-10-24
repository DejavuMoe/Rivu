<!-- 徽标：状态、角色、标签。状态类（good/warn/bad）必须带文字，dot 只是辅助。 -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { IconType, Tone } from "./types";

  interface Props {
    tone?: Tone;
    dot?: boolean;
    icon?: IconType;
    variant?: "soft" | "outline";
    children: Snippet;
  }

  let { tone = "neutral", dot = false, icon: Icon, variant = "soft", children }: Props = $props();
</script>

<span class="badge {tone} {variant}">
  {#if dot}<span class="dot" aria-hidden="true"></span>{/if}
  {#if Icon}<Icon size={12} strokeWidth={2} aria-hidden="true" />{/if}
  {@render children()}
</span>

<style>
  .badge {
    --tone-text: var(--text-muted);
    --tone-bg: var(--bg-element);
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 20px;
    padding: 0 6px;
    border-radius: var(--r-xs);
    background: var(--tone-bg);
    color: var(--tone-text);
    font-size: var(--fs-sm);
    font-weight: var(--fw-medium);
    line-height: 1;
    white-space: nowrap;
  }
  .outline {
    background: transparent;
    box-shadow: inset 0 0 0 var(--hairline) var(--border);
  }
  .accent,
  .info {
    --tone-text: var(--accent-text);
    --tone-bg: var(--accent-bg);
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
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: currentColor;
  }
</style>
