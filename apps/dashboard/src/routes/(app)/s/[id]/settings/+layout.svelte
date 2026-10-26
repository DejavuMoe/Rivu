<!--
  站点设置外壳：左侧分区导航 + 右侧内容，最大宽 880px；窄屏导航变成顶部 Tabs。
  站点的 settings 每个站点加载一次（需要它的分区才加载），分区之间切换不重新请求。
  分区可见性按角色（见 lib/admin/sections.ts）；不存在或无权的分区是 404。
-->
<script lang="ts">
  import { untrack, type Snippet } from "svelte";
  import { CircleAlert, EyeOff, Globe, Radio, Settings2, Share2, ShieldBan, Target, TriangleAlert } from "@lucide/svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import NotFound from "#lib/app/NotFound.svelte";
  import PageHeader from "#lib/app/PageHeader.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Card from "#lib/ui/Card.svelte";
  import EmptyState from "#lib/ui/EmptyState.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import Tabs from "#lib/ui/Tabs.svelte";
  import type { IconType } from "#lib/ui/types.ts";
  import { isSectionKey, visibleSections, type SectionKey } from "#lib/admin/sections.ts";
  import { t } from "#lib/i18n.ts";
  import { findSite } from "#lib/session.svelte.ts";
  import { provideSettings, SettingsStore } from "./settings.svelte.ts";
  import "./strings";

  let { children }: { children: Snippet } = $props();

  const icons: Record<SectionKey, IconType> = {
    general: Settings2,
    hostnames: Globe,
    collection: Radio,
    privacy: EyeOff,
    shield: ShieldBan,
    goals: Target,
    sharing: Share2,
    danger: TriangleAlert,
  };
  /** 这些分区要用站点的 settings；屏蔽规则、目标与共享链接各自加载自己的列表 */
  const NEEDS_SETTINGS = new Set<SectionKey>(["general", "hostnames", "collection", "privacy"]);

  const site = $derived(findSite(Number(page.params.id)));
  const siteId = $derived(site?.id);
  const visible = $derived(site ? visibleSections(site) : []);
  const section = $derived(page.params.section);
  const current = $derived(isSectionKey(section) && visible.includes(section) ? section : undefined);

  const store = $derived(siteId === undefined ? null : new SettingsStore(siteId));
  provideSettings(() => store!);

  $effect(() => {
    if (store && current && NEEDS_SETTINGS.has(current)) {
      const s = store;
      untrack(() => void s.load());
    }
  });

  // 没有「常规」权限的角色（editor）从 /settings 进来时落到第一个可见分区
  $effect(() => {
    if (site && !current && section === "general" && visible.length) {
      void goto(`/s/${site.id}/settings/${visible[0]}`, { replaceState: true });
    }
  });

  const go = (key: string) => void goto(`/s/${siteId}/settings/${key}`);
  const waiting = $derived(!!current && NEEDS_SETTINGS.has(current) && store?.status !== "ready");
</script>

{#if !site || !current}
  <NotFound />
{:else}
  <div class="page">
    <PageHeader title={t("nav.settings")} />

    <div class="layout">
      <nav class="side" aria-label={t("nav.settings")}>
        {#each visible as key (key)}
          {@const Icon = icons[key]}
          {#if key === "danger"}<span class="rule" aria-hidden="true"></span>{/if}
          <a class="item" class:danger={key === "danger"} href="/s/{site.id}/settings/{key}" aria-current={key === current ? "page" : undefined}>
            <Icon size={15} strokeWidth={1.75} />
            <span>{t(`set.nav.${key}`)}</span>
          </a>
        {/each}
      </nav>

      <div class="tabbar">
        <Tabs label={t("nav.settings")} value={current} onchange={go} options={visible.map((k) => ({ value: k, label: t(`set.nav.${k}`) }))} />
      </div>

      <div class="content">
        {#if waiting && store?.status === "error"}
          <Card>
            <EmptyState icon={CircleAlert} tone="bad" title={t("set.loadFailed")}>
              {#snippet actions()}<Button onclick={() => store?.load()}>{t("common.retry")}</Button>{/snippet}
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
        {:else}
          {#key site.id}
            {@render children()}
          {/key}
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
    max-width: 880px;
    margin: 0 auto;
    padding: var(--sp-6) var(--sp-8) var(--sp-12);
  }
  .layout {
    display: grid;
    grid-template-columns: 184px minmax(0, 1fr);
    gap: var(--sp-8);
    align-items: start;
  }
  .side {
    position: sticky;
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
  .item {
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
  .item:hover {
    background: var(--bg-element);
    color: var(--text);
  }
  .item[aria-current="page"] {
    background: var(--bg-hover);
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
