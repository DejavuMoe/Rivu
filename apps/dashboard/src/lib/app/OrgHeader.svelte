<!-- 组织页的页头：当前组织的名称与页面动作，下面是组织内的标签栏（成员 / API 密钥）。 -->
<script lang="ts">
  import type { Snippet } from "svelte";
  import { page } from "$app/state";
  import PageHeader from "./PageHeader.svelte";
  import { t } from "../i18n";
  import { currentOrg } from "../session.svelte";

  let { actions }: { actions?: Snippet } = $props();

  const org = $derived(currentOrg());
  const tabs = $derived(
    [
      { href: "/org/members", label: t("nav.members") },
      { href: "/org/api-keys", label: t("nav.apiKeys") },
    ].map((x) => ({ ...x, current: page.url.pathname.startsWith(x.href) })),
  );
</script>

<PageHeader title={org?.name ?? t("nav.org")} {tabs} tabsKey="org" {actions} compactTitle />
