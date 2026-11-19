<!--
  共享看板与公开看板（独立页，没有侧栏）。路径：
    /share/:slug            共享链接（正常）
    /share/locked…          密码门（原型里输入 rivu 视为正确）
    /share/missing…         链接无效、过期、吊销：服务端一律 404，界面上也不区分
    /public/:siteKey        公开看板（站点设置里开了「公开看板」）
  共享链接与公开看板都只授予只读统计权限；站点取 fixtures 里的公开站点。
  评审面板的「数据状态」演示入口信息的请求：加载是一块骨架，出错是「无法连接到服务器」加重试。
  App 把路径的第一个参数交给 param：共享链接是 slug，公开看板是站点密钥。
-->
<script lang="ts">
  import { CircleAlert, Link2 } from "@lucide/svelte";
  import ShareShell from "../../lib/app/ShareShell.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import DashboardPage from "../dashboard/DashboardPage.svelte";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import { SITES } from "../../lib/data/sites.svelte";
  import "../../lib/app/share-strings";

  let { kind, param }: { kind: "shared" | "public"; param: string } = $props();

  const site = $derived(kind === "public" ? SITES.find((s) => s.siteKey === param && s.isPublic) : SITES.find((s) => s.id === 2));

  // 密码门
  let unlocked = $state(false);
  let pw = $state("");
  let busy = $state(false);
  let wrong = $state(false);
  let failed = $state(false);
  /** 模拟服务端的限流：连续输错 5 次锁 60 秒 */
  let tries = $state(0);
  let lockedFor = $state(0);
  const blocked = $derived(lockedFor > 0);

  const ds = $derived(app.review.dataState);
  const phase = $derived.by<"loading" | "locked" | "ok" | "missing" | "failed">(() => {
    if (ds === "loading") return "loading";
    if (ds === "error" || ds === "busy" || ds === "timeout") return "failed";
    if (kind === "public") return site ? "ok" : "missing";
    if (!param || /^(missing|expired|revoked)/.test(param)) return "missing";
    if (param.startsWith("locked") && !unlocked) return "locked";
    return site ? "ok" : "missing";
  });

  $effect(() => {
    if (lockedFor <= 0) return;
    const id = setInterval(() => {
      lockedFor -= 1;
      if (lockedFor <= 0) {
        tries = 0;
        clearInterval(id);
      }
    }, 1000);
    return () => clearInterval(id);
  });

  function submit(e: SubmitEvent) {
    e.preventDefault();
    if (blocked || busy || !pw) return;
    busy = true;
    wrong = failed = false;
    setTimeout(() => {
      busy = false;
      const r = app.review.login;
      if (r === "rateLimited") lockedFor = 47;
      else if (r === "network" || r === "failed") failed = true;
      else if (pw === "rivu") unlocked = true;
      else {
        wrong = true;
        tries += 1;
        if (tries >= 5) lockedFor = 60;
      }
      pw = "";
    }, 450);
  }
</script>

<ShareShell {kind}>
  {#if phase === "ok" && site}
    {#key site.id}<DashboardPage {site} mode="shared" />{/key}
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
          {#snippet actions()}<Button onclick={() => toast(t("common.retry"))}>{t("common.retry")}</Button>{/snippet}
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
