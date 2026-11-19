<!-- 组织页的页头：组织名称与页面动作，下面是组织内的标签栏（成员 / API 密钥）。 -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import PageHeader from "./PageHeader.svelte";
  import { ORGS } from "../data/fixtures";
  import { t } from "../i18n";
  import { href, route } from "../router.svelte";

  let { orgId, actions }: { orgId: string; actions?: Snippet } = $props();

  const org = $derived(ORGS.find((o) => o.id === orgId));
  const tabs = $derived(
    [
      { key: "members", label: t("nav.members") },
      { key: "api-keys", label: t("nav.apiKeys") },
    ].map((x) => ({ label: x.label, href: href(`/org/${x.key}`), current: route.current.seg[1] === x.key })),
  );
</script>

<PageHeader title={org?.name ?? t("nav.org")} {tabs} tabsKey="org" {actions} compactTitle />
