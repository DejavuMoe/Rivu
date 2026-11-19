<!--
  安装引导：复制代码、验证安装两部分。
  验证：页面打开期间自动检测，没有手动重新检查；只有「等待」与「已收到」两种结果。
  原型里由评审面板的 app.review.install 驱动：「已收到」显示收到事件的页面，其余都按等待显示。
  脚本功能开关属于站点设置的数据采集，不在这里。
-->
<script lang="ts">
  import SiteHeader from "../../lib/app/SiteHeader.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Callout from "../../lib/ui/Callout.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import CopyField from "../../lib/ui/CopyField.svelte";
  import LiveDot from "../../lib/ui/LiveDot.svelte";
  import Tabs from "../../lib/ui/Tabs.svelte";
  import type { Site } from "../../lib/data/fixtures";
  import { t } from "../../lib/i18n";
  import { app } from "../../lib/state.svelte";
  import { href } from "../../lib/router.svelte";
  import "./strings";

  let { site }: { site: Site; section?: string } = $props();

  type Method = "site" | "manual" | "cdn";

  const BASE = "https://stats.example.com";
  const IGNORE = 'localStorage.rivu_ignore = "true"';

  let method = $state<Method>("site");

  const siteTag = $derived(`<script defer src="${BASE}/js/${site.siteKey}.js"><\/script>`);
  const code = $derived.by(() => {
    if (method === "site") return siteTag;
    if (method === "manual")
      return [
        "<script>window.rivu=window.rivu||function(){(rivu.q=rivu.q||[]).push(arguments)}<\/script>",
        siteTag,
        `<script>rivu('event','Signup',{props:{plan:'pro'}})<\/script>`,
      ].join("\n");
    return `<script defer src="${BASE}/js/t.js"\n        data-site="${site.siteKey}"><\/script>`;
  });
  const note = $derived(t(`in.${method}.note`));
  const exclude = $derived(t("in.exclude", { cmd: "{cmd}" }).split("{cmd}"));

  const received = $derived(app.review.install === "received");
</script>

<div class="page">
  <SiteHeader {site} />

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
    {#if !received}
      <p class="status"><LiveDot size={8} />{t("in.verify.waiting")}</p>
    {:else}
      <div class="stack">
        <Callout tone="good">{t("in.verify.ok", { path: "/pricing" })}</Callout>
        <div class="row end"><Button variant="primary" href={href(`/s/${site.id}`)}>{t("in.openDashboard")}</Button></div>
      </div>
    {/if}
  </Card>
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
