<!-- 独立页（登录、初始化）的外壳：标志、居中卡片、卡片下方至多一行说明。 -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import Logo from "../../lib/ui/Logo.svelte";

  interface Props {
    width?: number;
    children: Snippet;
    below?: Snippet;
  }
  let { width = 400, children, below }: Props = $props();
</script>

<div class="shell">
  <div class="col" style:max-width="{width}px">
    <div class="logo"><Logo size={28} /></div>
    <section class="card">{@render children()}</section>
    {#if below}<p class="below">{@render below()}</p>{/if}
  </div>
</div>

<style>
  .shell {
    display: grid;
    place-items: start center;
    min-height: 100vh;
    padding: 12vh var(--sp-4) var(--sp-12);
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
    width: 100%;
  }
  .logo {
    display: flex;
    justify-content: center;
  }
  .card {
    padding: var(--sp-6);
    background: var(--bg-app);
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-md);
  }
  .below {
    color: var(--text-muted);
    font-size: var(--fs-sm);
    text-align: center;
  }
  @media (max-width: 480px) {
    .shell {
      padding-top: var(--sp-8);
    }
    .card {
      padding: var(--sp-5);
    }
  }
</style>
