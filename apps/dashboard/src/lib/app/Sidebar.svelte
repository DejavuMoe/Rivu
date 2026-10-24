<!--
  左侧栏：组织切换与搜索 → 全部站点 → 当前组织的站点 → 用户菜单。
  当前项用背景色表示，不加色条。服务端还没有的管理页（接入诊断、站点设置、成员、API 密钥）暂不出现。
-->
<script lang="ts">
  import { ChevronsUpDown, LayoutGrid, LogOut, Search, ShieldCheck } from "@lucide/svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import Popover from "../ui/Popover.svelte";
  import MenuItem from "../ui/MenuItem.svelte";
  import MenuLabel from "../ui/MenuLabel.svelte";
  import Avatar from "../ui/Avatar.svelte";
  import Logo from "../ui/Logo.svelte";
  import Kbd from "../ui/Kbd.svelte";
  import Segmented from "../ui/Segmented.svelte";
  import { t } from "../i18n";
  import { app, setLang, setTheme, type ThemePref, type Lang } from "../state.svelte";
  import { currentOrg, displayName, orgSites, orgs, session, setOrg } from "../session.svelte";
  import { signOut } from "./sign-out";

  let { onsearch }: { onsearch?: () => void } = $props();

  const org = $derived(currentOrg());
  const sites = $derived(orgSites());
  const path = $derived(page.url.pathname);
  const currentSite = $derived(page.params.id ? Number(page.params.id) : null);
</script>

<aside class="sidebar">
  <div class="top">
    <Popover width={248}>
      {#snippet trigger({ toggle, props })}
        <button type="button" class="org" onclick={toggle} {...props}>
          <Logo wordmark={false} />
          <span class="org-name">{org?.name ?? "Rivu"}</span>
          <ChevronsUpDown size={14} strokeWidth={1.75} class="chev" />
        </button>
      {/snippet}
      {#snippet children({ close })}
        {#each orgs() as o (o.id)}
          <MenuItem
            checked={o.id === session.orgId}
            hint={o.role ? t(`role.${o.role}`) : t("sites.guest")}
            onclick={() => {
              setOrg(o.id);
              close();
              void goto("/sites");
            }}
          >
            {o.name}
          </MenuItem>
        {/each}
      {/snippet}
    </Popover>

    <button type="button" class="search" onclick={onsearch}>
      <Search size={16} strokeWidth={1.75} />
      <span>{t("nav.search")}</span>
      <Kbd>⌘K</Kbd>
    </button>
  </div>

  <nav class="nav" aria-label={t("nav.sites")}>
    <a class="item" href="/sites" aria-current={path === "/sites" ? "page" : undefined}>
      <LayoutGrid size={16} strokeWidth={1.75} />
      <span class="grow">{t("nav.allSites")}</span>
    </a>

    {#if sites.length}
      <div class="group"><span>{t("nav.sites")}</span></div>
    {/if}

    {#each sites as s (s.id)}
      <a class="item" href="/s/{s.id}" aria-current={s.id === currentSite ? "page" : undefined}>
        <Avatar name={s.name} shape="square" size={16} />
        <span class="grow">{s.name}</span>
      </a>
    {/each}
  </nav>

  <div class="bottom">
    <Popover width={248} side="top">
      {#snippet trigger({ toggle, props })}
        <button type="button" class="item me" onclick={toggle} {...props}>
          <Avatar name={displayName()} size={20} />
          <span class="grow">{displayName()}</span>
        </button>
      {/snippet}
      {#snippet children({ close })}
        <div class="who">
          <span>{displayName()}</span>
          {#if session.me?.user.name}<span class="muted">{session.me.user.email}</span>{/if}
        </div>
        <MenuLabel separator />
        <MenuItem
          icon={ShieldCheck}
          onclick={() => {
            close();
            void goto("/account");
          }}>{t("nav.account")}</MenuItem
        >
        <div class="menu-ctl">
          <Segmented
            block
            size="sm"
            label={t("nav.theme")}
            value={app.theme}
            onchange={(v: ThemePref) => setTheme(v)}
            options={[
              { value: "system", label: t("theme.system") },
              { value: "light", label: t("theme.light") },
              { value: "dark", label: t("theme.dark") },
            ]}
          />
          <Segmented
            block
            size="sm"
            label={t("nav.language")}
            value={app.lang}
            onchange={(v: Lang) => setLang(v)}
            options={[
              { value: "zh", label: "中文" },
              { value: "en", label: "English" },
            ]}
          />
        </div>
        <MenuLabel separator />
        <MenuItem
          icon={LogOut}
          onclick={() => {
            close();
            void signOut();
          }}>{t("nav.signOut")}</MenuItem
        >
        <p class="version">Rivu {__RIVU_VERSION__}</p>
      {/snippet}
    </Popover>
  </div>
</aside>

<style>
  .sidebar {
    position: sticky;
    top: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    width: var(--sidebar-w);
    height: 100vh;
    padding: var(--sp-3) var(--sp-2);
    border-right: var(--hairline) solid var(--border-subtle);
    background: var(--bg-subtle);
  }
  .top {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .top :global(.popover),
  .bottom :global(.popover) {
    display: flex;
  }

  /* 所有行共用：高 32，左内边距 8，图标 16，图标与文字间距 8 —— 图标、头像、Logo 在一条竖线上 */
  .item,
  .org,
  .search {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    width: 100%;
    height: var(--control-md);
    padding: 0 var(--sp-2);
    border: 0;
    border-radius: var(--r-sm);
    background: none;
    color: var(--text-muted);
    text-align: left;
  }
  .item :global(svg) {
    flex: none;
  }
  .item:hover,
  .org:hover,
  .org[aria-expanded="true"] {
    background: var(--bg-element);
    color: var(--text);
  }
  .item[aria-current="page"] {
    background: var(--bg-hover);
    color: var(--text);
    font-weight: var(--fw-medium);
  }
  .grow {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .org {
    color: var(--text);
    font-weight: var(--fw-semibold);
  }
  .org-name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .org :global(.chev) {
    color: var(--text-muted);
  }
  .search {
    padding-right: 6px;
    border: var(--hairline) solid var(--border);
    background: var(--bg-app);
  }
  .search span {
    flex: 1;
  }
  .search:hover {
    border-color: var(--border-hover);
  }

  .nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .group {
    display: flex;
    align-items: center;
    height: var(--control-md);
    margin-top: var(--sp-3);
    padding: 0 var(--sp-1) 0 var(--sp-2);
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }

  .bottom {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .me {
    color: var(--text);
  }
  .who {
    display: flex;
    flex-direction: column;
    padding: 6px var(--sp-2);
    font-size: var(--fs-sm);
  }
  .who span:first-child {
    font-size: var(--fs-md);
    font-weight: var(--fw-medium);
  }
  .menu-ctl {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    padding: var(--sp-1);
  }
  .version {
    padding: var(--sp-1) var(--sp-2);
    color: var(--text-placeholder);
    font-size: var(--fs-sm);
  }
</style>
