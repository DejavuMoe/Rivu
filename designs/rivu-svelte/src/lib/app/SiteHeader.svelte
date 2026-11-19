<!--
  站点页的页头：站点头像与名称、在线人数（看板才有）、页面动作，下面是站点内的标签栏（看板 / 接入诊断 / 安装 / 设置）。
  站点内的页面切换放在这里，不放在侧栏：侧栏只负责「去哪个站点」。
  标签可见性与服务端一致：接入诊断 editor 起；设置指向第一个可见分区，一个都看不到时没有这个标签。
  shared：共享链接与公开看板，只有标题，没有标签栏和链接。
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import PageHeader from "./PageHeader.svelte";
  import Avatar from "../ui/Avatar.svelte";
  import LiveDot from "../ui/LiveDot.svelte";
  import { t } from "../i18n";
  import { canEdit } from "../state.svelte";
  import { visibleSections } from "../data/sections";
  import { href, route } from "../router.svelte";
  import type { Site } from "../data/fixtures";
  import "./shell-strings";

  interface Props {
    site: Site;
    /** 在线人数：不传时看板页（含共享、公开看板）取站点的模拟值，其他页不显示；传 null 时不显示（加载中、出错、无数据） */
    live?: number | null;
    shared?: boolean;
    actions?: Snippet;
  }
  let { site, live, shared = false, actions }: Props = $props();

  const tab = $derived(shared ? "dashboard" : (route.current.seg[2] ?? "dashboard"));
  const liveNow = $derived(live !== undefined ? live : tab === "dashboard" ? site.live : null);
  const tabs = $derived.by(() => {
    if (shared) return undefined;
    const first = visibleSections(site)[0];
    return [
      { key: "dashboard", label: t("nav.dashboard"), href: href(`/s/${site.id}`) },
      canEdit(site) ? { key: "diagnostics", label: t("nav.diagnostics"), href: href(`/s/${site.id}/diagnostics`) } : null,
      { key: "install", label: t("nav.install"), href: href(`/s/${site.id}/install`) },
      first ? { key: "settings", label: t("nav.tabSettings"), href: href(`/s/${site.id}/settings/${first}`) } : null,
    ]
      .filter((x) => x !== null)
      .map((x) => ({ label: x.label, href: x.href, current: tab === x.key }));
  });
</script>

<PageHeader title={site.name} {tabs} tabsKey="site-{site.id}" {actions} compactTitle={!shared}>
  {#snippet leading()}<Avatar name={site.name} shape="square" size={24} />{/snippet}
  {#snippet meta()}
    {#if liveNow !== null}
      <svelte:element this={shared ? "span" : "a"} class="live" href={shared ? undefined : href(`/s/${site.id}?range=realtime`)}>
        <LiveDot off={liveNow === 0} />{t("dash.live", { n: liveNow })}
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
