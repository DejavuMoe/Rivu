<!-- 快捷键提示。 -->
<script lang="ts">
  import type { Snippet } from "svelte";
  let { children }: { children: Snippet } = $props();
</script>

<kbd>{@render children()}</kbd>

<style>
  kbd {
    display: inline-grid;
    place-items: center;
    min-width: 20px;
    height: 20px;
    padding: 0 4px;
    border-radius: var(--r-xs);
    background: var(--bg-element);
    color: var(--text-muted);
    font-family: var(--font-sans);
    font-size: var(--fs-sm);
    line-height: 1;
  }
</style>
