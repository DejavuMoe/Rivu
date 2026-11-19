<!--
  目标：事件 / 页面 / 滚动深度，每个目标最多 3 个属性条件。编辑者可写，没有写权限时不显示添加与删除入口。
-->
<script lang="ts">
  import { Plus, Trash2, X } from "@lucide/svelte";
  import Badge from "../../lib/ui/Badge.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Segmented from "../../lib/ui/Segmented.svelte";
  import Select from "../../lib/ui/Select.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import { t } from "../../lib/i18n";
  import { toast } from "../../lib/state.svelte";
  import type { Site } from "../../lib/data/fixtures";
  import { cfgs, nextId, type Goal, type GoalKind } from "./store.svelte";
  import "./strings";

  let { site, readOnly = false }: { site: Site; readOnly?: boolean } = $props();
  const cfg = $derived(cfgs[site.id]);

  let open = $state(false);
  let kind = $state<GoalKind>("event");
  let eventName = $state("");
  let pagePath = $state("");
  let depth = $state("75");
  let depthPath = $state("");
  let display = $state("");
  let conds = $state<{ key: string; value: string }[]>([]);

  const target = $derived(kind === "event" ? eventName.trim() : kind === "page" ? pagePath.trim() : depth);
  const targetErr = $derived.by((): string | null => {
    if (!target) return null;
    if (kind === "event") return target.length > 120 ? "set.goals.err.eventLen" : null;
    if (kind === "page") return /^\/\S*$/.test(target) ? null : "set.goals.err.page";
    return depthPath.trim() && !/^\/\S*$/.test(depthPath.trim()) ? "set.goals.err.page" : null;
  });
  const dup = $derived(
    !!target && cfg.goals.some((g) => g.kind === kind && g.target === target && g.path === (kind === "scroll" ? depthPath.trim() : "")),
  );
  const condsBad = $derived(conds.some((c) => !c.key.trim() || !c.value.trim()));
  const errKey = $derived(targetErr ?? (dup ? "set.goals.err.dup" : null));
  const canAdd = $derived(!!target && !errKey && !condsBad);

  function openAdd() {
    kind = "event";
    eventName = pagePath = depthPath = display = "";
    depth = "75";
    conds = [];
    open = true;
  }
  function add() {
    if (!canAdd) return;
    const g: Goal = {
      id: nextId(),
      kind,
      target,
      path: kind === "scroll" ? depthPath.trim() : "",
      display: display.trim(),
      props: conds.map((c) => ({ key: c.key.trim(), value: c.value.trim() })),
    };
    cfg.goals.push(g);
    open = false;
    toast(t("set.goals.added"), "good");
  }

  let removing = $state<number | null>(null);
  let removeOpen = $state(false);
  const removingGoal = $derived(cfg.goals.find((g) => g.id === removing));
  function confirmRemove() {
    cfg.goals = cfg.goals.filter((g) => g.id !== removing);
    removeOpen = false;
    toast(t("set.goals.removed"));
  }

  const targetText = (g: Goal) => (g.kind === "scroll" ? `≥ ${g.target}%` : g.target);
</script>

<Card title={t("set.goals.title")} flush>
  {#snippet actions()}
    {#if !readOnly}<Button variant="primary" icon={Plus} onclick={openAdd}>{t("set.goals.add")}</Button>{/if}
  {/snippet}
  {#if cfg.goals.length === 0}
    <EmptyState compact title={t("set.goals.none")} />
  {:else}
    <Table
      caption={t("set.goals.title")}
      rows={cfg.goals}
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
            <code>{targetText(g)}</code>
            {#if g.path}<span class="muted">{g.path}</span>{/if}
            {#if g.props.length}
              <span class="conds">
                {#each g.props as p (p.key)}<span class="cond"><code>{p.key}</code> = <code>{p.value}</code></span>{/each}
              </span>
            {/if}
          </div>
        {:else if c.key === "display"}
          {#if g.display}{g.display}{:else}<span class="muted">—</span>{/if}
        {:else}
          <Button
            variant="ghost"
            size="sm"
            icon={Trash2}
            aria-label="{t('common.delete')} {g.display || g.target}"
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
      add();
    }}
  >
    <Field label={t("set.goals.col.kind")}>
      <Segmented
        label={t("set.goals.col.kind")}
        block
        bind:value={kind}
        options={[
          { value: "event", label: t("set.goals.kind.event") },
          { value: "page", label: t("set.goals.kind.page") },
          { value: "scroll", label: t("set.goals.kind.scroll") },
        ]}
      />
    </Field>

    {#if kind === "event"}
      <Field label={t("set.goals.eventName")} for="g-event" hint={t("set.goals.eventHint")} error={target && errKey ? t(errKey) : undefined}>
        <Input id="g-event" mono bind:value={eventName} placeholder="Signup" invalid={!!target && !!errKey} autocomplete="off" spellcheck={false} />
      </Field>
    {:else if kind === "page"}
      <Field label={t("set.goals.pagePath")} for="g-page" error={target && errKey ? t(errKey) : undefined}>
        <Input id="g-page" mono bind:value={pagePath} placeholder="/pricing" invalid={!!target && !!errKey} autocomplete="off" spellcheck={false} />
      </Field>
    {:else}
      <Field label={t("set.goals.depth")} error={errKey === "set.goals.err.dup" ? t(errKey) : undefined}>
        <Select
          label={t("set.goals.depth")}
          block
          bind:value={depth}
          options={["25", "50", "75", "90", "100"].map((d) => ({ value: d, label: `${d}%` }))}
        />
      </Field>
      <Field label={t("set.goals.depthPath")} for="g-dpath" optional error={errKey === "set.goals.err.page" ? t(errKey) : undefined}>
        <Input id="g-dpath" mono bind:value={depthPath} placeholder="/blog/*" invalid={errKey === "set.goals.err.page"} autocomplete="off" spellcheck={false} />
      </Field>
    {/if}

    <Field label={t("set.goals.col.display")} for="g-display" optional>
      <Input id="g-display" bind:value={display} maxlength={80} placeholder={t("set.goals.displayPlaceholder")} />
    </Field>

    <Field label={t("set.goals.conds")} optional>
      {#each conds as c, i (i)}
        <div class="cond-row">
          <Input size="sm" mono bind:value={c.key} placeholder={t("set.goals.condKey")} aria-label={t("set.goals.condKey")} autocomplete="off" />
          <span class="eq" aria-hidden="true">=</span>
          <Input size="sm" mono bind:value={c.value} placeholder={t("set.goals.condValue")} aria-label={t("set.goals.condValue")} autocomplete="off" />
          <Button variant="ghost" size="sm" icon={X} aria-label={t("common.remove")} onclick={() => (conds = conds.filter((_, j) => j !== i))} />
        </div>
      {/each}
      {#if conds.length < 3}
        <div><Button size="sm" variant="ghost" icon={Plus} onclick={() => (conds = [...conds, { key: "", value: "" }])}>{t("set.goals.addCond")}</Button></div>
      {/if}
    </Field>  </form>
  {#snippet footer()}
    <Button onclick={() => (open = false)}>{t("common.cancel")}</Button>
    <Button variant="primary" type="submit" form="goal-form" disabled={!canAdd}>{t("common.add")}</Button>
  {/snippet}
</Dialog>

<Dialog bind:open={removeOpen} size="sm" title={t("set.goals.rm.title")}>
  <p>{t("set.goals.rm.body", { g: removingGoal?.display || removingGoal?.target || "" })}</p>
  {#snippet footer()}
    <Button onclick={() => (removeOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" onclick={confirmRemove}>{t("common.delete")}</Button>
  {/snippet}
</Dialog>

<style>
  .tg {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 6px 0;
    min-width: 0;
  }
  .tg .muted {
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
  }
  .conds {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .cond {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    height: 20px;
    padding: 0 6px;
    border-radius: var(--r-xs);
    background: var(--bg-element);
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .cond-row {
    display: grid;
    grid-template-columns: 1fr auto 1fr auto;
    align-items: center;
    gap: 6px;
  }
  .eq {
    color: var(--text-muted);
  }
</style>