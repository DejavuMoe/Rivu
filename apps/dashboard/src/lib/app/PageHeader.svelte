<!--
  页头：标题 + 右侧动作，可选下面一行标签栏。没有副标题、没有上级路径（侧栏已经给出所在位置）。
  leading 放标题前的标记（站点头像），meta 放紧跟标题的状态（在线人数）。
  tabs：同一对象下的几个页面（站点的看板 / 接入诊断 / 安装 / 设置，组织的成员 / API 密钥）。下划线在标签之间滑动；
  换页时整页淡入淡出，页头自己不参与（view-transition-name），所以下划线的移动看得见。
  compactTitle：窄屏顶栏已经显示这个名字时，页头不再重复标题与 leading。
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { trackIndicator } from "../ui/indicator";

  export interface HeaderTab {
    label: string;
    href: string;
    current: boolean;
  }

  interface Props {
    title: string;
    leading?: Snippet;
    meta?: Snippet;
    actions?: Snippet;
    tabs?: HeaderTab[];
    tabsLabel?: string;
    /** 同一组标签的标识（如 site-1）：换页后下划线从上一页的位置滑过来 */
    tabsKey?: string;
    compactTitle?: boolean;
  }
  let { title, leading, meta, actions, tabs, tabsLabel, tabsKey, compactTitle = false }: Props = $props();

  let bar: HTMLElement | undefined = $state();
  let ind: HTMLSpanElement | undefined = $state();
  $effect(() => {
    if (bar && ind) return trackIndicator(bar, ind, '[aria-current="page"]', "underline", tabsKey);
  });
</script>

<header class="ph" class:tabbed={tabs} class:compact={compactTitle}>
  <div class="row">
    <div class="left">
      {#if leading}<span class="lead">{@render leading()}</span>{/if}
      <h1>{title}</h1>
      {#if meta}{@render meta()}{/if}
    </div>
    {#if actions}<div class="actions">{@render actions()}</div>{/if}
  </div>

  {#if tabs}
    <nav class="tabs" aria-label={tabsLabel ?? title} bind:this={bar}>
      <span class="line" bind:this={ind} aria-hidden="true"></span>
      {#each tabs as x (x.href)}
        <a href={x.href} aria-current={x.current ? "page" : undefined}>{x.label}</a>
      {/each}
    </nav>
  {/if}
</header>

<style>
  .ph {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .tabbed {
    view-transition-name: page-header;
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-2) var(--sp-4);
    flex-wrap: wrap;
    min-height: var(--control-md);
  }
  .left {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
  }
  .lead {
    display: inline-flex;
    flex: none;
  }
  h1 {
    overflow: hidden;
    font-size: var(--fs-lg);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .actions {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }

  /* 标签栏：底部一条细线，选中项下方 2px 的强调色下划线。第一个标签的文字与标题左缘对齐（标签有 8px 内边距）。 */
  .tabs {
    position: relative;
    display: flex;
    gap: var(--sp-1);
    margin-left: calc(-1 * var(--sp-2));
    overflow-x: auto;
    scrollbar-width: none;
  }
  .tabs::after {
    content: "";
    position: absolute;
    left: var(--sp-2);
    right: 0;
    bottom: 0;
    height: var(--hairline);
    background: var(--border-subtle);
  }
  .tabs a {
    position: relative;
    isolation: isolate;
    display: inline-flex;
    align-items: center;
    flex: none;
    height: 36px;
    padding: 0 var(--sp-2);
    color: var(--text-muted);
    font-weight: var(--fw-medium);
    white-space: nowrap;
    transition: color var(--dur-fast) var(--ease);
  }
  .tabs a::before {
    /* 悬停底色落在下划线上方 */
    content: "";
    position: absolute;
    inset: 4px 0 6px;
    z-index: -1;
    border-radius: var(--r-sm);
    background: var(--bg-element);
    opacity: 0;
    transition: opacity var(--dur-fast) var(--ease);
  }
  .tabs a:hover,
  .tabs a[aria-current="page"] {
    color: var(--text);
  }
  .tabs a:hover::before {
    opacity: 1;
  }
  .tabs a:focus-visible {
    box-shadow: none;
  }
  .tabs a:focus-visible::before {
    opacity: 1;
    box-shadow: var(--focus-ring);
  }
  .line {
    position: absolute;
    left: 0;
    bottom: 0;
    z-index: 1;
    height: 2px;
    border-radius: 1px;
    background: var(--accent-solid);
    transition:
      transform var(--dur) var(--ease-move),
      width var(--dur) var(--ease-move);
  }

  :global(::view-transition-old(page-header)) {
    display: none;
  }
  :global(::view-transition-new(page-header)) {
    animation: none;
  }

  @media (max-width: 860px) {
    .compact .lead,
    .compact h1 {
      display: none;
    }
  }
  @media (max-width: 720px) {
    /* 窄屏标签栏贴满屏宽，可以横向滑动 */
    .tabs {
      margin: 0 calc(-1 * var(--sp-4));
      padding: 0 var(--sp-2);
    }
    .tabs::after {
      left: 0;
    }
  }
</style>
