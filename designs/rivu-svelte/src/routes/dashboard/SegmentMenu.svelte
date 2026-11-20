<!--
  分群：筛选栏里的一个按钮（用着分群时变成「分群：名称」加一个移除按钮），点开是分群列表。
  点列表里的分群就应用它（换掉当前的临时筛选）；之后再加的筛选叠加在分群上。
  editor 起（canWrite）多两项：把当前筛选保存为分群、管理分群（改名、删除）。查看者只能应用，
  站点还没有分群时查看者看不到这个按钮。共享链接与公开看板不显示（DashboardPage 不传进来）。
  被共享链接绑定的分群不能删除：先说明是哪些链接在用，给出去共享链接设置的入口。
-->
<script lang="ts">
  import { Bookmark, BookmarkPlus, Pencil, Settings2, Trash2, X } from "@lucide/svelte";
  import Popover from "../../lib/ui/Popover.svelte";
  import MenuItem from "../../lib/ui/MenuItem.svelte";
  import MenuLabel from "../../lib/ui/MenuLabel.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import { href } from "../../lib/router.svelte";
  import { canShareLinks } from "../../lib/data/sections";
  import type { Site } from "../../lib/data/fixtures";
  import { PROP_PREFIX, isPropDim, type Filter } from "./query";
  import { displayValue } from "./values";
  import {
    SEGMENT_NAME_MAX,
    addSegment,
    deleteSegment,
    linksUsing,
    nameTaken,
    renameSegment,
    segmentsOf,
    type Segment,
  } from "./segments.svelte";
  import "./segment-strings";
  import "../settings/strings";

  interface Props {
    site: Site;
    /** 正在使用的分群 */
    active: Segment | undefined;
    /** 分群之外临时加的筛选（URL 的 f=） */
    filters: Filter[];
    /** editor 起：能保存、改名、删除 */
    canWrite: boolean;
    /** 应用分群（null 表示不再使用）；应用时换掉临时筛选 */
    onapply: (s: Segment | null) => void;
  }
  let { site, active, filters, canWrite, onapply }: Props = $props();

  const segments = $derived(segmentsOf(site.id));
  const visible = $derived(!!active || canWrite || segments.length > 0);

  const dimName = (d: string) => (isPropDim(d) ? d.slice(PROP_PREFIX.length) : t(`fdim.${d}`));
  function describe(f: Filter): string {
    return `${dimName(f.dim)} ${t(`op.${f.op}`)} ${f.values.map((v) => displayValue(f.dim, v, app.lang)).join(t("filter.or"))}`;
  }
  const countText = (s: Segment) => (s.filters.length === 1 ? t("seg.condition") : t("seg.conditions", { n: s.filters.length }));
  /** 保存时存下的是当前生效的全部条件：分群的加上临时筛选 */
  const effective = $derived([...(active?.filters ?? []), ...filters]);

  // ---------- 保存 ----------
  let saveOpen = $state(false);
  let name = $state("");
  let tried = $state(false);
  let saving = $state(false);
  const nameMissing = $derived(!name.trim());
  const taken = $derived(!nameMissing && nameTaken(site.id, name));
  const nameError = $derived(taken ? t("seg.name.taken") : tried && nameMissing ? t("seg.name.required") : undefined);

  function openSave() {
    name = "";
    tried = false;
    saveOpen = true;
  }
  function save() {
    tried = true;
    if (nameMissing || taken || saving) return;
    saving = true;
    setTimeout(() => {
      saving = false;
      const ds = app.review.dataState;
      if (ds === "error" || ds === "busy" || ds === "timeout") {
        toast(t("seg.save.failed"), "bad");
        return;
      }
      const s = addSegment(site.id, name, effective);
      saveOpen = false;
      onapply(s);
      toast(t("seg.save.done", { name: s.name }), "good");
    }, 500);
  }

  // ---------- 管理：改名、删除 ----------
  let manageOpen = $state(false);
  let target = $state<Segment | null>(null);

  let renameOpen = $state(false);
  let newName = $state("");
  let renameTried = $state(false);
  const renameMissing = $derived(!newName.trim());
  const renameTaken = $derived(!renameMissing && !!target && nameTaken(site.id, newName, target.id));
  const renameError = $derived(renameTaken ? t("seg.name.taken") : renameTried && renameMissing ? t("seg.name.required") : undefined);

  function openRename(s: Segment) {
    target = s;
    newName = s.name;
    renameTried = false;
    renameOpen = true;
  }
  function rename() {
    renameTried = true;
    if (!target || renameMissing || renameTaken) return;
    renameSegment(target.id, newName);
    renameOpen = false;
    toast(t("seg.renamed"), "good");
  }

  let deleteOpen = $state(false);
  let deleting = $state(false);
  const blockers = $derived(target ? linksUsing(site.id, target.id) : []);
  const linkNames = $derived(
    blockers.map((l) => (app.lang === "zh" ? `「${l.name || t("set.share.unnamed")}」` : `“${l.name || t("set.share.unnamed")}”`)).join(app.lang === "zh" ? "、" : ", "),
  );

  function openDelete(s: Segment) {
    target = s;
    deleteOpen = true;
  }
  function remove() {
    const s = target;
    if (!s || deleting) return;
    deleting = true;
    setTimeout(() => {
      deleting = false;
      deleteSegment(s.id);
      deleteOpen = false;
      if (active?.id === s.id) onapply(null);
      toast(t("seg.delete.done", { name: s.name }));
    }, 400);
  }
</script>

{#if visible}
  <Popover width={288} label={t("seg.menu")}>
    {#snippet trigger({ toggle, props })}
      {#if active}
        <span class="chip">
          <button type="button" class="chip-main" onclick={toggle} {...props}>
            <Bookmark size={14} strokeWidth={1.75} aria-hidden="true" />
            <span class="chip-dim">{t("seg.term")}</span>
            <span class="chip-val">{active.name}</span>
          </button>
          <button type="button" class="chip-x" aria-label={t("seg.clear")} title={t("seg.clear")} onclick={() => onapply(null)}>
            <X size={14} strokeWidth={1.75} />
          </button>
        </span>
      {:else}
        <button type="button" class="ctl" onclick={toggle} {...props}>
          <Bookmark size={16} strokeWidth={1.75} aria-hidden="true" />
          <span>{t("seg.menu")}</span>
        </button>
      {/if}
    {/snippet}
    {#snippet children({ close })}
      {#if segments.length}
        {#each segments as s (s.id)}
          <MenuItem
            checked={active?.id === s.id}
            hint={countText(s)}
            onclick={() => {
              close();
              onapply(active?.id === s.id ? null : s);
            }}>{s.name}</MenuItem
          >
        {/each}
      {:else}
        <div class="none">
          <p>{t("seg.none")}</p>
          {#if canWrite && filters.length === 0}<p class="muted">{t("seg.none.hint")}</p>{/if}
        </div>
      {/if}
      {#if canWrite && (filters.length > 0 || segments.length > 0)}
        <MenuLabel separator />
        {#if filters.length > 0}
          <MenuItem
            icon={BookmarkPlus}
            onclick={() => {
              close();
              openSave();
            }}>{t("seg.saveCurrent")}</MenuItem
          >
        {/if}
        {#if segments.length > 0}
          <MenuItem
            icon={Settings2}
            onclick={() => {
              close();
              manageOpen = true;
            }}>{t("seg.manage")}</MenuItem
          >
        {/if}
      {/if}
    {/snippet}
  </Popover>
{/if}

<Dialog bind:open={saveOpen} title={t("seg.save.title")}>
  <form
    id="seg-save"
    class="form"
    onsubmit={(e) => {
      e.preventDefault();
      save();
    }}
  >
    <Field label={t("seg.name")} for="seg-name" error={nameError}>
      <Input id="seg-name" bind:value={name} maxlength={SEGMENT_NAME_MAX} invalid={!!nameError} autocomplete="off" />
    </Field>
    <div class="conds">
      <span class="label">{t("seg.save.conditions")}</span>
      <ul>
        {#each effective as f, i (i)}<li>{describe(f)}</li>{/each}
      </ul>
    </div>
  </form>
  {#snippet footer()}
    <Button onclick={() => (saveOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="primary" type="submit" form="seg-save" loading={saving} disabled={taken}>{t("seg.save.submit")}</Button>
  {/snippet}
</Dialog>

<Dialog bind:open={manageOpen} title={t("seg.manage")} size="lg">
  <div class="tbl">
    <Table
      caption={t("seg.manage")}
      rows={segments}
      rowKey={(s) => s.id}
      columns={[
        { key: "name", label: t("seg.col.name") },
        { key: "cond", label: t("seg.col.conditions"), optional: true },
        { key: "x", label: "", width: "80px", align: "right" },
      ]}
    >
      {#snippet cell(s, c)}
        {#if c.key === "name"}
          <span class="seg-name">{s.name}</span>
        {:else if c.key === "cond"}
          <span class="muted cond">{s.filters.map(describe).join(app.lang === "zh" ? "；" : "; ")}</span>
        {:else}
          <span class="acts">
            <Button variant="ghost" size="sm" icon={Pencil} aria-label="{t('seg.rename')} {s.name}" title={t("seg.rename")} onclick={() => openRename(s)} />
            <Button variant="ghost" size="sm" icon={Trash2} aria-label="{t('seg.delete')} {s.name}" title={t("seg.delete")} onclick={() => openDelete(s)} />
          </span>
        {/if}
      {/snippet}
    </Table>
  </div>
  {#snippet footer()}
    <Button onclick={() => (manageOpen = false)}>{t("common.close")}</Button>
  {/snippet}
</Dialog>

<Dialog bind:open={renameOpen} size="sm" title={t("seg.rename.title", { name: target?.name ?? "" })}>
  <form
    id="seg-rename"
    onsubmit={(e) => {
      e.preventDefault();
      rename();
    }}
  >
    <Field label={t("seg.name")} for="seg-rename-name" error={renameError}>
      <Input id="seg-rename-name" bind:value={newName} maxlength={SEGMENT_NAME_MAX} invalid={!!renameError} autocomplete="off" />
    </Field>
  </form>
  {#snippet footer()}
    <Button onclick={() => (renameOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="primary" type="submit" form="seg-rename" disabled={renameTaken}>{t("common.save")}</Button>
  {/snippet}
</Dialog>

<Dialog
  bind:open={deleteOpen}
  size="sm"
  title={blockers.length ? t("seg.blocked.title", { name: target?.name ?? "" }) : t("seg.delete.title", { name: target?.name ?? "" })}
>
  {#if blockers.length}
    <p>{t(blockers.length > 1 ? "seg.blocked.bodyMany" : "seg.blocked.body", { links: linkNames })}</p>
  {:else}
    <p>{t("seg.delete.body")}</p>
  {/if}
  {#snippet footer()}
    {#if blockers.length}
      <Button onclick={() => (deleteOpen = false)}>{t("common.close")}</Button>
      {#if canShareLinks(site)}
        <Button variant="primary" href={href(`/s/${site.id}/settings/sharing`)}>{t("seg.blocked.open")}</Button>
      {/if}
    {:else}
      <Button onclick={() => (deleteOpen = false)}>{t("common.cancel")}</Button>
      <Button variant="danger" loading={deleting} onclick={remove}>{t("common.delete")}</Button>
    {/if}
  {/snippet}
</Dialog>

<style>
  .ctl {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    height: var(--control-md);
    padding: 0 12px 0 10px;
    border: var(--hairline) solid var(--border);
    border-radius: var(--r-sm);
    background: var(--bg-app);
    font-weight: var(--fw-medium);
  }
  .ctl :global(svg) {
    color: var(--text-muted);
  }
  .ctl:hover,
  .ctl[aria-expanded="true"] {
    background: var(--bg-element);
    border-color: var(--border-hover);
  }
  /* 用着分群时：与筛选条件的标签同一种外观，前面多一个书签图标 */
  .chip {
    display: inline-flex;
    align-items: stretch;
    height: var(--control-md);
    border-radius: var(--r-sm);
    background: var(--bg-element);
  }
  .chip-main {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    max-width: 280px;
    padding: 0 4px 0 10px;
    border: 0;
    border-radius: var(--r-sm) 0 0 var(--r-sm);
    background: none;
  }
  .chip-main :global(svg) {
    flex: none;
    color: var(--text-muted);
  }
  .chip-main:hover,
  .chip-main[aria-expanded="true"],
  .chip-x:hover {
    background: var(--bg-hover);
  }
  .chip-dim {
    color: var(--text-muted);
  }
  .chip-val {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-weight: var(--fw-medium);
  }
  .chip-x {
    display: grid;
    place-items: center;
    width: 28px;
    border: 0;
    border-radius: 0 var(--r-sm) var(--r-sm) 0;
    background: none;
    color: var(--text-muted);
  }
  .chip-x:hover {
    color: var(--text);
  }
  .none {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 6px 8px;
  }
  .none .muted,
  .muted {
    color: var(--text-muted);
  }
  .none .muted {
    font-size: var(--fs-sm);
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .conds {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .conds .label {
    font-weight: var(--fw-medium);
  }
  .conds ul {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    margin: 0;
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--r-sm);
    background: var(--bg-subtle);
    list-style: none;
  }
  .tbl {
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-sm);
    overflow: hidden;
  }
  .seg-name {
    font-weight: var(--fw-medium);
  }
  .cond {
    display: block;
    padding: 4px 0;
  }
  .acts {
    display: inline-flex;
    gap: 2px;
  }
</style>
