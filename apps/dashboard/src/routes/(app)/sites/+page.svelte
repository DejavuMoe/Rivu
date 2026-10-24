<!--
  站点列表：当前组织的站点网格。卡片整体可点进看板；24 小时内没有数据的站点链到安装页。
  每张卡片各查两次：按小时的访客走势，以及 24 小时去重访客总数（分小时的访客数不能相加）。
  卡片进入视口才查询，同时最多 4 张卡片在查（站点多时不一次把查询并发打满）。
  站点多于 6 个时才出现搜索框。owner / admin 在页头和空状态里有「添加站点」入口。
-->
<script lang="ts">
  import { Inbox, Plus, Search, X } from "@lucide/svelte";
  import PageHeader from "#lib/app/PageHeader.svelte";
  import Sparkline from "#lib/charts/Sparkline.svelte";
  import Avatar from "#lib/ui/Avatar.svelte";
  import Badge from "#lib/ui/Badge.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Card from "#lib/ui/Card.svelte";
  import EmptyState from "#lib/ui/EmptyState.svelte";
  import Input from "#lib/ui/Input.svelte";
  import LiveDot from "#lib/ui/LiveDot.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import { api } from "#lib/api.ts";
  import { compact } from "#lib/format.ts";
  import { t } from "#lib/i18n.ts";
  import { canAddSite, orgSites } from "#lib/session.svelte.ts";
  import { app } from "#lib/state.svelte.ts";
  import { inview, limiter } from "#lib/lazy.ts";
  import "./strings";

  type Stat = { values: number[]; total: number } | "error";

  let q = $state("");
  let stats = $state<Record<number, Stat>>({});

  const all = $derived(orgSites());
  const shown = $derived.by(() => {
    const k = q.trim().toLowerCase();
    return k ? all.filter((s) => s.name.toLowerCase().includes(k)) : all;
  });
  const key = $derived(all.map((s) => s.id).join(","));

  async function load(id: number, signal: AbortSignal): Promise<Stat> {
    try {
      const [series, total] = await Promise.all([
        api.query(id, { metrics: ["visitors"], date_range: "24h", dimensions: ["time:hour"], include: { time_labels: true } }, { signal }),
        api.query(id, { metrics: ["visitors"], date_range: "24h" }, { signal }),
      ]);
      const byLabel = new Map(series.results.map((r) => [r.dimensions[0], r.metrics[0]]));
      const labels = series.meta.time_labels ?? series.results.map((r) => r.dimensions[0]);
      return { values: labels.map((l) => byLabel.get(l) ?? 0), total: total.results[0]?.metrics[0] ?? 0 };
    } catch {
      return "error";
    }
  }

  const run = limiter(4);
  let ctl = new AbortController();

  // 组织（站点集合）变了就清空重来；离开页面或切换组织时取消在途与排队的请求
  $effect(() => {
    void key;
    ctl = new AbortController();
    stats = {};
    const mine = ctl;
    return () => mine.abort();
  });

  function visible(id: number) {
    const signal = ctl.signal;
    run(() => load(id, signal), signal).then(
      (v) => {
        if (!signal.aborted) stats[id] = v;
      },
      () => {
        /* 取消 */
      },
    );
  }
</script>

<div class="page">
  <PageHeader title={t("sites.title")}>
    {#snippet actions()}
      {#if canAddSite()}
        <Button variant="primary" icon={Plus} href="/sites/new">{t("nav.addSite")}</Button>
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
      <EmptyState icon={Inbox} title={t("sites.empty.title")} body={canAddSite() ? undefined : t("sl.empty.viewer")}>
        {#snippet actions()}
          {#if canAddSite()}<Button variant="primary" icon={Plus} href="/sites/new">{t("nav.addSite")}</Button>{/if}
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
        {@const st = stats[s.id]}
        <li class="site" use:inview={() => visible(s.id)}>
          <div class="head">
            <Avatar name={s.name} shape="square" size={32} />
            <div class="names">
              <a class="name" href="/s/{s.id}" aria-label={t("sl.openSite", { name: s.name })}>{s.name}</a>
            </div>
            {#if s.is_public || s.access === "guest"}
              <div class="badges">
                {#if s.is_public}<Badge>{t("sites.public")}</Badge>{/if}
                {#if s.access === "guest"}<Badge>{t("sites.guest")}</Badge>{/if}
              </div>
            {/if}
          </div>

          {#if st === undefined}
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
              {#if s.access === "member"}
                <a class="text-link" href="/s/{s.id}/install">{t("sl.install")}</a>
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
</div>

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
  }
  .site:hover {
    background: var(--bg-subtle);
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
