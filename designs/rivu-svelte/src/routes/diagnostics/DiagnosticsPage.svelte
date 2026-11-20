<!--
  接入诊断：页头（标题、最后收到事件、时间范围）→ 指标与趋势 → 丢弃原因 → 脚本版本。
  数据来自 ClickHouse ingest_counters（每分钟，按 metric 与 script_version 计数，保留 90 天）。
  editor 起可见（viewer 看到 404）；只有能改设置的角色才有「处理」链接。
  状态：加载（骨架）、空（未接入 / 无事件）、出错 / 排队 / 超时。
-->
<script lang="ts">
  import { ChartSpline, CircleAlert, Clock, Hourglass, Inbox, Table2 } from "@lucide/svelte";
  import SiteHeader from "../../lib/app/SiteHeader.svelte";
  import NotFound from "../NotFound.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Segmented from "../../lib/ui/Segmented.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import TrendChart from "../../lib/charts/TrendChart.svelte";
  import { t } from "../../lib/i18n";
  import { app, canEdit, canManage, toast } from "../../lib/state.svelte";
  import { compact, dateShort, full, percent, relativeTime } from "../../lib/format";
  import { href, route, setQuery } from "../../lib/router.svelte";
  import type { Site } from "../../lib/data/fixtures";
  import { diagnose, REASONS, type RangeId, type ReasonDef } from "./data";
  import "./strings";

  let { site }: { site: Site; section?: string } = $props();

  /** 服务端要求有效角色 editor 起；viewer 与不存在的站点一样显示 404 */
  const allowed = $derived(canEdit(site));

  const rangeParam = $derived(route.current.query.get("range"));
  const range = $derived<RangeId>(rangeParam === "1h" || rangeParam === "7d" ? rangeParam : "24h");

  const ds = $derived(app.review.dataState);
  const loading = $derived(ds === "loading");
  const failed = $derived(ds === "error" || ds === "busy" || ds === "timeout");
  /** 所有时间都没有事件（包括很久以前）：还没接入。数据到齐之后才判断 */
  const noData = $derived(!loading && !failed && (site.scale === 0 || ds === "empty"));
  const failure = $derived(
    {
      error: { icon: CircleAlert, tone: "bad" as const, title: t("diag.error.title"), body: t("diag.error.body") },
      busy: { icon: Hourglass, tone: "warn" as const, title: t("diag.busy.title"), body: t("diag.busy.body") },
      timeout: { icon: Clock, tone: "warn" as const, title: t("diag.timeout.title"), body: t("diag.timeout.body") },
    }[ds as "error" | "busy" | "timeout"],
  );

  const cur = $derived(diagnose(site, range));
  const rate = $derived(cur.accepted + cur.dropped ? cur.dropped / (cur.accepted + cur.dropped) : 0);

  const reasonRows = $derived([...cur.reasons].sort((a, b) => b.count - a.count));
  const mayManage = $derived(canManage(site));
  const def = (id: string) => REASONS[id];
  const reasonName = (id: string) => (def(id) ? t(`diag.r.${id}.name`) : id);

  // ---- 图表 ----
  const unit = $derived(range === "1h" ? "minute" : range === "24h" ? "hour" : "6h");
  const pad = (n: number) => String(n).padStart(2, "0");
  const hm = (d: Date) => `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  const xLabels = $derived(
    cur.buckets.map((b) =>
      range === "7d" && b.t.getHours() === 0 ? dateShort(b.t, app.lang) : range === "7d" ? `${pad(b.t.getHours())}:00` : hm(b.t),
    ),
  );
  const tipLabels = $derived(
    cur.buckets.map((b) => (range === "7d" ? `${dateShort(b.t, app.lang)} ${pad(b.t.getHours())}:00` : range === "24h" ? `${dateShort(b.t, app.lang)} ${hm(b.t)}` : hm(b.t))),
  );
  let view = $state<"chart" | "table">("chart");
  /** 原因代码（dropped_…）只在对照日志与监控指标时有用：默认不显示 */
  let showCodes = $state(false);

  const lastEvent = $derived(
    cur.lastEventSec === null
      ? t("diag.lastEvent.none")
      : t("diag.lastEvent", {
          when: relativeTime(Math.floor(cur.lastEventSec / 60), app.lang),
        }),
  );

  const kpis = $derived([
    { key: "accepted", value: full(cur.accepted, app.lang) },
    { key: "dropped", value: full(cur.dropped, app.lang) },
    { key: "rate", value: percent(rate) },
  ]);
</script>

{#if !allowed}
  <NotFound />
{:else}
  <div class="page">
    <SiteHeader {site}>
      {#snippet actions()}
        {#if !noData && !loading && !failed}
          <span class="last">{lastEvent}</span>
        {/if}
        {#if !noData}
          <Segmented
            label={t("diag.range.label")}
            value={range}
            onchange={(v: RangeId) => setQuery({ range: v === "24h" ? null : v })}
            options={[
              { value: "1h", label: t("diag.range.1h") },
              { value: "24h", label: t("diag.range.24h") },
              { value: "7d", label: t("diag.range.7d") },
            ]}
          />
        {/if}
      {/snippet}
    </SiteHeader>

    {#if noData}
      <Card>
        <EmptyState icon={Inbox} title={t("diag.empty.title")} body={t("diag.empty.body")}>
          {#snippet actions()}
            <Button variant="primary" href={href(`/s/${site.id}/install`)}>{t("diag.empty.action")}</Button>
          {/snippet}
        </EmptyState>
      </Card>
    {:else}
      <section class="main" aria-label={t("diag.chart.title", { unit: t(`diag.unit.${unit}`) })}>
        <div class="strip">
          {#each kpis as k (k.key)}
            <div class="kpi">
              <span class="label">{t(`diag.kpi.${k.key}`)}</span>
              {#if loading || failed}
                <Skeleton width="72px" height={28} />
              {:else}
                <span class="value">{k.value}</span>
              {/if}
            </div>
          {/each}
        </div>

        <div class="chart-head">
          <div class="legend">
            <span class="item"><span class="key s1" aria-hidden="true"></span>{t("diag.kpi.accepted")}</span>
            <span class="item"><span class="key s2" aria-hidden="true"></span>{t("diag.kpi.dropped")}</span>
          </div>
          <Segmented
            size="sm"
            label={t("common.view")}
            bind:value={view}
            options={[
              { value: "chart", label: "", icon: ChartSpline, hint: t("common.chart") },
              { value: "table", label: "", icon: Table2, hint: t("common.table") },
            ]}
          />
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
              summary={t("diag.chart.summary", { a: tipLabels[0], b: tipLabels[tipLabels.length - 1] })}
              {xLabels}
              {tipLabels}
              format={(n) => compact(n, app.lang)}
              partialLast={cur.partialLast}
              partialText={t("diag.chart.partial")}
              series={[
                { key: "accepted", label: t("diag.kpi.accepted"), values: cur.buckets.map((b) => b.accepted) },
                { key: "dropped", label: t("diag.kpi.dropped"), values: cur.buckets.map((b) => b.dropped) },
              ]}
            />
          {:else}
            <div class="tbl">
              <Table
                sticky
                caption={t("diag.chart.title", { unit: t(`diag.unit.${unit}`) })}
                rows={[...cur.buckets].reverse().map((b, i) => ({ b, label: [...tipLabels].reverse()[i] }))}
                rowKey={(r) => r.label}
                columns={[
                  { key: "t", label: t("common.time") },
                  { key: "a", label: t("diag.kpi.accepted"), align: "right" },
                  { key: "d", label: t("diag.kpi.dropped"), align: "right" },
                  { key: "r", label: t("diag.kpi.rate"), align: "right", optional: true },
                ]}
              >
                {#snippet cell(r, c)}
                  {#if c.key === "t"}{r.label}
                  {:else if c.key === "a"}{full(r.b.accepted, app.lang)}
                  {:else if c.key === "d"}{full(r.b.dropped, app.lang)}
                  {:else}{percent(r.b.accepted + r.b.dropped ? r.b.dropped / (r.b.accepted + r.b.dropped) : 0)}{/if}
                {/snippet}
              </Table>
            </div>
          {/if}
        </div>
      </section>

      <Card title={t("diag.reasons.title")} flush>
        {#snippet actions()}
          {#if !loading && !failed}
            <Button size="sm" variant="ghost" active={showCodes} aria-pressed={showCodes} onclick={() => (showCodes = !showCodes)}>{t("diag.reasons.codes")}</Button>
          {/if}
        {/snippet}
        {#if loading}
          <div class="rows-sk">
            {#each [1, 2, 3, 4] as i (i)}<Skeleton height={40} radius={4} />{/each}
          </div>
        {:else if failed && failure}
          <EmptyState compact icon={failure.icon} tone={failure.tone} title={failure.title} body={failure.body} />
        {:else if !reasonRows.length}
          <EmptyState compact icon={Inbox} title={t("diag.reasons.none")} />
        {:else}
          <Table
            caption={t("diag.reasons.title")}
            rows={reasonRows}
            rowKey={(r) => r.id}
            columns={[
              { key: "reason", label: t("diag.col.reason") },
              { key: "count", label: t("diag.col.count"), align: "right", width: "96px" },
              { key: "share", label: t("diag.col.share"), align: "right", width: "80px" },
              { key: "fix", label: t("diag.col.fix"), width: "136px", optional: true },
            ]}
          >
            {#snippet cell(r, c)}
              {@const d = def(r.id)}
              {#if c.key === "reason"}
                <div class="reason">
                  <span class="rname">{reasonName(r.id)}{#if showCodes}<code class="rid">dropped_{r.id}</code>{/if}</span>
                  {#if d?.why}<span class="rwhy">{t(`diag.r.${r.id}.why`)}</span>{/if}
                </div>
              {:else if c.key === "count"}
                {full(r.count, app.lang)}
              {:else if c.key === "share"}
                {percent(r.count / cur.dropped)}
              {:else}
                {@render fix(d)}
              {/if}
            {/snippet}
          </Table>
        {/if}
      </Card>

      <Card title={t("diag.ver.title")} flush>
        {#if loading}
          <div class="rows-sk">
            {#each [1, 2, 3] as i (i)}<Skeleton height={40} radius={4} />{/each}
          </div>
        {:else if failed && failure}
          <EmptyState compact icon={failure.icon} tone={failure.tone} title={failure.title} body={failure.body} />
        {:else}
          <Table
            caption={t("diag.ver.title")}
            rows={cur.versions}
            rowKey={(v) => v.version}
            columns={[
              { key: "v", label: t("diag.ver.version") },
              { key: "e", label: t("diag.ver.events"), align: "right", width: "120px" },
              { key: "l", label: t("diag.ver.last"), align: "right", width: "120px", optional: true },
            ]}
          >
            {#snippet cell(v, c)}
              {#if c.key === "v"}
                {#if v.version === 0}<span class="muted">{t("diag.ver.unknown")}</span>{:else}<code>{v.version}</code>{/if}
              {:else if c.key === "e"}
                {full(v.events, app.lang)}
              {:else}
                {relativeTime(v.lastSeenMin, app.lang)}
              {/if}
            {/snippet}
          </Table>
        {/if}
      </Card>
    {/if}
  </div>
{/if}

{#snippet fix(d: ReasonDef | undefined)}
  {#if d?.fix && mayManage}
    <a class="text-link" href={href(`/s/${site.id}/settings/${d.fix}`)}>{t(`diag.fix.${d.fix}`)}</a>
  {:else if d?.admin}
    <span class="muted">{t("diag.fix.admin")}</span>
  {/if}
{/snippet}

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
  .last {
    color: var(--text-muted);
    font-size: var(--fs-sm);
    font-variant-numeric: tabular-nums;
  }
  .main {
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-lg);
    background: var(--bg-app);
    box-shadow: var(--shadow-card);
    overflow: hidden;
  }
  .strip {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    border-bottom: var(--hairline) solid var(--border-subtle);
  }
  .kpi {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: var(--sp-1);
    min-width: 0;
    padding: var(--sp-4) var(--sp-5);
    border-left: var(--hairline) solid var(--border-subtle);
  }
  .kpi:first-child {
    border-left: 0;
  }
  .label {
    color: var(--text-muted);
  }
  .value {
    font-family: var(--font-display);
    font-size: var(--fs-2xl);
    font-weight: var(--fw-semibold);
    line-height: var(--lh-display);
    letter-spacing: var(--track-display);
    white-space: nowrap;
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
    gap: var(--sp-4);
    font-weight: var(--fw-medium);
  }
  .item {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .key {
    width: 12px;
    height: 2px;
    border-radius: 1px;
  }
  .s1 {
    background: var(--series-1);
  }
  .s2 {
    background: var(--series-2);
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
  .rows-sk {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: 0 var(--sp-5) var(--sp-4);
  }
  .reason {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: var(--sp-2) 0;
  }
  .rname {
    display: flex;
    align-items: baseline;
    flex-wrap: wrap;
    gap: 0 var(--sp-2);
    font-weight: var(--fw-medium);
  }
  .rid {
    color: var(--text-muted);
    font-size: var(--fs-sm);
    font-weight: var(--fw-regular);
  }
  .rwhy {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  @media (max-width: 720px) {
    .page {
      padding: var(--sp-4) var(--sp-4) var(--sp-10);
    }
    .strip {
      grid-template-columns: 1fr;
    }
    .kpi {
      border-left: 0;
      border-top: var(--hairline) solid var(--border-subtle);
    }
    .kpi:first-child {
      border-top: 0;
    }
  }
</style>
