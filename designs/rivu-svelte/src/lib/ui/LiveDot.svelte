<!-- 在线状态点：有人在线时是灯光色，带一圈慢慢扩散的光（「此刻」唯一的动效）；没人时是灰的、静止。 -->
<script lang="ts">
  let { off = false, size = 8 }: { off?: boolean; size?: number } = $props();
</script>

<span class="live" class:off style:--s="{size}px" aria-hidden="true"></span>

<style>
  .live {
    position: relative;
    display: inline-block;
    flex: none;
    width: var(--s);
    height: var(--s);
    border-radius: 50%;
    background: var(--live);
  }
  .live::after {
    content: "";
    position: absolute;
    inset: 0;
    border-radius: 50%;
    background: var(--live);
    animation: lamp var(--dur-pulse) var(--ease) infinite;
  }
  @keyframes lamp {
    from {
      opacity: 0.55;
      transform: scale(1);
    }
    to {
      opacity: 0;
      transform: scale(2.8);
    }
  }
  .off {
    background: var(--text-placeholder);
  }
  .off::after {
    display: none;
  }
</style>
