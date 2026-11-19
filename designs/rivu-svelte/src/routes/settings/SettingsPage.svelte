<!--
  站点设置：站点页头（标签栏选中「设置」）下面，左侧分区导航 + 右侧每组字段一张卡片（最宽 720px）；窄屏分区导航变成横向 Tabs。
  分区导航的选中底色在分区之间滑动。
  分区可见性按角色：常规 / 主机名 / 数据采集 / 隐私 / 导入 / 危险操作 需要 canManage；
  屏蔽规则编辑者可看但只读；目标与共享编辑者可写。
-->
<script lang="ts">
  import { Globe, Radio, Settings2, Share2, ShieldBan, Target, TriangleAlert, EyeOff, Upload } from "@lucide/svelte";
  import SiteHeader from "../../lib/app/SiteHeader.svelte";
  import { trackIndicator } from "../../lib/ui/indicator";
  import Tabs from "../../lib/ui/Tabs.svelte";
  import NotFound from "../NotFound.svelte";
  import { t } from "../../lib/i18n";
  import { canEdit, canManage } from "../../lib/state.svelte";
  import { href, navigate } from "../../lib/router.svelte";
  import type { Site } from "../../lib/data/fixtures";
  import type { IconType } from "../../lib/ui/types";
  import GeneralSection from "./GeneralSection.svelte";
  import HostnamesSection from "./HostnamesSection.svelte";
  import CollectionSection from "./CollectionSection.svelte";
  import PrivacySection from "./PrivacySection.svelte";
  import ShieldSection from "./ShieldSection.svelte";
  import GoalsSection from "./GoalsSection.svelte";
  import SharingSection from "./SharingSection.svelte";
  import ImportsSection from "./ImportsSection.svelte";
  import DangerSection from "./DangerSection.svelte";
  import "./strings";

  let { site, section = "general" }: { site: Site; section?: string } = $props();

  interface Def {
    key: string;
    icon: IconType;
    /** 能进入这个分区 */
    visible: () => boolean;
    /** 进入后是只读 */
    readOnly: () => boolean;
  }
  const defs: Def[] = [
    { key: "general", icon: Settings2, visible: canManage, readOnly: () => !canManage() },
    { key: "hostnames", icon: Globe, visible: canManage, readOnly: () => !canManage() },
    { key: "collection", icon: Radio, visible: canManage, readOnly: () => !canManage() },
    { key: "privacy", icon: EyeOff, visible: canManage, readOnly: () => !canManage() },
    { key: "shield", icon: ShieldBan, visible: canEdit, readOnly: () => !canManage() },
    { key: "goals", icon: Target, visible: canEdit, readOnly: () => !canEdit() },
    { key: "sharing", icon: Share2, visible: canEdit, readOnly: () => !canEdit() },
    { key: "imports", icon: Upload, visible: canManage, readOnly: () => !canManage() },
    { key: "danger", icon: TriangleAlert, visible: canManage, readOnly: () => !canManage() },
  ];

  const visible = $derived(defs.filter((d) => d.visible()));
  const current = $derived(visible.find((d) => d.key === section));
  const go = (key: string) => navigate(`/s/${site.id}/settings/${key}`);

  let side: HTMLElement | undefined = $state();
  let ind: HTMLSpanElement | undefined = $state();
  $effect(() => {
    if (side && ind) return trackIndicator(side, ind, '[aria-current="page"]', "fill", `settings-${site.id}`);
  });
</script>

{#if !current}
  <NotFound />
{:else}
  <div class="page">
    <SiteHeader {site} />

    <div class="layout">
      <nav class="side" aria-label={t("nav.settings")} bind:this={side}>
        <span class="ind" bind:this={ind} aria-hidden="true"></span>
        {#each visible as d (d.key)}
          {#if d.key === "danger"}<span class="rule" aria-hidden="true"></span>{/if}
          <a class="item" class:danger={d.key === "danger"} href={href(`/s/${site.id}/settings/${d.key}`)} aria-current={d.key === section ? "page" : undefined}>
            <d.icon size={15} strokeWidth={1.75} />
            <span>{t(`set.nav.${d.key}`)}</span>
          </a>
        {/each}
      </nav>

      <div class="tabbar">
        <Tabs
          label={t("nav.settings")}
          value={section}
          onchange={go}
          options={visible.map((d) => ({ value: d.key, label: t(`set.nav.${d.key}`) }))}
        />
      </div>

      <div class="content">
        {#if current.key === "general"}
          <GeneralSection {site} readOnly={current.readOnly()} />
        {:else if current.key === "hostnames"}
          <HostnamesSection {site} readOnly={current.readOnly()} />
        {:else if current.key === "collection"}
          <CollectionSection {site} readOnly={current.readOnly()} />
        {:else if current.key === "privacy"}
          <PrivacySection {site} readOnly={current.readOnly()} />
        {:else if current.key === "shield"}
          <ShieldSection {site} readOnly={current.readOnly()} />
        {:else if current.key === "goals"}
          <GoalsSection {site} readOnly={current.readOnly()} />
        {:else if current.key === "sharing"}
          <SharingSection {site} readOnly={current.readOnly()} />
        {:else if current.key === "imports"}
          <ImportsSection {site} readOnly={current.readOnly()} />
        {:else if current.key === "danger"}
          <DangerSection {site} readOnly={current.readOnly()} />
        {/if}
      </div>
    </div>
  </div>
{/if}

<style>
  .page {
    display: flex;
    flex-direction: column;
    gap: var(--sp-6);
    width: 100%;
    max-width: var(--content-max);
    margin: 0 auto;
    padding: var(--sp-6) var(--sp-8) var(--sp-12);
  }
  .layout {
    display: grid;
    grid-template-columns: 184px minmax(0, 720px);
    gap: var(--sp-8);
    align-items: start;
  }
  .side {
    position: sticky;
    isolation: isolate;
    view-transition-name: settings-nav;
    /* 图标与页头左缘对齐（分区项有 10px 内边距） */
    margin-left: -10px;
    top: var(--sp-6);
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .rule {
    height: var(--hairline);
    margin: var(--sp-2) 0;
    background: var(--border-subtle);
  }
  /* 换分区时分区导航不参与整页淡入淡出，选中底色的滑动看得见（同 SiteHeader） */
  :global(::view-transition-old(settings-nav)) {
    display: none;
  }
  :global(::view-transition-new(settings-nav)) {
    animation: none;
  }
  .ind {
    position: absolute;
    top: 0;
    left: 0;
    z-index: -1;
    border-radius: var(--r-sm);
    background: var(--bg-hover);
    transition:
      transform var(--dur) var(--ease-move),
      width var(--dur) var(--ease-move);
  }
  .item {
    position: relative;
    display: flex;
    align-items: center;
    gap: 10px;
    height: var(--control-md);
    padding: 0 10px;
    border-radius: var(--r-sm);
    color: var(--text-muted);
  }
  .item :global(svg) {
    flex: none;
  }
  .item:hover:not([aria-current]) {
    background: var(--bg-element);
    color: var(--text);
  }
  .item[aria-current="page"] {
    color: var(--text);
    font-weight: var(--fw-medium);
  }
  .item.danger {
    color: var(--bad-text);
  }
  .tabbar {
    display: none;
    border-bottom: var(--hairline) solid var(--border-subtle);
  }
  .content {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    min-width: 0;
  }
  @media (max-width: 720px) {
    .page {
      padding: var(--sp-4) var(--sp-4) var(--sp-10);
      gap: var(--sp-4);
    }
    .layout {
      grid-template-columns: minmax(0, 1fr);
      gap: var(--sp-4);
    }
    .side {
      display: none;
    }
    .tabbar {
      display: block;
    }
  }
</style>