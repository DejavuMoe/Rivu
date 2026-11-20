<!--
  「全部站点」的页头：标题「站点」与添加站点，组织里能读的站点有两个及以上时下面有标签栏（列表 / 合并看板）。
  列表页和合并看板共用它，切换标签时页头不动。
-->
<script lang="ts">
  import { Plus } from "@lucide/svelte";
  import PageHeader from "../../lib/app/PageHeader.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import { t } from "../../lib/i18n";
  import { canAddSite } from "../../lib/state.svelte";
  import { href, route } from "../../lib/router.svelte";
  import "./strings";

  let { orgId, tabbed }: { orgId: string; tabbed: boolean } = $props();

  const combined = $derived(route.current.seg[1] === "combined");
  const tabs = $derived(
    tabbed
      ? [
          { label: t("sl.tab.list"), href: href("/sites"), current: !combined },
          { label: t("cv.tab"), href: href("/sites/combined"), current: combined },
        ]
      : undefined,
  );
</script>

<PageHeader title={t("sites.title")} {tabs} tabsKey="sites-{orgId}">
  {#snippet actions()}
    {#if canAddSite(orgId)}
      <Button variant="primary" icon={Plus} href={href("/sites/new")}>{t("nav.addSite")}</Button>
    {/if}
  {/snippet}
</PageHeader>
