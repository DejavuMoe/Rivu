<!--
  图表批注的对话框，两种用法：
    list：点开图上的标记，列出这个时间桶里的批注；editor 起每条可以就地修改、删除（删除前就地确认），底部可以再加一条；
    add：工具栏的「添加批注」，填日期和内容。
  只读角色只看到列表和「关闭」。站点批注满 500 条时不能再加，说明原因。
-->
<script lang="ts">
  import { Pencil, Trash2 } from "@lucide/svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Callout from "../../lib/ui/Callout.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import { dateLong } from "../../lib/format";
  import type { Site } from "../../lib/data/fixtures";
  import { NOTES_PER_SITE, NOTE_TEXT_MAX, addNote, deleteNote, noteCount, today, updateNote, type Note } from "./notes.svelte";
  import "./note-strings";

  interface Props {
    open: boolean;
    site: Site;
    mode: "list" | "add";
    /** list：要列出的批注 */
    notes?: Note[];
    /** list：对话框标题（时间桶的名字） */
    heading?: string;
    /** list：标题不是某一天（按周、按月的桶，或合并的标记）时，每条批注前面写日期 */
    showDates?: boolean;
    /** add：默认日期 */
    date?: string;
    canWrite: boolean;
  }
  let { open = $bindable(), site, mode = $bindable(), notes = [], heading = "", showDates = false, date, canWrite }: Props = $props();

  const full = $derived(noteCount(site.id) >= NOTES_PER_SITE);
  const failing = () => ["error", "busy", "timeout"].includes(app.review.dataState);
  const label = (d: string) => dateLong(new Date(`${d}T00:00:00`), app.lang);

  // ---------- 列表里的修改与删除 ----------
  let editing = $state<number | null>(null);
  let editText = $state("");
  let confirming = $state<number | null>(null);

  // ---------- 添加 ----------
  let addDate = $state("");
  let addText = $state("");
  let tried = $state(false);
  let busy = $state(false);

  $effect(() => {
    if (open) {
      editing = confirming = null;
      addDate = date ?? today();
      addText = "";
      tried = false;
      rows = [...notes];
    }
  });

  /** 列表里还剩的批注（打开时取 notes，删除后即时更新） */
  let rows = $state<Note[]>([]);
  const dated = $derived(showDates || new Set(rows.map((r) => r.date)).size > 1);

  function startEdit(n: Note) {
    confirming = null;
    editing = n.id;
    editText = n.text;
  }
  function saveEdit(n: Note) {
    if (!editText.trim()) return;
    if (failing()) {
      toast(t("note.failed"), "bad");
      return;
    }
    updateNote(n.id, editText);
    editing = null;
    toast(t("note.updated"), "good");
  }
  function remove(n: Note) {
    deleteNote(n.id);
    rows = rows.filter((x) => x.id !== n.id);
    confirming = null;
    toast(t("note.deleted"));
    if (!rows.length) open = false;
  }

  function startAdd() {
    addDate = rows[0]?.date ?? date ?? today();
    addText = "";
    tried = false;
    mode = "add";
  }
  const textMissing = $derived(!addText.trim());
  function save() {
    tried = true;
    if (textMissing || !addDate || full || busy) return;
    busy = true;
    setTimeout(() => {
      busy = false;
      if (failing()) {
        toast(t("note.failed"), "bad");
        return;
      }
      addNote(site.id, addDate, addText);
      open = false;
      toast(t("note.added"), "good");
    }, 400);
  }
</script>

<Dialog bind:open title={mode === "add" ? t("note.add") : heading}>
  {#if mode === "list"}
    <ul class="notes">
      {#each rows as n (n.id)}
        <li>
          {#if editing === n.id}
            <form
              class="edit"
              onsubmit={(e) => {
                e.preventDefault();
                saveEdit(n);
              }}
            >
              <Input bind:value={editText} maxlength={NOTE_TEXT_MAX} aria-label={t("note.text")} autocomplete="off" />
              <div class="row-acts">
                <Button size="sm" onclick={() => (editing = null)}>{t("common.cancel")}</Button>
                <Button size="sm" variant="primary" type="submit" disabled={!editText.trim()}>{t("common.save")}</Button>
              </div>
            </form>
          {:else}
            <div class="note">
              <div class="body">
                {#if dated}<span class="date">{label(n.date)}</span>{/if}
                <p>{n.text}</p>
              </div>
              {#if canWrite && confirming !== n.id}
                <span class="acts">
                  <Button variant="ghost" size="sm" icon={Pencil} aria-label="{t('note.edit')} {n.text}" title={t("note.edit")} onclick={() => startEdit(n)} />
                  <Button
                    variant="ghost"
                    size="sm"
                    icon={Trash2}
                    aria-label="{t('note.delete')} {n.text}"
                    title={t("note.delete")}
                    onclick={() => {
                      editing = null;
                      confirming = n.id;
                    }}
                  />
                </span>
              {/if}
            </div>
            {#if confirming === n.id}
              <div class="confirm" role="group" aria-label={t("note.deleteAsk")}>
                <span>{t("note.deleteAsk")}</span>
                <div class="row-acts">
                  <Button size="sm" onclick={() => (confirming = null)}>{t("common.cancel")}</Button>
                  <Button size="sm" variant="danger" onclick={() => remove(n)}>{t("common.delete")}</Button>
                </div>
              </div>
            {/if}
          {/if}
        </li>
      {/each}
    </ul>
  {:else}
    <form
      id="note-add"
      class="form"
      onsubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      {#if full}<Callout tone="warn">{t("note.full")}</Callout>{/if}
      <Field label={t("note.date")} for="note-date" error={tried && !addDate ? t("note.date.required") : undefined}>
        <Input id="note-date" type="date" bind:value={addDate} invalid={tried && !addDate} disabled={full} />
      </Field>
      <Field
        label={t("note.text")}
        for="note-text"
        hint={addText.length > NOTE_TEXT_MAX - 40 ? `${addText.length} / ${NOTE_TEXT_MAX}` : undefined}
        error={tried && textMissing ? t("note.text.required") : undefined}
      >
        <Input id="note-text" bind:value={addText} maxlength={NOTE_TEXT_MAX} invalid={tried && textMissing} disabled={full} autocomplete="off" />
      </Field>
    </form>
  {/if}
  {#snippet footer()}
    {#if mode === "list"}
      <Button onclick={() => (open = false)}>{t("common.close")}</Button>
      {#if canWrite && !full}<Button variant="primary" onclick={startAdd}>{t("note.add")}</Button>{/if}
    {:else}
      <Button onclick={() => (open = false)}>{t("common.cancel")}</Button>
      {#if !full}<Button variant="primary" type="submit" form="note-add" loading={busy}>{t("note.save")}</Button>{/if}
    {/if}
  {/snippet}
</Dialog>

<style>
  .notes {
    display: flex;
    flex-direction: column;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .notes li {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    padding: var(--sp-3) 0;
    border-bottom: var(--hairline) solid var(--border-subtle);
  }
  .notes li:first-child {
    padding-top: 0;
  }
  .notes li:last-child {
    border-bottom: 0;
    padding-bottom: 0;
  }
  .note {
    display: flex;
    align-items: flex-start;
    gap: var(--sp-3);
  }
  .body {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .date {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .acts {
    display: inline-flex;
    flex: none;
    gap: 2px;
  }
  .edit,
  .confirm {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .confirm {
    padding: var(--sp-2) var(--sp-3);
    border-radius: var(--r-sm);
    background: var(--bg-subtle);
  }
  .row-acts {
    display: flex;
    justify-content: flex-end;
    gap: var(--sp-2);
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
</style>
