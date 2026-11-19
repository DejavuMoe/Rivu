<!--
  站点列表：当前组织的站点网格；所有者和管理员在网格下方还能看到 7 天恢复期内的已删除站点并恢复。卡片整体可点进看板；未接入的站点链到安装页。
  状态：加载（骨架）、空组织、出错、搜索无结果。只有所有者和管理员能看到「添加站点」。
  站点多于 6 个时才出现搜索框。
-->
<script lang="ts">
  import { ArrowDownRight, ArrowUpRight, Inbox, Plus, Search, TriangleAlert, X } from "@lucide/svelte";
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
  import { daily, type Site } from "../../lib/data/fixtures";
  import { DELETED_SITES, SITES, daysUntilPurge, restoreSite } from "../../lib/data/sites.svelte";
  import { compact, delta } from "../../lib/format";
  import { t } from "../../lib/i18n";
  import { app, canManage, toast } from "../../lib/state.svelte";
  import { href } from "../../lib/router.svelte";
  import "./strings";

  let { orgId }: { orgId: string } = $props();

  let q = $state("");

  const ds = $derived(app.review.dataState);
  const loading = $derived(ds === "loading");
  const failed = $derived(ds === "error" || ds === "busy" || ds === "timeout");
  const orgSites = $derived(ds === "empty" ? [] : SITES.filter((s) => s.org === orgId));
  const shown = $derived.by(() => {
    const k = q.trim().toLowerCase();
    return k ? orgSites.filter((s) => s.name.toLowerCase().includes(k) || s.domain.toLowerCase().includes(k)) : orgSites;
  });

  // 已删除、仍在恢复期内的站点：只有所有者和管理员看得到
  const trash = $derived(
    !loading && !failed && canManage() && app.review.deleted === "some" ? DELETED_SITES.filter((s) => s.org === orgId) : [],
  );
  let restoring = $state<Site | null>(null);
  let restoreOpen = $state(false);

  function purgeText(s: Site) {
    const n = daysUntilPurge(s);
    return n === 0 ? t("sl.deleted.purge.today") : n === 1 ? t("sl.deleted.purge.one") : t("sl.deleted.purge", { n });
  }
  function confirmRestore() {
    if (!restoring) return;
    const name = restoring.name;
    restoreSite(restoring.id);
    restoreOpen = false;
    toast(t("sl.restore.done", { name }), "good");
  }

  /** 过去 7 天（不含今天）与前 7 天的访客数、走势。 */
  function stats(s: Site) {
    const cur = daily(s, 7, 1);
    const prev = daily(s, 7, 8);
    const sum = (p: typeof cur) => p.reduce((a, b) => a + b.visitors, 0);
    return { values: cur.map((p) => p.visitors), total: sum(cur), delta: delta(sum(cur), sum(prev)) };
  }
</script>

<div class="page">
  <PageHeader title={t("sites.title")}>
    {#snippet actions()}
      {#if canManage()}
        <Button variant="primary" icon={Plus} href={href("/sites/new")}>{t("nav.addSite")}</Button>
      {/if}
    {/snippet}
  </PageHeader>

  {#if !loading && !failed && orgSites.length > 6}
    <div class="search">
      <Input icon={Search} type="search" placeholder={t("sl.search")} aria-label={t("sl.search")} bind:value={q}>
        {#snippet suffix()}
          {#if q}<Button variant="ghost" size="sm" icon={X} aria-label={t("common.clear")} onclick={() => (q = "")} />{/if}
        {/snippet}
      </Input>
    </div>
  {/if}

  {#if loading}
    <div class="grid" aria-busy="true">
      {#each [0, 1, 2] as i (i)}
        <div class="site">
          <div class="head"><Skeleton width="32px" height={32} radius={4} /><Skeleton width="55%" height={14} /></div>
          <Skeleton width="40%" height={24} />
          <Skeleton height={32} radius={6} />
        </div>
      {/each}
    </div>
  {:else if failed}
    <Card>
      <EmptyState icon={TriangleAlert} tone="bad" title={t("sl.error.title")}>
        {#snippet actions()}<Button onclick={() => toast(t("common.retry"))}>{t("common.retry")}</Button>{/snippet}
      </EmptyState>
    </Card>
  {:else if orgSites.length === 0}
    <Card>
      <EmptyState icon={Inbox} title={t("sl.empty.title")} body={canManage() ? undefined : t("sl.empty.viewer")}>
        {#snippet actions()}
          {#if canManage()}<Button variant="primary" icon={Plus} href={href("/sites/new")}>{t("nav.addSite")}</Button>{/if}
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
        {@const st = stats(s)}
        <li class="site">
          <div class="head">
            <Avatar name={s.name} shape="square" size={32} />
            <div class="names">
              <a class="name" href={href(`/s/${s.id}`)} aria-label={t("sl.openSite", { name: s.name })}>{s.name}</a>
              <span class="domain">{s.domain}</span>
            </div>
            {#if s.isPublic || s.guest}
              <div class="badges">
                {#if s.isPublic}<Badge>{t("sites.public")}</Badge>{/if}
                {#if s.guest}<Badge>{t("sites.guest")}</Badge>{/if}
              </div>
            {/if}
          </div>

          {#if s.scale === 0}
            <div class="wait">
              <span class="wait-title"><LiveDot off size={8} />{t("sl.waiting")}</span>
              <a class="text-link" href={href(`/s/${s.id}/install`)}>{t("sl.install")}</a>
            </div>
          {:else}
            <div class="metric">
              <div class="num-block">
                <span class="label">{t("sl.visitors7d")}</span>
                <span class="row">
                  <span class="value">{compact(st.total, app.lang)}</span>
                  {#if st.delta.dir !== "flat"}
                    <span class="delta" class:good={st.delta.dir === "up"} class:bad={st.delta.dir === "down"}>
                      {#if st.delta.dir === "up"}<ArrowUpRight size={12} strokeWidth={2} aria-hidden="true" />{:else}<ArrowDownRight size={12} strokeWidth={2} aria-hidden="true" />{/if}
                      {st.delta.text}
                    </span>
                  {/if}
                </span>
              </div>
              <Sparkline values={st.values} width={112} height={40} label={t("sl.visitors7d")} />
            </div>
            <span class="live" class:off={s.live === 0}>
              <LiveDot off={s.live === 0} size={8} />
              {s.live > 0 ? t("sl.online", { n: s.live }) : t("sl.offline")}
            </span>
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
          { key: "name", label: t("sl.deleted.name") },
          { key: "domain", label: t("sl.deleted.domain"), optional: true },
          { key: "purge", label: "", optional: true },
          { key: "act", label: "", align: "right", width: "96px" },
        ]}
      >
        {#snippet cell(s, c)}
          {#if c.key === "name"}<span class="strong">{s.name}</span>
          {:else if c.key === "domain"}<span class="muted">{s.domain}</span>
          {:else if c.key === "purge"}<span class="muted">{purgeText(s)}</span>
          {:else}<Button
              size="sm"
              aria-label="{t('sl.restore')} {s.name}"
              onclick={() => {
                restoring = s;
                restoreOpen = true;
              }}>{t("sl.restore")}</Button
            >{/if}
        {/snippet}
      </Table>
    </Card>
  {/if}
</div>

<Dialog bind:open={restoreOpen} size="sm" title={t("sl.restore.title", { name: restoring?.name ?? "" })}>
  <p>{t("sl.restore.body")}</p>
  {#snippet footer()}
    <Button onclick={() => (restoreOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="primary" onclick={confirmRestore}>{t("sl.restore")}</Button>
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
  .strong {
    font-weight: var(--fw-medium);
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
  .domain {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-muted);
    font-size: var(--fs-sm);
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
  .row {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
  }
  .value {
    font-size: var(--fs-xl);
    font-weight: var(--fw-semibold);
    line-height: var(--lh-tight);
  }
  .delta {
    display: inline-flex;
    align-items: center;
    gap: 1px;
    font-size: var(--fs-sm);
    font-weight: var(--fw-medium);
    font-variant-numeric: tabular-nums;
  }
  .good {
    color: var(--good-text);
  }
  .bad {
    color: var(--bad-text);
  }
  .live {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    font-variant-numeric: tabular-nums;
  }
  .live.off {
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
