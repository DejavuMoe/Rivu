<!-- 在线状态点：有人在线时为绿色，否则灰色。静止，不做动画。 -->
<script lang="ts">
  let { off = false, size = 8 }: { off?: boolean; size?: number } = $props();
</script>

<span class="live" class:off style:--s="{size}px" aria-hidden="true"></span>

<style>
  .live {
    display: inline-block;
    flex: none;
    width: var(--s);
    height: var(--s);
    border-radius: 50%;
    background: var(--live);
  }
  .off {
    background: var(--text-placeholder);
  }
</style>
