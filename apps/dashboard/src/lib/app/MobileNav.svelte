<!--
  窄屏（860px 以下）顶部栏 + 左侧抽屉。大屏不显示。
  抽屉是原生 <dialog>，里面复用 Sidebar。应用外壳在窄屏用 .app :global(.sidebar){display:none} 隐藏侧栏，
  这里用更具体的选择器（.drawer .panel .sidebar）覆盖回来。路由变化时自动关闭抽屉。
-->
<script lang="ts">
  import { Menu, Search } from "@lucide/svelte";
  import Sidebar from "./Sidebar.svelte";
  import Button from "../ui/Button.svelte";
  import Logo from "../ui/Logo.svelte";
  import { page } from "$app/state";
  import { t } from "../i18n";
  import { currentOrg, findSite } from "../session.svelte";
  import "./shell-strings";

  let { onsearch }: { onsearch?: () => void } = $props();

  let open = $state(false);
  let dlg: HTMLDialogElement;

  const site = $derived(page.params.id ? findSite(Number(page.params.id)) : undefined);
  const title = $derived(site?.name ?? currentOrg()?.name ?? "Rivu");

  $effect(() => {
    if (open && !dlg.open) dlg.showModal();
    if (!open && dlg.open) dlg.close();
  });

  // 路由变化关闭抽屉
  $effect(() => {
    void page.url.pathname;
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
    {#if open}<Sidebar onsearch={openPalette} />{/if}
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
      background: var(--bg-app);
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
    width: var(--sidebar-w);
    max-width: 86vw;
    height: 100dvh;
    max-height: none;
    margin: 0;
    padding: 0;
    border: 0;
    background: var(--bg-app);
    color: var(--text);
    box-shadow: var(--shadow-pop);
    overflow: hidden;
  }
  .drawer::backdrop {
    background: var(--overlay);
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

