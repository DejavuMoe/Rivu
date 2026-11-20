<!--
  合并看板（/sites/combined）：把当前组织里你能读的所有站点加在一起看。只在能读的站点有两个及以上时出现（否则 404），
  共享链接与公开看板没有它。没有筛选、没有对比、没有实时：时间范围 → 指标条与趋势（图 / 表）→ 各站点的明细与访客占比。
  访客是各站点访客数之和（Rivu 不跨站点识别访客），指标名和说明里都这样写。
  状态由评审面板的数据状态驱动：加载、出错 / 排队 / 超时、无数据（所选范围内所有站点都没有数据）。
-->
<script lang="ts">
  import { CalendarRange, ChartSpline, CircleAlert, Clock, Hourglass, Inbox, Table2 } from "@lucide/svelte";
  import Avatar from "../../lib/ui/Avatar.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import MenuItem from "../../lib/ui/MenuItem.svelte";
  import MenuLabel from "../../lib/ui/MenuLabel.svelte";
  import Popover from "../../lib/ui/Popover.svelte";
  import Segmented from "../../lib/ui/Segmented.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import TrendChart from "../../lib/charts/TrendChart.svelte";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import { percent } from "../../lib/format";
  import { href, navigate, route, setQuery } from "../../lib/router.svelte";
  import KpiStrip from "../dashboard/KpiStrip.svelte";
  import { grans, type Gran, type MetricKey } from "../dashboard/query";
  import { fmtAxis, fmtMetric } from "../dashboard/metrics";
  import SitesHeader from "./SitesHeader.svelte";
  import { COMBINED_RANGES, readCombined, readableSites, runCombined } from "./combined";
  import "./strings";

  let { orgId }: { orgId: string } = $props();

  const sites = $derived(readableSites(orgId));
  const st = $derived(readCombined(route.current.query));
  const view = $derived(route.current.query.get("view") === "table" ? "table" : "chart");

  const ds = $derived(app.review.dataState);
  const loading = $derived(ds === "loading");
  const failed = $derived(ds === "error" || ds === "busy" || ds === "timeout");
  const res = $derived(runCombined(sites, st, app.lang, ds === "empty"));
  const empty = $derived(!loading && !failed && res.visitorsSum === 0);
  const pending = $derived(loading || failed);

  const metric = $derived(st.metric);
  const values = $derived(res.points.map((p) => p[metric] as number));
  const labelOf = (k: MetricKey) => (k === "visitors" ? t("cv.visitorsSum") : t(`metric.${k}`));
  const defOf = (k: MetricKey) => (k === "visitors" ? t("cv.visitors.def") : t(`metric.${k}.def`));

  /** 带上当前范围去单个站点的看板 */
  const siteHref = (id: number) => href(`/s/${id}${st.range === "30d" ? "" : `?range=${st.range}`}`);

  const failure = $derived(
    {
      error: { icon: CircleAlert, title: t("dash.error.title"), body: t("dash.error.body"), tone: "bad" as const },
      busy: { icon: Hourglass, title: t("dash.busy.title"), body: t("dash.busy.body"), tone: "warn" as const },
      timeout: { icon: Clock, title: t("dash.timeout.title"), body: t("dash.timeout.body"), tone: "warn" as const },
    }[ds as "error" | "busy" | "timeout"],
  );
</script>

<div class="page">
  <SitesHeader {orgId} tabbed />

  <div class="toolbar">
    <Popover width={200} label={t("range.label")}>
      {#snippet trigger({ toggle, props })}
        <button type="button" class="ctl" onclick={toggle} {...props}>
          <CalendarRange size={16} strokeWidth={1.75} />
          <span>{t(`range.${st.range}`)}</span>
        </button>
      {/snippet}
      {#snippet children({ close })}
        {#each COMBINED_RANGES as group, gi (gi)}
          {#if gi > 0}<MenuLabel separator />{/if}
          {#each group as r (r)}
            <MenuItem
              checked={st.range === r}
              onclick={() => {
                setQuery({ range: r === "30d" ? null : r, gran: null });
                close();
              }}>{t(`range.${r}`)}</MenuItem
            >
          {/each}
        {/each}
      {/snippet}
    </Popover>
  </div>

  {#if empty}
    <Card>
      <EmptyState icon={Inbox} title={t("dash.empty.title")}>
        {#snippet actions()}
          {#if st.range !== "all" && ds !== "empty"}
            <Button onclick={() => setQuery({ range: "all", gran: null })}>{t("dash.viewAll")}</Button>
          {/if}
        {/snippet}
      </EmptyState>
    </Card>
  {:else}
    <section class="main" aria-label={labelOf(metric)}>
      <KpiStrip
        kpis={res.kpis}
        selected={metric}
        onselect={(k: MetricKey) => setQuery({ metric: k === "visitors" ? null : k })}
        loading={pending}
        {labelOf}
        {defOf}
      />
      <div class="chart-head">
        <div class="lead">
          <div class="legend">
            <span class="key" aria-hidden="true"></span>
            <span>{labelOf(metric)}</span>
          </div>
          <p class="def">{defOf(metric)}</p>
        </div>
        <div class="tools">
          {#if grans(st.range).length > 1}
            <Segmented
              size="sm"
              label={t("gran.label")}
              value={st.gran}
              onchange={(g: Gran) => setQuery({ gran: g === grans(st.range)[0] ? null : g })}
              options={grans(st.range).map((g) => ({ value: g, label: t(`gran.${g}`) }))}
            />
          {/if}
          <Segmented
            size="sm"
            label={t("common.view")}
            value={view}
            onchange={(v) => setQuery({ view: v === "chart" ? null : v })}
            options={[
              { value: "chart", label: "", icon: ChartSpline, hint: t("common.chart") },
              { value: "table", label: "", icon: Table2, hint: t("common.table") },
            ]}
          />
        </div>
      </div>
      <div class="chart-body">
        {#if loading}
          <Skeleton height={248} radius={4} />
        {:else if failed && failure}
          <EmptyState icon={failure.icon} tone={failure.tone} title={failure.title} body={failure.body}>
            {#snippet actions()}<Button size="sm" onclick={() => toast(t("common.retry"))}>{t("common.retry")}</Button>{/snippet}
          </EmptyState>
        {:else if view === "chart"}
          <TrendChart
            height={264}
            summary={`${labelOf(metric)}：${res.xLabels[0]} – ${res.xLabels[res.xLabels.length - 1]}`}
            xLabels={res.xLabels}
            tipLabels={res.tipLabels}
            format={(n) => fmtAxis(metric, n, app.lang)}
            partialLast={res.partialLast}
            partialText={t("dash.partial")}
            series={[{ key: "cur", label: labelOf(metric), values }]}
          />
        {:else}
          <div class="tbl">
            <Table
              sticky
              caption={labelOf(metric)}
              rows={res.points.map((p, i) => ({ i, p }))}
              rowKey={(r) => r.i}
              columns={[
                { key: "t", label: t("common.time") },
                ...(["visitors", "visits", "pageviews", "bounce_rate", "visit_duration"] as MetricKey[]).map((k) => ({
                  key: k,
                  label: labelOf(k),
                  align: "right" as const,
                  optional: k !== metric && k !== "visitors",
                })),
              ]}
            >
              {#snippet cell(r, c)}
                {#if c.key === "t"}{res.tipLabels[r.i]}{:else}{fmtMetric(c.key as MetricKey, r.p[c.key as MetricKey] as number, app.lang, "full")}{/if}
              {/snippet}
            </Table>
          </div>
        {/if}
      </div>
    </section>

    <Card title={t("cv.bySite")} flush>
      {#if loading}
        <div class="sk" aria-busy="true">
          {#each sites as s (s.id)}<Skeleton height={24} radius={4} />{/each}
        </div>
      {:else if failed && failure}
        <EmptyState compact icon={failure.icon} tone={failure.tone} title={failure.title} />
      {:else}
        <Table
          caption={t("cv.bySite")}
          rows={res.rows}
          rowKey={(r) => r.site.id}
          onrowclick={(r) => navigate(`/s/${r.site.id}${st.range === "30d" ? "" : `?range=${st.range}`}`)}
          columns={[
            { key: "site", label: t("cv.col.site") },
            { key: "visitors", label: t("metric.visitors"), align: "right", width: "96px" },
            { key: "share", label: t("cv.col.share"), width: "168px" },
            { key: "visits", label: t("metric.visits"), align: "right", width: "96px", optional: true },
            { key: "pageviews", label: t("metric.pageviews"), align: "right", width: "96px", optional: true },
            { key: "bounce_rate", label: t("metric.bounce_rate"), align: "right", width: "88px", optional: true },
            { key: "visit_duration", label: t("metric.visit_duration"), align: "right", width: "104px", optional: true },
          ]}
        >
          {#snippet cell(r, c)}
            {#if c.key === "site"}
              <a class="site" href={siteHref(r.site.id)} onclick={(e) => e.stopPropagation()}>
                <Avatar name={r.site.name} shape="square" size={16} />
                <span class="name">{r.site.name}</span>
              </a>
            {:else if c.key === "share"}
              <span class="share">
                <span class="bar" aria-hidden="true"><span style:width="{Math.max(r.share * 100, r.share > 0 ? 1 : 0)}%"></span></span>
                <span class="pct">{percent(r.share)}</span>
              </span>
            {:else if r.visits === 0 && c.key !== "visitors"}
              <span class="muted">—</span>
            {:else}
              <span class="num">{fmtMetric(c.key as MetricKey, (r as unknown as Record<string, number>)[c.key], app.lang, "full")}</span>
            {/if}
          {/snippet}
        </Table>
      {/if}
    </Card>
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
  .toolbar {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .ctl {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: var(--control-md);
    padding: 0 12px 0 10px;
    border: var(--hairline) solid var(--border);
    border-radius: var(--r-sm);
    background: var(--bg-app);
    font-weight: var(--fw-medium);
  }
  .ctl :global(svg) {
    color: var(--text-muted);
  }
  .ctl:hover,
  .ctl[aria-expanded="true"] {
    background: var(--bg-element);
    border-color: var(--border-hover);
  }
  .main {
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-md);
    overflow: hidden;
  }
  .chart-head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--sp-3);
    flex-wrap: wrap;
    padding: var(--sp-4) var(--sp-4) 0 var(--sp-5);
  }
  .lead {
    display: flex;
    flex: 1 1 240px;
    flex-direction: column;
    gap: var(--sp-1);
    min-width: 0;
  }
  .legend {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    min-height: var(--control-sm);
    font-weight: var(--fw-medium);
  }
  .def {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .key {
    width: 12px;
    height: 2px;
    border-radius: 1px;
    background: var(--series-1);
  }
  .tools {
    display: flex;
    gap: var(--sp-2);
  }
  .chart-body {
    min-height: 288px;
    padding: var(--sp-3) var(--sp-4) var(--sp-3) var(--sp-2);
  }
  .tbl {
    max-height: 288px;
    overflow: auto;
    margin-left: var(--sp-3);
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-sm);
  }
  .sk {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    padding: var(--sp-4) var(--sp-5);
  }
  .site {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    max-width: 100%;
    min-width: 0;
    font-weight: var(--fw-medium);
  }
  .site:hover .name {
    text-decoration: underline;
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .share {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .bar {
    flex: 1;
    height: 6px;
    border-radius: var(--r-full);
    overflow: hidden;
  }
  /* 与条形列表一样用中性色：份额是长短，不是强调 */
  .bar span {
    display: block;
    height: 100%;
    border-radius: var(--r-full);
    background: var(--chart-bar);
  }
  .pct {
    width: 44px;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .num {
    font-variant-numeric: tabular-nums;
  }
  .muted {
    color: var(--text-muted);
  }
  @media (max-width: 720px) {
    .page {
      padding: var(--sp-4) var(--sp-4) var(--sp-10);
    }
  }
</style>
