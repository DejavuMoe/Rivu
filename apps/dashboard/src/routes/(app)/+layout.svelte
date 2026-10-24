<!--
  登录后的应用外壳：侧栏（窄屏换成顶部栏 + 抽屉）、命令面板。
  先加载会话；没有登录跳到登录页并带上 next，加载失败给重试。
-->
<script lang="ts">
  import { CircleAlert } from "@lucide/svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import Sidebar from "#lib/app/Sidebar.svelte";
  import MobileNav from "#lib/app/MobileNav.svelte";
  import CommandPalette from "#lib/app/CommandPalette.svelte";
  import EmptyState from "#lib/ui/EmptyState.svelte";
  import Button from "#lib/ui/Button.svelte";
  import { t } from "#lib/i18n.ts";
  import { isUnauthorized, loadSession, session } from "#lib/session.svelte.ts";

  let { children } = $props();

  let status = $state<"loading" | "ready" | "failed">(session.me ? "ready" : "loading");
  let paletteOpen = $state(false);

  async function load() {
    status = "loading";
    try {
      await loadSession();
      status = "ready";
    } catch (e) {
      if (isUnauthorized(e)) {
        const next = page.url.pathname + page.url.search;
        await goto(`/login?next=${encodeURIComponent(next)}`, { replace: true });
      } else status = "failed";
    }
  }

  $effect(() => {
    if (!session.me) void load();
  });

  function onKeydown(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k" && status === "ready") {
      e.preventDefault();
      paletteOpen = !paletteOpen;
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if status === "ready" && session.me}
  <div class="app">
    <Sidebar onsearch={() => (paletteOpen = true)} />
    <div class="column">
      <MobileNav onsearch={() => (paletteOpen = true)} />
      <main class="content">{@render children()}</main>
    </div>
  </div>
  <CommandPalette bind:open={paletteOpen} />
{:else if status === "failed"}
  <div class="failed">
    <EmptyState icon={CircleAlert} tone="bad" title={t("app.loadFailed")}>
      {#snippet actions()}<Button onclick={load}>{t("common.retry")}</Button>{/snippet}
    </EmptyState>
  </div>
{/if}

<style>
  .app {
    display: grid;
    grid-template-columns: var(--sidebar-w) minmax(0, 1fr);
    min-height: 100vh;
  }
  .column,
  .content {
    min-width: 0;
  }
  .failed {
    max-width: 560px;
    margin: var(--sp-12) auto;
    padding: 0 var(--sp-4);
  }
  @media (max-width: 860px) {
    .app {
      grid-template-columns: 1fr;
    }
    .app :global(.sidebar) {
      display: none;
    }
  }
</style>
