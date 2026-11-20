<!--
  窄屏（860px 以下）顶部栏 + 左侧抽屉。大屏不显示。
  抽屉是原生 <dialog>，里面复用 Sidebar（带关闭按钮、40px 行高）。打开时从左侧滑入、背景淡入，关闭时反向；
  Sidebar 始终挂载，关闭动画期间内容不会消失。应用外壳在窄屏用 .app :global(.sidebar){display:none} 隐藏侧栏，
  这里用更具体的选择器（.drawer .panel .sidebar）覆盖回来。路由变化时自动关闭抽屉。
-->
<script lang="ts">
  import { Menu, Search } from "@lucide/svelte";
  import Sidebar from "./Sidebar.svelte";
  import Button from "../ui/Button.svelte";
  import Logo from "../ui/Logo.svelte";
  import { ORGS } from "../data/fixtures";
  import { SITES } from "../data/sites.svelte";
  import { t } from "../i18n";
  import { route } from "../router.svelte";
  import "./shell-strings";

  let { orgId = $bindable(), onsearch }: { orgId: string; onsearch?: () => void } = $props();

  let open = $state(false);
  let dlg: HTMLDialogElement;

  const seg = $derived(route.current.seg);
  const site = $derived(seg[0] === "s" ? SITES.find((s) => s.id === Number(seg[1])) : undefined);
  const title = $derived(site?.name ?? ORGS.find((o) => o.id === orgId)?.name ?? "Rivu");

  $effect(() => {
    if (open && !dlg.open) dlg.showModal();
    if (!open && dlg.open) dlg.close();
  });

  // 路由变化关闭抽屉
  $effect(() => {
    void route.current.path;
    open = false;
  });

  // 变宽回到桌面布局时关闭
  $effect(() => {
    const mq = matchMedia("(min-width: 861px)");
    const on = () => {
      if (mq.matches) open = false;
    };
    mq.addEventListener("change", on);
    return () => mq.removeEventListener("change", on);
  });

  function openPalette() {
    open = false;
    onsearch?.();
  }
</script>

<header class="bar">
  <Button variant="ghost" icon={Menu} aria-label={t("mn.menu")} aria-haspopup="dialog" aria-expanded={open} onclick={() => (open = true)} />
  <Logo wordmark={false} size={24} />
  <span class="title">{title}</span>
  <Button variant="ghost" icon={Search} aria-label={t("mn.search")} onclick={openPalette} />
</header>

<dialog
  bind:this={dlg}
  class="drawer"
  aria-label={t("mn.drawer")}
  onclose={() => (open = false)}
  onclick={(e) => {
    if (e.target === dlg) dlg.close();
  }}
>
  <div class="panel">
    <Sidebar bind:orgId onsearch={openPalette} onclose={() => (open = false)} />
  </div>
</dialog>

<style>
  .bar {
    display: none;
  }
  @media (max-width: 860px) {
    .bar {
      position: sticky;
      top: 0;
      z-index: 40;
      display: flex;
      align-items: center;
      gap: var(--sp-2);
      height: 52px;
      padding: 0 var(--sp-2);
      background: var(--bg-canvas);
      border-bottom: var(--hairline) solid var(--border-subtle);
    }
  }
  .title {
    flex: 1;
    min-width: 0;
    margin-left: var(--sp-1);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: var(--fw-semibold);
  }

  .drawer {
    position: fixed;
    inset: 0 auto 0 0;
    width: min(300px, 85vw);
    height: 100dvh;
    max-width: none;
    max-height: none;
    margin: 0;
    padding: 0;
    border: 0;
    border-radius: 0 var(--r-lg) var(--r-lg) 0;
    background: var(--bg-canvas);
    color: var(--text);
    box-shadow: var(--shadow-pop);
    overflow: hidden;
    transform: translateX(-100%);
    transition:
      transform var(--dur) var(--ease-move),
      overlay var(--dur) allow-discrete,
      display var(--dur) allow-discrete;
  }
  .drawer[open] {
    transform: none;
  }
  .drawer::backdrop {
    background: var(--overlay);
    opacity: 0;
    transition:
      opacity var(--dur) var(--ease),
      overlay var(--dur) allow-discrete,
      display var(--dur) allow-discrete;
  }
  .drawer[open]::backdrop {
    opacity: 1;
  }
  @starting-style {
    .drawer[open] {
      transform: translateX(-100%);
    }
    .drawer[open]::backdrop {
      opacity: 0;
    }
  }
  .panel {
    position: relative;
    height: 100%;
  }
  /* 覆盖 App 里对 .sidebar 的隐藏：选择器层级更高 */
  .drawer .panel :global(.sidebar) {
    display: flex;
    position: static;
    width: 100%;
    height: 100%;
    border-right: 0;
  }
</style>

