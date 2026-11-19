<!--
  屏蔽规则：ip / cidr / country / hostname / page。命中「拒绝」规则的事件被丢弃，原因出现在接入诊断里。
  添加对话框按类型切换校验；上限 500 条，用量显示在表头右侧。编辑者只读（没有添加与删除入口）。
-->
<script lang="ts">
  import { Plus, Trash2 } from "@lucide/svelte";
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
  import { NOW, type Site } from "../../lib/data/fixtures";
  import { cfgs, MAX_RULES, nextId, ymd, type RuleType } from "./store.svelte";
  import "./strings";

  let { site, readOnly = false }: { site: Site; readOnly?: boolean } = $props();
  const cfg = $derived(cfgs[site.id]);
  const used = $derived(cfg.rules.length);
  const full = $derived(used >= MAX_RULES);

  const types: RuleType[] = ["ip", "cidr", "country", "hostname", "page"];

  let open = $state(false);
  let type = $state<RuleType>("ip");
  let action = $state<"deny" | "allow">("deny");
  let value = $state("");
  let note = $state("");

  const IPV4 = /^(25[0-5]|2[0-4]\d|1?\d?\d)(\.(25[0-5]|2[0-4]\d|1?\d?\d)){3}$/;
  const IPV6 = /^[0-9a-f:]+$/i;
  const isV6 = (s: string) => s.includes(":") && IPV6.test(s) && s.split(":").length >= 3 && s.split(":").length <= 8 && !/:::/.test(s);

  /** 按类型规范化输入：country 自动大写，hostname 小写。 */
  const norm = $derived.by(() => {
    const v = value.trim();
    if (type === "country") return v.toUpperCase();
    if (type === "hostname") return v.toLowerCase();
    return v;
  });

  const error = $derived.by((): string | null => {
    if (!norm) return null;
    switch (type) {
      case "ip":
        return IPV4.test(norm) || isV6(norm) ? null : "set.shield.err.ip";
      case "cidr": {
        const [addr, bits, ...rest] = norm.split("/");
        if (rest.length || bits === undefined || !/^\d{1,3}$/.test(bits)) return "set.shield.err.cidr";
        if (IPV4.test(addr)) return Number(bits) <= 32 ? null : "set.shield.err.cidr";
        return isV6(addr) && Number(bits) <= 128 ? null : "set.shield.err.cidr";
      }
      case "country":
        return /^[A-Z]{2}$/.test(norm) ? null : "set.shield.err.country";
      case "hostname":
        return /^([a-z0-9-]+\.)*[a-z0-9-]+$/.test(norm) ? null : "set.shield.err.hostname";
      case "page":
        return /^\/\S*$/.test(norm) && norm.length <= 500 ? null : "set.shield.err.page";
    }
  });
  const dup = $derived(!!norm && cfg.rules.some((r) => r.type === type && r.value === norm));
  const errKey = $derived(error ?? (dup ? "set.shield.err.dup" : null));
  const canAdd = $derived(!!norm && !errKey && !full);

  function openAdd() {
    type = "ip";
    action = "deny";
    value = "";
    note = "";
    open = true;
  }
  function add() {
    if (!canAdd) return;
    cfg.rules.unshift({ id: nextId(), type, action, value: norm, note: note.trim(), createdAt: NOW });
    open = false;
    toast(t("set.shield.added"), "good");
  }

  let removing = $state<number | null>(null);
  let removeOpen = $state(false);
  const removingRule = $derived(cfg.rules.find((r) => r.id === removing));
  function confirmRemove() {
    cfg.rules = cfg.rules.filter((r) => r.id !== removing);
    removeOpen = false;
    toast(t("set.shield.removed"));
  }
</script>

<Card title={t("set.shield.title")} flush>
  {#snippet actions()}
    <span class="used num">{t("set.shield.used", { n: used, max: MAX_RULES })}</span>
    {#if !readOnly}<Button variant="primary" icon={Plus} disabled={full} onclick={openAdd}>{t("set.shield.add")}</Button>{/if}
  {/snippet}
  {#if cfg.rules.length === 0}
    <EmptyState compact title={t("set.shield.none")} />
  {:else}
    <Table
      caption={t("set.shield.title")}
      rows={cfg.rules}
      rowKey={(r) => r.id}
      columns={[
        { key: "type", label: t("set.shield.col.type"), width: "112px" },
        { key: "action", label: t("set.shield.col.action"), width: "84px" },
        { key: "value", label: t("set.shield.col.value") },
        { key: "created", label: t("set.shield.col.created"), width: "112px", optional: true },
        ...(readOnly ? [] : [{ key: "x", label: "", width: "56px", align: "right" as const }]),
      ]}
    >
      {#snippet cell(r, c)}
        {#if c.key === "type"}
          {t(`set.shield.type.${r.type}`)}
        {:else if c.key === "action"}
          {#if r.action === "deny"}<Badge tone="bad">{t("set.shield.deny")}</Badge>{:else}<Badge tone="good">{t("set.shield.allow")}</Badge>{/if}
        {:else if c.key === "value"}
          <div class="val">
            <code>{r.value}</code>
            {#if r.note}<span class="muted">{r.note}</span>{/if}
          </div>
        {:else if c.key === "created"}
          <span class="muted num">{ymd(r.createdAt)}</span>
        {:else}
          <Button
            variant="ghost"
            size="sm"
            icon={Trash2}
            aria-label="{t('common.delete')} {r.value}"
            onclick={() => {
              removing = r.id;
              removeOpen = true;
            }}
          />
        {/if}
      {/snippet}
    </Table>
  {/if}
</Card>

<Dialog bind:open title={t("set.shield.dlg.title")}>
  <form
    id="rule-form"
    class="form"
    onsubmit={(e) => {
      e.preventDefault();
      add();
    }}
  >
    <div class="two">
      <Field label={t("set.shield.col.type")}>
        <Select
          label={t("set.shield.col.type")}
          block
          bind:value={type}
          onchange={() => (value = "")}
          options={types.map((x) => ({ value: x, label: t(`set.shield.type.${x}`) }))}
        />
      </Field>
      <Field label={t("set.shield.col.action")}>
        <Segmented
          label={t("set.shield.col.action")}
          block
          bind:value={action}
          options={[
            { value: "deny", label: t("set.shield.deny") },
            { value: "allow", label: t("set.shield.allow") },
          ]}
        />
      </Field>
    </div>
    <Field label={t("set.shield.col.value")} for="rule-value" error={norm && errKey ? t(errKey) : undefined}>
      <Input
        id="rule-value"
        mono
        bind:value
        invalid={!!norm && !!errKey}
        placeholder={t(`set.shield.ph.${type}`)}
        autocomplete="off"
        spellcheck={false}
      />
    </Field>
    <Field label={t("set.shield.note")} for="rule-note" optional>
      <Input id="rule-note" bind:value={note} maxlength={120} />
    </Field>
  </form>
  {#snippet footer()}
    <Button onclick={() => (open = false)}>{t("common.cancel")}</Button>
    <Button variant="primary" type="submit" form="rule-form" disabled={!canAdd}>{t("common.add")}</Button>
  {/snippet}
</Dialog>

<Dialog bind:open={removeOpen} size="sm" title={t("set.shield.rm.title")}>
  <p><code>{removingRule?.value}</code></p>
  <p class="muted">{t("set.shield.rm.body")}</p>
  {#snippet footer()}
    <Button onclick={() => (removeOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" onclick={confirmRemove}>{t("common.delete")}</Button>
  {/snippet}
</Dialog>

<style>
  .used {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .val {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 6px 0;
    min-width: 0;
  }
  .val code {
    overflow-wrap: anywhere;
  }
  .val .muted {
    font-size: var(--fs-sm);
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--sp-3);
  }
  @media (max-width: 480px) {
    .two {
      grid-template-columns: 1fr;
    }
  }
</style>
