<!--
  原型入口：主题与语言落到 <html>，按 hash 路由选页面。
  外壳分两种：带侧栏的应用页（站点、看板、设置……）和居中的独立页（登录、初始化、共享链接）。
  应用页的权限与生产一致：看站点上的有效角色，没有权限的站点页（接入诊断、没有可见分区的设置）与不存在的站点一样是 404。
  评审面板的「应用加载」为失败时，应用页只剩「无法连接到服务器」和重试（没有侧栏），独立页不受影响。

  路由表（与正式实现的路径一致，去掉 #）：
    /login  /forgot-password  /reset-password/:token  /setup  /invite/:token   独立页
    /weekly-report/confirm/:token                                              确认周报邮箱（邮件里的链接，独立页）
    /share/:slug  /public/:key                                                 共享链接、公开看板（独立页）
    /sites  /sites/new                                    站点列表、添加站点
    /s/:id  /s/:id/install  /s/:id/diagnostics           看板、安装、接入诊断
    /s/:id/settings/:section                               站点设置
    /org/members  /org/api-keys  /account                组织与账号
-->
<script lang="ts">
  import { CircleAlert } from "@lucide/svelte";
  import Sidebar from "./lib/app/Sidebar.svelte";
  import MobileNav from "./lib/app/MobileNav.svelte";
  import CommandPalette from "./lib/app/CommandPalette.svelte";
  import ReviewPanel from "./lib/app/ReviewPanel.svelte";
  import Button from "./lib/ui/Button.svelte";
  import EmptyState from "./lib/ui/EmptyState.svelte";
  import Toaster from "./lib/ui/Toaster.svelte";
  import { t } from "./lib/i18n";
  import { app, canEdit, toast } from "./lib/state.svelte";
  import { route } from "./lib/router.svelte";
  import { SITES } from "./lib/data/sites.svelte";
  import { visibleSections } from "./lib/data/sections";
  import DashboardPage from "./routes/dashboard/DashboardPage.svelte";
  import LoginPage from "./routes/auth/LoginPage.svelte";
  import ForgotPasswordPage from "./routes/auth/ForgotPasswordPage.svelte";
  import ResetPasswordPage from "./routes/auth/ResetPasswordPage.svelte";
  import SetupPage from "./routes/auth/SetupPage.svelte";
  import InvitePage from "./routes/auth/InvitePage.svelte";
  import WeeklyConfirmPage from "./routes/auth/WeeklyConfirmPage.svelte";
  import SitesPage from "./routes/sites/SitesPage.svelte";
  import AddSitePage from "./routes/sites/AddSitePage.svelte";
  import InstallPage from "./routes/sites/InstallPage.svelte";
  import DiagnosticsPage from "./routes/diagnostics/DiagnosticsPage.svelte";
  import SettingsPage from "./routes/settings/SettingsPage.svelte";
  import MembersPage from "./routes/org/MembersPage.svelte";
  import ApiKeysPage from "./routes/org/ApiKeysPage.svelte";
  import AccountPage from "./routes/account/AccountPage.svelte";
  import SharedPage from "./routes/share/SharedPage.svelte";
  import NotFound from "./routes/NotFound.svelte";

  let paletteOpen = $state(false);

  $effect(() => {
    const root = document.documentElement;
    if (app.theme === "system") root.removeAttribute("data-theme");
    else root.dataset.theme = app.theme;
    root.lang = app.lang === "zh" ? "zh-CN" : "en";
  });

  const seg = $derived(route.current.seg);
  // 公开看板的路径是 /public/:key；/share/public/:key 是原型早期的写法，照样认得
  const publicKey = $derived(seg[0] === "public" ? (seg[1] ?? "") : seg[0] === "share" && seg[1] === "public" ? (seg[2] ?? "") : undefined);
  const standalone = $derived(["login", "forgot-password", "reset-password", "setup", "share", "public", "invite", "weekly-report"].includes(seg[0]));
  const site = $derived(seg[0] === "s" ? SITES.find((s) => s.id === Number(seg[1])) : undefined);
  const failed = $derived(app.review.shell === "failed");

  // 站点页跟着站点所属的组织走：侧栏的站点列表与组织菜单随之切换
  $effect(() => {
    if (site && site.org !== app.orgId) app.orgId = site.org;
  });

  $effect(() => {
    document.title = site ? `${site.name} – Rivu` : "Rivu";
  });

  function onKeydown(e: KeyboardEvent) {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k" && !standalone && !failed) {
      e.preventDefault();
      paletteOpen = !paletteOpen;
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if standalone}
  <main class="standalone">
    {#if seg[0] === "login"}
      <LoginPage />
    {:else if seg[0] === "forgot-password"}
      <ForgotPasswordPage />
    {:else if seg[0] === "reset-password"}
      <ResetPasswordPage token={seg[1] ?? ""} />
    {:else if seg[0] === "setup"}
      <SetupPage />
    {:else if seg[0] === "invite"}
      <InvitePage />
    {:else if seg[0] === "weekly-report"}
      <WeeklyConfirmPage token={seg[1] === "confirm" ? (seg[2] ?? "") : ""} />
    {:else if publicKey !== undefined}
      <SharedPage kind="public" param={publicKey} />
    {:else}
      <SharedPage kind="shared" param={seg[1] ?? ""} />
    {/if}
  </main>
{:else if failed}
  <main class="failed">
    <EmptyState icon={CircleAlert} tone="bad" title={t("app.loadFailed")}>
      {#snippet actions()}<Button onclick={() => toast(t("common.retry"))}>{t("common.retry")}</Button>{/snippet}
    </EmptyState>
  </main>
{:else}
  <div class="app">
    <Sidebar bind:orgId={app.orgId} onsearch={() => (paletteOpen = true)} />
    <div class="column">
      <MobileNav bind:orgId={app.orgId} onsearch={() => (paletteOpen = true)} />
      <main class="content">
        {#if seg[0] === "s"}
          {#if !site}
            <NotFound />
          {:else if !seg[2]}
            {#key site.id}<DashboardPage {site} />{/key}
          {:else if seg[2] === "install"}
            <InstallPage {site} />
          {:else if seg[2] === "diagnostics" && canEdit(site)}
            <DiagnosticsPage {site} />
          {:else if seg[2] === "settings" && visibleSections(site).length > 0}
            {#key site.id}<SettingsPage {site} section={seg[3] ?? "general"} />{/key}
          {:else}
            <NotFound />
          {/if}
        {:else if seg[0] === "sites" && seg[1] === "new"}
          <AddSitePage />
        {:else if seg[0] === "sites" || seg.length === 0}
          <SitesPage orgId={app.orgId} />
        {:else if seg[0] === "org" && seg[1] === "members"}
          <MembersPage orgId={app.orgId} />
        {:else if seg[0] === "org" && seg[1] === "api-keys"}
          <ApiKeysPage orgId={app.orgId} />
        {:else if seg[0] === "account"}
          <AccountPage />
        {:else}
          <NotFound />
        {/if}
      </main>
    </div>
  </div>
  <CommandPalette bind:open={paletteOpen} orgId={app.orgId} />
{/if}

{#if route.current.query.get("capture") !== "1"}<ReviewPanel />{/if}
<Toaster />

<style>
  .app {
    display: grid;
    grid-template-columns: var(--sidebar-w) minmax(0, 1fr);
    min-height: 100vh;
  }
  .column,
  .content {
    min-width: 0;
  }
  .standalone {
    min-height: 100vh;
  }
  .failed {
    max-width: 560px;
    margin: var(--sp-12) auto;
    padding: 0 var(--sp-4);
  }
  @media (max-width: 860px) {
    .app {
      grid-template-columns: 1fr;
    }
    .app :global(.sidebar) {
      display: none;
    }
  }
</style>
