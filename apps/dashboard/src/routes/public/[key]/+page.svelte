<!-- 公开看板（站点设置里开了「公开看板」）：匿名只读。站点不存在或没有公开一律 404。 -->
<script lang="ts">
  import { CircleAlert, Link2 } from "@lucide/svelte";
  import { page } from "$app/state";
  import ShareShell from "#lib/app/ShareShell.svelte";
  import DashboardView from "#lib/dashboard/DashboardView.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Card from "#lib/ui/Card.svelte";
  import EmptyState from "#lib/ui/EmptyState.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import { api, ApiError, type PublicSite } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import "#lib/app/share-strings.ts";

  const key = $derived(page.params.key ?? "");

  let phase = $state<"loading" | "ok" | "missing" | "failed">("loading");
  let site = $state<PublicSite | null>(null);
  let reload = $state(0);

  $effect(() => {
    void reload;
    const k = key;
    phase = "loading";
    site = null;
    let stale = false;
    api
      .publicSite(k)
      .then((r) => {
        if (stale) return;
        site = r.site;
        phase = "ok";
      })
      .catch((e) => {
        if (stale) return;
        phase = e instanceof ApiError && e.status === 404 ? "missing" : "failed";
      });
    return () => (stale = true);
  });
</script>

<ShareShell kind="public">
  {#if phase === "ok" && site}
    {#key site.id}<DashboardView source={{ site: key }} name={site.name} mode="shared" />{/key}
  {:else if phase === "missing"}
    <div class="center">
      <Card>
        <EmptyState icon={Link2} title={t("share.notFound.title")} />
      </Card>
    </div>
  {:else if phase === "failed"}
    <div class="center">
      <Card>
        <EmptyState icon={CircleAlert} tone="bad" title={t("app.loadFailed")}>
          {#snippet actions()}<Button onclick={() => reload++}>{t("common.retry")}</Button>{/snippet}
        </EmptyState>
      </Card>
    </div>
  {:else}
    <div class="center" aria-busy="true">
      <Skeleton width="min(400px, 100%)" height={120} radius={8} />
    </div>
  {/if}
</ShareShell>

<style>
  .center {
    display: grid;
    place-items: start center;
    padding: 96px var(--sp-4) var(--sp-8);
  }
  .center > :global(.card) {
    width: min(400px, 100%);
  }
  @media (max-width: 720px) {
    .center {
      padding-top: var(--sp-10);
    }
  }
</style>
