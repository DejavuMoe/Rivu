<!--
  导入：把 Plausible 导出的 zip 拖进上传区（或点选），上传后服务端在后台写入日汇总层，列表里的状态随之更新。
  上传区在导入期间原地变成进度（同一个框、同样高度，不挤动下面的列表）；一个站点同时只跑一个导入。
  只导入早于今天、且早于首个 Rivu 原生日的完整历史日（按站点时区）。
  导入记录列出日期范围与状态；删除已完成的导入要确认，失败的记录直接删。
-->
<script lang="ts">
  import { CircleAlert, FileUp, Trash2 } from "@lucide/svelte";
  import Badge from "../../lib/ui/Badge.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import { NOW, USER, type Site } from "../../lib/data/fixtures";
  import { cfgs, firstDay, nextId, ymd, type Import } from "./store.svelte";
  import "./strings";

  let { site }: { site: Site } = $props();
  const cfg = $derived(cfgs[site.id]);
  const first = $derived(firstDay(site));
  const hasData = $derived(site.scale > 0);

  /** 与服务端的上传上限一致 */
  const MAX_BYTES = 128 * 1024 * 1024;

  const loading = $derived(app.review.dataState === "loading");
  const failed = $derived(["error", "busy", "timeout"].includes(app.review.dataState));

  const days = (a: string, b: string) => Math.round((Date.parse(b) - Date.parse(a)) / 864e5) + 1;
  const shift = (d: string, n: number) => ymd(new Date(Date.parse(`${d}T12:00:00`) + n * 864e5));

  // ---- 上传与进度（原型里按时间模拟；真实实现是上传进度 + 轮询导入状态） ----
  let phase = $state<"idle" | "uploading" | "writing">("idle");
  let progress = $state(0);
  let current = $state("");
  let over = $state(false);
  let error = $state("");
  let input: HTMLInputElement | undefined = $state();

  function start(file: File | undefined) {
    if (!file || phase !== "idle") return;
    if (!file.name.toLowerCase().endsWith(".zip")) {
      error = t("set.imp.notZip");
      return;
    }
    if (file.size > MAX_BYTES) {
      error = t("set.imp.tooLarge");
      return;
    }
    error = "";
    current = file.name;
    const rec: Import = { id: nextId(), file: file.name, status: "importing", from: "", to: "", errorCode: "", by: USER.name, createdAt: NOW };
    cfg.imports.unshift(rec);
    phase = "uploading";
    progress = 0;
    const timer = setInterval(() => {
      progress = Math.min(100, progress + 9);
      if (progress < 100) return;
      clearInterval(timer);
      phase = "writing";
      setTimeout(() => finish(rec.id), 1200);
    }, 110);
  }

  function finish(id: number) {
    const rec = cfg.imports.find((i) => i.id === id);
    phase = "idle";
    if (input) input.value = "";
    if (!rec) return;
    // 导出覆盖到昨天；只取站点自己的数据开始之前的日子
    const to = shift(first, -1);
    const from = shift(to, -699);
    const clash = cfg.imports.find((i) => i.status === "completed" && i.from <= to && i.to >= from);
    if (clash) {
      rec.status = "failed";
      rec.errorCode = "overlap";
      toast(t("set.imp.fail"), "bad");
      return;
    }
    rec.status = "completed";
    rec.from = from;
    rec.to = to;
    toast(t("set.imp.done", { n: days(from, to) }), "good");
  }

  function onDrop(e: DragEvent) {
    e.preventDefault();
    over = false;
    start(e.dataTransfer?.files[0]);
  }

  // ---- 删除 ----
  let deleting = $state<Import | null>(null);
  let delOpen = $state(false);
  function remove(rec: Import) {
    if (rec.status === "failed") {
      cfg.imports = cfg.imports.filter((i) => i.id !== rec.id);
      return;
    }
    deleting = rec;
    delOpen = true;
  }
  function confirmDelete() {
    cfg.imports = cfg.imports.filter((i) => i.id !== deleting?.id);
    delOpen = false;
    toast(t("set.imp.deleted"));
  }
</script>

<Card title={t("set.imp.title")}>
  <div class="intro">
    <p>{t("set.imp.how")}</p>
    {#if !loading && !failed}
      <p class="muted">
        {hasData ? t("set.imp.before", { d: first }) : t("set.imp.beforeEmpty")}
        {t("set.imp.cutover")}
      </p>
    {/if}
  </div>

  <div
    class="zone"
    class:over
    class:busy={phase !== "idle"}
    role="group"
    aria-label={t("set.imp.title")}
    ondragover={(e) => {
      e.preventDefault();
      if (phase === "idle") over = true;
    }}
    ondragleave={() => (over = false)}
    ondrop={onDrop}
  >
    {#if phase !== "idle"}
      <div class="progress" role="status">
        <div class="line">
          <span class="file">{current}</span>
          <span class="muted num">{phase === "uploading" ? t("set.imp.uploading", { p: progress }) : t("set.imp.writing")}</span>
        </div>
        <div class="bar" class:writing={phase === "writing"} aria-hidden="true">
          <span style:transform="scaleX({phase === 'writing' ? 1 : progress / 100})"></span>
        </div>
      </div>
    {:else}
      <!-- 整个区域都能点；触屏没有拖放，只说「选择」 -->
      <button type="button" class="pick" disabled={loading || failed} onclick={() => input?.click()}>
        <FileUp size={18} strokeWidth={1.75} aria-hidden="true" />
        {#if over}<span>{t("set.imp.dropActive")}</span>{:else}
          <span class="drag">{t("set.imp.drop")}</span>
          <span class="link">{t("set.imp.pick")}</span>
          <span class="touch">{t("set.imp.pickZip")}</span>
        {/if}
      </button>
    {/if}
    <input bind:this={input} type="file" accept=".zip,application/zip" hidden onchange={(e) => start(e.currentTarget.files?.[0])} />
  </div>
  {#if error}<p class="err" role="alert">{error}</p>{/if}
</Card>

<Card title={t("set.imp.list")} flush>
  {#if failed}
    <EmptyState compact icon={CircleAlert} tone="bad" title={t("set.loadFailed")}>
      {#snippet actions()}<Button onclick={() => toast(t("common.retry"))}>{t("common.retry")}</Button>{/snippet}
    </EmptyState>
  {:else if loading}
    <div class="sk" aria-busy="true"><Skeleton height={32} radius={6} /><Skeleton height={32} radius={6} /></div>
  {:else if cfg.imports.length === 0}
    <EmptyState compact title={t("set.imp.none")} />
  {:else}
    <Table
      caption={t("set.imp.list")}
      rows={cfg.imports}
      rowKey={(i) => i.id}
      columns={[
        { key: "file", label: t("set.imp.col.file") },
        { key: "status", label: t("set.imp.col.status"), width: "96px" },
        { key: "by", label: t("set.imp.col.by"), width: "104px", optional: true },
        { key: "x", label: "", width: "56px", align: "right" },
      ]}
    >
      {#snippet cell(i, c)}
        {#if c.key === "file"}
          <span class="rec">
            <span class="file">{i.file}</span>
            {#if i.status === "completed"}
              <span class="muted num">{t("set.imp.range", { a: i.from, b: i.to, n: days(i.from, i.to) })}</span>
            {:else if i.status === "failed"}
              <span class="bad">{t(`set.imp.err.${i.errorCode || "internal"}`)}</span>
            {/if}
          </span>
        {:else if c.key === "status"}
          {#if i.status === "importing"}<Badge tone="info" dot>{t("set.imp.importing")}</Badge>
          {:else if i.status === "completed"}<Badge tone="good">{t("set.imp.completed")}</Badge>
          {:else}<Badge tone="bad">{t("set.imp.failed")}</Badge>{/if}
        {:else if c.key === "by"}
          <span class="by">
            <span>{i.by}</span>
            <span class="muted num">{ymd(i.createdAt)}</span>
          </span>
        {:else if i.status !== "importing"}
          <Button
            variant="ghost"
            size="sm"
            icon={Trash2}
            aria-label="{t('set.imp.delete')} {i.file}"
            title={i.status === "failed" ? t("set.imp.del.failed") : t("set.imp.delete")}
            onclick={() => remove(i)}
          />
        {/if}
      {/snippet}
    </Table>
  {/if}
</Card>

<Dialog bind:open={delOpen} size="sm" title={t("set.imp.del.title", { n: deleting?.file ?? "" })}>
  <p>{t("set.imp.del.body", { a: deleting?.from ?? "", b: deleting?.to ?? "" })}</p>
  {#snippet footer()}
    <Button onclick={() => (delOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" onclick={confirmDelete}>{t("set.imp.delete")}</Button>
  {/snippet}
</Dialog>

<style>
  .intro {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    margin-bottom: var(--sp-4);
  }
  /* 上传区：空闲、拖入、进度三种状态共用一个框，高度固定，切换时不挤动下方内容 */
  .zone {
    display: flex;
    align-items: center;
    height: 88px;
    border-radius: var(--r-sm);
    background: var(--bg-subtle);
    box-shadow: inset 0 0 0 var(--hairline) var(--border);
    color: var(--text-muted);
    transition:
      background-color var(--dur-fast) var(--ease),
      box-shadow var(--dur-fast) var(--ease),
      color var(--dur-fast) var(--ease);
  }
  .zone.over {
    background: var(--accent-bg);
    box-shadow: inset 0 0 0 1px var(--accent-border);
    color: var(--accent-text);
  }
  .zone:not(.busy):hover {
    background: var(--bg-element);
  }
  .pick {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-wrap: wrap;
    gap: 4px var(--sp-2);
    width: 100%;
    height: 100%;
    padding: 0 var(--sp-4);
    border: 0;
    border-radius: inherit;
    background: none;
    color: inherit;
    font: inherit;
    cursor: pointer;
  }
  .pick:disabled {
    cursor: default;
  }
  .pick:focus-visible {
    outline: 2px solid var(--accent-focus);
    outline-offset: -2px;
  }
  .link {
    margin-left: -4px;
    color: var(--accent-text);
    font-weight: var(--fw-medium);
  }
  .touch {
    display: none;
    color: var(--accent-text);
    font-weight: var(--fw-medium);
  }
  @media (hover: none) {
    .drag,
    .link {
      display: none;
    }
    .touch {
      display: inline;
    }
  }
  .progress {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    width: 100%;
    padding: 0 var(--sp-4);
    animation: fade-in var(--dur-pop) var(--ease);
  }
  .line {
    display: flex;
    justify-content: space-between;
    gap: var(--sp-4);
    color: var(--text);
  }
  .bar {
    position: relative;
    height: 4px;
    overflow: hidden;
    border-radius: 2px;
    background: var(--bg-element);
  }
  .bar span {
    position: absolute;
    inset: 0;
    border-radius: inherit;
    background: var(--accent-solid);
    transform-origin: left;
    transition: transform var(--dur) var(--ease-move);
  }
  /* 写入阶段没有可量的进度：满条上一道光扫过，表示仍在进行 */
  .bar.writing span::after {
    content: "";
    position: absolute;
    inset: 0;
    /* design-lint-allow raw-color effect: 已登记的例外，全站唯一一处渐变；去留见 docs/ui/design-review-2026-11.md */
    background: linear-gradient(90deg, transparent, rgb(255 255 255 / 0.45), transparent);
    transform: translateX(-100%);
    /* design-lint-allow motion: 循环动画的周期，不属于 --dur 的三档 */
    animation: sweep 1.1s var(--ease-move) infinite;
  }
  @keyframes sweep {
    to {
      transform: translateX(100%);
    }
  }
  @keyframes fade-in {
    from {
      opacity: 0;
    }
  }
  .file {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: var(--fw-medium);
  }
  .err {
    margin-top: var(--sp-2);
    color: var(--bad-text);
  }
  .rec {
    display: flex;
    flex-direction: column;
    min-width: 0;
    padding: 4px 0;
  }
  .rec .muted,
  .rec .bad,
  .by .muted {
    font-size: var(--fs-sm);
  }
  .bad {
    color: var(--bad-text);
  }
  .by {
    display: flex;
    flex-direction: column;
    padding: 4px 0;
  }
  .sk {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-4);
  }
</style>
