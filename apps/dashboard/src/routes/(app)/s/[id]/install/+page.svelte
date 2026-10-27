<!--
  安装引导：复制代码、验证安装两部分。
  验证：页面打开期间每 5 秒查一次今天的浏览量，收到后停止。还没有接入诊断接口，所以只有「等待」与「已收到」。
  脚本功能开关属于站点设置的数据采集，不在这里。
-->
<script lang="ts">
  import NotFound from "#lib/app/NotFound.svelte";
  import SiteHeader from "#lib/app/SiteHeader.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Callout from "#lib/ui/Callout.svelte";
  import Card from "#lib/ui/Card.svelte";
  import CopyField from "#lib/ui/CopyField.svelte";
  import LiveDot from "#lib/ui/LiveDot.svelte";
  import Tabs from "#lib/ui/Tabs.svelte";
  import { page } from "$app/state";
  import { api } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import { findSite } from "#lib/session.svelte.ts";
  import "./strings";

  type Method = "site" | "manual" | "cdn";

  const IGNORE = 'localStorage.rivu_ignore = "true"';
  const POLL_MS = 5000;

  // 看板与接口同源（Caddy 反代），脚本地址直接取当前站点的 origin
  const BASE = location.origin;

  const site = $derived(findSite(Number(page.params.id)));

  let method = $state<Method>("site");
  /** null：还没收到；字符串：收到的第一个页面路径（取不到时为空串） */
  let received = $state<string | null>(null);

  const siteTag = $derived(`<script defer src="${BASE}/js/${site?.site_key}.js"><\/script>`);
  const code = $derived.by(() => {
    if (method === "site") return siteTag;
    if (method === "manual")
      return [
        "<script>window.rivu=window.rivu||function(){(rivu.q=rivu.q||[]).push(arguments)}<\/script>",
        siteTag,
        `<script>rivu('event','Signup',{props:{plan:'pro'}})<\/script>`,
      ].join("\n");
    return `<script defer src="${BASE}/js/t.js"\n        data-site="${site?.site_key}"><\/script>`;
  });
  const note = $derived(t(`in.${method}.note`));
  const exclude = $derived(t("in.exclude", { cmd: "{cmd}" }).split("{cmd}"));

  // 轮询：站点 id 变了重新开始；收到或离开页面时停止
  $effect(() => {
    const id = site?.id;
    if (id === undefined) return;
    received = null;
    const ctl = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;

    async function check() {
      try {
        const r = await api.query(id!, { metrics: ["pageviews"], dimensions: ["event:page"], date_range: "day" }, { signal: ctl.signal });
        const hit = r.results.find((row) => row.metrics[0] > 0);
        if (hit) {
          received = hit.dimensions[0] ?? "";
          return;
        }
      } catch {
        if (ctl.signal.aborted) return;
        /* 查询失败按没收到处理，下一轮再试 */
      }
      timer = setTimeout(check, POLL_MS);
    }
    void check();

    return () => {
      ctl.abort();
      clearTimeout(timer);
    };
  });
</script>

{#if !site}
  <NotFound />
{:else}
  <div class="page">
    <SiteHeader siteId={site.id} name={site.name} />

    <Card title={t("in.copy")}>
      <div class="stack">
        <Tabs
          label={t("in.method")}
          bind:value={method}
          options={[
            { value: "site", label: t("in.method.site") },
            { value: "manual", label: t("in.method.manual") },
            { value: "cdn", label: t("in.method.cdn") },
          ]}
        />
        <CopyField multiline label={t("in.code")} value={code} />
        <div class="notes">
          <p>{note}</p>
          <p>{exclude[0]}<code>{IGNORE}</code>{exclude[1]}</p>
        </div>
      </div>
    </Card>

    <Card title={t("in.verify")}>
      {#if received === null}
        <p class="status"><LiveDot size={8} />{t("in.verify.waiting")}</p>
      {:else}
        <div class="stack">
          <Callout tone="good">{t("in.verify.ok", { path: received })}</Callout>
          <div class="row end"><Button variant="primary" href="/s/{site.id}">{t("in.openDashboard")}</Button></div>
        </div>
      {/if}
    </Card>
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
  /* 页头与其他站点页同宽（切换标签时不跳），正文限宽 */
  .page > :global(:not(.ph)) {
    max-width: 880px;
  }
  .stack {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .notes {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: var(--sp-3);
  }
  .row.end {
    justify-content: flex-end;
  }
  .status {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  @media (max-width: 720px) {
    .page {
      padding: var(--sp-4) var(--sp-4) var(--sp-10);
    }
  }
</style>
