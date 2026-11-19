<!--
  我的 API 密钥：密钥表、创建对话框（创建后在同一对话框里只显示一次完整密钥，并附 curl 示例）、
  表格下方一行限额说明。所有组织角色（包括查看者）都能管理自己的密钥，只列出自己的。
  只以站点访客身份看到的组织：访客不能有 API 密钥，没有这一页。
  创建过程中、以及展示一次性密钥时，对话框不能用遮罩、Esc、关闭按钮退出，只能点「完成」。
-->
<script lang="ts">
  import { Inbox, Plus, TriangleAlert, Users } from "@lucide/svelte";
  import OrgHeader from "../../lib/app/OrgHeader.svelte";
  import Badge from "../../lib/ui/Badge.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Callout from "../../lib/ui/Callout.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Checkbox from "../../lib/ui/Checkbox.svelte";
  import CopyField from "../../lib/ui/CopyField.svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Segmented from "../../lib/ui/Segmented.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import type { Column } from "../../lib/ui/types";
  import { ORGS } from "../../lib/data/fixtures";
  import { SITES } from "../../lib/data/sites.svelte";
  import { full, relativeTime } from "../../lib/format";
  import { t } from "../../lib/i18n";
  import { href } from "../../lib/router.svelte";
  import { app, toast } from "../../lib/state.svelte";
  import { API_KEYS, daysFromNow, isoDate, type ApiKey } from "./data";
  import "./strings";

  let { orgId }: { orgId: string } = $props();

  let keys = $state<ApiKey[]>(structuredClone(API_KEYS));
  let busy = $state(false);

  const org = $derived(ORGS.find((o) => o.id === orgId));
  const sites = $derived(SITES.filter((s) => s.org === orgId));
  const mine = $derived(app.review.dataState === "empty" ? [] : keys.filter((k) => k.org === orgId));
  const loading = $derived(app.review.dataState === "loading");
  const failed = $derived(["error", "busy", "timeout"].includes(app.review.dataState));
  /** 用户在这个组织里没有角色（只以站点访客身份可见）：访客不能有 API 密钥 */
  const notMember = $derived(!org?.role);
  const ready = $derived(!loading && !notMember && !failed);
  const siteName = (id: string) => SITES.find((s) => s.id === Number(id))?.name ?? id;

  /** 范围描述：全部站点、站点名或站点数。 */
  function scopeText(k: ApiKey): string {
    if (k.scopes.includes("stats:read:*")) return t("ak.scope.all");
    if (k.scopes.length === 1) return siteName(k.scopes[0].split("/")[1]);
    return t("ak.scope.sites", { n: k.scopes.length });
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
  /** 示例请求里的站点标识：所选的第一个站点，没选就用组织的第一个站点 */
  let mintedSite = $state("");
  let seq = 0;

  const pickedIds = $derived(sites.filter((s) => picked[s.id]).map((s) => s.id));
  const nameLen = $derived([...name.trim()].length);
  const nameError = $derived(nameLen < 1 || nameLen > 100 ? t("ak.name.required") : "");
  const pickError = $derived(scopeMode === "pick" && pickedIds.length === 0 ? t("ak.scope.pick.required") : "");
  const expiryDate = $derived(expiry === "never" ? null : daysFromNow(Number(expiry)));

  const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
  /** 原型里的假密钥：rivu_ + 43 个 base64url 字符（32 字节），固定序列、不用随机数。 */
  function mint(n: number) {
    let x = Math.imul(n + 7, 2654435761) >>> 0;
    let s = "rivu_";
    for (let i = 0; i < 43; i++) {
      x = (Math.imul(x, 1664525) + 1013904223) >>> 0;
      s += ALPHABET[x >>> 26];
    }
    return s;
  }

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

  function create(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    if (nameError || pickError || busy) return;
    const label = name.trim();
    const ids = scopeMode === "pick" ? [...pickedIds] : [];
    const days = expiry === "never" ? null : Number(expiry);
    busy = true;
    setTimeout(() => {
      minted = mint(++seq);
      mintedSite = sites.find((s) => s.id === ids[0])?.siteKey ?? sites[0]?.siteKey ?? "<site_key>";
      keys.unshift({
        id: `k${Date.now()}`,
        org: orgId,
        name: label,
        prefix: minted.slice(0, 12),
        scopes: ids.length === 0 ? ["stats:read:*"] : ids.map((id) => `stats:read:site/${id}`),
        createdDaysAgo: 0,
        lastUsedMinutesAgo: null,
        expiresInDays: days,
      });
      busy = false;
      phase = "done";
      toast(t("ak.created.toast", { name: label }), "good");
    }, 600);
  }

  // ---- 撤销 ----
  let revokeOpen = $state(false);
  let revoking = $state<ApiKey | null>(null);
  function askRevoke(k: ApiKey) {
    revoking = k;
    revokeOpen = true;
  }
  function confirmRevoke() {
    const k = revoking;
    if (!k || busy) return;
    busy = true;
    setTimeout(() => {
      keys = keys.filter((x) => x.id !== k.id);
      busy = false;
      revokeOpen = false;
      toast(t("ak.revoked", { name: k.name }));
    }, 400);
  }

  const curl = $derived(`curl -X POST https://stats.example.com/api/v1/query \\
  -H "Authorization: Bearer ${minted}" \\
  -H "Content-Type: application/json" \\
  -d '{"site":"${mintedSite}","metrics":["visitors"],"date_range":"7d"}'`);
</script>

<div class="page">
  <OrgHeader {orgId}>
    {#snippet actions()}
      {#if ready}<Button variant="primary" icon={Plus} onclick={openCreate}>{t("ak.create")}</Button>{/if}
    {/snippet}
  </OrgHeader>

  {#if loading}
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
        {#snippet actions()}<Button href={href("/sites")}>{t("nav.allSites")}</Button>{/snippet}
      </EmptyState>
    </Card>
  {:else if failed}
    <Card>
      <EmptyState icon={TriangleAlert} tone="bad" title={t("ak.error.title")}>
        {#snippet actions()}<Button onclick={() => toast(t("common.retry"))}>{t("common.retry")}</Button>{/snippet}
      </EmptyState>
    </Card>
  {:else if mine.length === 0}
    <Card>
      <EmptyState icon={Inbox} title={t("ak.empty.title")}>
        {#snippet actions()}<Button variant="primary" icon={Plus} onclick={openCreate}>{t("ak.create")}</Button>{/snippet}
      </EmptyState>
    </Card>
  {:else}
    <Card flush>
      <Table columns={cols} rows={mine} rowKey={(k) => k.id} caption={t("ak.title")}>
        {#snippet cell(k, c)}
          {#if c.key === "name"}
            <span class="kname">{k.name}</span>
          {:else if c.key === "prefix"}
            <code class="muted">{k.prefix}</code>
          {:else if c.key === "scope"}
            {scopeText(k)}
          {:else if c.key === "used"}
            <span class="muted">{k.lastUsedMinutesAgo === null ? t("ak.neverUsed") : relativeTime(k.lastUsedMinutesAgo, app.lang)}</span>
          {:else if c.key === "expires"}
            {#if k.expiresInDays === null}
              <span class="muted">{t("ak.never")}</span>
            {:else if k.expiresInDays <= 0}
              <Badge tone="warn">{t("ak.expired")}</Badge>
            {:else if k.expiresInDays <= 7}
              <Badge tone="warn">{t("ak.expiresSoon", { n: k.expiresInDays })}</Badge>
            {:else}
              <span class="muted num">{isoDate(daysFromNow(k.expiresInDays))}</span>
            {/if}
          {:else if c.key === "act"}
            <Button variant="ghost" size="sm" aria-label={`${t("ak.revoke")} ${k.name}`} onclick={() => askRevoke(k)}>{t("ak.revoke")}</Button>
          {/if}
        {/snippet}
      </Table>
    </Card>
    <p class="limits">{t("ak.limits", { hourly: full(10000, app.lang), burst: 60, concurrent: 4 })}</p>
  {/if}
</div>

<Dialog bind:open={createOpen} requireConfirmation={busy || phase !== "form"} title={phase === "form" ? t("ak.dialog.title") : t("ak.created.title")} onclose={() => (minted = "")}>
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
              <Checkbox bind:checked={picked[s.id]}>{s.name}</Checkbox>
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
        <p class="hint">{expiryDate ? t("ak.exp.on", { date: isoDate(expiryDate) }) : t("ak.exp.neverNote")}</p>
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
      <Button variant="ghost" disabled={busy} onclick={() => (createOpen = false)}>{t("common.cancel")}</Button>
      <Button type="submit" form="key-form" variant="primary" loading={busy}>{t("ak.create")}</Button>
    {:else}
      <Button variant="primary" onclick={() => (createOpen = false)}>{t("common.done")}</Button>
    {/if}
  {/snippet}
</Dialog>

<Dialog bind:open={revokeOpen} size="sm" title={t("ak.revoke.title", { name: revoking?.name ?? "" })} description={t("ak.revoke.body")}>
  <span class="sr-only">{revoking?.prefix}</span>
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
    max-width: var(--content-max);
    margin: 0 auto;
    padding: var(--sp-6) var(--sp-8) var(--sp-12);
  }
  /* 与其他页面同一左缘，整页（含页头）限宽：动作按钮挨着正文 */
  .page > :global(*) {
    max-width: 880px;
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
