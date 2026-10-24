<!--
  屏蔽规则：ip / cidr / country / hostname / page。命中「拒绝」规则的事件被丢弃。
  添加对话框按类型做初筛，服务端校验为准（400 的 message 直接显示在值字段旁）；上限由服务端给出，用量显示在表头右侧。
  只有 owner / admin 能写；editor 只读（没有添加与删除入口）。
-->
<script lang="ts">
  import { CircleAlert, Plus, Trash2 } from "@lucide/svelte";
  import Badge from "#lib/ui/Badge.svelte";
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
  import { localYmd } from "#lib/admin/expiry.ts";
  import { api, ApiError, type ShieldRule, type ShieldType, type Site } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import { toast } from "#lib/state.svelte.ts";
  import { errText } from "./settings.svelte.ts";
  import "./strings";

  let { site, readOnly = false }: { site: Site; readOnly?: boolean } = $props();

  const siteId = $derived(site.id);
  let rules = $state<ShieldRule[] | null>(null);
  let max = $state(500);
  let failed = $state(false);

  async function load() {
    failed = false;
    try {
      const r = await api.shield(siteId);
      rules = r.rules;
      max = r.max_rules;
    } catch {
      failed = true;
    }
  }
  // 布局按站点 id 重建本组件，这里只在挂载时加载一次
  $effect(() => {
    void load();
  });

  const used = $derived(rules?.length ?? 0);
  const full = $derived(used >= max);
  const types: ShieldType[] = ["ip", "cidr", "country", "hostname", "page"];

  let open = $state(false);
  let type = $state<ShieldType>("ip");
  let action = $state<"deny" | "allow">("deny");
  let value = $state("");
  let note = $state("");
  let busy = $state(false);
  let serverErr = $state("");

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
  const dup = $derived(!!norm && !!rules?.some((r) => r.type === type && r.value === norm));
  const errKey = $derived(error ?? (dup ? "set.shield.err.dup" : null));
  const errMsg = $derived(serverErr || (errKey ? t(errKey) : ""));
  const canAdd = $derived(!!norm && !errKey && !full && !busy);

  function openAdd() {
    type = "ip";
    action = "deny";
    value = "";
    note = "";
    serverErr = "";
    open = true;
  }
  async function add() {
    if (!canAdd || !rules) return;
    busy = true;
    try {
      const { rule } = await api.addShieldRule(site.id, { type, action, value: norm, description: note.trim() || undefined });
      rules.unshift(rule);
      open = false;
      toast(t("set.shield.added"), "good");
    } catch (e) {
      if (e instanceof ApiError && e.code === "limit_reached") serverErr = t("set.shield.err.full");
      else if (e instanceof ApiError && e.status === 409) serverErr = t("set.shield.err.dup");
      else if (e instanceof ApiError && e.status === 400) serverErr = e.message;
      else toast(errText(e), "bad");
    } finally {
      busy = false;
    }
  }

  let removing = $state<string | null>(null);
  let removeOpen = $state(false);
  const removingRule = $derived(rules?.find((r) => r.id === removing));
  async function confirmRemove() {
    if (removing === null || !rules || busy) return;
    const id = removing;
    busy = true;
    try {
      await api.removeShieldRule(site.id, id);
      rules = rules.filter((r) => r.id !== id);
      removeOpen = false;
      toast(t("set.shield.removed"));
    } catch (e) {
      toast(errText(e), "bad");
    } finally {
      busy = false;
    }
  }
</script>

<Card title={t("set.shield.title")} flush>
  {#snippet actions()}
    {#if rules}
      <span class="used num">{t("set.shield.used", { n: used, max })}</span>
      {#if !readOnly}<Button variant="primary" icon={Plus} disabled={full} onclick={openAdd}>{t("set.shield.add")}</Button>{/if}
    {/if}
  {/snippet}
  {#if failed}
    <EmptyState compact icon={CircleAlert} tone="bad" title={t("set.loadFailed")}>
      {#snippet actions()}<Button onclick={load}>{t("common.retry")}</Button>{/snippet}
    </EmptyState>
  {:else if rules === null}
    <div class="sk" aria-busy="true"><Skeleton height={32} radius={6} /><Skeleton height={32} radius={6} /></div>
  {:else if rules.length === 0}
    <EmptyState compact title={t("set.shield.none")} />
  {:else}
    <Table
      caption={t("set.shield.title")}
      rows={rules}
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
            {#if r.description}<span class="muted">{r.description}</span>{/if}
          </div>
        {:else if c.key === "created"}
          <span class="muted num">{localYmd(new Date(r.created_at))}</span>
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
      void add();
    }}
  >
    <div class="two">
      <Field label={t("set.shield.col.type")}>
        <Select
          label={t("set.shield.col.type")}
          block
          bind:value={type}
          onchange={() => {
            value = "";
            serverErr = "";
          }}
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
    <Field label={t("set.shield.col.value")} for="rule-value" error={(norm && errKey) || serverErr ? errMsg : undefined}>
      <Input
        id="rule-value"
        mono
        bind:value
        oninput={() => (serverErr = "")}
        invalid={(!!norm && !!errKey) || !!serverErr}
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
    <Button variant="primary" type="submit" form="rule-form" loading={busy} disabled={!canAdd}>{t("common.add")}</Button>
  {/snippet}
</Dialog>

<Dialog bind:open={removeOpen} size="sm" title={t("set.shield.rm.title")}>
  <p><code>{removingRule?.value}</code></p>
  <p class="muted">{t("set.shield.rm.body")}</p>
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
