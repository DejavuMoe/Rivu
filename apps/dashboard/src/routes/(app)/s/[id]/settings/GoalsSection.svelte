<!--
  目标：事件（事件名）、页面（路径，* 是通配）与滚动深度（阈值，可限定页面）。看板的目标卡片按目标的名字
  （显示名，否则目标值）列出转化。
  添加对话框做初筛，服务端校验为准（400 的 message 显示在字段旁）；上限由服务端给出，用量显示在表头右侧。
  editor 起（含站点访客 editor）可写；没有写权限时不显示添加与删除入口。
-->
<script lang="ts">
  import { CircleAlert, Plus, Trash2 } from "@lucide/svelte";
  import Button from "#lib/ui/Button.svelte";
  import Card from "#lib/ui/Card.svelte";
  import Dialog from "#lib/ui/Dialog.svelte";
  import EmptyState from "#lib/ui/EmptyState.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import Segmented from "#lib/ui/Segmented.svelte";
  import Select from "#lib/ui/Select.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import Table from "#lib/ui/Table.svelte";
  import {
    eventNameError,
    goalTarget,
    isDuplicateGoal,
    labelTaken,
    normalizePagePath,
    pagePathError,
    SCROLL_ALL_PAGES,
    SCROLL_DEPTHS,
    scrollLabel,
  } from "#lib/admin/goals.ts";
  import { api, ApiError, type Goal, type GoalKind, type Site } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import { toast } from "#lib/state.svelte.ts";
  import { errText } from "./settings.svelte.ts";
  import "./strings";

  let { site, readOnly = false }: { site: Site; readOnly?: boolean } = $props();

  const siteId = $derived(site.id);
  let goals = $state<Goal[] | null>(null);
  let max = $state(100);
  let failed = $state(false);

  async function load() {
    failed = false;
    try {
      const r = await api.goals(siteId);
      goals = r.goals;
      max = r.max_goals;
    } catch {
      failed = true;
    }
  }
  // 布局按站点 id 重建本组件，这里只在挂载时加载一次
  $effect(() => {
    void load();
  });

  const used = $derived(goals?.length ?? 0);
  const full = $derived(used >= max);

  let open = $state(false);
  let kind = $state<GoalKind>("event");
  let eventName = $state("");
  let pagePath = $state("");
  let depth = $state("75");
  let depthPath = $state("");
  let display = $state("");
  let busy = $state(false);
  let serverErr = $state("");

  /** 滚动深度目标的路径：不填是所有页面 */
  const scrollPath = $derived(normalizePagePath(depthPath) || SCROLL_ALL_PAGES);
  const target = $derived(kind === "event" ? eventName.trim() : kind === "page" ? normalizePagePath(pagePath) : scrollPath);
  const label = $derived(display.trim() || (kind === "scroll" ? scrollLabel(Number(depth)) : target));
  const targetErr = $derived(kind === "event" ? eventNameError(eventName) : pagePathError(kind === "page" ? pagePath : depthPath));
  const dup = $derived(!!goals && isDuplicateGoal(goals, kind, target, kind === "scroll" ? Number(depth) : null));
  const nameTaken = $derived(!dup && !!goals && labelTaken(goals, label));
  const errKey = $derived(targetErr ?? (dup ? "set.goals.err.dup" : null));
  const targetMsg = $derived(serverErr || (target && errKey ? t(errKey) : ""));
  /** 滚动深度：重复与服务端的错误显示在阈值旁，路径格式错误显示在路径旁 */
  const depthMsg = $derived(serverErr || (dup ? t("set.goals.err.dup") : ""));
  const depthPathMsg = $derived(targetErr ? t(targetErr) : "");
  const canAdd = $derived(!!target && !errKey && !nameTaken && !full && !busy);

  function openAdd() {
    kind = "event";
    eventName = pagePath = depthPath = display = "";
    depth = "75";
    serverErr = "";
    open = true;
  }

  async function add() {
    if (!canAdd || !goals) return;
    busy = true;
    try {
      const display_name = display.trim() || undefined;
      const body =
        kind === "event"
          ? { kind, event_name: target, display_name }
          : kind === "page"
            ? { kind, page_path: target, display_name }
            : { kind, page_path: target, scroll_threshold: Number(depth), display_name };
      const { goal } = await api.addGoal(site.id, body);
      goals.push(goal);
      open = false;
      toast(t("set.goals.added"), "good");
    } catch (e) {
      if (e instanceof ApiError && e.code === "limit_reached") serverErr = t("set.goals.err.full");
      else if (e instanceof ApiError && e.status === 409) serverErr = t("set.goals.err.dup");
      else if (e instanceof ApiError && e.status === 400) serverErr = e.message;
      else toast(errText(e), "bad");
    } finally {
      busy = false;
    }
  }

  let removing = $state<number | null>(null);
  let removeOpen = $state(false);
  const removingGoal = $derived(goals?.find((g) => g.id === removing));
  async function confirmRemove() {
    if (removing === null || !goals || busy) return;
    const id = removing;
    busy = true;
    try {
      await api.removeGoal(site.id, id);
      goals = goals.filter((g) => g.id !== id);
      removeOpen = false;
      toast(t("set.goals.removed"));
    } catch (e) {
      toast(errText(e), "bad");
    } finally {
      busy = false;
    }
  }
</script>

<Card title={t("set.goals.title")} flush>
  {#snippet actions()}
    {#if goals}
      <span class="used num">{t("set.goals.used", { n: used, max })}</span>
      {#if !readOnly}<Button variant="primary" icon={Plus} disabled={full} onclick={openAdd}>{t("set.goals.add")}</Button>{/if}
    {/if}
  {/snippet}
  {#if failed}
    <EmptyState compact icon={CircleAlert} tone="bad" title={t("set.loadFailed")}>
      {#snippet actions()}<Button onclick={load}>{t("common.retry")}</Button>{/snippet}
    </EmptyState>
  {:else if goals === null}
    <div class="sk" aria-busy="true"><Skeleton height={32} radius={6} /><Skeleton height={32} radius={6} /></div>
  {:else if goals.length === 0}
    <EmptyState compact title={t("set.goals.none")} />
  {:else}
    <Table
      caption={t("set.goals.title")}
      rows={goals}
      rowKey={(g) => g.id}
      columns={[
        { key: "kind", label: t("set.goals.col.kind"), width: "112px" },
        { key: "target", label: t("set.goals.col.target") },
        { key: "display", label: t("set.goals.col.display"), width: "160px", optional: true },
        ...(readOnly ? [] : [{ key: "x", label: "", width: "56px", align: "right" as const }]),
      ]}
    >
      {#snippet cell(g, c)}
        {#if c.key === "kind"}
          {t(`set.goals.kind.${g.kind}`)}
        {:else if c.key === "target"}
          <div class="tg">
            <code>{goalTarget(g)}</code>
            {#if g.kind === "scroll" && g.page_path && g.page_path !== SCROLL_ALL_PAGES}<span class="muted">{g.page_path}</span>{/if}
          </div>
        {:else if c.key === "display"}
          {#if g.display_name}{g.display_name}{:else}<span class="muted">—</span>{/if}
        {:else}
          <Button
            variant="ghost"
            size="sm"
            icon={Trash2}
            aria-label="{t('common.delete')} {g.label}"
            onclick={() => {
              removing = g.id;
              removeOpen = true;
            }}
          />
        {/if}
      {/snippet}
    </Table>
  {/if}
</Card>

<Dialog bind:open title={t("set.goals.dlg.title")}>
  <form
    id="goal-form"
    class="form"
    onsubmit={(e) => {
      e.preventDefault();
      void add();
    }}
  >
    <Field label={t("set.goals.col.kind")}>
      <Segmented
        label={t("set.goals.col.kind")}
        block
        bind:value={kind}
        onchange={() => (serverErr = "")}
        options={[
          { value: "event", label: t("set.goals.kind.event") },
          { value: "page", label: t("set.goals.kind.page") },
          { value: "scroll", label: t("set.goals.kind.scroll") },
        ]}
      />
    </Field>

    {#if kind === "event"}
      <Field label={t("set.goals.eventName")} for="g-event" hint={t("set.goals.eventHint")} error={targetMsg || undefined}>
        <Input
          id="g-event"
          mono
          bind:value={eventName}
          oninput={() => (serverErr = "")}
          placeholder="Signup"
          invalid={!!targetMsg}
          autocomplete="off"
          spellcheck={false}
        />
      </Field>
    {:else if kind === "page"}
      <Field label={t("set.goals.pagePath")} for="g-page" hint={t("set.goals.pageHint")} error={targetMsg || undefined}>
        <Input
          id="g-page"
          mono
          bind:value={pagePath}
          oninput={() => (serverErr = "")}
          placeholder="/pricing"
          invalid={!!targetMsg}
          autocomplete="off"
          spellcheck={false}
        />
      </Field>
    {:else}
      <Field label={t("set.goals.depth")} error={depthMsg || undefined}>
        <Select
          label={t("set.goals.depth")}
          block
          bind:value={depth}
          onchange={() => (serverErr = "")}
          options={SCROLL_DEPTHS.map((d) => ({ value: String(d), label: `${d}%` }))}
        />
      </Field>
      <Field label={t("set.goals.depthPath")} for="g-dpath" optional hint={t("set.goals.pageHint")} error={depthPathMsg || undefined}>
        <Input
          id="g-dpath"
          mono
          bind:value={depthPath}
          oninput={() => (serverErr = "")}
          placeholder="/blog/*"
          invalid={!!depthPathMsg}
          autocomplete="off"
          spellcheck={false}
        />
      </Field>
    {/if}

    <Field label={t("set.goals.col.display")} for="g-display" optional error={nameTaken ? t("set.goals.err.name") : undefined}>
      <Input
        id="g-display"
        bind:value={display}
        maxlength={100}
        invalid={nameTaken}
        placeholder={t("set.goals.displayPlaceholder")}
      />
    </Field>
  </form>
  {#snippet footer()}
    <Button onclick={() => (open = false)}>{t("common.cancel")}</Button>
    <Button variant="primary" type="submit" form="goal-form" loading={busy} disabled={!canAdd}>{t("common.add")}</Button>
  {/snippet}
</Dialog>

<Dialog bind:open={removeOpen} size="sm" title={t("set.goals.rm.title")}>
  <p>{t("set.goals.rm.body", { g: removingGoal?.label ?? "" })}</p>
  {#snippet footer()}
    <Button onclick={() => (removeOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" loading={busy} onclick={confirmRemove}>{t("common.delete")}</Button>
  {/snippet}
</Dialog>

<style>
  .used {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .sk {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    padding: var(--sp-4) var(--sp-5);
  }
  .tg {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 6px 0;
    min-width: 0;
  }
  .tg code {
    overflow-wrap: anywhere;
  }
  .tg .muted {
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
</style>
