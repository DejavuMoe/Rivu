<!--
  站点看板：页头（站点、在线人数）→ 筛选栏 → 指标与趋势 → 来源、页面、地区、设备。
  状态都在 URL 查询串里，可分享、刷新不丢。
  mode="shared" 用于共享链接与公开看板：只读，没有任何管理入口。
  数据：指标条一个合计查询、主图一个时间序列查询、每张分解卡片查自己当前的标签页；
  页头的在线人数每 30 秒刷新，实时视图下其余数据也每 30 秒刷新。目标卡片在没有转化时不显示。
-->
<script lang="ts">
  import { ChartSpline, CircleAlert, Clock, Download, Hourglass, Inbox, Share2, Table2 } from "@lucide/svelte";
  import { page } from "$app/state";
  import SiteHeader from "../app/SiteHeader.svelte";
  import Callout from "../ui/Callout.svelte";
  import Card from "../ui/Card.svelte";
  import Button from "../ui/Button.svelte";
  import Segmented from "../ui/Segmented.svelte";
  import EmptyState from "../ui/EmptyState.svelte";
  import Skeleton from "../ui/Skeleton.svelte";
  import Table from "../ui/Table.svelte";
  import TrendChart from "../charts/TrendChart.svelte";
  import { t } from "../i18n";
  import { app, toast } from "../state.svelte";
  import { delta } from "../format";
  import { setQuery } from "../nav";
  import { api, type QueryResult } from "../api";
  import FilterBar from "./FilterBar.svelte";
  import KpiStrip from "./KpiStrip.svelte";
  import BreakdownCard from "./BreakdownCard.svelte";
  import GoalsCard from "./GoalsCard.svelte";
  import PropsCard from "./PropsCard.svelte";
  import {
    breakdownSpec,
    exportBody,
    goalsSpec,
    propertyKeysSpec,
    readGoals,
    CITY_LEVEL_DIMS,
    DIMS,
    encodeFilters,
    grans,
    METRICS,
    readRows,
    readSeries,
    readState,
    readTotals,
    seriesSpec,
    totalsSpec,
    type DashState,
    type Filter,
    type Gran,
    type Kpi,
    type MetricKey,
    type Series,
  } from "./spec";
  import { alignTo, axisLabels, tipLabels } from "./labels";
  import { fmtAxis, fmtMetric, lowerIsBetter } from "./metrics";
  import { failureOf, isAbort, runPropertyKeys, runQuery, runRealtime, type Failure, type Source } from "./source";

  interface Props {
    source: Source;
    name: string;
    mode?: "app" | "shared";
    /** 登录用户的站点 id（站内链接用） */
    siteId?: number;
    /** 能管理共享链接：页头出现「分享」入口（只在 app 模式） */
    canShare?: boolean;
    /** 能导出 CSV：页头出现「导出」入口（只在 app 模式） */
    canExport?: boolean;
  }
  let { source, name, mode = "app", siteId, canShare = false, canExport = false }: Props = $props();
  const shared = $derived(mode === "shared");

  const POLL_MS = 30_000;

  const dash = $derived(readState(page.url.searchParams));
  const view = $derived(page.url.searchParams.get("view") === "table" ? "table" : "chart");
  const realtime = $derived(dash.range === "realtime");
  const metric = $derived(dash.metric);
  /** 实时视图下递增，触发所有查询刷新 */
  let tick = $state(0);

  // ---------- 在线人数 ----------

  let live = $state<number | null>(null);
  $effect(() => {
    const src = source;
    let ctl = new AbortController();
    const poll = () => {
      ctl.abort();
      ctl = new AbortController();
      runRealtime(src, ctl.signal).then(
        (r) => (live = r.results[0]?.metrics[0] ?? 0),
        () => {
          /* 在线人数只是附带信息，失败时保留上一次的值 */
        },
      );
    };
    poll();
    const id = setInterval(poll, POLL_MS);
    return () => {
      clearInterval(id);
      ctl.abort();
    };
  });

  $effect(() => {
    if (!realtime) return;
    const id = setInterval(() => tick++, POLL_MS);
    return () => clearInterval(id);
  });

  // ---------- CSV 导出 ----------

  let exporting = $state(false);
  async function exportCsv() {
    if (exporting || siteId === undefined) return;
    exporting = true;
    try {
      const { blob, name: file } = await api.exportCsv(siteId, exportBody(dash));
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = file;
      a.click();
      URL.revokeObjectURL(url);
      toast(t("dash.exportedFile", { f: file }), "good");
    } catch {
      toast(t("dash.exportFailed"), "bad");
    } finally {
      exporting = false;
    }
  }

  // ---------- 指标条与主图 ----------

  let status = $state<"loading" | "ready" | Failure>("loading");
  let totals = $state<ReturnType<typeof readTotals> | null>(null);
  let series = $state<Series | null>(null);
  let importedExcluded = $state(false);
  /** 当前范围为空时，站点是否从来没有收到过数据（决定空状态的文案与动作） */
  let neverReceived = $state(false);
  let shownKey = "";

  function stateKey(s: DashState) {
    return JSON.stringify([s.range, s.compare, s.gran, s.filters]);
  }

  $effect(() => {
    const s = dash;
    const src = source;
    void tick;
    const key = stateKey(s);
    // 换了范围或筛选才显示骨架；实时刷新时保留旧数据
    if (key !== shownKey) {
      status = "loading";
      beyondRetention = false;
    }
    const ctl = new AbortController();
    const now = new Date();
    const metrics = s.range === "realtime" ? ["visitors", "pageviews"] : METRICS;
    Promise.all([runQuery(src, totalsSpec(s, now), ctl.signal), runQuery(src, seriesSpec(s, now), ctl.signal)])
      .then(async ([tot, ser]) => {
        const nextTotals = readTotals(tot, metrics);
        let never = false;
        if (s.range !== "realtime" && !s.filters.length && !nextTotals.values.visitors && !nextTotals.values.pageviews) {
          // 范围内没有数据：再看一眼全部时间，区分「还没接入」与「这段时间没有访问」
          const all = await runQuery(src, { metrics: ["pageviews"], date_range: "all" }, ctl.signal);
          never = !(all.results[0]?.metrics[0] ?? 0);
        }
        totals = nextTotals;
        series = readSeries(ser, metrics);
        if (pastRetention(tot) || pastRetention(ser)) beyondRetention = true;
        importedExcluded = [tot, ser].some((res) => res.meta.warnings?.some((w) => w.code === "imported_data_excluded"));
        neverReceived = never;
        shownKey = key;
        status = "ready";
      })
      .catch((e) => {
        if (isAbort(e)) return;
        shownKey = "";
        status = failureOf(e);
      });
    return () => ctl.abort();
  });

  const empty = $derived(status === "ready" && !realtime && !!totals && !totals.values.visitors && !totals.values.pageviews);

  const kpis = $derived.by<Kpi[]>(() => {
    if (realtime) {
      return [
        { key: "current", value: live ?? 0, prev: null },
        { key: "visitors", value: totals?.values.visitors ?? 0, prev: null },
        { key: "pageviews", value: totals?.values.pageviews ?? 0, prev: null },
      ];
    }
    return METRICS.map((k) => ({ key: k, value: totals?.values[k] ?? 0, prev: totals?.prev?.[k] ?? null }));
  });
  const totalVisitors = $derived(totals?.values.visitors ?? 0);

  const labels = $derived(series?.labels ?? []);
  const xLabels = $derived(axisLabels(labels, dash.gran, app.lang));
  const tips = $derived(tipLabels(labels, dash.gran, app.lang));
  const values = $derived(series?.values[metric] ?? []);
  const prevValues = $derived(series?.prevValues ? alignTo(series.prevValues[metric] ?? [], labels.length) : null);
  const prevTips = $derived(series?.prevLabels ? alignTo(tipLabels(series.prevLabels, dash.gran, app.lang), labels.length).map((l) => l ?? "") : []);
  const compareText = $derived(dash.compare === "year_over_year" ? t("compare.year_over_year") : t("compare.previous_period"));

  const failure = $derived(
    {
      error: { icon: CircleAlert, title: t("dash.error.title"), body: t("dash.error.body"), tone: "bad" as const },
      busy: { icon: Hourglass, title: t("dash.busy.title"), body: t("dash.busy.body"), tone: "warn" as const },
      timeout: { icon: Clock, title: t("dash.timeout.title"), body: t("dash.timeout.body"), tone: "warn" as const },
    }[status as Failure],
  );
  const pending = $derived(status !== "ready");

  // ---------- 分解卡片与筛选 ----------

  /** 地理库是否到城市级（只有国家级时不显示地区） */
  let geoCity = $state(false);
  let geoAttribution = $state<QueryResult["meta"]["geo_attribution"]>(null);

  /** 有一部分日子早于原始数据保留期、又不能由汇总层回答：换范围或筛选时重置，主查询与分解卡片的结果都可以置位 */
  let beyondRetention = $state(false);
  function pastRetention(res: QueryResult): boolean {
    return !!res.meta.warnings?.some((w) => w.code === "range_beyond_raw_retention");
  }

  function noteGeo(res: QueryResult) {
    if (pastRetention(res)) beyondRetention = true;
    if (res.meta.geo_capability) geoCity = res.meta.geo_capability === "city";
    if (res.meta.geo_attribution) geoAttribution = res.meta.geo_attribution;
  }

  const loadRows = $derived.by(() => {
    const s = dash;
    const src = source;
    void tick;
    return (dim: string, limit: number, signal: AbortSignal) =>
      runQuery(src, breakdownSpec(s, dim, limit, new Date()), signal).then((res) => {
        noteGeo(res);
        return readRows(res);
      });
  });

  /** 目标卡片的查询：看板状态变了换一个新函数，卡片随之重新查询 */
  const loadGoals = $derived.by(() => {
    const s = dash;
    const src = source;
    void tick;
    return (signal: AbortSignal) => runQuery(src, goalsSpec(s, new Date()), signal).then(readGoals);
  });

  /** 属性卡片的键列表：看板状态变了换一个新函数 */
  const loadPropertyKeys = $derived.by(() => {
    const s = dash;
    const src = source;
    void tick;
    return (signal: AbortSignal) => runPropertyKeys(src, propertyKeysSpec(s, new Date()), signal);
  });

  /** 筛选对话框里的候选值：该维度的排行，去掉这个维度自己的筛选 */
  function suggest(dim: string, signal: AbortSignal): Promise<string[]> {
    const s = { ...dash, filters: dash.filters.filter((f) => f.dim !== dim) };
    return runQuery(source, breakdownSpec(s, dim, 100, new Date()), signal).then((res) => readRows(res).map((r) => r.label));
  }

  const filterDims = $derived(Object.keys(DIMS).filter((d) => d !== "city" && (geoCity || !CITY_LEVEL_DIMS.has(d))));

  function addFilter(dim: string, value: string) {
    const rest = dash.filters.filter((f) => f.dim !== dim);
    setQuery({ f: encodeFilters([...rest, { dim, op: "is", values: [value] }]) });
  }
</script>

{#snippet attribution()}
  <a href={geoAttribution?.url} target="_blank" rel="noreferrer">{geoAttribution?.text}</a>
{/snippet}

<div class="page">
  <SiteHeader {siteId} {name} {live} {shared}>
    {#snippet actions()}
      {#if !shared && canExport}
        <Button icon={Download} loading={exporting} disabled={realtime || (status === "ready" && neverReceived)} onclick={exportCsv}
          >{t("dash.exportView")}</Button
        >
      {/if}
      {#if !shared && canShare}
        <Button icon={Share2} href="/s/{siteId}/settings/sharing">{t("dash.share")}</Button>
      {/if}
    {/snippet}
  </SiteHeader>

  <div class="toolbar">
    <FilterBar
      {dash}
      dims={filterDims}
      {suggest}
      onrange={(r) => setQuery({ range: r === "30d" ? null : r, gran: null })}
      oncompare={(c) => setQuery({ cmp: c === "previous_period" ? null : c })}
      onfilters={(fs: Filter[]) => setQuery({ f: fs.length ? encodeFilters(fs) : null })}
    />
  </div>

  {#if beyondRetention && !empty}
    <Callout tone="warn">{t("dash.retentionWarn")}</Callout>
  {/if}
  {#if status === "ready" && importedExcluded}
    <Callout tone="warn">{t("dash.importedExcludedWarn")}</Callout>
  {/if}

  {#if empty}
    <Card>
      <EmptyState
        icon={Inbox}
        title={neverReceived ? t("dash.waiting.title") : t("dash.empty.title")}
        body={!shared && neverReceived ? t("dash.empty.body") : undefined}
      >
        {#snippet actions()}
          {#if !shared && neverReceived}
            <Button variant="primary" href="/s/{siteId}/install">{t("dash.installGuide")}</Button>
          {:else if !neverReceived && dash.range !== "all"}
            <!-- 所选范围没有数据，但站点以前收到过 -->
            <Button onclick={() => setQuery({ range: "all", gran: null })}>{t("dash.viewAll")}</Button>
          {/if}
        {/snippet}
      </EmptyState>
    </Card>
  {:else}
    <section class="main" aria-label={t(`metric.${metric}`)}>
      <KpiStrip
        {kpis}
        selected={metric}
        onselect={(k: MetricKey) => setQuery({ metric: k === "visitors" ? null : k })}
        loading={pending}
      />
      <div class="chart-head">
        <div class="lead">
          <div class="legend">
            <span class="key" aria-hidden="true"></span>
            <span>{t(`metric.${metric}`)}</span>
            {#if prevValues}
              <span class="key compare" aria-hidden="true"></span>
              <span class="muted">{compareText}</span>
            {/if}
          </div>
          <!-- 访客、访问、浏览量容易混：选中哪个指标，就在图上方用一句话说明它怎么算 -->
          <p class="def">{t(`metric.${metric}.def`)}</p>
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
        {#if status === "loading"}
          <Skeleton height={248} radius={4} />
        {:else if failure}
          <EmptyState icon={failure.icon} tone={failure.tone} title={failure.title} body={failure.body}>
            {#snippet actions()}<Button size="sm" onclick={() => tick++}>{t("common.retry")}</Button>{/snippet}
          </EmptyState>
        {:else if view === "chart"}
          <TrendChart
            height={264}
            summary={`${t(`metric.${metric}`)}：${xLabels[0] ?? ""} – ${xLabels[xLabels.length - 1] ?? ""}`}
            {xLabels}
            tipLabels={tips}
            format={(n) => fmtAxis(metric, n, app.lang)}
            partialLast
            partialText={t("dash.partial")}
            deltaOf={(cur, prev) => delta(cur, prev, metric === "bounce_rate" ? app.lang : undefined)}
            lowerIsBetter={lowerIsBetter(metric)}
            series={[
              { key: "cur", label: t(`metric.${metric}`), values },
              ...(prevValues ? [{ key: "prev", label: compareText, values: prevValues, compare: true, labels: prevTips }] : []),
            ]}
          />
        {:else}
          {@const cols = (realtime ? ["visitors", "pageviews"] : ["visitors", "visits", "pageviews", "bounce_rate", "visit_duration"]) as MetricKey[]}
          <div class="tbl">
            <Table
              sticky
              caption={t(`metric.${metric}`)}
              rows={labels.map((_, i) => i)}
              rowKey={(i) => i}
              columns={[
                { key: "t", label: t("common.time") },
                ...cols.map((k) => ({
                  key: k,
                  label: t(`metric.${k}`),
                  align: "right" as const,
                  optional: k !== metric && k !== "visitors",
                })),
              ]}
            >
              {#snippet cell(i, c)}
                {#if c.key === "t"}{tips[i]}{:else}{fmtMetric(c.key as MetricKey, series?.values[c.key]?.[i] ?? 0, app.lang, "full")}{/if}
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
        load={loadRows}
        total={totalVisitors}
        onpick={addFilter}
      />
      <BreakdownCard
        title={t("dash.card.pages")}
        dims={["page", "entry_page", "exit_page"]}
        load={loadRows}
        total={totalVisitors}
        onpick={addFilter}
      />
      <BreakdownCard
        title={t("dash.card.locations")}
        dims={geoCity ? ["country", "region"] : ["country"]}
        load={loadRows}
        total={totalVisitors}
        onpick={addFilter}
        footer={geoAttribution ? attribution : undefined}
      />
      <BreakdownCard
        title={t("dash.card.devices")}
        dims={["device", "browser", "os"]}
        load={loadRows}
        total={totalVisitors}
        onpick={addFilter}
      />
    </div>

    <div class="grid">
      <GoalsCard load={loadGoals} onpick={addFilter} />
      <PropsCard loadKeys={loadPropertyKeys} load={loadRows} total={totalVisitors} onpick={addFilter} />
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
    /* 与右侧工具（28px 高）同一行居中 */
    min-height: var(--control-sm);
    font-weight: var(--fw-medium);
  }
  .def {
    color: var(--text-muted);
    font-size: var(--fs-sm);
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
    /* 补空行只为并排的卡片等高，单列时不需要 */
    .grid :global(.row.filler) {
      display: none;
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
