<!--
  接入诊断：页头（标题、最后收到事件、时间范围）→ 指标与趋势 → 丢弃原因 → 脚本版本。
  数据来自 GET /sites/{site}/diagnostics（ClickHouse ingest_counters，每分钟，按原因与脚本版本计数）。
  范围在查询串 ?range= 里。editor 起可见；只有能改设置的角色才有「处理」链接。
  状态：加载（骨架）、空（还没有任何事件）、出错 / 排队 / 超时。
-->
<script lang="ts">
  import { ChartSpline, CircleAlert, Clock, Hourglass, Inbox, Table2 } from "@lucide/svelte";
  import { page } from "$app/state";
  import NotFound from "#lib/app/NotFound.svelte";
  import PageHeader from "#lib/app/PageHeader.svelte";
  import TrendChart from "#lib/charts/TrendChart.svelte";
  import { failureOf, isAbort, type Failure } from "#lib/dashboard/source.ts";
  import Button from "#lib/ui/Button.svelte";
  import Card from "#lib/ui/Card.svelte";
  import EmptyState from "#lib/ui/EmptyState.svelte";
  import Segmented from "#lib/ui/Segmented.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import Table from "#lib/ui/Table.svelte";
  import { api, type Diagnostics, type DiagnosticsRange } from "#lib/api.ts";
  import { compact, dateShort, full, percent, relativeTime } from "#lib/format.ts";
  import { t } from "#lib/i18n.ts";
  import { setQuery } from "#lib/nav.ts";
  import { canEdit, canManage, findSite } from "#lib/session.svelte.ts";
  import { app } from "#lib/state.svelte.ts";
  import { REASONS } from "./reasons.ts";
  import "./strings";

  const site = $derived(findSite(Number(page.params.id)));
  /** 服务端要求有效角色 editor 起；站点访客 viewer 与组织 viewer 一样不可见 */
  const allowed = $derived(canEdit(site));

  const rangeParam = $derived(page.url.searchParams.get("range"));
  const range = $derived<DiagnosticsRange>(rangeParam === "1h" || rangeParam === "7d" ? rangeParam : "24h");

  let status = $state<"loading" | "ready" | Failure>("loading");
  let data = $state<Diagnostics | null>(null);
  let tick = $state(0);

  $effect(() => {
    const id = site?.id;
    const r = range;
    void tick;
    if (id === undefined || !allowed) return;
    status = "loading";
    const ctl = new AbortController();
    api.diagnostics(id, r, { signal: ctl.signal }).then(
      (d) => {
        data = d;
        status = "ready";
      },
      (e) => {
        if (isAbort(e)) return;
        status = failureOf(e);
      },
    );
    return () => ctl.abort();
  });

  $effect(() => {
    document.title = site ? `${t("diag.title")} – ${site.name} – Rivu` : "Rivu";
  });

  const loading = $derived(status === "loading");
  const failed = $derived(status !== "loading" && status !== "ready");
  const failure = $derived(
    {
      error: { icon: CircleAlert, tone: "bad" as const, title: t("diag.error.title"), body: t("diag.error.body") },
      busy: { icon: Hourglass, tone: "warn" as const, title: t("diag.busy.title"), body: t("diag.busy.body") },
      timeout: { icon: Clock, tone: "warn" as const, title: t("diag.timeout.title"), body: t("diag.timeout.body") },
    }[status as Failure],
  );

  const accepted = $derived(data?.totals.accepted ?? 0);
  const dropped = $derived(data?.totals.dropped ?? 0);
  const rate = $derived(accepted + dropped ? dropped / (accepted + dropped) : 0);
  /** 所有时间都没有事件（包括很久以前）：还没接入 */
  const noData = $derived(status === "ready" && !!data && data.last_event_at === null && !accepted && !dropped);
  const mayManage = $derived(canManage(site));
  const def = (id: string) => REASONS[id];

  // ---- 图表：桶是 UTC 对齐的，横轴按浏览器本地时间显示 ----
  const buckets = $derived(data?.buckets ?? []);
  const unit = $derived(range === "1h" ? "minute" : range === "24h" ? "hour" : "6h");
  const pad = (n: number) => String(n).padStart(2, "0");
  const hm = (d: Date) => `${pad(d.getHours())}:${pad(d.getMinutes())}`;
  const dates = $derived(buckets.map((b) => new Date(b.t)));
  const xLabels = $derived(
    dates.map((d) => (range === "7d" ? (d.getHours() === 0 ? dateShort(d, app.lang) : `${pad(d.getHours())}:00`) : hm(d))),
  );
  const tipLabels = $derived(
    dates.map((d) => (range === "7d" ? `${dateShort(d, app.lang)} ${pad(d.getHours())}:00` : range === "24h" ? `${dateShort(d, app.lang)} ${hm(d)}` : hm(d))),
  );
  let view = $state<"chart" | "table">("chart");

  const minutesAgo = (iso: string) => Math.max(0, Math.floor((Date.now() - new Date(iso).getTime()) / 60000));
  const lastEvent = $derived(
    !data || data.last_event_at === null
      ? t("diag.lastEvent.none")
      : t("diag.lastEvent", { when: relativeTime(minutesAgo(data.last_event_at), app.lang) }),
  );

  const kpis = $derived([
    { key: "accepted", value: full(accepted, app.lang) },
    { key: "dropped", value: full(dropped, app.lang) },
    { key: "rate", value: percent(rate) },
  ]);
  const reasonName = (id: string) => (def(id) ? t(`diag.r.${id}.name`) : id);
</script>

{#if !site || !allowed}
  <NotFound />
{:else}
  <div class="page">
    <PageHeader title={t("diag.title")}>
      {#snippet actions()}
        {#if !noData && status === "ready"}
          <span class="last">{lastEvent}</span>
        {/if}
        {#if !noData}
          <Segmented
            label={t("diag.range.label")}
            value={range}
            onchange={(v: DiagnosticsRange) => setQuery({ range: v === "24h" ? null : v })}
            options={[
              { value: "1h", label: t("diag.range.1h") },
              { value: "24h", label: t("diag.range.24h") },
              { value: "7d", label: t("diag.range.7d") },
            ]}
          />
        {/if}
      {/snippet}
    </PageHeader>

    {#if noData}
      <Card>
        <EmptyState icon={Inbox} title={t("diag.empty.title")} body={t("diag.empty.body")}>
          {#snippet actions()}
            <Button variant="primary" href="/s/{site.id}/install">{t("diag.empty.action")}</Button>
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
              {#snippet actions()}<Button size="sm" onclick={() => tick++}>{t("common.retry")}</Button>{/snippet}
            </EmptyState>
          {:else if view === "chart"}
            <TrendChart
              height={264}
              summary={t("diag.chart.summary", { a: tipLabels[0] ?? "", b: tipLabels[tipLabels.length - 1] ?? "" })}
              {xLabels}
              {tipLabels}
              format={(n) => compact(n, app.lang)}
              partialLast={data?.partial_last ?? false}
              partialText={t("diag.chart.partial")}
              series={[
                { key: "accepted", label: t("diag.kpi.accepted"), values: buckets.map((b) => b.accepted) },
                { key: "dropped", label: t("diag.kpi.dropped"), values: buckets.map((b) => b.dropped) },
              ]}
            />
          {:else}
            <div class="tbl">
              <Table
                sticky
                caption={t("diag.chart.title", { unit: t(`diag.unit.${unit}`) })}
                rows={buckets.map((b, i) => ({ b, label: tipLabels[i] })).reverse()}
                rowKey={(r) => r.b.t}
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
        {#if loading}
          <div class="rows-sk">
            {#each [1, 2, 3, 4] as i (i)}<Skeleton height={40} radius={4} />{/each}
          </div>
        {:else if failed && failure}
          <EmptyState compact icon={failure.icon} tone={failure.tone} title={failure.title} body={failure.body} />
        {:else if !data?.reasons.length}
          <EmptyState compact icon={Inbox} title={t("diag.reasons.none")} />
        {:else}
          <Table
            caption={t("diag.reasons.title")}
            rows={data.reasons}
            rowKey={(r) => r.reason}
            columns={[
              { key: "reason", label: t("diag.col.reason") },
              { key: "count", label: t("diag.col.count"), align: "right", width: "96px" },
              { key: "share", label: t("diag.col.share"), align: "right", width: "80px" },
              { key: "fix", label: t("diag.col.fix"), width: "136px", optional: true },
            ]}
          >
            {#snippet cell(r, c)}
              {@const d = def(r.reason)}
              {#if c.key === "reason"}
                <div class="reason">
                  <span class="rname">{reasonName(r.reason)}<code class="rid">dropped_{r.reason}</code></span>
                  {#if d?.why}<span class="rwhy">{t(`diag.r.${r.reason}.why`)}</span>{/if}
                </div>
              {:else if c.key === "count"}
                {full(r.count, app.lang)}
              {:else if c.key === "share"}
                {percent(r.count / dropped)}
              {:else if d?.fix && mayManage}
                <a class="text-link" href="/s/{site.id}/settings/{d.fix}">{t(`diag.fix.${d.fix}`)}</a>
              {:else if d?.admin}
                <span class="muted">{t("diag.fix.admin")}</span>
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
        {:else if data}
          <Table
            caption={t("diag.ver.title")}
            rows={data.versions}
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
                {relativeTime(minutesAgo(v.last_seen), app.lang)}
              {/if}
            {/snippet}
          </Table>
        {/if}
      </Card>
    {/if}
  </div>
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
  .last {
    color: var(--text-muted);
    font-size: var(--fs-sm);
    font-variant-numeric: tabular-nums;
  }
  .main {
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-md);
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
    font-size: var(--fs-xl);
    font-weight: var(--fw-semibold);
    line-height: var(--lh-tight);
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
