<!-- 设置分区分发；站点与分区的权限已在布局里检查，这里只选组件。 -->
<script lang="ts">
  import { page } from "$app/state";
  import { sectionReadOnly } from "#lib/admin/sections.ts";
  import { findSite } from "#lib/session.svelte.ts";
  import GeneralSection from "../GeneralSection.svelte";
  import HostnamesSection from "../HostnamesSection.svelte";
  import CollectionSection from "../CollectionSection.svelte";
  import PrivacySection from "../PrivacySection.svelte";
  import ShieldSection from "../ShieldSection.svelte";
  import GoalsSection from "../GoalsSection.svelte";
  import SharingSection from "../SharingSection.svelte";
  import ImportsSection from "../ImportsSection.svelte";
  import DangerSection from "../DangerSection.svelte";

  const site = $derived(findSite(Number(page.params.id)));
  const section = $derived(page.params.section);

  $effect(() => {
    document.title = site ? `${site.name} – Rivu` : "Rivu";
  });
</script>

{#if site}
  {@const readOnly = section === "shield" && sectionReadOnly("shield", site)}
  {#if section === "general"}
    <GeneralSection />
  {:else if section === "hostnames"}
    <HostnamesSection />
  {:else if section === "collection"}
    <CollectionSection />
  {:else if section === "privacy"}
    <PrivacySection />
  {:else if section === "shield"}
    <ShieldSection {site} {readOnly} />
  {:else if section === "goals"}
    <GoalsSection {site} readOnly={sectionReadOnly("goals", site)} />
  {:else if section === "sharing"}
    <SharingSection {site} />
  {:else if section === "imports"}
    <ImportsSection {site} />
  {:else if section === "danger"}
    <DangerSection {site} />
  {/if}
{/if}
