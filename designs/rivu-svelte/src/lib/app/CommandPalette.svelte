<!--
  ⌘K / Ctrl+K 命令面板：居中浮层（原生 <dialog>，视觉与 Dialog 一致）。
  结果分三组（站点、页面、操作），组间用细线隔开，没有组标题；每项只有图标和名称。
  方向键移动，Enter 执行，Esc 关闭（原生 dialog 自带）。站点范围：在某个站点里时取该站点所在组织，否则取当前组织。
-->
<script lang="ts">
  import {
    Activity,
    BookOpen,
    ChartSpline,
    CornerDownLeft,
    KeyRound,
    LayoutGrid,
    Languages,
    Moon,
    Plus,
    Search,
    Settings2,
    ShieldCheck,
    Sun,
    Users,
  } from "@lucide/svelte";
  import Avatar from "../ui/Avatar.svelte";
  import Kbd from "../ui/Kbd.svelte";
  import type { IconType } from "../ui/types";
  import { SITES } from "../data/sites.svelte";
  import { t } from "../i18n";
  import { app, canManage, setLang, setTheme } from "../state.svelte";
  import { navigate, route } from "../router.svelte";
  import "./shell-strings";

  let { open = $bindable(false), orgId }: { open: boolean; orgId: string } = $props();

  type Group = "site" | "page" | "action";
  interface Item {
    id: string;
    group: Group;
    label: string;
    /** 只参与搜索，不显示 */
    keywords?: string;
    icon?: IconType;
    siteName?: string;
    run: () => void;
  }

  let dlg: HTMLDialogElement;
  let list: HTMLDivElement | undefined = $state();
  let q = $state("");
  let active = $state(0);

  $effect(() => {
    if (open && !dlg.open) {
      q = "";
      active = 0;
      dlg.showModal();
    }
    if (!open && dlg.open) dlg.close();
  });

  const seg = $derived(route.current.seg);
  const current = $derived(seg[0] === "s" ? SITES.find((s) => s.id === Number(seg[1])) : undefined);
  const scopedSites = $derived(SITES.filter((s) => s.org === (current?.org ?? orgId)));
  const target = $derived(current ?? scopedSites[0]);

  const isDark = $derived(
    app.theme === "dark" || (app.theme === "system" && typeof matchMedia === "function" && matchMedia("(prefers-color-scheme: dark)").matches),
  );

  const items = $derived.by<Item[]>(() => {
    const out: Item[] = [];
    for (const s of scopedSites) {
      out.push({
        id: `site-${s.id}`,
        group: "site",
        label: s.name,
        keywords: s.domain,
        siteName: s.name,
        run: () => navigate(`/s/${s.id}`),
      });
    }
    out.push({ id: "p-sites", group: "page", label: t("nav.allSites"), icon: LayoutGrid, run: () => navigate("/sites") });
    if (target) {
      out.push({ id: "p-dash", group: "page", label: `${target.name} ${t("nav.dashboard")}`, icon: ChartSpline, run: () => navigate(`/s/${target.id}`) });
      out.push({ id: "p-diag", group: "page", label: `${target.name} ${t("nav.diagnostics")}`, icon: Activity, run: () => navigate(`/s/${target.id}/diagnostics`) });
      if (canManage())
        out.push({ id: "p-set", group: "page", label: `${target.name} ${t("nav.settings")}`, icon: Settings2, run: () => navigate(`/s/${target.id}/settings/general`) });
    }
    out.push({ id: "p-mem", group: "page", label: t("nav.members"), icon: Users, run: () => navigate("/org/members") });
    out.push({ id: "p-key", group: "page", label: t("nav.apiKeys"), icon: KeyRound, run: () => navigate("/org/api-keys") });
    out.push({ id: "p-acc", group: "page", label: t("nav.account"), icon: ShieldCheck, run: () => navigate("/account") });
    out.push({ id: "p-doc", group: "page", label: t("nav.docs"), icon: BookOpen, run: () => window.open("https://github.com/", "_blank", "noreferrer") });
    if (canManage()) out.push({ id: "a-add", group: "action", label: t("nav.addSite"), icon: Plus, run: () => navigate("/sites/new") });
    out.push({
      id: "a-theme",
      group: "action",
      label: t("cp.toggleTheme"),
      icon: isDark ? Sun : Moon,
      run: () => setTheme(isDark ? "light" : "dark"),
    });
    out.push({
      id: "a-lang",
      group: "action",
      label: t("cp.toggleLang"),
      icon: Languages,
      run: () => setLang(app.lang === "zh" ? "en" : "zh"),
    });
    return out;
  });

  const shown = $derived.by(() => {
    const tokens = q.trim().toLowerCase().split(/\s+/).filter(Boolean);
    if (!tokens.length) return items;
    return items.filter((i) => {
      const hay = `${i.label} ${i.keywords ?? ""}`.toLowerCase();
      return tokens.every((k) => hay.includes(k));
    });
  });
  const groups = $derived(
    (["site", "page", "action"] as Group[])
      .map((g) => ({ g, rows: shown.map((it, idx) => ({ it, idx })).filter((r) => r.it.group === g) }))
      .filter((x) => x.rows.length > 0),
  );

  $effect(() => {
    void q;
    active = 0;
  });
  $effect(() => {
    if (!open || !list) return;
    list.querySelector<HTMLElement>(`[data-i="${active}"]`)?.scrollIntoView({ block: "nearest" });
  });

  function run(it: Item) {
    open = false;
    dlg.close();
    it.run();
  }

  function onKeydown(e: KeyboardEvent) {
    const n = shown.length;
    if (e.key === "ArrowDown" && n) {
      e.preventDefault();
      active = (active + 1) % n;
    } else if (e.key === "ArrowUp" && n) {
      e.preventDefault();
      active = (active - 1 + n) % n;
    } else if (e.key === "Home" && n) {
      e.preventDefault();
      active = 0;
    } else if (e.key === "End" && n) {
      e.preventDefault();
      active = n - 1;
    } else if (e.key === "Enter" && shown[active]) {
      e.preventDefault();
      run(shown[active]);
    }
  }
</script>

<dialog
  bind:this={dlg}
  aria-label={t("cp.label")}
  onclose={() => (open = false)}
  onclick={(e) => {
    if (e.target === dlg) dlg.close();
  }}
>
  <div class="inner">
    <div class="search">
      <Search size={18} strokeWidth={1.75} aria-hidden="true" />
      <input
        type="text"
        role="combobox"
        aria-expanded="true"
        aria-controls="cp-list"
        aria-activedescendant={shown[active] ? `cp-opt-${active}` : undefined}
        aria-autocomplete="list"
        autocomplete="off"
        spellcheck="false"
        placeholder={t("cp.placeholder")}
        aria-label={t("cp.placeholder")}
        bind:value={q}
        onkeydown={onKeydown}
      />
    </div>

    <div class="results" id="cp-list" role="listbox" aria-label={t("cp.label")} bind:this={list}>
      {#each groups as grp (grp.g)}
        <div class="group" role="group">
          {#each grp.rows as { it, idx } (it.id)}
            <button
              type="button"
              role="option"
              id="cp-opt-{idx}"
              data-i={idx}
              tabindex="-1"
              class="opt"
              class:active={idx === active}
              aria-selected={idx === active}
              onpointermove={() => {
                if (active !== idx) active = idx;
              }}
              onclick={() => run(it)}
            >
              {#if it.siteName}
                <Avatar name={it.siteName} shape="square" size={20} />
              {:else if it.icon}
                <it.icon size={16} strokeWidth={1.75} class="ico" aria-hidden="true" />
              {/if}
              <span class="label">{it.label}</span>
              {#if idx === active}<span class="tail"><Kbd><CornerDownLeft size={12} strokeWidth={2} aria-label="Enter" /></Kbd></span>{/if}
            </button>
          {/each}
        </div>
      {:else}
        <p class="none" role="status">{t("cp.none")}</p>
      {/each}
    </div>
  </div>
</dialog>

<style>
  dialog {
    width: min(calc(100vw - 32px), 600px);
    max-height: min(70vh, 560px);
    margin: 14vh auto auto;
    padding: 0;
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-lg);
    background: var(--bg-app);
    color: var(--text);
    box-shadow: var(--shadow-pop);
    overflow: hidden;
  }
  dialog[open] {
    animation: dialog-in var(--dur-pop) var(--ease);
  }
  dialog::backdrop {
    background: var(--overlay);
  }
  dialog[open]::backdrop {
    animation: fade-in var(--dur-pop) var(--ease);
  }
  .inner {
    display: flex;
    flex-direction: column;
    max-height: inherit;
  }
  .search {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: 0 var(--sp-4);
    height: 56px;
    border-bottom: var(--hairline) solid var(--border-subtle);
    color: var(--text-muted);
  }
  .search input {
    flex: 1;
    min-width: 0;
    height: 100%;
    padding: 0;
    border: 0;
    outline: 0;
    background: transparent;
    box-shadow: none;
    color: var(--text);
    font-size: var(--fs-lg);
  }
  .search input::placeholder {
    color: var(--text-placeholder);
  }
  .results {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: var(--sp-2);
  }
  .group + .group {
    margin-top: var(--sp-2);
    padding-top: var(--sp-2);
    border-top: var(--hairline) solid var(--border-subtle);
  }
  .opt {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    width: 100%;
    min-height: 36px;
    padding: 6px var(--sp-2);
    border: 0;
    border-radius: var(--r-xs);
    background: none;
    color: var(--text);
    font-size: var(--fs-md);
    text-align: left;
  }
  .opt.active {
    background: var(--bg-hover);
  }
  .opt :global(.ico) {
    flex: none;
    color: var(--text-muted);
  }
  .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tail {
    display: inline-flex;
    align-items: center;
    flex: none;
    margin-left: auto;
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .none {
    padding: var(--sp-8) var(--sp-4);
    color: var(--text-muted);
    font-size: var(--fs-md);
    text-align: center;
  }
</style>

