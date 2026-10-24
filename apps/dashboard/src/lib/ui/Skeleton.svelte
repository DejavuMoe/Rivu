<!-- 加载占位：只在首次加载用。重新查询时保留旧结果并降低不透明度（见 .refetching），不闪骨架。 -->
<script lang="ts">
  let { width = "100%", height = 12, radius = 4 }: { width?: string; height?: number; radius?: number } = $props();
</script>

<span class="sk" style:width style:height="{height}px" style:border-radius="{radius}px" aria-hidden="true"></span>

<style>
  .sk {
    display: block;
    background: var(--bg-element);
  }
</style>
