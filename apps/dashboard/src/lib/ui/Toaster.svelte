<!-- 底部居中的轻提示，由 state.svelte.ts 的 toast() 推入。 -->
<script lang="ts">
  import { CircleAlert, CircleCheck } from "@lucide/svelte";
  import { app } from "../state.svelte";
</script>

<div class="toaster" aria-live="polite">
  {#each app.toasts as item (item.id)}
    <div class="toast {item.tone}">
      {#if item.tone === "bad"}<CircleAlert size={15} strokeWidth={1.9} />{:else}<CircleCheck size={15} strokeWidth={1.9} />{/if}
      {item.text}
    </div>
  {/each}
</div>

<style>
  .toaster {
    position: fixed;
    left: 50%;
    bottom: 24px;
    z-index: 100;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: var(--sp-2);
    transform: translateX(-50%);
    pointer-events: none;
  }
  .toast {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: 36px;
    padding: 0 14px 0 12px;
    border-radius: var(--r-md);
    background: var(--text);
    color: var(--bg-app);
    box-shadow: var(--shadow-pop);
    font-weight: var(--fw-medium);
  }
</style>
