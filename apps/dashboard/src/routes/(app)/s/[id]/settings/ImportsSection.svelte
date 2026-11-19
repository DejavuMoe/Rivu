<!--
  导入：把 Plausible 导出的 zip 拖进上传区（或点选），上传后服务端在后台写入日汇总层，这里轮询列表直到导入结束。
  上传区在导入期间原地变成进度（同一个框、同样高度，不挤动下面的列表）；一个站点同时只跑一个导入，
  所以有进行中的导入时（包括别人发起的、刷新页面前发起的）上传区显示它的进度。
  导入记录列出日期范围与状态；删除已完成的导入要确认，失败的记录直接删。
-->
<script lang="ts">
  import { CircleAlert, FileUp, Trash2 } from "@lucide/svelte";
  import Badge from "#lib/ui/Badge.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Card from "#lib/ui/Card.svelte";
  import Dialog from "#lib/ui/Dialog.svelte";
  import EmptyState from "#lib/ui/EmptyState.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import Table from "#lib/ui/Table.svelte";
  import { localYmd } from "#lib/admin/expiry.ts";
  import { api, ApiError, type Site, type SiteImport } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import { toast } from "#lib/state.svelte.ts";
  import { errText } from "./settings.svelte.ts";
  import "./strings";

  let { site }: { site: Site } = $props();

  /** 与服务端 IMPORT_BODY_MAX 一致 */
  const MAX_BYTES = 128 * 1024 * 1024;
  const POLL_MS = 1500;

  const siteId = $derived(site.id);
  let rows = $state<SiteImport[] | null>(null);
  let nativeFrom = $state<string | null>(null);
  let failed = $state(false);

  async function load() {
    try {
      const r = await api.imports(siteId);
      rows = r.imports;
      nativeFrom = r.native_from;
      failed = false;
    } catch {
      // 轮询中的偶发失败不打断页面；首次加载失败才显示错误
      if (rows === null) failed = true;
    }
  }
  // 布局按站点 id 重建本组件，这里只在挂载时加载一次
  $effect(() => {
    void load();
  });

  const running = $derived(rows?.find((r) => r.status === "importing"));
  $effect(() => {
    if (!running) return;
    const timer = setInterval(load, POLL_MS);
    return () => clearInterval(timer);
  });

  // 本页发起的导入结束时给出结果
  let watching = $state<number | null>(null);
  $effect(() => {
    if (watching === null || !rows) return;
    const r = rows.find((x) => x.id === watching);
    if (!r || r.status === "importing") return;
    watching = null;
    if (r.status === "completed") toast(t("set.imp.done", { n: days(r) }), "good");
    else toast(t("set.imp.fail"), "bad");
  });

  const days = (r: SiteImport) =>
    r.start_date && r.end_date ? Math.round((Date.parse(r.end_date) - Date.parse(r.start_date)) / 864e5) + 1 : 0;

  // ---- 上传 ----
  let uploading = $state<{ name: string; progress: number } | null>(null);
  let over = $state(false);
  let error = $state("");
  let input: HTMLInputElement | undefined = $state();
  const busy = $derived(!!uploading || !!running);

  async function start(file: File | undefined) {
    if (!file || busy) return;
    if (!file.name.toLowerCase().endsWith(".zip")) {
      error = t("set.imp.notZip");
      return;
    }
    if (file.size > MAX_BYTES) {
      error = t("set.imp.tooLarge");
      return;
    }
    error = "";
    uploading = { name: file.name, progress: 0 };
    try {
      const { import: rec } = await api.uploadImport(siteId, file, (p) => {
        if (uploading) uploading.progress = p;
      });
      rows = [rec, ...(rows ?? []).filter((r) => r.id !== rec.id)];
      watching = rec.id;
    } catch (e) {
      if (e instanceof ApiError && e.code === "import_running") {
        error = t("set.imp.running");
        void load();
      } else if (e instanceof ApiError && e.status === 413) {
        error = t(e.code === "payload_too_large" ? "set.imp.tooLarge" : "set.imp.proxyLimit");
      } else toast(errText(e), "bad");
    } finally {
      uploading = null;
      if (input) input.value = "";
    }
  }

  function onDrop(e: DragEvent) {
    e.preventDefault();
    over = false;
    start(e.dataTransfer?.files[0]);
  }

  // ---- 删除 ----
  let deleting = $state<SiteImport | null>(null);
  let delOpen = $state(false);
  let delBusy = $state(false);

  async function removeRow(r: SiteImport) {
    if (r.status !== "failed") {
      deleting = r;
      delOpen = true;
      return;
    }
    try {
      await api.removeImport(siteId, r.id);
      rows = (rows ?? []).filter((x) => x.id !== r.id);
    } catch (e) {
      toast(errText(e), "bad");
    }
  }

  async function confirmDelete() {
    if (!deleting || delBusy) return;
    const id = deleting.id;
    delBusy = true;
    try {
      await api.removeImport(siteId, id);
      rows = (rows ?? []).filter((x) => x.id !== id);
      delOpen = false;
      toast(t("set.imp.deleted"));
    } catch (e) {
      toast(errText(e), "bad");
    } finally {
      delBusy = false;
    }
  }
</script>

<Card title={t("set.imp.title")}>
  <div class="intro">
    <p>{t("set.imp.how")}</p>
    {#if rows}
      <p class="muted">
        {nativeFrom ? t("set.imp.before", { d: nativeFrom }) : t("set.imp.beforeEmpty")}
        {t("set.imp.cutover")}
      </p>
    {/if}
  </div>

  <div
    class="zone"
    class:over
    class:busy
    role="group"
    aria-label={t("set.imp.title")}
    ondragover={(e) => {
      e.preventDefault();
      if (!busy) over = true;
    }}
    ondragleave={() => (over = false)}
    ondrop={onDrop}
  >
    {#if uploading}
      <div class="progress" role="status">
        <div class="line">
          <span class="file">{uploading.name}</span>
          <span class="muted num">{t("set.imp.uploading", { p: Math.round(uploading.progress * 100) })}</span>
        </div>
        <div class="bar" aria-hidden="true"><span style:transform="scaleX({uploading.progress})"></span></div>
      </div>
    {:else if running}
      <div class="progress" role="status">
        <div class="line">
          <span class="file">{running.file_name}</span>
          <span class="muted">{t("set.imp.writing")}</span>
        </div>
        <div class="bar writing" aria-hidden="true"><span></span></div>
      </div>
    {:else}
      <!-- 整个区域都能点；触屏没有拖放，只说「选择」 -->
      <button type="button" class="pick" disabled={rows === null} onclick={() => input?.click()}>
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
      {#snippet actions()}<Button onclick={load}>{t("common.retry")}</Button>{/snippet}
    </EmptyState>
  {:else if rows === null}
    <div class="sk" aria-busy="true"><Skeleton height={32} radius={6} /><Skeleton height={32} radius={6} /></div>
  {:else if rows.length === 0}
    <EmptyState compact title={t("set.imp.none")} />
  {:else}
    <Table
      caption={t("set.imp.list")}
      {rows}
      rowKey={(r) => r.id}
      columns={[
        { key: "file", label: t("set.imp.col.file") },
        { key: "status", label: t("set.imp.col.status"), width: "96px" },
        { key: "by", label: t("set.imp.col.by"), width: "104px", optional: true },
        { key: "x", label: "", width: "56px", align: "right" },
      ]}
    >
      {#snippet cell(r, c)}
        {#if c.key === "file"}
          <span class="rec">
            <span class="file">{r.file_name}</span>
            {#if r.status === "completed"}
              <span class="muted num">{t("set.imp.range", { a: r.start_date ?? "", b: r.end_date ?? "", n: days(r) })}</span>
            {:else if r.status === "failed"}
              <span class="bad">{t(`set.imp.err.${r.error_code ?? "internal"}`)}</span>
            {/if}
          </span>
        {:else if c.key === "status"}
          {#if r.status === "importing"}<Badge tone="info" dot>{t("set.imp.importing")}</Badge>
          {:else if r.status === "completed"}<Badge tone="good">{t("set.imp.completed")}</Badge>
          {:else}<Badge tone="bad">{t("set.imp.failed")}</Badge>{/if}
        {:else if c.key === "by"}
          <span class="by">
            {#if r.imported_by}<span>{r.imported_by}</span>{/if}
            <span class="muted num">{localYmd(new Date(r.created_at))}</span>
          </span>
        {:else if r.status !== "importing"}
          <Button
            variant="ghost"
            size="sm"
            icon={Trash2}
            aria-label="{t('set.imp.delete')} {r.file_name}"
            title={r.status === "failed" ? t("set.imp.del.failed") : t("set.imp.delete")}
            onclick={() => removeRow(r)}
          />
        {/if}
      {/snippet}
    </Table>
  {/if}
</Card>

<Dialog bind:open={delOpen} size="sm" title={t("set.imp.del.title", { n: deleting?.file_name ?? "" })}>
  <p>{t("set.imp.del.body", { a: deleting?.start_date ?? "", b: deleting?.end_date ?? "" })}</p>
  {#snippet footer()}
    <Button onclick={() => (delOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" loading={delBusy} onclick={confirmDelete}>{t("set.imp.delete")}</Button>
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
  .zone:not(.busy):hover {
    background: var(--bg-element);
  }
  .zone.over {
    background: var(--accent-bg);
    box-shadow: inset 0 0 0 1px var(--accent-border);
    color: var(--accent-text);
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
