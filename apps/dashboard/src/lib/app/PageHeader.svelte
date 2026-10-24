<!--
  页头：标题 + 右侧动作。没有副标题、没有上级路径（侧栏已经给出所在位置）。
  leading 放标题前的标记（站点头像），meta 放紧跟标题的状态（在线人数）。
-->
<script lang="ts">
  import type { Snippet } from "svelte";

  interface Props {
    title: string;
    leading?: Snippet;
    meta?: Snippet;
    actions?: Snippet;
  }
  let { title, leading, meta, actions }: Props = $props();
</script>

<header class="ph">
  <div class="left">
    {#if leading}{@render leading()}{/if}
    <h1>{title}</h1>
    {#if meta}{@render meta()}{/if}
  </div>
  {#if actions}<div class="actions">{@render actions()}</div>{/if}
</header>

<style>
  .ph {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-4);
    flex-wrap: wrap;
    min-height: var(--control-md);
  }
  .left {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
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
</style>
