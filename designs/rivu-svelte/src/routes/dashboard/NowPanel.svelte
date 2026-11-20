<!--
  「此刻」：看板主区块右边的一扇夜空的窗。左边的趋势图讲这段时间，这里讲现在：有多少人在线、他们在地球的哪里。
  - 窗里永远是夜色（不随主题变浅）：灯光只有在暗处才看得见。地球很大，被窗框裁掉一部分。
  - 只画在线的访客（灯光色的光点），不画时间范围内的分布；那是「地区」卡片「地图」标签页的事。
  - 列出在线人数最多的三个国家；地球只是画面，这三行和在线人数才是内容。
-->
<script lang="ts">
  import type { Component, Snippet } from "svelte";
  import { ArrowUpRight } from "@lucide/svelte";
  import CountUp from "../../lib/ui/CountUp.svelte";
  import LiveDot from "../../lib/ui/LiveDot.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import { t } from "../../lib/i18n";
  import { app } from "../../lib/state.svelte";
  import { compact } from "../../lib/format";
  import type { Row } from "./query";
  import { displayValue } from "./values";

  interface Props {
    /** 在线人数；null：还没取到 */
    online: number | null;
    /** 此刻各国的在线人数（label 是国家代码） */
    rows: Row[];
    /** 地球组件（按需加载，没到时只有数字） */
    globe: Component<any> | null;
    /** 「看实时」的链接；实时范围里和共享看板上没有 */
    realtime?: string;
    /** 地理库的署名 */
    attribution?: Snippet;
  }
  let { online, rows, globe: Globe, realtime, attribution }: Props = $props();

  const live = $derived(
    [...rows].sort((a, b) => b.visitors - a.visitors).map((r) => ({ code: r.label, label: displayValue("country", r.label, app.lang), value: r.visitors })),
  );
  const top = $derived(live.slice(0, 3));
  const summary = $derived(
    live.length ? t("dash.now.summary", { list: top.map((r) => `${r.label} ${r.value}`).join(t("dash.map.sep")) }) : t("dash.now.none"),
  );
</script>

<aside class="now" data-theme="dark" aria-label={t("dash.now")}>
  <div class="sky">
    {#if Globe}
      <Globe rows={[]} {live} format={(n: number) => compact(n, app.lang)} liveText={(n: number) => t("dash.now.live", { n })} {summary} spin={5} />
    {/if}
  </div>
  <div class="over">
    <div class="head">
      <span class="tag"><LiveDot off={online === 0} />{t("dash.now")}</span>
      {#if realtime}
        <a class="open" href={realtime}>{t("dash.now.open")}<ArrowUpRight size={14} strokeWidth={2} aria-hidden="true" /></a>
      {/if}
    </div>
    <p class="count">
      {#if online === null}
        <Skeleton width="96px" height={44} />
      {:else}
        <span class="n"><CountUp value={online} format={(n) => compact(Math.round(n), app.lang)} /></span>
        <span class="unit">{t("dash.now.unit")}</span>
      {/if}
    </p>
    {#if top.length}
      <ol class="top">
        {#each top as r (r.code)}
          <li>
            <span class="code">{r.code}</span>
            <span class="name">{r.label}</span>
            <span class="num">{r.value}</span>
          </li>
        {/each}
      </ol>
    {/if}
    {#if attribution}<p class="attr">{@render attribution()}</p>{/if}
  </div>
</aside>

<style>
  .now {
    position: relative;
    display: flex;
    min-width: 0;
    min-height: 320px;
    overflow: hidden;
    background: var(--night);
    color: var(--text);
  }
  /* 地球比窗大，圆心在窗的右下：窗里看到的是它的左上大半个 */
  .sky {
    position: absolute;
    right: -52%;
    bottom: -40%;
    width: 150%;
  }
  /* 左下角压一层夜色，国家那三行不和陆地的点混在一起 */
  .now::before {
    content: "";
    position: absolute;
    inset: 40% 30% 0 0;
    z-index: 1;
    background: var(--night-scrim);
    pointer-events: none;
  }
  .over {
    position: relative;
    z-index: 2;
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: var(--sp-2);
    min-width: 0;
    padding: var(--sp-5);
    /* 字和数字压在夜空上，地球仍然可以拖 */
    pointer-events: none;
  }
  .over a,
  .over .attr :global(*) {
    pointer-events: auto;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .tag {
    display: inline-flex;
    align-items: center;
    gap: 10px;
    padding-left: 2px;
    color: var(--text-muted);
    font-weight: var(--fw-medium);
  }
  .open {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    color: var(--text-muted);
    font-size: var(--fs-sm);
    border-radius: var(--r-xs);
  }
  .open:hover {
    color: var(--text);
  }
  .count {
    display: flex;
    align-items: baseline;
    gap: var(--sp-2);
    min-height: 46px;
  }
  .n {
    font-family: var(--font-display);
    font-size: var(--fs-3xl);
    font-weight: var(--fw-semibold);
    line-height: var(--lh-display);
    letter-spacing: var(--track-display-lg);
    font-variant-numeric: tabular-nums;
  }
  .unit {
    color: var(--text-muted);
  }
  .top {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    margin-top: auto;
    list-style: none;
    font-size: var(--fs-sm);
  }
  .top li {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .code {
    display: inline-grid;
    place-items: center;
    flex: none;
    width: 24px;
    height: 18px;
    border-radius: var(--r-xs);
    background: var(--bg-element);
    color: var(--text-muted);
    font-family: var(--font-mono);
  }
  .name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .top .num {
    color: var(--live);
    font-weight: var(--fw-medium);
  }
  .attr {
    max-width: 62%;
    margin-top: var(--sp-1);
    color: var(--text-placeholder);
    font-size: var(--fs-sm);
  }
  .attr :global(a:hover) {
    color: var(--text-muted);
  }
  @media (max-width: 960px) {
    .now {
      min-height: 300px;
    }
    .sky {
      right: -14%;
      bottom: -78%;
      width: 86%;
      min-width: 420px;
    }
    .now::before {
      inset: 30% 45% 0 0;
    }
  }
</style>
