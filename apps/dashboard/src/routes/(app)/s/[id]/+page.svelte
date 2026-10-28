<!-- 登录用户的站点看板。站点不在可见列表里（不存在或无权访问）时显示 404。 -->
<script lang="ts">
  import { page } from "$app/state";
  import DashboardView from "#lib/dashboard/DashboardView.svelte";
  import NotFound from "#lib/app/NotFound.svelte";
  import { canExport, canShareLinks } from "#lib/admin/sections.ts";
  import { findSite } from "#lib/session.svelte.ts";

  const site = $derived(findSite(Number(page.params.id)));

  $effect(() => {
    document.title = site ? `${site.name} – Rivu` : "Rivu";
  });
</script>

{#if site}
  {#key site.id}
    <DashboardView source={{ site: site.id }} name={site.name} siteId={site.id} canShare={canShareLinks(site)} canExport={canExport(site)} />
  {/key}
{:else}
  <NotFound />
{/if}
