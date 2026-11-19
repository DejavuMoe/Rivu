<!--
  站点列表：当前组织的站点网格。卡片整体可点进看板；24 小时内没有数据的站点链到安装页。
  每张卡片显示按小时的访客走势，以及 24 小时去重访客总数（分小时的访客数不能相加）。
  站点多于 6 个时才出现搜索框。所有者和管理员在页头和空状态里有「添加站点」入口，列表下方是恢复期内的已删除站点。
  只以站点访客身份看到的组织：没有「添加站点」，站点带「访客权限」标记，也没有安装指引的链接。

  状态由评审面板的数据状态驱动：
    加载：卡片里的访客数据显示骨架
    无数据：组织没有站点
    出错、排队、超时：站点名单已经在会话里，只有各卡片的访客查询失败
  会话本身加载失败（app.loadFailed）是应用外壳的状态，见评审面板的「应用加载」，不在本页。
-->
<script lang="ts">
  import { Inbox, Plus, Search, X } from "@lucide/svelte";
  import PageHeader from "../../lib/app/PageHeader.svelte";
  import Sparkline from "../../lib/charts/Sparkline.svelte";
  import Avatar from "../../lib/ui/Avatar.svelte";
  import Badge from "../../lib/ui/Badge.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import LiveDot from "../../lib/ui/LiveDot.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import { hourly, type Site } from "../../lib/data/fixtures";
  import { DELETED_SITES, SITES, daysUntilPurge, restoreSite } from "../../lib/data/sites.svelte";
  import { compact } from "../../lib/format";
  import { t } from "../../lib/i18n";
  import { app, canAddSite, toast } from "../../lib/state.svelte";
  import { href } from "../../lib/router.svelte";
  import "./strings";

  let { orgId }: { orgId: string } = $props();

  type Stat = { values: number[]; total: number } | "loading" | "error";

  let q = $state("");

  const ds = $derived(app.review.dataState);
  const all = $derived(ds === "empty" ? [] : SITES.filter((s) => s.org === orgId));
  const shown = $derived.by(() => {
    const k = q.trim().toLowerCase();
    return k ? all.filter((s) => s.name.toLowerCase().includes(k)) : all;
  });
  // 只有组织的所有者和管理员能添加站点；只以站点访客身份可见的组织不行
  const canAdd = $derived(canAddSite(orgId));

  /** 过去 24 小时按小时的访客走势与去重总数。同一访客会出现在多个小时里，总数不是各小时之和，这里按 0.8 折算。 */
  function stat(s: Site): Stat {
    if (ds === "loading") return "loading";
    if (ds === "error" || ds === "busy" || ds === "timeout") return "error";
    const values = hourly(s, 24).map((p) => p.visitors);
    return { values, total: Math.round(values.reduce((a, b) => a + b, 0) * 0.8) };
  }

  // 已删除、仍在恢复期内的站点：只有所有者和管理员看得到
  const trash = $derived(ds !== "loading" && canAdd && app.review.deleted === "some" ? DELETED_SITES.filter((s) => s.org === orgId) : []);
  let restoring = $state<Site | null>(null);
  let restoreOpen = $state(false);
  let restoreBusy = $state(false);

  function askRestore(s: Site) {
    restoring = s;
    restoreOpen = true;
  }
  function restore() {
    const s = restoring;
    if (!s || restoreBusy) return;
    restoreBusy = true;
    setTimeout(() => {
      restoreSite(s.id);
      restoreBusy = false;
      restoreOpen = false;
      toast(t("sl.deleted.restored", { name: s.name }), "good");
    }, 500);
  }
</script>

<div class="page">
  <PageHeader title={t("sites.title")}>
    {#snippet actions()}
      {#if canAdd}
        <Button variant="primary" icon={Plus} href={href("/sites/new")}>{t("nav.addSite")}</Button>
      {/if}
    {/snippet}
  </PageHeader>

  {#if all.length > 6}
    <div class="search">
      <Input icon={Search} type="search" placeholder={t("sl.search")} aria-label={t("sl.search")} bind:value={q}>
        {#snippet suffix()}
          {#if q}<Button variant="ghost" size="sm" icon={X} aria-label={t("common.clear")} onclick={() => (q = "")} />{/if}
        {/snippet}
      </Input>
    </div>
  {/if}

  {#if all.length === 0}
    <Card>
      <EmptyState icon={Inbox} title={t("sites.empty.title")} body={canAdd ? undefined : t("sl.empty.viewer")}>
        {#snippet actions()}
          {#if canAdd}<Button variant="primary" icon={Plus} href={href("/sites/new")}>{t("nav.addSite")}</Button>{/if}
        {/snippet}
      </EmptyState>
    </Card>
  {:else if shown.length === 0}
    <Card>
      <EmptyState icon={Search} title={t("sl.noMatch.title")}>
        {#snippet actions()}<Button onclick={() => (q = "")}>{t("common.clear")}</Button>{/snippet}
      </EmptyState>
    </Card>
  {:else}
    <ul class="grid">
      {#each shown as s (s.id)}
        {@const st = stat(s)}
        <li class="site">
          <div class="head">
            <Avatar name={s.name} shape="square" size={32} />
            <div class="names">
              <a class="name" href={href(`/s/${s.id}`)} aria-label={t("sl.openSite", { name: s.name })}>{s.name}</a>
            </div>
            {#if s.isPublic || s.guest}
              <div class="badges">
                {#if s.isPublic}<Badge>{t("sites.public")}</Badge>{/if}
                {#if s.guest}<Badge>{t("sites.guest")}</Badge>{/if}
              </div>
            {/if}
          </div>

          {#if st === "loading"}
            <div class="metric" aria-busy="true">
              <div class="num-block">
                <span class="label">{t("sites.visitors24h")}</span>
                <Skeleton width="64px" height={24} />
              </div>
              <Skeleton width="112px" height={40} radius={6} />
            </div>
          {:else if st === "error"}
            <p class="failed">{t("sl.statsFailed")}</p>
          {:else if st.total === 0 && st.values.every((v) => v === 0)}
            <div class="wait">
              <span class="wait-title"><LiveDot off size={8} />{t("sites.noData")}</span>
              {#if !s.guest}
                <a class="text-link" href={href(`/s/${s.id}/install`)}>{t("sl.install")}</a>
              {/if}
            </div>
          {:else}
            <div class="metric">
              <div class="num-block">
                <span class="label">{t("sites.visitors24h")}</span>
                <span class="value">{compact(st.total, app.lang)}</span>
              </div>
              <Sparkline values={st.values} width={112} height={40} label={t("sites.visitors24h")} />
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if trash.length}
    <Card title={t("sl.deleted.title")} flush>
      <Table
        caption={t("sl.deleted.title")}
        rows={trash}
        rowKey={(s) => s.id}
        columns={[
          { key: "n", label: t("sl.deleted.name") },
          { key: "d", label: t("as.domain"), optional: true },
          { key: "p", label: t("sl.deleted.purge"), width: "160px" },
          { key: "a", label: "", width: "88px", align: "right" },
        ]}
      >
        {#snippet cell(s, c)}
          {#if c.key === "n"}{s.name}
          {:else if c.key === "d"}<span class="muted">{s.domain || "—"}</span>
          {:else if c.key === "p"}
            {@const n = daysUntilPurge(s)}
            <span class="muted">{n === 0 ? t("sl.deleted.today") : t("sl.deleted.days", { n })}</span>
          {:else}<Button size="sm" aria-label="{t('sl.deleted.restore')} {s.name}" onclick={() => askRestore(s)}>{t("sl.deleted.restore")}</Button>{/if}
        {/snippet}
      </Table>
    </Card>
  {/if}
</div>

<Dialog bind:open={restoreOpen} size="sm" title={t("sl.deleted.dlg.title", { name: restoring?.name ?? "" })}>
  <p>{t("sl.deleted.dlg.body")}</p>
  {#snippet footer()}
    <Button onclick={() => (restoreOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="primary" loading={restoreBusy} onclick={restore}>{t("sl.deleted.restore")}</Button>
  {/snippet}
</Dialog>

<style>
  .page {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    width: 100%;
    max-width: var(--content-max);
    margin: 0 auto;
    padding: var(--sp-6) var(--sp-8) var(--sp-12);
  }
  .search {
    max-width: 360px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
    gap: var(--sp-4);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .site {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    min-width: 0;
    padding: var(--sp-4) var(--sp-5);
    background: var(--bg-app);
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-md);
    transition:
      background-color var(--dur-fast) var(--ease),
      border-color var(--dur-fast) var(--ease);
  }
  .site:hover {
    background: var(--bg-subtle);
    border-color: var(--border);
  }
  .site:focus-within {
    border-color: var(--accent-focus);
  }
  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
  }
  .names {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-width: 0;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: var(--fw-semibold);
    outline: none;
    box-shadow: none;
  }
  /* 整张卡片都是点击区域；安装链接用 z-index 压在上面 */
  .name::after {
    content: "";
    position: absolute;
    inset: 0;
    border-radius: var(--r-md);
  }
  .badges {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: var(--sp-1);
    flex: none;
  }
  .metric {
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .num-block {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .label {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .value {
    font-size: var(--fs-xl);
    font-weight: var(--fw-semibold);
    line-height: var(--lh-tight);
  }
  .failed {
    color: var(--text-muted);
  }
  .wait {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    padding: var(--sp-3);
    border-radius: var(--r-sm);
    background: var(--bg-element);
  }
  .wait-title {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    color: var(--text-muted);
  }
  .wait .text-link {
    position: relative;
    z-index: 1;
  }
  @media (max-width: 720px) {
    .page {
      padding: var(--sp-4) var(--sp-4) var(--sp-10);
    }
    .grid {
      grid-template-columns: 1fr;
    }
    .search {
      max-width: none;
    }
  }
</style>
