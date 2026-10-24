<!--
  共享链接看板（独立页，没有侧栏）：先问入口信息；要密码且未授权时显示密码门，通过后进看板。
  链接无效、过期、吊销服务端一律 404，界面上也不区分。
-->
<script lang="ts">
  import { CircleAlert, Link2 } from "@lucide/svelte";
  import { page } from "$app/state";
  import ShareShell from "#lib/app/ShareShell.svelte";
  import DashboardView from "#lib/dashboard/DashboardView.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Card from "#lib/ui/Card.svelte";
  import EmptyState from "#lib/ui/EmptyState.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import { api, ApiError, type PublicSite } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import "#lib/app/share-strings.ts";

  const slug = $derived(page.params.slug ?? "");

  let phase = $state<"loading" | "locked" | "ok" | "missing" | "failed">("loading");
  let site = $state<PublicSite | null>(null);
  let reload = $state(0);

  // 密码门
  let pw = $state("");
  let busy = $state(false);
  let wrong = $state(false);
  let failed = $state(false);
  let lockedFor = $state(0);
  const blocked = $derived(lockedFor > 0);

  $effect(() => {
    void reload;
    const s = slug;
    phase = "loading";
    site = null;
    let stale = false;
    api
      .shared(s)
      .then((r) => {
        if (stale) return;
        if (r.authorized && r.site) {
          site = r.site;
          phase = "ok";
        } else phase = "locked";
      })
      .catch((e) => {
        if (stale) return;
        phase = e instanceof ApiError && e.status === 404 ? "missing" : "failed";
      });
    return () => (stale = true);
  });

  $effect(() => {
    if (lockedFor <= 0) return;
    const id = setInterval(() => {
      lockedFor -= 1;
      if (lockedFor <= 0) clearInterval(id);
    }, 1000);
    return () => clearInterval(id);
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (blocked || busy || !pw) return;
    busy = true;
    wrong = failed = false;
    try {
      const r = await api.sharedAuth(slug, pw);
      site = r.site;
      phase = "ok";
    } catch (err) {
      pw = "";
      if (err instanceof ApiError && err.status === 401) wrong = true;
      else if (err instanceof ApiError && err.status === 429) lockedFor = err.retryAfter ?? 60;
      else if (err instanceof ApiError && err.status === 404) phase = "missing";
      else failed = true;
    } finally {
      busy = false;
    }
  }
</script>

<ShareShell kind="shared">
  {#if phase === "ok" && site}
    {#key site.id}<DashboardView source={{ site: site.site_key, share: slug }} name={site.name} mode="shared" />{/key}
  {:else if phase === "locked"}
    <div class="center">
      <Card>
        <form class="gate" onsubmit={submit}>
          <h1>{t("share.lock.title")}</h1>
          <Field
            label={t("share.lock.label")}
            for="share-pw"
            error={blocked ? t("share.lock.tooMany", { s: lockedFor }) : wrong ? t("share.lock.wrong") : failed ? t("share.lock.failed") : undefined}
          >
            <Input
              id="share-pw"
              type="password"
              autocomplete="current-password"
              bind:value={pw}
              invalid={blocked || wrong || failed}
              disabled={blocked}
            />
          </Field>
          <Button type="submit" variant="primary" block loading={busy} disabled={blocked || !pw}>{t("share.lock.submit")}</Button>
        </form>
      </Card>
    </div>
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
  .gate {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    padding: var(--sp-5);
  }
  h1 {
    font-size: var(--fs-lg);
  }
  @media (max-width: 720px) {
    .center {
      padding-top: var(--sp-10);
    }
  }
</style>
