<!--
  站点设置外壳：站点页头（标签栏选中「设置」）下面，左侧分区导航 + 右侧内容（最宽 720px）；窄屏分区导航变成横向 Tabs。
  分区导航的选中底色在分区之间滑动。
  需要站点 settings 的分区（常规、主机名、数据采集、隐私）先加载：评审面板的「加载」显示骨架，「出错 / 排队 / 超时」显示重试；
  其余分区各自加载自己的列表（见各分区）。
  分区可见性按角色（见 lib/data/sections.ts 的 visibleSections）；不存在或无权的分区是「找不到页面」。
-->
<script lang="ts">
  import { Bell, CircleAlert, Globe, Radio, Settings2, Share2, ShieldBan, Target, TriangleAlert, EyeOff, Upload } from "@lucide/svelte";
  import SiteHeader from "../../lib/app/SiteHeader.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import { trackIndicator } from "../../lib/ui/indicator";
  import Tabs from "../../lib/ui/Tabs.svelte";
  import NotFound from "../NotFound.svelte";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import { href, navigate } from "../../lib/router.svelte";
  import type { Site } from "../../lib/data/fixtures";
  import type { IconType } from "../../lib/ui/types";
  import { isSectionKey, sectionReadOnly, visibleSections, type SectionKey } from "../../lib/data/sections";
  import GeneralSection from "./GeneralSection.svelte";
  import HostnamesSection from "./HostnamesSection.svelte";
  import CollectionSection from "./CollectionSection.svelte";
  import PrivacySection from "./PrivacySection.svelte";
  import ShieldSection from "./ShieldSection.svelte";
  import GoalsSection from "./GoalsSection.svelte";
  import SharingSection from "./SharingSection.svelte";
  import AlertsSection from "./AlertsSection.svelte";
  import ImportsSection from "./ImportsSection.svelte";
  import DangerSection from "./DangerSection.svelte";
  import "./strings";

  let { site, section = "general" }: { site: Site; section?: string } = $props();

  const icons: Record<SectionKey, IconType> = {
    general: Settings2,
    hostnames: Globe,
    collection: Radio,
    privacy: EyeOff,
    shield: ShieldBan,
    goals: Target,
    sharing: Share2,
    alerts: Bell,
    imports: Upload,
    danger: TriangleAlert,
  };
  /** 这些分区要用站点的 settings；屏蔽规则、目标、共享链接与导入各自加载自己的列表 */
  const NEEDS_SETTINGS = new Set<SectionKey>(["general", "hostnames", "collection", "privacy", "alerts"]);

  const visible = $derived(visibleSections(site));
  const current = $derived(isSectionKey(section) && visible.includes(section) ? section : undefined);

  // 没有「常规」权限的角色（editor）从 /settings 进来时落到第一个可见分区
  $effect(() => {
    if (!current && section === "general" && visible.length) navigate(`/s/${site.id}/settings/${visible[0]}`, { replace: true });
  });

  const go = (key: string) => navigate(`/s/${site.id}/settings/${key}`);

  const loading = $derived(app.review.dataState === "loading");
  const failed = $derived(["error", "busy", "timeout"].includes(app.review.dataState));
  const waiting = $derived(!!current && NEEDS_SETTINGS.has(current) && (loading || failed));

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
        {#each visible as key (key)}
          {@const Icon = icons[key]}
          {#if key === "danger"}<span class="rule" aria-hidden="true"></span>{/if}
          <a class="item" class:danger={key === "danger"} href={href(`/s/${site.id}/settings/${key}`)} aria-current={key === current ? "page" : undefined}>
            <Icon size={15} strokeWidth={1.75} />
            <span>{t(`set.nav.${key}`)}</span>
          </a>
        {/each}
      </nav>

      <div class="tabbar">
        <Tabs label={t("nav.settings")} value={current} onchange={go} options={visible.map((k) => ({ value: k, label: t(`set.nav.${k}`) }))} />
      </div>

      <div class="content">
        {#if waiting && failed}
          <Card>
            <EmptyState icon={CircleAlert} tone="bad" title={t("set.loadFailed")}>
              {#snippet actions()}<Button onclick={() => toast(t("common.retry"))}>{t("common.retry")}</Button>{/snippet}
            </EmptyState>
          </Card>
        {:else if waiting}
          <Card>
            <div class="sk" aria-busy="true">
              <Skeleton width="40%" height={16} />
              <Skeleton height={32} radius={6} />
              <Skeleton height={32} radius={6} />
            </div>
          </Card>
        {:else if current === "general"}
          <GeneralSection {site} />
        {:else if current === "hostnames"}
          <HostnamesSection {site} />
        {:else if current === "collection"}
          <CollectionSection {site} />
        {:else if current === "privacy"}
          <PrivacySection {site} />
        {:else if current === "shield"}
          <ShieldSection {site} readOnly={sectionReadOnly("shield", site)} />
        {:else if current === "goals"}
          <GoalsSection {site} readOnly={sectionReadOnly("goals", site)} />
        {:else if current === "sharing"}
          <SharingSection {site} />
        {:else if current === "alerts"}
          <AlertsSection {site} />
        {:else if current === "imports"}
          <ImportsSection {site} />
        {:else if current === "danger"}
          <DangerSection {site} />
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
  .sk {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
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