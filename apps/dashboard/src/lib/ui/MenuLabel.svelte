<!-- 菜单分组标题；separator 为 true 时上方带一条分隔线。 -->
<script lang="ts">
  import type { Snippet } from "svelte";
  let { children, separator = false }: { children?: Snippet; separator?: boolean } = $props();
</script>

{#if separator}<div class="sep" role="separator"></div>{/if}
{#if children}<div class="label">{@render children()}</div>{/if}

<style>
  .sep {
    height: 0;
    margin: 4px -4px;
    border-top: var(--hairline) solid var(--border-subtle);
  }
  .label {
    padding: 8px 8px 4px;
    color: var(--text-muted);
    font-size: var(--fs-sm);
    font-weight: var(--fw-medium);
  }
</style>
