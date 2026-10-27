<!--
  站点页的页头：站点头像与名称、在线人数（看板才有）、页面动作，下面是站点内的标签栏（看板 / 接入诊断 / 安装 / 设置）。
  站点内的页面切换放在这里，不放在侧栏：侧栏只负责「去哪个站点」。
  标签可见性与服务端一致：接入诊断 editor 起；设置指向第一个可见分区，一个都看不到时没有这个标签。
  shared：共享链接与公开看板，只有标题，没有标签栏和链接。
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { page } from "$app/state";
  import PageHeader from "./PageHeader.svelte";
  import Avatar from "../ui/Avatar.svelte";
  import LiveDot from "../ui/LiveDot.svelte";
  import { t } from "../i18n";
  import { visibleSections } from "../admin/sections";
  import { canEdit, findSite } from "../session.svelte";
  import "./shell-strings";

  interface Props {
    siteId?: number;
    name: string;
    /** 在线人数；null 时不显示 */
    live?: number | null;
    shared?: boolean;
    actions?: Snippet;
  }
  let { siteId, name, live = null, shared = false, actions }: Props = $props();

  const site = $derived(siteId === undefined ? undefined : findSite(siteId));
  const tab = $derived.by(() => {
    const rest = page.url.pathname.slice(`/s/${siteId}`.length);
    if (rest.startsWith("/diagnostics")) return "diagnostics";
    if (rest.startsWith("/install")) return "install";
    if (rest.startsWith("/settings")) return "settings";
    return "dashboard";
  });
  const tabs = $derived.by(() => {
    if (shared || !site) return undefined;
    const first = visibleSections(site)[0];
    return [
      { key: "dashboard", label: t("nav.dashboard"), href: `/s/${site.id}` },
      canEdit(site) ? { key: "diagnostics", label: t("nav.diagnostics"), href: `/s/${site.id}/diagnostics` } : null,
      { key: "install", label: t("nav.install"), href: `/s/${site.id}/install` },
      first ? { key: "settings", label: t("nav.tabSettings"), href: `/s/${site.id}/settings/${first}` } : null,
    ]
      .filter((x) => x !== null)
      .map((x) => ({ label: x.label, href: x.href, current: tab === x.key }));
  });
</script>

<PageHeader title={name} {tabs} tabsKey="site-{siteId}" {actions} compactTitle={!shared}>
  {#snippet leading()}<Avatar {name} shape="square" size={24} />{/snippet}
  {#snippet meta()}
    {#if live !== null}
      <svelte:element this={shared ? "span" : "a"} class="live" href={shared ? undefined : `/s/${siteId}?range=realtime`}>
        <LiveDot off={live === 0} />{t("dash.live", { n: live })}
      </svelte:element>
    {/if}
  {/snippet}
</PageHeader>

<style>
  .live {
    display: inline-flex;
    flex: none;
    align-items: center;
    gap: 6px;
    color: var(--text-muted);
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  a.live:hover {
    color: var(--text);
  }
</style>
