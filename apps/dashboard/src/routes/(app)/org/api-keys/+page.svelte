<!--
  我的 API 密钥：密钥表、创建对话框（创建后在同一对话框里只显示一次完整密钥，并附 curl 示例）、
  表格下方一行限额说明。所有组织角色（包括查看者）都能管理自己的密钥，只列出自己的。
-->
<script lang="ts">
  import { Inbox, Plus, TriangleAlert, Users } from "@lucide/svelte";
  import { untrack } from "svelte";
  import PageHeader from "#lib/app/PageHeader.svelte";
  import Badge from "#lib/ui/Badge.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Callout from "#lib/ui/Callout.svelte";
  import Card from "#lib/ui/Card.svelte";
  import Checkbox from "#lib/ui/Checkbox.svelte";
  import CopyField from "#lib/ui/CopyField.svelte";
  import Dialog from "#lib/ui/Dialog.svelte";
  import EmptyState from "#lib/ui/EmptyState.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import Segmented from "#lib/ui/Segmented.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import Table from "#lib/ui/Table.svelte";
  import type { Column } from "#lib/ui/types.ts";
  import { localYmd } from "#lib/admin/expiry.ts";
  import { curlExample, keyExpiry, keyScope, minutesSince } from "#lib/admin/org.ts";
  import { api, type ApiKeyItem, type ApiKeysBody } from "#lib/api.ts";
  import { full, relativeTime } from "#lib/format.ts";
  import { t } from "#lib/i18n.ts";
  import { currentOrg, orgSites, session } from "#lib/session.svelte.ts";
  import { app, toast } from "#lib/state.svelte.ts";
  import { orgErrText } from "../errors.ts";
  import "../strings";

  let data = $state<ApiKeysBody | null>(null);
  let status = $state<"loading" | "ready" | "failed">("loading");
  let seq = 0;
  let busy = $state(false);
  let now = $state(Date.now());

  const orgId = $derived(session.orgId);
  const keys = $derived(data?.keys ?? []);
  const sites = $derived(orgSites());
  /** 只以站点访客身份看到这个组织（或没有组织）：访客不能有 API 密钥 */
  const notMember = $derived(orgId === null || !currentOrg()?.role);

  async function load(id: number | null, silent = false) {
    const mine = ++seq;
    if (id === null) {
      status = "failed";
      return;
    }
    if (!silent) status = "loading";
    try {
      const body = await api.apiKeys(id);
      if (mine !== seq) return;
      data = body;
      now = Date.now();
      status = "ready";
    } catch (e) {
      if (mine !== seq) return;
      if (silent) toast(orgErrText(e), "bad");
      else status = "failed";
    }
  }

  $effect(() => {
    const id = notMember ? null : orgId;
    untrack(() => {
      data = null;
      void load(id);
    });
  });

  function scopeText(k: ApiKeyItem): string {
    const s = keyScope(k);
    if (s.kind === "all") return t("ak.scope.all");
    if (s.kind === "site") return s.name;
    return t("ak.scope.sites", { n: s.n });
  }
  function usedText(k: ApiKeyItem): string {
    const m = minutesSince(k.last_used_at, now);
    return m === null ? t("ak.neverUsed") : relativeTime(m, app.lang);
  }

  const cols = $derived<Column[]>([
    { key: "name", label: t("ak.col.name") },
    { key: "prefix", label: t("ak.col.prefix"), width: "140px" },
    { key: "scope", label: t("ak.col.scope"), width: "160px", optional: true },
    { key: "used", label: t("ak.col.used"), optional: true, width: "110px" },
    { key: "expires", label: t("ak.col.expires"), optional: true, width: "130px" },
    { key: "act", label: "", align: "right", width: "80px" },
  ]);

  // ---- 创建 ----
  type Phase = "form" | "done";
  let createOpen = $state(false);
  let phase = $state<Phase>("form");
  let name = $state("");
  let scopeMode = $state<"all" | "pick">("all");
  let picked = $state<Record<string, boolean>>({});
  let expiry = $state<"30" | "90" | "365" | "never">("90");
  let tried = $state(false);
  let minted = $state("");
  let mintedSite = $state("");

  const pickedKeys = $derived(sites.filter((s) => picked[s.site_key]).map((s) => s.site_key));
  const nameLen = $derived([...name.trim()].length);
  const nameError = $derived(nameLen < 1 || nameLen > 100 ? t("ak.name.required") : "");
  const pickError = $derived(scopeMode === "pick" && pickedKeys.length === 0 ? t("ak.scope.pick.required") : "");
  const expiryDate = $derived(expiry === "never" ? null : new Date(Date.now() + Number(expiry) * 864e5));

  function openCreate() {
    phase = "form";
    name = "";
    scopeMode = "all";
    picked = {};
    expiry = "90";
    tried = false;
    minted = "";
    createOpen = true;
  }

  async function create(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    if (nameError || pickError || busy || orgId === null) return;
    busy = true;
    try {
      const res = await api.createApiKey(orgId, {
        name: name.trim(),
        sites: scopeMode === "all" ? null : pickedKeys,
        expires_in_days: expiry === "never" ? null : Number(expiry),
      });
      minted = res.key;
      mintedSite = pickedKeys[0] ?? sites[0]?.site_key ?? "<site_key>";
      phase = "done";
      toast(t("ak.created.toast", { name: name.trim() }), "good");
      void load(orgId, true);
    } catch (err) {
      toast(orgErrText(err), "bad");
    } finally {
      busy = false;
    }
  }

  // ---- 撤销 ----
  let revokeOpen = $state(false);
  let revoking = $state<ApiKeyItem | null>(null);
  function askRevoke(k: ApiKeyItem) {
    revoking = k;
    revokeOpen = true;
  }
  async function confirmRevoke() {
    if (!revoking || orgId === null || busy) return;
    const k = revoking;
    busy = true;
    try {
      await api.revokeApiKey(orgId, k.id);
      toast(t("ak.revoked", { name: k.name }));
    } catch (e) {
      toast(orgErrText(e), "bad");
    } finally {
      busy = false;
      revokeOpen = false;
    }
    await load(orgId, true);
  }

  const curl = $derived(curlExample(typeof location === "undefined" ? "" : location.origin, minted, mintedSite));
</script>

<div class="page">
  <PageHeader title={t("ak.title")}>
    {#snippet actions()}
      {#if status === "ready"}<Button variant="primary" icon={Plus} onclick={openCreate}>{t("ak.create")}</Button>{/if}
    {/snippet}
  </PageHeader>

  {#if status === "loading"}
    <Card flush>
      <div class="sk" aria-busy="true">
        {#each [0, 1, 2] as i (i)}
          <div class="sk-row"><Skeleton width="22%" height={12} /><Skeleton width="14%" height={12} /><Skeleton width="20%" height={12} /></div>
        {/each}
      </div>
    </Card>
  {:else if notMember}
    <Card>
      <EmptyState icon={Users} title={t("mb.noOrg")}>
        {#snippet actions()}<Button href="/sites">{t("nav.allSites")}</Button>{/snippet}
      </EmptyState>
    </Card>
  {:else if status === "failed" || !data}
    <Card>
      <EmptyState icon={TriangleAlert} tone="bad" title={t("ak.error.title")}>
        {#snippet actions()}<Button onclick={() => load(orgId)}>{t("common.retry")}</Button>{/snippet}
      </EmptyState>
    </Card>
  {:else if keys.length === 0}
    <Card>
      <EmptyState icon={Inbox} title={t("ak.empty.title")}>
        {#snippet actions()}<Button variant="primary" icon={Plus} onclick={openCreate}>{t("ak.create")}</Button>{/snippet}
      </EmptyState>
    </Card>
  {:else}
    <Card flush>
      <Table columns={cols} rows={keys} rowKey={(k) => k.id} caption={t("ak.title")}>
        {#snippet cell(k, c)}
          {#if c.key === "name"}
            <span class="kname">{k.name}</span>
          {:else if c.key === "prefix"}
            <code class="muted">{k.key_prefix}</code>
          {:else if c.key === "scope"}
            {scopeText(k)}
          {:else if c.key === "used"}
            <span class="muted">{usedText(k)}</span>
          {:else if c.key === "expires"}
            {@const ex = keyExpiry(k.expires_at, now)}
            {#if ex.kind === "never"}
              <span class="muted">{t("ak.never")}</span>
            {:else if ex.kind === "expired"}
              <Badge tone="warn">{t("ak.expired")}</Badge>
            {:else if ex.kind === "soon"}
              <Badge tone="warn">{t("ak.expiresSoon", { n: ex.days })}</Badge>
            {:else}
              <span class="muted num">{localYmd(ex.at)}</span>
            {/if}
          {:else if c.key === "act"}
            <Button variant="ghost" size="sm" aria-label={`${t("ak.revoke")} ${k.name}`} onclick={() => askRevoke(k)}>{t("ak.revoke")}</Button>
          {/if}
        {/snippet}
      </Table>
    </Card>
    <p class="limits">
      {t("ak.limits", { hourly: full(data.limits.hourly, app.lang), burst: data.limits.burst, concurrent: data.limits.concurrency })}
    </p>
  {/if}
</div>

<Dialog bind:open={createOpen} title={phase === "form" ? t("ak.dialog.title") : t("ak.created.title")} onclose={() => (minted = "")}>
  {#if phase === "form"}
    <form id="key-form" class="form" onsubmit={create} novalidate>
      <Field label={t("ak.name")} for="key-name" error={tried ? nameError : ""}>
        <Input id="key-name" placeholder={t("ak.name.placeholder")} autocomplete="off" bind:value={name} invalid={tried && !!nameError} />
      </Field>
      <Field label={t("ak.scope")} error={tried ? pickError : ""}>
        <Segmented
          label={t("ak.scope")}
          value={scopeMode}
          onchange={(v: "all" | "pick") => (scopeMode = v)}
          options={[
            { value: "all", label: t("ak.scope.optAll") },
            { value: "pick", label: t("ak.scope.optPick"), disabled: sites.length === 0 },
          ]}
        />
        {#if scopeMode === "pick"}
          <div class="picks">
            {#each sites as s (s.id)}
              <Checkbox bind:checked={picked[s.site_key]}>{s.name}</Checkbox>
            {/each}
          </div>
        {/if}
      </Field>
      <Field label={t("ak.expiry")}>
        <Segmented
          label={t("ak.expiry")}
          value={expiry}
          onchange={(v: "30" | "90" | "365" | "never") => (expiry = v)}
          options={[
            { value: "30", label: t("ak.exp.30") },
            { value: "90", label: t("ak.exp.90") },
            { value: "365", label: t("ak.exp.365") },
            { value: "never", label: t("ak.exp.never") },
          ]}
        />
        <p class="hint">{expiryDate ? t("ak.exp.on", { date: localYmd(expiryDate) }) : t("ak.exp.neverNote")}</p>
      </Field>
    </form>
  {:else}
    <div class="form">
      <Callout tone="warn">{t("ak.created.once")}</Callout>
      <CopyField secret label={t("ak.created.label")} value={minted} />
      <Field label={t("ak.example")}>
        <CopyField multiline label={t("ak.example")} value={curl} />
      </Field>
    </div>
  {/if}
  {#snippet footer()}
    {#if phase === "form"}
      <Button variant="ghost" onclick={() => (createOpen = false)}>{t("common.cancel")}</Button>
      <Button type="submit" form="key-form" variant="primary" loading={busy}>{t("ak.create")}</Button>
    {:else}
      <Button variant="primary" onclick={() => (createOpen = false)}>{t("common.done")}</Button>
    {/if}
  {/snippet}
</Dialog>

<Dialog bind:open={revokeOpen} size="sm" title={t("ak.revoke.title", { name: revoking?.name ?? "" })} description={t("ak.revoke.body")}>
  <span class="sr-only">{revoking?.key_prefix}</span>
  {#snippet footer()}
    <Button variant="ghost" onclick={() => (revokeOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" loading={busy} onclick={confirmRevoke}>{t("ak.revoke")}</Button>
  {/snippet}
</Dialog>

<style>
  .page {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    width: 100%;
    max-width: 880px;
    margin: 0 auto;
    padding: var(--sp-6) var(--sp-8) var(--sp-12);
  }
  .kname {
    font-weight: var(--fw-medium);
  }
  .limits {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .sk {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    padding: var(--sp-5);
  }
  .sk-row {
    display: flex;
    gap: var(--sp-6);
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .picks {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    margin-top: var(--sp-3);
    padding: var(--sp-3) var(--sp-4);
    border: var(--hairline) solid var(--border-subtle);
    border-radius: var(--r-sm);
    background: var(--bg-subtle);
  }
  .hint {
    color: var(--text-muted);
    font-size: var(--fs-sm);
    line-height: var(--lh);
  }
  @media (max-width: 720px) {
    .page {
      padding: var(--sp-4) var(--sp-4) var(--sp-10);
    }
  }
</style>
