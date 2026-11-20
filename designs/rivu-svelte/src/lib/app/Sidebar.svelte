<!--
  左侧栏：组织切换与搜索 → 全部站点 → 当前组织的站点 → 组织 → 用户菜单。owner / admin 在「站点」标题行右侧有添加站点的加号。
  侧栏只负责「去哪个对象」；站点内的看板 / 接入诊断 / 安装 / 设置在页头的标签栏（SiteHeader），组织的成员 / API 密钥同理（OrgHeader）。
  组织菜单里也有成员、API 密钥入口；只以站点访客身份可见的组织（菜单里标「访客权限」）没有这两页。当前项用背景色表示，不加色条。
  onclose 有值时是移动端抽屉：顶部多一个关闭按钮，行高加大到 40px 便于触摸。
-->
<script lang="ts">
  import { ChevronsUpDown, KeyRound, LayoutGrid, LogOut, Plus, Search, ShieldCheck, Users, X } from "@lucide/svelte";
  import Popover from "../ui/Popover.svelte";
  import MenuItem from "../ui/MenuItem.svelte";
  import MenuLabel from "../ui/MenuLabel.svelte";
  import Avatar from "../ui/Avatar.svelte";
  import Logo from "../ui/Logo.svelte";
  import Kbd from "../ui/Kbd.svelte";
  import Segmented from "../ui/Segmented.svelte";
  import { ORGS, USER } from "../data/fixtures";
  import { account } from "../../routes/account/email.svelte";
  import { SITES } from "../data/sites.svelte";
  import { t } from "../i18n";
  import { app, canAddSite, setLang, setTheme, type ThemePref, type Lang } from "../state.svelte";
  import { href, navigate, route } from "../router.svelte";

  let {
    orgId = $bindable(),
    onsearch,
    onclose,
  }: { orgId: string; onsearch?: () => void; onclose?: () => void } = $props();

  const org = $derived(ORGS.find((o) => o.id === orgId)!);
  const sites = $derived(SITES.filter((s) => s.org === orgId));
  const seg = $derived(route.current.seg);
  const currentSite = $derived(seg[0] === "s" ? Number(seg[1]) : null);

</script>

<aside class="sidebar" class:drawer={!!onclose}>
  <div class="top">
    <div class="head">
    <Popover width="trigger">
      {#snippet trigger({ toggle, props })}
        <button type="button" class="org" onclick={toggle} {...props}>
          <Logo wordmark={false} />
          <span class="org-name">{org.name}</span>
          <ChevronsUpDown size={14} strokeWidth={1.75} class="chev" />
        </button>
      {/snippet}
      {#snippet children({ close })}
        {#each ORGS as o (o.id)}
          <MenuItem
            checked={o.id === orgId}
            hint={o.role ? t(`role.${o.role}`) : t("sites.guest")}
            onclick={() => {
              orgId = o.id;
              close();
              navigate("/sites");
            }}
          >
            {o.name}
          </MenuItem>
        {/each}
        {#if org.role}
          <MenuLabel separator />
          <MenuItem icon={Users} onclick={() => { close(); navigate("/org/members"); }}>{t("nav.members")}</MenuItem>
          <MenuItem icon={KeyRound} onclick={() => { close(); navigate("/org/api-keys"); }}>{t("nav.apiKeys")}</MenuItem>
        {/if}
      {/snippet}
    </Popover>
    {#if onclose}
      <button type="button" class="close" aria-label={t("mn.close")} onclick={onclose}><X size={18} strokeWidth={1.75} /></button>
    {/if}
    </div>

    <button type="button" class="search" onclick={onsearch}>
      <Search size={16} strokeWidth={1.75} />
      <span>{t("nav.search")}</span>
      <Kbd>⌘K</Kbd>
    </button>
  </div>

  <nav class="nav" aria-label={t("nav.sites")}>
    <a class="item" href={href("/sites")} aria-current={seg[0] === "sites" && !seg[1] ? "page" : undefined}>
      <LayoutGrid size={16} strokeWidth={1.75} />
      <span class="grow">{t("nav.allSites")}</span>
    </a>

    {#if sites.length || canAddSite(orgId)}
      <div class="group">
        <span>{t("nav.sites")}</span>
        {#if canAddSite(orgId)}
          <a class="add" href={href("/sites/new")} aria-label={t("nav.addSite")} title={t("nav.addSite")}><Plus size={16} strokeWidth={1.75} /></a>
        {/if}
      </div>
    {/if}

    {#each sites as s (s.id)}
      <a class="item" href={href(`/s/${s.id}`)} aria-current={s.id === currentSite ? "page" : undefined}>
        <Avatar name={s.name} shape="square" size={16} />
        <span class="grow">{s.name}</span>
      </a>
    {/each}
  </nav>

  <div class="bottom">
    {#if org.role}
      <a class="item" href={href("/org/members")} aria-current={seg[0] === "org" ? "page" : undefined}>
        <Users size={16} strokeWidth={1.75} />
        <span class="grow">{t("nav.org")}</span>
      </a>
    {/if}

    <Popover width="trigger" side="top">
      {#snippet trigger({ toggle, props })}
        <button type="button" class="item me" onclick={toggle} {...props}>
          <Avatar name={USER.name} size={20} />
          <span class="grow">{USER.name}</span>
        </button>
      {/snippet}
      {#snippet children({ close })}
        <div class="who">
          <span>{USER.name}</span>
          <span class="muted">{account.email}</span>
        </div>
        <MenuLabel separator />
        <MenuItem icon={ShieldCheck} onclick={() => { close(); navigate("/account"); }}>{t("nav.account")}</MenuItem>
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
        <MenuItem icon={LogOut} onclick={() => { close(); navigate("/login"); }}>{t("nav.signOut")}</MenuItem>
        <p class="version">Rivu 0.1.0</p>
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
  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-1);
  }
  .head :global(.popover) {
    flex: 1;
    min-width: 0;
  }
  .close {
    display: grid;
    place-items: center;
    flex: none;
    width: var(--control-md);
    height: var(--control-md);
    border: 0;
    border-radius: var(--r-sm);
    background: none;
    color: var(--text-muted);
  }
  .close:hover {
    background: var(--bg-element);
    color: var(--text);
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
    transition:
      background-color var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
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
    justify-content: space-between;
    height: var(--control-md);
    margin-top: var(--sp-3);
    padding: 0 var(--sp-1) 0 var(--sp-2);
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .add {
    display: grid;
    place-items: center;
    width: 24px;
    height: 24px;
    border-radius: var(--r-xs);
    color: var(--text-muted);
  }
  .add:hover {
    background: var(--bg-element);
    color: var(--text);
  }
  .bottom {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  /* 头像 20px 比图标宽 4px：左内边距与间距各少 2px，头像中心与图标中心、名字与其他行的文字都在同一条竖线上（6 + 20 + 6 = 8 + 16 + 8） */
  .me {
    gap: 6px;
    padding-left: 6px;
    color: var(--text);
  }

  /* 移动端抽屉：行高 40，内容避开刘海与底部手势条 */
  .drawer {
    padding-top: max(var(--sp-3), env(safe-area-inset-top));
    padding-bottom: max(var(--sp-3), env(safe-area-inset-bottom));
    padding-left: max(var(--sp-2), env(safe-area-inset-left));
  }
  .drawer .item,
  .drawer .org,
  .drawer .search,
  .drawer .close {
    height: 40px;
  }
  .drawer .close {
    width: 40px;
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
