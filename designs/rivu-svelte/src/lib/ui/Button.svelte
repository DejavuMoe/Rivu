<!--
  按钮。variant：primary 主操作（每个区域最多一个），secondary 默认，ghost 工具栏与次要操作，danger 对话框里的破坏性确认。
  传 href 时渲染成链接，外观一致。
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import type { HTMLButtonAttributes } from "svelte/elements";
  import type { IconType } from "./types";

  interface Props extends Omit<HTMLButtonAttributes, "children"> {
    variant?: "primary" | "secondary" | "ghost" | "danger";
    size?: "sm" | "md" | "lg";
    icon?: IconType;
    iconRight?: IconType;
    href?: string;
    loading?: boolean;
    /** 选中/展开态（工具栏切换按钮） */
    active?: boolean;
    block?: boolean;
    children?: Snippet;
  }

  let {
    variant = "secondary",
    size = "md",
    icon: Icon,
    iconRight: IconRight,
    href,
    loading = false,
    active = false,
    block = false,
    disabled,
    type = "button",
    children,
    class: cls = "",
    ...rest
  }: Props = $props();
</script>

{#snippet inner()}
  {#if Icon}<Icon size={16} strokeWidth={1.75} aria-hidden="true" />{/if}
  {#if children}<span class="label">{@render children()}</span>{/if}
  {#if IconRight}<IconRight size={16} strokeWidth={1.75} aria-hidden="true" />{/if}
{/snippet}

{#if href && !disabled}
  <a {href} class="btn {variant} {size} {cls}" class:block class:active class:icon-only={!children} {...rest as Record<string, unknown>}>
    {@render inner()}
  </a>
{:else}
  <button
    {type}
    class="btn {variant} {size} {cls}"
    class:block
    class:active
    class:icon-only={!children}
    disabled={disabled || loading}
    aria-busy={loading || undefined}
    {...rest}
  >
    {@render inner()}
  </button>
{/if}

<style>
  /* 按下时略微收缩，给点击一个触感反馈 */
  .btn:active:not(:disabled) {
    transform: scale(0.97);
  }
  .btn {
    --h: var(--control-md);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    height: var(--h);
    padding: 0 12px;
    border: var(--hairline) solid transparent;
    border-radius: var(--r-sm);
    font-size: var(--fs-md);
    font-weight: var(--fw-medium);
    line-height: 1;
    white-space: nowrap;
    user-select: none;
    transition:
      background-color var(--dur-fast) var(--ease),
      border-color var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease),
      transform var(--dur-fast) var(--ease);
  }
  /* 带图标时左侧内边距收 2px：图标的视觉边界比文字更靠外，光学上才对齐 */
  .btn:has(> :global(svg:first-child)):not(.icon-only) {
    padding-left: 10px;
  }
  .sm {
    --h: var(--control-sm);
    padding: 0 10px;
  }
  .sm:has(> :global(svg:first-child)):not(.icon-only) {
    padding-left: 8px;
  }
  .lg {
    --h: var(--control-lg);
  }
  .icon-only {
    width: var(--h);
    padding: 0;
  }
  .block {
    display: flex;
    width: 100%;
  }

  /* 主操作是墨色的，与标志一致；颜色留给数据 */
  .primary {
    background: var(--ink);
    color: var(--ink-contrast);
  }
  .primary:hover:not(:disabled) {
    background: var(--ink-hover);
  }

  .secondary {
    background: var(--bg-app);
    border-color: var(--border);
    color: var(--text);
    box-shadow: var(--shadow-control);
  }
  .secondary:hover:not(:disabled),
  .secondary.active {
    background: var(--bg-element);
    border-color: var(--border-hover);
  }

  .ghost {
    background: transparent;
    color: var(--text-muted);
  }
  .ghost:hover:not(:disabled) {
    background: var(--bg-element);
    color: var(--text);
  }
  .ghost.active {
    background: var(--bg-hover);
    color: var(--text);
  }

  .danger {
    background: var(--bad-solid);
    color: var(--accent-contrast);
  }
  .danger:hover:not(:disabled) {
    background: var(--bad-solid-hover);
  }

  .btn:disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }
  .btn[aria-busy="true"] {
    cursor: progress;
  }

  .label {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
</style>
