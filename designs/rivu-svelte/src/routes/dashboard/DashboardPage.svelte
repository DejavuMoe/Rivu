<!--
  站点看板：页头（站点、在线人数）→ 筛选栏 → 指标与趋势 → 来源、页面、地区、设备 → 目标、属性。
  状态都在 URL 查询串里，可分享、刷新不丢。
  mode="shared" 用于共享链接与公开看板：只读，没有任何管理入口，也没有分群。
  图表批注：登录后的看板、非实时范围在趋势图下方显示批注标记；editor 起可以添加、修改、删除，表格视图多一列「批注」。
  分群：URL 的 seg=<id> 引用站点的一个分群，它的条件和 f= 里的临时筛选同时生效；分群已被删除时提示一句，按没有分群查询。
-->
<script lang="ts">
  import { ChartSpline, CircleAlert, Clock, Download, Hourglass, Inbox, Info, MessageSquarePlus, Share2, Table2 } from "@lucide/svelte";
  import Popover from "../../lib/ui/Popover.svelte";
  import SiteHeader from "../../lib/app/SiteHeader.svelte";
  import Callout from "../../lib/ui/Callout.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Segmented from "../../lib/ui/Segmented.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import TrendChart from "../../lib/charts/TrendChart.svelte";
  import { t } from "../../lib/i18n";
  import { app, canEdit, siteAccess, toast } from "../../lib/state.svelte";
  import { delta, duration, percent } from "../../lib/format";
  import { href, route, setQuery } from "../../lib/router.svelte";
  import { propertyKeys, type Site } from "../../lib/data/fixtures";
  import FilterBar from "./FilterBar.svelte";
  import KpiStrip from "./KpiStrip.svelte";
  import BreakdownCard from "./BreakdownCard.svelte";
  import GoalsCard from "./GoalsCard.svelte";
  import PropsCard from "./PropsCard.svelte";
  import SegmentMenu from "./SegmentMenu.svelte";
  import { findSegment, type Segment } from "./segments.svelte";
  import "./segment-strings";
  import NotesDialog from "./NotesDialog.svelte";
  import { bucketIndex, notesOf, type Note } from "./notes.svelte";
  import { dateShort } from "../../lib/format";
  import type { ChartMark } from "../../lib/charts/types";
  import "./note-strings";
  import {
    DIMS,
    encodeFilters,
    firstDay,
    geoAttribution,
    geoCountryOnly,
    geoLevels,
    pageEngagement,
    grans,
    readState,
    runQuery,
    suggestValues,
    type Filter,
    type Gran,
    type MetricKey,
  } from "./query";
  import { fmtAxis, fmtMetric, lowerIsBetter } from "./metrics";
  import type { Details } from "./BreakdownCard.svelte";
  import { NOW } from "../../lib/data/fixtures";
  import { periodText, rangeQuery, ymd, type RangeParam } from "./period";
  import { rangeUi } from "./range";

  let { site, mode = "app" }: { site: Site; mode?: "app" | "shared" } = $props();
  const shared = $derived(mode === "shared");
  /** 安装文档里地理库一节（链接，不加载任何资源） */
  const GEO_DOCS = "https://github.com/DejavuMoe/Rivu/blob/master/docs/install.md#geoip-%E5%9C%B0%E7%90%86%E5%BA%93";

  const dash = $derived(readState(route.current.query));
  /** URL 里的分群 id；分群只在登录后的看板里生效 */
  const segId = $derived.by(() => {
    const raw = route.current.query.get("seg");
    return shared || !raw || !/^\d+$/.test(raw) ? null : Number(raw);
  });
  const activeSeg = $derived(findSegment(site.id, segId));
  const segMissing = $derived(segId !== null && !activeSeg);
  /** 查询用的状态：分群的条件在前，临时筛选叠加在后 */
  const query = $derived(activeSeg ? { ...dash, filters: [...activeSeg.filters, ...dash.filters] } : dash);
  const view = $derived(route.current.query.get("view") === "table" ? "table" : "chart");
  const realtime = $derived(dash.range === "realtime");
  const ds = $derived(app.review.dataState);
  const loading = $derived(ds === "loading");
  const failed = $derived(ds === "error" || ds === "busy" || ds === "timeout");
  /** 数据到齐了：提示条、地理库署名、目标与属性卡片都要等到这时才出现 */
  const settled = $derived(!loading && !failed);
  /** 范围内没有数据。实时视图没有空状态，数字都是 0 */
  const noData = $derived(site.scale === 0 || ds === "empty");
  const empty = $derived(settled && noData && !realtime);
  /** 站点从来没有收到过数据（决定空状态的文案与动作）；带筛选的查询不做这个判断 */
  const neverReceived = $derived(site.scale === 0 && !query.filters.length);
  /** 页头的在线人数：加载中、出错时还没取到，不显示；范围里没有数据时是 0；其余取站点的模拟值 */
  const live = $derived(!settled ? null : noData ? 0 : undefined);

  const res = $derived(runQuery(site, query, app.lang, noData && realtime));
  const metric = $derived(dash.metric);
  const values = $derived(res.points.map((p) => p[metric] as number));
  const prevValues = $derived(res.prevPoints?.map((p) => p[metric] as number) ?? null);
  const compareText = $derived(
    dash.compare === "year_over_year" ? t("compare.year_over_year") : t("compare.previous_period"),
  );

  // ---------- 图表批注 ----------
  /** 批注在这个视图里显示：登录后的看板、不是实时 */
  const notesOn = $derived(!shared && !realtime);
  const mayNote = $derived(notesOn && canEdit(site));
  /** 每个时间桶里的批注 */
  const noteBuckets = $derived.by(() => {
    const m = new Map<number, Note[]>();
    if (!notesOn) return m;
    for (const n of notesOf(site.id)) {
      const i = bucketIndex(res.points, dash.gran, n.date);
      if (i >= 0) m.set(i, [...(m.get(i) ?? []), n]);
    }
    return m;
  });
  /** 一个桶跨好几天（按周、按月）时，提示里的每条批注前面写上日期 */
  const multiDay = $derived(dash.gran === "week" || dash.gran === "month");
  const noteLine = (n: Note) => (multiDay ? `${dateShort(new Date(`${n.date}T00:00:00`), app.lang)} ${n.text}` : n.text);
  const marks = $derived.by<ChartMark[] | undefined>(() => {
    if (!notesOn) return undefined;
    return [...noteBuckets].map(([index, ns]) => ({
      index,
      count: ns.length,
      label: ns.length === 1 ? t("note.markOne", { date: res.tipLabels[index] }) : t("note.mark", { date: res.tipLabels[index], n: ns.length }),
      lines: [...ns.slice(0, 3).map(noteLine), ...(ns.length > 3 ? [t("note.more", { n: ns.length - 3 })] : [])],
    }));
  });

  let notesOpen = $state(false);
  let notesMode = $state<"list" | "add">("list");
  let notesList = $state<Note[]>([]);
  let notesHeading = $state("");
  let notesDated = $state(false);
  function openMark(indices: number[]) {
    notesList = indices.flatMap((i) => noteBuckets.get(i) ?? []);
    notesHeading = indices.length === 1 ? res.tipLabels[indices[0]] : `${res.tipLabels[indices[0]]} – ${res.tipLabels[indices[indices.length - 1]]}`;
    notesDated = multiDay || indices.length > 1;
    notesMode = "list";
    notesOpen = true;
  }
  function openAddNote() {
    notesMode = "add";
    notesOpen = true;
  }

  // 范围：按钮文字、上一周期 / 下一周期、自定义对话框的初始值。图例里写出主区间和对比期的具体日期，
  // 让人知道「过去 30 天」「对比上一周期」到底是哪几天；导出的文件名用同样的日期
  const first = $derived(firstDay(site));
  const periodLabel = $derived(realtime ? "" : periodText(res.period, app.lang, NOW));
  const compareLabel = $derived(res.comparePeriod ? periodText(res.comparePeriod, app.lang, NOW) : "");
  const ru = $derived(rangeUi(dash, res.period, first, site.timezone, app.lang));
  function setRange(r: RangeParam) {
    // 换范围时粒度回到默认（不同范围允许的粒度不同）
    setQuery({ ...rangeQuery(r), gran: null });
  }

  /** 导出：有效角色 editor 起（含站点访客 editor）；分享：组织成员里的 editor 起，站点访客不行 */
  const mayExport = $derived(canEdit(site));
  const mayShare = $derived(canEdit(site) && siteAccess(site).access === "member");

  /**
   * 位置卡片的级别：城市级地理库下有国家、地区、城市三个标签页，筛选里也有地区和城市；
   * 只有国家级地理库（默认的 DB-IP Lite）时只有国家，卡片底部一个「为什么只有国家」说明原因和要配置什么。
   */
  const geoDims = $derived(geoLevels(site));
  const countryOnly = $derived(geoCountryOnly(site));
  const attr = $derived(geoAttribution(site));
  const filterDims = $derived(DIMS.filter((d) => (d !== "region" && d !== "city") || geoDims.includes(d as "region" | "city")));

  // 页面报表的明细表多两列：平均滚动深度、平均停留时间（只有「页面」，入口页和退出页没有）。
  // 卡片太窄放不下三列数字，所以只在「查看全部」里出现
  const pageDetails = $derived<Details>({
    dims: ["page"],
    headers: [t("metric.scroll_depth"), t("metric.time_on_page")],
    values: (page) => {
      const e = pageEngagement(site, page);
      return e ? [percent(e.scroll_depth / 100, 0), duration(e.time_on_page, app.lang)] : null;
    },
    notes: [
      t("dash.defLine", { m: t("metric.scroll_depth"), d: t("metric.scroll_depth.def") }),
      t("dash.defLine", { m: t("metric.time_on_page"), d: t("metric.time_on_page.def") }),
    ],
    missing: t("dash.engagementMissing"),
  });

  function addFilter(dim: string, value: string) {
    const rest = dash.filters.filter((f) => f.dim !== dim);
    setQuery({ f: encodeFilters([...rest, { dim, op: "is", values: [value] }]) });
  }

  const failure = $derived(
    {
      error: { icon: CircleAlert, title: t("dash.error.title"), body: t("dash.error.body"), tone: "bad" as const },
      busy: { icon: Hourglass, title: t("dash.busy.title"), body: t("dash.busy.body"), tone: "warn" as const },
      timeout: { icon: Clock, title: t("dash.timeout.title"), body: t("dash.timeout.body"), tone: "warn" as const },
    }[ds as "error" | "busy" | "timeout"],
  );
  const pending = $derived(loading || failed);

  // 导出当前视图（时间范围与筛选）为 CSV 压缩包；实时不能导出。原型里只模拟生成的耗时，
  // 评审开关选了出错、排队、超时时导出也失败
  let exporting = $state(false);
  function exportCsv() {
    exporting = true;
    setTimeout(() => {
      exporting = false;
      if (failed) {
        toast(t("dash.exportFailed"), "bad");
        return;
      }
      // 文件名带区间的起止日（站点本地日，终点封顶到今天），和看板上的范围一致
      const span = `${ymd(res.period.from)}-${ymd(res.period.kind === "instant" ? new Date(res.period.to.getTime() - 1) : res.period.to)}`;
      toast(t("dash.exportedFile", { f: `rivu-${site.domain}-${span}.zip` }), "good");
    }, 900);
  }
</script>

{#snippet attribution()}
  <a href={attr.url} target="_blank" rel="noreferrer">{attr.text}</a>
  {#if countryOnly && !shared}
    <Popover width={300} role="dialog" align="end" side="top" label={t("dash.geo.why")}>
      {#snippet trigger({ toggle, props })}
        <button type="button" class="why" onclick={toggle} {...props}><Info size={14} strokeWidth={1.75} />{t("dash.geo.why")}</button>
      {/snippet}
      {#snippet children()}
        <div class="why-pop">
          <p>{t("dash.geo.whyBody")}</p>
          <a href={GEO_DOCS} target="_blank" rel="noreferrer">{t("dash.geo.whyLink")}</a>
        </div>
      {/snippet}
    </Popover>
  {/if}
{/snippet}

<div class="page">
  <SiteHeader {site} {live} {shared}>
    {#snippet actions()}
      {#if !shared && mayExport}
        <Button icon={Download} loading={exporting} disabled={realtime || (settled && neverReceived)} onclick={exportCsv}>{t("dash.exportView")}</Button>
      {/if}
      {#if !shared && mayShare}
        <Button icon={Share2} href={href(`/s/${site.id}/settings/sharing`)}>{t("dash.share")}</Button>
      {/if}
    {/snippet}
  </SiteHeader>

  <div class="toolbar">
    <FilterBar
      {dash}
      dims={filterDims}
      suggest={(dim) => suggestValues(site, dim)}
      rangeText={ru.rangeText}
      prev={ru.prev}
      next={ru.next}
      custom={ru.custom}
      onrange={setRange}
      oncompare={(c) => setQuery({ cmp: c === "previous_period" ? null : c })}
      onfilters={(fs: Filter[]) => setQuery({ f: fs.length ? encodeFilters(fs) : null })}
    >
      {#snippet segment()}
        {#if !shared}
          <SegmentMenu
            {site}
            active={activeSeg}
            filters={dash.filters}
            canWrite={canEdit(site)}
            onapply={(s: Segment | null) => setQuery({ seg: s ? String(s.id) : null, f: null })}
          />
        {/if}
      {/snippet}
    </FilterBar>
  </div>

  {#if segMissing}
    <Callout tone="warn">
      {t("seg.missing")}
      {#snippet actions()}<Button size="sm" onclick={() => setQuery({ seg: null })}>{t("common.close")}</Button>{/snippet}
    </Callout>
  {/if}

  {#if settled && res.beyondRetention && !empty}
    <Callout tone="warn">{t("dash.retentionWarn")}</Callout>
  {/if}
  {#if settled && res.importedExcluded}
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
            <Button variant="primary" href={href(`/s/${site.id}/install`)}>{t("dash.installGuide")}</Button>
          {:else if !neverReceived && dash.range !== "all"}
            <!-- 所选范围没有数据，但站点以前收到过 -->
            <Button onclick={() => setRange({ range: "all" })}>{t("dash.viewAll")}</Button>
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
        <div class="lead">
          <div class="legend">
            <span class="item">
              <span class="key" aria-hidden="true"></span>
              <span>{t(`metric.${metric}`)}</span>
              {#if periodLabel}<span class="muted num">{periodLabel}</span>{/if}
            </span>
            {#if prevValues}
              <span class="item">
                <span class="key compare" aria-hidden="true"></span>
                <span class="muted">{compareText}</span>
                <span class="muted num">{compareLabel}</span>
              </span>
            {/if}
          </div>
          <!-- 访客、访问、浏览量容易混：选中哪个指标，就在图上方用一句话说明它怎么算 -->
          <p class="def">{t(`metric.${metric}.def`)}</p>
        </div>
        <div class="tools">
          {#if grans(dash).length > 1}
            <Segmented
              size="sm"
              label={t("gran.label")}
              value={dash.gran}
              onchange={(g: Gran) => setQuery({ gran: g === grans(dash)[0] ? null : g })}
              options={grans(dash).map((g) => ({ value: g, label: t(`gran.${g}`) }))}
            />
          {/if}
          {#if mayNote}
            <Button variant="ghost" size="sm" icon={MessageSquarePlus} aria-label={t("note.add")} title={t("note.add")} onclick={openAddNote} />
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
            deltaOf={(cur, prev) => delta(cur, prev, metric === "bounce_rate" ? app.lang : undefined)}
            lowerIsBetter={lowerIsBetter(metric)}
            {marks}
            onmark={openMark}
            series={[
              { key: "cur", label: t(`metric.${metric}`), values },
              ...(prevValues ? [{ key: "prev", label: compareText, values: prevValues, compare: true, labels: res.prevTipLabels }] : []),
            ]}
          />
        {:else}
          {@const cols = (realtime ? ["visitors", "pageviews"] : ["visitors", "visits", "pageviews", "bounce_rate", "visit_duration"]) as MetricKey[]}
          <div class="tbl">
            <Table
              sticky
              caption={t(`metric.${metric}`)}
              rows={res.points.map((p, i) => ({ i, p }))}
              rowKey={(r) => r.i}
              columns={[
                { key: "t", label: t("common.time") },
                ...cols.map((k) => ({
                  key: k,
                  label: t(`metric.${k}`),
                  align: "right" as const,
                  optional: k !== metric && k !== "visitors",
                })),
                ...(noteBuckets.size ? [{ key: "notes", label: t("note.col"), optional: true }] : []),
              ]}
            >
              {#snippet cell(r, c)}
                {#if c.key === "t"}{res.tipLabels[r.i]}{:else if c.key === "notes"}<span class="note-cell">{(noteBuckets.get(r.i) ?? []).map(noteLine).join(app.lang === "zh" ? "；" : "; ")}</span>{:else}{fmtMetric(c.key as MetricKey, r.p[c.key as MetricKey] as number, app.lang, "full")}{/if}
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
        {loading}
        {failed}
      />
      <BreakdownCard
        title={t("dash.card.pages")}
        dims={["page", "entry_page", "exit_page"]}
        details={pageDetails}
        rowsOf={res.breakdown}
        total={res.totalVisitors}
        onpick={addFilter}
        {loading}
        {failed}
      />
      <BreakdownCard
        title={t("dash.card.locations")}
        dims={geoDims}
        rowsOf={res.breakdown}
        total={res.totalVisitors}
        onpick={addFilter}
        {loading}
        {failed}
        footer={settled ? attribution : undefined}
      />
      <BreakdownCard
        title={t("dash.card.devices")}
        dims={["device", "browser", "os"]}
        rowsOf={res.breakdown}
        total={res.totalVisitors}
        onpick={addFilter}
        {loading}
        {failed}
      />
    </div>

    <div class="grid">
      <GoalsCard rows={res.goals} onpick={addFilter} {loading} {failed} />
      <PropsCard keys={propertyKeys(site)} rowsOf={res.breakdown} total={res.totalVisitors} onpick={addFilter} {loading} {failed} />
    </div>
  {/if}
</div>

{#if notesOn}
  <NotesDialog bind:open={notesOpen} bind:mode={notesMode} {site} notes={notesList} heading={notesHeading} showDates={notesDated} canWrite={mayNote} />
{/if}

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
    flex-wrap: wrap;
    column-gap: var(--sp-4);
    row-gap: var(--sp-1);
    /* 与右侧工具（28px 高）同一行居中 */
    min-height: var(--control-sm);
    font-weight: var(--fw-medium);
  }
  .def {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .legend .item {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
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
  .note-cell {
    color: var(--text-muted);
    white-space: normal;
  }
  .why {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-1);
    flex: none;
    padding: 0;
    border: 0;
    background: none;
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .why:hover,
  .why[aria-expanded="true"] {
    color: var(--text);
  }
  .why-pop {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-2) var(--sp-3);
    font-size: var(--fs-sm);
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
