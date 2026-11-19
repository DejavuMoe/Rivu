<!-- 404：不存在的页面，以及无权访问的站点（不暴露站点是否存在）。 -->
<script lang="ts">
  import { SearchX } from "@lucide/svelte";
  import Card from "../lib/ui/Card.svelte";
  import EmptyState from "../lib/ui/EmptyState.svelte";
  import Button from "../lib/ui/Button.svelte";
  import { t } from "../lib/i18n";
  import { href } from "../lib/router.svelte";
</script>

<div class="wrap">
  <Card>
    <EmptyState icon={SearchX} title={t("notFound.title")}>
      {#snippet actions()}<Button href={href("/sites")}>{t("notFound.action")}</Button>{/snippet}
    </EmptyState>
  </Card>
</div>

<style>
  .wrap {
    max-width: 560px;
    margin: var(--sp-12) auto;
    padding: 0 var(--sp-4);
  }
</style>
