<!--
  共享看板（独立页，没有侧栏）。路径：
    /share/:slug            共享链接（正常）
    /share/expired…         链接已过期
    /share/locked…          密码门（原型里输入 rivu 视为正确）
    /share/public/:siteKey  公开看板（站点设置里开了「公开看板」）
  共享链接与公开看板都只授予只读统计权限；站点取 fixtures 里的公开站点。
  注意：App 把 seg[1] 当作 slug 传进来，公开看板的 seg[1] 是 "public"，所以这里直接读路由。
-->
<script lang="ts">
  import { Clock, Link2, Monitor, Moon, Sun } from "@lucide/svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Logo from "../../lib/ui/Logo.svelte";
  import Segmented from "../../lib/ui/Segmented.svelte";
  import DashboardPage from "../dashboard/DashboardPage.svelte";
  import { t } from "../../lib/i18n";
  import { app, setTheme, type ThemePref } from "../../lib/state.svelte";
  import { route } from "../../lib/router.svelte";
  import { SITES } from "../../lib/data/sites.svelte";
  import "./strings";

  // slug 由 App 传入，但公开看板的路径需要第二段，所以以路由为准
  let { slug: _slug }: { slug: string } = $props();

  const seg = $derived(route.current.seg);
  const isPublic = $derived(seg[1] === "public");
  const slug = $derived(isPublic ? "" : (seg[1] ?? ""));

  const site = $derived(
    isPublic ? SITES.find((s) => s.siteKey === seg[2] && s.isPublic) : SITES.find((s) => s.id === 2),
  );
  const kind = $derived<"ok" | "expired" | "locked" | "missing">(
    isPublic
      ? site
        ? "ok"
        : "missing"
      : !slug
        ? "missing"
        : slug.startsWith("expired")
          ? "expired"
          : slug.startsWith("locked")
            ? "locked"
            : "ok",
  );

  // 密码门
  let unlocked = $state(false);
  let pw = $state("");
  let wrong = $state(0);
  let lockedFor = $state(0);
  let showError = $state(false);

  const blocked = $derived(lockedFor > 0);

  $effect(() => {
    if (lockedFor <= 0) return;
    const id = setInterval(() => {
      lockedFor -= 1;
      if (lockedFor <= 0) wrong = 0;
    }, 1000);
    return () => clearInterval(id);
  });

  function submit(e: SubmitEvent) {
    e.preventDefault();
    if (blocked) return;
    if (app.review.login === "rateLimited") {
      lockedFor = 47;
      return;
    }
    if (pw === "rivu") {
      unlocked = true;
      showError = false;
      return;
    }
    wrong += 1;
    showError = true;
    pw = "";
    if (wrong >= 5) lockedFor = 60;
  }
</script>

<div class="shell">
  <header class="bar">
    <div class="brand">
      <Logo size={20} />
      <span class="what">{isPublic ? t("share.brandPublic") : t("share.brand")}</span>
    </div>
    <Segmented
      size="sm"
      label={t("share.theme")}
      value={app.theme}
      onchange={(v: ThemePref) => setTheme(v)}
      options={[
        { value: "system", label: "", icon: Monitor, hint: t("theme.system") },
        { value: "light", label: "", icon: Sun, hint: t("theme.light") },
        { value: "dark", label: "", icon: Moon, hint: t("theme.dark") },
      ]}
    />
  </header>

  {#if kind === "ok" && site}
    {#key site.id}<DashboardPage {site} mode="shared" />{/key}
  {:else if kind === "locked" && !unlocked}
    <div class="center">
      <Card>
        <form class="gate" onsubmit={submit}>
          <h1>{t("share.lock.title")}</h1>
          <Field
            label={t("share.lock.label")}
            for="share-pw"
            error={blocked ? t("share.lock.tooMany", { s: lockedFor }) : showError ? t("share.lock.wrong") : undefined}
          >
            <Input
              id="share-pw"
              type="password"
              autocomplete="current-password"
              bind:value={pw}
              invalid={blocked || showError}
              disabled={blocked}
            />
            {#if showError && !blocked && wrong > 0}
              <p class="left">{t("share.lock.left", { n: 5 - wrong })}</p>
            {/if}
          </Field>
          <Button type="submit" variant="primary" block disabled={blocked || !pw}>{t("share.lock.submit")}</Button>
        </form>
      </Card>
    </div>
  {:else if kind === "locked"}
    {#if site}{#key site.id}<DashboardPage {site} mode="shared" />{/key}{/if}
  {:else if kind === "expired"}
    <div class="center">
      <Card>
        <EmptyState icon={Clock} tone="warn" title={t("share.expired.title")} />
      </Card>
    </div>
  {:else}
    <div class="center">
      <Card>
        <EmptyState icon={Link2} title={t("share.notFound.title")} />
      </Card>
    </div>
  {/if}
</div>

<style>
  .shell {
    min-height: 100vh;
  }
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    height: 48px;
    padding: 0 var(--sp-8);
    border-bottom: var(--hairline) solid var(--border-subtle);
    background: var(--bg-app);
  }
  .brand {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
  }
  .what {
    font-weight: var(--fw-medium);
    white-space: nowrap;
  }
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
  .left {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  @media (max-width: 720px) {
    .bar {
      padding: 0 var(--sp-4);
    }
    .center {
      padding-top: var(--sp-10);
    }
  }
</style>