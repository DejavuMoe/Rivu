<!--
  共享链接：只读统计权限；可设密码、有效期。表格 + 右上角「创建链接」；密码的 12 小时有效期写在字段提示里。
  有效期选项换算成所选日期当天本地时间的末尾（见 store.svelte.ts 的 computeExpiry）。已过期的链接仍列出，标「已过期」。
  组织角色 editor 起可管理，站点访客不行（分区本身对访客不可见）。
-->
<script lang="ts">
  import { CircleAlert, Copy, Lock, Plus, Trash2 } from "@lucide/svelte";
  import Badge from "../../lib/ui/Badge.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Dialog from "../../lib/ui/Dialog.svelte";
  import EmptyState from "../../lib/ui/EmptyState.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Segmented from "../../lib/ui/Segmented.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import Table from "../../lib/ui/Table.svelte";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import { NOW, USER, type Site } from "../../lib/data/fixtures";
  import { cfgs, computeExpiry, INSTANCE, nextId, ymd, type ExpiryKind, type ShareLink } from "./store.svelte";
  import "./strings";

  let { site }: { site: Site } = $props();
  const cfg = $derived(cfgs[site.id]);

  const loading = $derived(app.review.dataState === "loading");
  const failed = $derived(["error", "busy", "timeout"].includes(app.review.dataState));

  const expired = (d: Date | null) => !!d && d.getTime() < NOW.getTime();

  let open = $state(false);
  let name = $state("");
  let password = $state("");
  let expiry = $state<ExpiryKind>("never");
  let custom = $state("");

  const minDate = ymd(new Date(NOW.getTime() + 864e5));
  const exp = $derived(computeExpiry(expiry, custom, NOW));
  const customBad = $derived(expiry === "custom" && !!custom && exp.error !== null);
  // 与服务端一致：按字符计，12 到 128
  const pwBad = $derived(password.length > 0 && [...password].length < 12);
  const canCreate = $derived(!pwBad && exp.error === null);

  function openAdd() {
    name = password = custom = "";
    expiry = "never";
    open = true;
  }

  function slug() {
    const alphabet = "abcdefghijklmnopqrstuvwxyz0123456789";
    const bytes = crypto.getRandomValues(new Uint8Array(24));
    return Array.from(bytes, (b) => alphabet[b % alphabet.length]).join("");
  }

  function create() {
    if (!canCreate) return;
    cfg.links.unshift({
      id: nextId(),
      name: name.trim(),
      slug: slug(),
      password: password.length > 0,
      expires: exp.at,
      by: USER.name,
      createdAt: NOW,
    });
    open = false;
    toast(t("set.share.created"), "good");
  }

  async function copy(slugText: string) {
    try {
      await navigator.clipboard.writeText(`${INSTANCE}/share/${slugText}`);
      toast(t("set.share.copied"), "good");
    } catch {
      toast(t("set.err.generic"), "bad");
    }
  }

  let revoking = $state<number | null>(null);
  let revokeOpen = $state(false);
  const revokingLink = $derived(cfg.links.find((l) => l.id === revoking));
  function confirmRevoke() {
    cfg.links = cfg.links.filter((l) => l.id !== revoking);
    revokeOpen = false;
    toast(t("set.share.revoked"));
  }
  const label = (l: ShareLink) => l.name || t("set.share.unnamed");
</script>

<Card title={t("set.share.title")} flush>
  {#snippet actions()}
    {#if !loading && !failed}<Button variant="primary" icon={Plus} onclick={openAdd}>{t("set.share.add")}</Button>{/if}
  {/snippet}
  {#if failed}
    <EmptyState compact icon={CircleAlert} tone="bad" title={t("set.loadFailed")}>
      {#snippet actions()}<Button onclick={() => toast(t("common.retry"))}>{t("common.retry")}</Button>{/snippet}
    </EmptyState>
  {:else if loading}
    <div class="sk" aria-busy="true"><Skeleton height={32} radius={6} /><Skeleton height={32} radius={6} /></div>
  {:else if cfg.links.length === 0}
    <EmptyState compact title={t("set.share.none")} />
  {:else}
    <Table
      caption={t("set.share.title")}
      rows={cfg.links}
      rowKey={(l) => l.id}
      columns={[
        { key: "name", label: t("set.share.col.name") },
        { key: "exp", label: t("set.share.col.expiry"), width: "110px" },
        { key: "by", label: t("set.share.col.created"), width: "104px", optional: true },
        { key: "x", label: "", width: "88px", align: "right" },
      ]}
    >
      {#snippet cell(l, c)}
        {#if c.key === "name"}
          <span class="name" class:dead={expired(l.expires)}>
            {label(l)}
            {#if l.password}<Badge icon={Lock}>{t("set.share.password")}</Badge>{/if}
          </span>
        {:else if c.key === "exp"}
          {#if !l.expires}
            <span class="muted">{t("set.share.never")}</span>
          {:else if expired(l.expires)}
            <Badge tone="warn">{t("set.share.expiredBadge")}</Badge>
          {:else}
            <span class="num">{ymd(l.expires)}</span>
          {/if}
        {:else if c.key === "by"}
          <span class="by">
            <span>{l.by}</span>
            <span class="muted num">{ymd(l.createdAt)}</span>
          </span>
        {:else}
          <span class="acts">
            <Button variant="ghost" size="sm" icon={Copy} aria-label="{t('set.share.copyLink')} {label(l)}" onclick={() => copy(l.slug)} />
            <Button
              variant="ghost"
              size="sm"
              icon={Trash2}
              aria-label="{t('set.share.revoke')} {label(l)}"
              onclick={() => {
                revoking = l.id;
                revokeOpen = true;
              }}
            />
          </span>
        {/if}
      {/snippet}
    </Table>
  {/if}
</Card>

<Dialog bind:open title={t("set.share.dlg.title")}>
  <form
    id="share-form"
    class="form"
    onsubmit={(e) => {
      e.preventDefault();
      create();
    }}
  >
    <Field label={t("set.share.name")} for="l-name" optional>
      <Input id="l-name" bind:value={name} maxlength={100} />
    </Field>
    <Field label={t("set.share.pw")} for="l-pw" optional hint={t("set.share.pwHint")} error={pwBad ? t("set.share.pwShort") : undefined}>
      <Input id="l-pw" type="password" bind:value={password} maxlength={128} autocomplete="new-password" invalid={pwBad} />
    </Field>
    <Field label={t("set.share.expiry")} error={customBad ? t("set.share.expPast") : undefined}>
      <Segmented
        label={t("set.share.expiry")}
        block
        bind:value={expiry}
        options={[
          { value: "never", label: t("set.share.exp.never") },
          { value: "7", label: t("set.share.exp.7") },
          { value: "30", label: t("set.share.exp.30") },
          { value: "custom", label: t("set.share.exp.custom") },
        ]}
      />
      {#if expiry === "custom"}
        <Input type="date" bind:value={custom} min={minDate} aria-label={t("set.share.exp.custom")} invalid={customBad} />
      {/if}
    </Field>
  </form>
  {#snippet footer()}
    <Button onclick={() => (open = false)}>{t("common.cancel")}</Button>
    <Button variant="primary" type="submit" form="share-form" disabled={!canCreate}>{t("set.share.create")}</Button>
  {/snippet}
</Dialog>

<Dialog bind:open={revokeOpen} size="sm" title={t("set.share.rv.title", { n: revokingLink ? label(revokingLink) : "" })}>
  <p>{t("set.share.rv.body")}</p>
  {#snippet footer()}
    <Button onclick={() => (revokeOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" onclick={confirmRevoke}>{t("set.share.revoke")}</Button>
  {/snippet}
</Dialog>

<style>
  .sk {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    padding: var(--sp-4) var(--sp-5);
  }
  .name {
    display: inline-flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 6px var(--sp-2);
    font-weight: var(--fw-medium);
  }
  .dead {
    color: var(--text-muted);
  }
  .by {
    display: flex;
    flex-direction: column;
    padding: 4px 0;
  }
  .by .muted {
    font-size: var(--fs-sm);
  }
  .acts {
    display: inline-flex;
    gap: 2px;
  }
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
</style>
