<!--
  站点看板：页头（站点、在线人数）→ 筛选栏 → 指标与趋势 → 来源、页面、地区、设备 → 目标、属性。
  状态都在 URL 查询串里，可分享、刷新不丢。
  mode="shared" 用于共享链接与公开看板：只读，没有任何管理入口。
-->
<script lang="ts">
  import { ChartSpline, CircleAlert, Clock, Download, Hourglass, Inbox, Share2, Table2 } from "@lucide/svelte";
  import SiteHeader from "../../lib/app/SiteHeader.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Segmented from "../../lib/ui/Segmented.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import TrendChart from "../../lib/charts/TrendChart.svelte";
  import { t } from "../../lib/i18n";
  import { app, canEdit, toast } from "../../lib/state.svelte";
  import { delta } from "../../lib/format";
  import { href, route, setQuery } from "../../lib/router.svelte";
  import { propertyKeys, type Site } from "../../lib/data/fixtures";
  import FilterBar from "./FilterBar.svelte";
  import KpiStrip from "./KpiStrip.svelte";
  import BreakdownCard from "./BreakdownCard.svelte";
  import PropsCard from "./PropsCard.svelte";
  import { encodeFilters, grans, readState, runQuery, type Filter, type Gran, type MetricKey } from "./query";
  import { fmtAxis, fmtMetric, lowerIsBetter } from "./metrics";

  let { site, mode = "app" }: { site: Site; mode?: "app" | "shared" } = $props();
  const shared = $derived(mode === "shared");

  const dash = $derived(readState(route.current.query));
  const view = $derived(route.current.query.get("view") === "table" ? "table" : "chart");
  const ds = $derived(app.review.dataState);
  const noData = $derived(site.scale === 0 || ds === "empty");
  const loading = $derived(ds === "loading");
  const failed = $derived(ds === "error" || ds === "busy" || ds === "timeout");

  const res = $derived(runQuery(site, dash, app.lang));
  const metric = $derived(dash.metric);
  const values = $derived(res.points.map((p) => p[metric] as number));
  const prevValues = $derived(res.prevPoints?.map((p) => p[metric] as number) ?? null);
  const compareText = $derived(
    dash.compare === "year_over_year" ? t("compare.year_over_year") : t("compare.previous_period"),
  );

  function addFilter(dim: string, value: string) {
    const rest = dash.filters.filter((f) => f.dim !== dim);
    setQuery({ f: encodeFilters([...rest, { dim, op: "is", values: [value] }]) });
  }

  const deviceName = (dim: string, label: string) => (dim === "device" ? t(`device.${label}`) : label);

  const failure = $derived(
    {
      error: { icon: CircleAlert, title: t("dash.error.title"), body: t("dash.error.body"), tone: "bad" as const },
      busy: { icon: Hourglass, title: t("dash.busy.title"), body: t("dash.busy.body"), tone: "warn" as const },
      timeout: { icon: Clock, title: t("dash.timeout.title"), body: t("dash.timeout.body"), tone: "warn" as const },
    }[ds as "error" | "busy" | "timeout"],
  );
  const pending = $derived(loading || failed);

  // 导出当前视图（时间范围与筛选）为 CSV 压缩包；实时不能导出。原型里只模拟生成的耗时
  let exporting = $state(false);
  function exportCsv() {
    exporting = true;
    setTimeout(() => {
      exporting = false;
      const day = (d: Date | undefined) => (d ? d.toISOString().slice(0, 10) : "");
      const span = `${day(res.points[0]?.t)}-${day(res.points.at(-1)?.t)}`;
      toast(t("dash.exportedFile", { f: `rivu-${site.domain}-${span}.zip` }), "good");
    }, 900);
  }
</script>

<div class="page">
<SiteHeader {site} {shared}>
    {#snippet actions()}
      {#if !shared && canEdit()}
        <Button icon={Download} loading={exporting} disabled={noData || dash.range === "realtime"} onclick={exportCsv}>{t("dash.exportView")}</Button>
        <Button icon={Share2} href={href(`/s/${site.id}/settings/sharing`)}>{t("dash.share")}</Button>
      {/if}
    {/snippet}
  </SiteHeader>

  <div class="toolbar">
    <FilterBar
      {dash}
      onrange={(r) => setQuery({ range: r === "30d" ? null : r, gran: null })}
      oncompare={(c) => setQuery({ cmp: c === "previous_period" ? null : c })}
      onfilters={(fs: Filter[]) => setQuery({ f: fs.length ? encodeFilters(fs) : null })}
    />
  </div>

  {#if noData}
    <Card>
      <EmptyState
        icon={Inbox}
        title={site.scale === 0 ? t("dash.waiting.title") : t("dash.empty.title")}
        body={!shared && site.scale === 0 ? t("dash.empty.body") : undefined}
      >
        {#snippet actions()}
          {#if !shared && site.scale === 0}
            <Button variant="primary" href={href(`/s/${site.id}/install`)}>{t("dash.installGuide")}</Button>
          {:else if !shared}
            <Button href={href(`/s/${site.id}/diagnostics`)}>{t("dash.empty.action")}</Button>
          {/if}
        {/snippet}
      </EmptyState>
    </Card>
  {:else}
    <section class="main" aria-label={t(`metric.${metric}`)}>
      <KpiStrip
        kpis={res.kpis}
        selected={metric}
        onselect={(k: MetricKey) => setQuery({ metric: k === "visitors" ? null : k })}
        loading={pending}
      />
      <div class="chart-head">
        <div class="legend">
          <span class="key" aria-hidden="true"></span>
          <span>{t(`metric.${metric}`)}</span>
          {#if prevValues}
            <span class="key compare" aria-hidden="true"></span>
            <span class="muted">{compareText}</span>
          {/if}
        </div>
        <div class="tools">
          {#if grans(dash.range).length > 1}
            <Segmented
              size="sm"
              label={t("gran.label")}
              value={dash.gran}
              onchange={(g: Gran) => setQuery({ gran: g === grans(dash.range)[0] ? null : g })}
              options={grans(dash.range).map((g) => ({ value: g, label: t(`gran.${g}`) }))}
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
            summary={`${t(`metric.${metric}`)}：${res.xLabels[0]} – ${res.xLabels[res.xLabels.length - 1]}`}
            xLabels={res.xLabels}
            tipLabels={res.tipLabels}
            format={(n) => fmtAxis(metric, n, app.lang)}
            partialLast={res.partialLast}
            partialText={t("dash.partial")}
            deltaOf={delta}
            lowerIsBetter={lowerIsBetter(metric)}
            series={[
              { key: "cur", label: t(`metric.${metric}`), values },
              ...(prevValues ? [{ key: "prev", label: compareText, values: prevValues, compare: true, labels: res.prevTipLabels }] : []),
            ]}
          />
        {:else}
          <div class="tbl">
            <Table
              sticky
              caption={t(`metric.${metric}`)}
              rows={res.points.map((p, i) => ({ i, p }))}
              rowKey={(r) => r.i}
              columns={[
                { key: "t", label: t("common.time") },
                ...(["visitors", "visits", "pageviews", "bounce_rate", "visit_duration"] as MetricKey[]).map((k) => ({
                  key: k,
                  label: t(`metric.${k}`),
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

    <div class="grid">
      <BreakdownCard
        title={t("dash.card.sources")}
        dims={["channel", "source", "referrer", "utm_campaign"]}
        rowsOf={res.breakdown}
        total={res.totalVisitors}
        onpick={addFilter}
        loading={pending}
      />
      <BreakdownCard
        title={t("dash.card.pages")}
        dims={["page", "entry_page", "exit_page"]}
        rowsOf={res.breakdown}
        total={res.totalVisitors}
        onpick={addFilter}
        loading={pending}
      />
      <BreakdownCard
        title={t("dash.card.locations")}
        dims={["country", "region", "city"]}
        rowsOf={res.breakdown}
        total={res.totalVisitors}
        onpick={addFilter}
        loading={pending}
      >
        {#snippet footer()}
          <a href="https://db-ip.com" target="_blank" rel="noreferrer">{t("dash.geoAttribution")}</a>
        {/snippet}
      </BreakdownCard>
      <BreakdownCard
        title={t("dash.card.devices")}
        dims={["device", "browser", "os"]}
        rowsOf={res.breakdown}
        total={res.totalVisitors}
        onpick={addFilter}
        loading={pending}
        display={deviceName}
      />
    </div>

    <div class="grid">
      <BreakdownCard
        title={t("dim.goal")}
        dims={["goal"]}
        minRows={0}
        rowsOf={res.breakdown}
        total={res.totalVisitors}
        onpick={addFilter}
        loading={pending}
      />
      <PropsCard keys={propertyKeys(site)} minRows={0} rowsOf={res.breakdown} total={res.totalVisitors} onpick={addFilter} loading={pending} />
    </div>
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
    position: sticky;
    top: 0;
    z-index: 20;
    margin: 0 calc(-1 * var(--sp-8));
    padding: var(--sp-2) var(--sp-8);
    background: var(--bg-app);
  }
  .main {
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-md);
    overflow: hidden;
  }
  .chart-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    flex-wrap: wrap;
    padding: var(--sp-4) var(--sp-4) 0 var(--sp-5);
  }
  .legend {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    font-weight: var(--fw-medium);
  }
  .legend .muted {
    font-weight: var(--fw-regular);
  }
  .key {
    width: 12px;
    height: 2px;
    border-radius: 1px;
    background: var(--series-1);
  }
  .key.compare {
    margin-left: var(--sp-2);
    background: var(--series-compare);
  }
  .tools {
    display: flex;
    gap: var(--sp-2);
  }
  /* 纵轴文字在 48px 的左侧留白里，图表整体左移让刻度文字与上方图例对齐 */
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
  .grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--sp-4);
  }
  @media (max-width: 960px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
  @media (max-width: 720px) {
    .page {
      padding: var(--sp-4) var(--sp-4) var(--sp-10);
    }
    .toolbar {
      margin: 0 calc(-1 * var(--sp-4));
      padding: var(--sp-2) var(--sp-4);
    }
  }
</style>
