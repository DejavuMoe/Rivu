<!--
  站点页的页头：站点头像与名称、在线人数、页面动作，下面是站点内的标签栏（看板 / 接入诊断 / 安装 / 设置）。
  站点内的页面切换放在这里，不放在侧栏：侧栏只负责「去哪个站点」。
  shared：共享链接与公开看板，只有标题，没有标签栏和链接。
-->
<script lang="ts">
  import type { Snippet } from "svelte";
  import PageHeader from "./PageHeader.svelte";
  import Avatar from "../ui/Avatar.svelte";
  import LiveDot from "../ui/LiveDot.svelte";
  import { t } from "../i18n";
  import { canEdit } from "../state.svelte";
  import { href, route } from "../router.svelte";
  import type { Site } from "../data/fixtures";
  import "./shell-strings";

  interface Props {
    site: Site;
    shared?: boolean;
    actions?: Snippet;
  }
  let { site, shared = false, actions }: Props = $props();

  const tab = $derived(route.current.seg[2] ?? "dashboard");
  const tabs = $derived(
    shared
      ? undefined
      : [
          { key: "dashboard", label: t("nav.dashboard"), path: "" },
          { key: "diagnostics", label: t("nav.diagnostics"), path: "/diagnostics" },
          { key: "install", label: t("nav.install"), path: "/install" },
          { key: "settings", label: t("nav.tabSettings"), path: "/settings", edit: true },
        ]
          .filter((x) => !x.edit || canEdit())
          .map((x) => ({ label: x.label, href: href(`/s/${site.id}${x.path}`), current: tab === x.key })),
  );
</script>

<PageHeader title={site.name} {tabs} tabsKey="site-{site.id}" {actions} compactTitle={!shared}>
  {#snippet leading()}<Avatar name={site.name} shape="square" size={24} />{/snippet}
  {#snippet meta()}
    <svelte:element this={shared ? "span" : "a"} class="live" href={shared ? undefined : href(`/s/${site.id}?range=realtime`)}>
      <LiveDot off={site.live === 0} />{t("dash.live", { n: site.live })}
    </svelte:element>
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
