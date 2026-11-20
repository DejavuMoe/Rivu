<!--
  独立页（登录、初始化、邀请、确认链接）的外壳：一片夜空，地球从底边升起来，上面浮着标志和一张纸。
  这里还没有登录，不知道任何统计，所以地球上没有访客的灯光，只有按此刻太阳位置分出的昼夜，慢慢地转。
  地球按需加载，不耽误表单出现；加载失败或系统要求减少动效时，夜空照常，只是没有（或不转的）地球。
-->
<script lang="ts">
  import type { Component, Snippet } from "svelte";
  import { onMount } from "svelte";
  import Logo from "../../lib/ui/Logo.svelte";

  interface Props {
    width?: number;
    children: Snippet;
    below?: Snippet;
  }
  let { width = 400, children, below }: Props = $props();

  let Globe = $state<Component<any> | null>(null);
  onMount(() => {
    import("../../lib/charts/Globe.svelte").then(
      (m) => (Globe = m.default),
      () => {},
    );
    // 整页都是夜空，包括给滚动条留的那一条
    document.documentElement.classList.add("night");
    return () => document.documentElement.classList.remove("night");
  });
</script>

<div class="shell">
  <div class="earth" data-theme="dark" aria-hidden="true">
    {#if Globe}<Globe rows={[]} format={String} summary="" fit="horizon" spin={1.6} interactive={false} view={[72, -26]} />{/if}
  </div>
  <div class="col" style:max-width="{width}px">
    <div class="logo" data-theme="dark"><Logo size={32} /></div>
    <section class="card">{@render children()}</section>
    {#if below}<p class="below" data-theme="dark">{@render below()}</p>{/if}
  </div>
</div>

<style>
  .shell {
    position: relative;
    display: grid;
    place-items: start center;
    min-height: 100vh;
    padding: 11vh var(--sp-4) var(--sp-12);
    overflow: hidden;
    background: var(--night);
  }
  /* 地平线：只露出地球最上面的一段弧 */
  .earth {
    position: absolute;
    inset: 0;
    animation: fade-in var(--dur-draw) var(--ease) backwards;
  }
  .col {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: var(--sp-6);
    width: 100%;
  }
  .logo {
    display: flex;
    justify-content: center;
    color: var(--text);
  }
  .card {
    padding: var(--sp-8) var(--sp-6) var(--sp-6);
    background: var(--bg-app);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-pop);
  }
  .card :global(h1) {
    font-family: var(--font-display);
    font-size: var(--fs-xl);
    letter-spacing: var(--track-display);
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
      padding: var(--sp-6) var(--sp-5) var(--sp-5);
    }
  }
</style>
