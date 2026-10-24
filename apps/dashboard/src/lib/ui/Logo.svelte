<!--
  Rivu 标志：圆角方块里一条上扬的曲线。单色，跟随主要文字色。wordmark 为 true 时右侧带文字。
-->
<script lang="ts">
  let { size = 20, wordmark = true }: { size?: number; wordmark?: boolean } = $props();
</script>

<span class="logo">
  <svg width={size} height={size} viewBox="0 0 32 32" aria-hidden="true">
    <rect width="32" height="32" rx="7" fill="var(--text)" />
    <path d="M7 21.5c2.6-6 5.4-.4 8.3-4.6 2.5-3.6 4.6-6.4 9.7-4.4" stroke="var(--bg-app)" stroke-width="3" fill="none" stroke-linecap="round" />
  </svg>
  {#if wordmark}<span class="word">Rivu</span>{/if}
</span>

<style>
  .logo {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .word {
    font-weight: var(--fw-semibold);
  }
</style>
