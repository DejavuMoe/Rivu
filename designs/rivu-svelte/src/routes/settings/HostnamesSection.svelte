<!--
  主机名：只有列表里的主机名发来的事件会被接收，列表为空时所有事件都会被丢弃。
  添加对话框做实时校验，并按真实的匹配函数给出「匹配 / 不匹配」的示例。
-->
<script lang="ts">
  import { Check, Plus, Trash2, X } from "@lucide/svelte";
  import Badge from "../../lib/ui/Badge.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Callout from "../../lib/ui/Callout.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import { t } from "../../lib/i18n";
  import { toast } from "../../lib/state.svelte";
  import { NOW, type Site } from "../../lib/data/fixtures";
  import { cfgs, checkPattern, hostExamples, ymd } from "./store.svelte";
  import "./strings";

  let { site }: { site: Site } = $props();
  const cfg = $derived(cfgs[site.id]);

  let addOpen = $state(false);
  let draft = $state("");
  const pattern = $derived(draft.trim().toLowerCase());
  const err = $derived(checkPattern(pattern, cfg.hostnames.map((h) => h.pattern)));
  const showErr = $derived(pattern.length > 0 && err !== null);
  const examples = $derived(pattern && !err ? hostExamples(pattern) : []);

  let removing = $state<string | null>(null);
  let removeOpen = $state(false);

  function add() {
    if (err) return;
    cfg.hostnames.push({ pattern, addedAt: NOW });
    addOpen = false;
    draft = "";
    toast(t("set.host.added", { p: pattern }), "good");
  }
  function confirmRemove() {
    if (removing === null) return;
    const p = removing;
    cfg.hostnames = cfg.hostnames.filter((h) => h.pattern !== p);
    removeOpen = false;
    toast(t("set.host.removed", { p }));
  }
  const isLast = $derived(cfg.hostnames.length === 1);
</script>

<Card title={t("set.host.title")} flush>
  {#snippet actions()}
    <Button
      variant="primary"
      icon={Plus}
      onclick={() => {
        draft = "";
        addOpen = true;
      }}>{t("set.host.add")}</Button
    >
  {/snippet}
  {#if cfg.hostnames.length === 0}
    <div class="warn"><Callout tone="warn">{t("set.host.empty")}</Callout></div>
  {:else}
    <Table
      caption={t("set.host.title")}
      rows={cfg.hostnames}
      rowKey={(h) => h.pattern}
      columns={[
        { key: "p", label: t("set.host.col.pattern") },
        { key: "a", label: t("set.host.col.added"), width: "130px", optional: true },
        { key: "x", label: "", width: "56px", align: "right" },
      ]}
    >
      {#snippet cell(h, c)}
        {#if c.key === "p"}
          <span class="pat"><code>{h.pattern}</code>{#if h.pattern.startsWith("*.")}<Badge>{t("set.host.wildcard")}</Badge>{/if}</span>
        {:else if c.key === "a"}
          <span class="muted num">{ymd(h.addedAt)}</span>
        {:else}
          <Button
            variant="ghost"
            size="sm"
            icon={Trash2}
            aria-label="{t('common.remove')} {h.pattern}"
            onclick={() => {
              removing = h.pattern;
              removeOpen = true;
            }}
          />
        {/if}
      {/snippet}
    </Table>
  {/if}
</Card>

<Dialog bind:open={addOpen} title={t("set.host.add")}>
  <form
    id="host-form"
    onsubmit={(e) => {
      e.preventDefault();
      add();
    }}
  >
    <Field label={t("set.host.col.pattern")} for="host-pattern" error={showErr ? t(`set.host.err.${err}`) : undefined}>
      <Input id="host-pattern" mono bind:value={draft} placeholder="*.example.com" invalid={showErr} autocomplete="off" spellcheck={false} />
    </Field>
  </form>
  {#if examples.length}
    <div class="ex" aria-live="polite">
      <p class="ex-title">{t("set.host.ex.title", { p: pattern })}</p>
      <ul>
        {#each examples as ex (ex.host)}
          <li>
            <code>{ex.host}</code>
            {#if ex.match}
              <Badge tone="good" icon={Check}>{t("set.host.ex.yes")}</Badge>
            {:else}
              <Badge icon={X}>{t("set.host.ex.no")}</Badge>
            {/if}
          </li>
        {/each}
      </ul>
      {#if pattern.startsWith("*.")}<p class="ex-note">{t("set.host.ex.apex", { b: pattern.slice(2) })}</p>{/if}
    </div>
  {/if}
  {#snippet footer()}
    <Button onclick={() => (addOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="primary" type="submit" form="host-form" disabled={!!err}>{t("common.add")}</Button>
  {/snippet}
</Dialog>

<Dialog bind:open={removeOpen} size="sm" title={t("set.host.rm.title", { p: removing ?? "" })}>
  <p>{isLast ? t("set.host.rm.last") : t("set.host.rm.body")}</p>
  {#snippet footer()}
    <Button onclick={() => (removeOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" onclick={confirmRemove}>{t("common.remove")}</Button>
  {/snippet}
</Dialog>

<style>
  .warn {
    padding: 0 var(--sp-5) var(--sp-5);
  }
  .pat {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .ex {
    margin-top: var(--sp-4);
    padding: var(--sp-3) var(--sp-4);
    border-radius: var(--r-sm);
    background: var(--bg-subtle);
  }
  .ex-title {
    margin-bottom: var(--sp-2);
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .ex ul {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .ex li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
  }
  .ex-note {
    margin-top: var(--sp-3);
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
</style>
