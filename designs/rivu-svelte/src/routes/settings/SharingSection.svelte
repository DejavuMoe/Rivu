<!--
  共享链接：只读统计权限；可设密码、绑定分群、设有效期。
  表格 + 右上角「创建链接」；密码的 12 小时有效期写在创建对话框的字段提示里。
-->
<script lang="ts">
  import { Copy, Lock, Plus, Trash2 } from "@lucide/svelte";
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
  import { NOW, USER, type Site } from "../../lib/data/fixtures";
  import { cfgs, INSTANCE, nextId, SEGMENTS, ymd } from "./store.svelte";
  import "./strings";

  let { site, readOnly = false }: { site: Site; readOnly?: boolean } = $props();
  const cfg = $derived(cfgs[site.id]);

  const expired = (d: Date | null) => !!d && d.getTime() < NOW.getTime();

  let open = $state(false);
  let name = $state("");
  let password = $state("");
  let expiry = $state<"never" | "7" | "30" | "custom">("never");
  let custom = $state("");
  let segment = $state("");

  const minDate = ymd(new Date(NOW.getTime() + 864e5));
  const customDate = $derived(custom ? new Date(`${custom}T23:59:59`) : null);
  const customBad = $derived(expiry === "custom" && (!customDate || Number.isNaN(customDate.getTime()) || customDate.getTime() < NOW.getTime()));
  const nameBad = $derived(name.trim().length === 0);
  const pwBad = $derived(password.length > 0 && password.length < 8);
  const canCreate = $derived(!nameBad && !pwBad && !customBad);

  function openAdd() {
    name = password = custom = segment = "";
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
    const expires =
      expiry === "never" ? null : expiry === "custom" ? customDate : new Date(NOW.getTime() + Number(expiry) * 864e5);
    cfg.links.unshift({
      id: nextId(),
      name: name.trim(),
      slug: slug(),
      password: password.length > 0,
      segment,
      expires,
      by: USER.name,
      createdAt: NOW,
    });
    open = false;
    toast(t("set.share.created"), "good");
  }

  async function copy(slugText: string) {
    try {
      await navigator.clipboard.writeText(`${INSTANCE}/share/${slugText}`);
    } catch {
      /* 原型里剪贴板不可用时照样给反馈 */
    }
    toast(t("set.share.copied"), "good");
  }

  let revoking = $state<number | null>(null);
  let revokeOpen = $state(false);
  const revokingLink = $derived(cfg.links.find((l) => l.id === revoking));
  function confirmRevoke() {
    cfg.links = cfg.links.filter((l) => l.id !== revoking);
    revokeOpen = false;
    toast(t("set.share.revoked"));
  }
</script>

<Card title={t("set.share.title")} flush>
  {#snippet actions()}
    {#if !readOnly}<Button variant="primary" icon={Plus} onclick={openAdd}>{t("set.share.add")}</Button>{/if}
  {/snippet}
  {#if cfg.links.length === 0}
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
        { key: "x", label: "", width: readOnly ? "56px" : "88px", align: "right" },
      ]}
    >
      {#snippet cell(l, c)}
        {#if c.key === "name"}
          <span class="name" class:dead={expired(l.expires)}>
            {l.name}
            {#if l.password}<Badge icon={Lock}>{t("set.share.password")}</Badge>{/if}
            {#if l.segment}<Badge>{l.segment}</Badge>{/if}
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
            <Button variant="ghost" size="sm" icon={Copy} aria-label="{t('set.share.copyLink')} {l.name}" onclick={() => copy(l.slug)} />
            {#if !readOnly}
              <Button
                variant="ghost"
                size="sm"
                icon={Trash2}
                aria-label="{t('set.share.revoke')} {l.name}"
                onclick={() => {
                  revoking = l.id;
                  revokeOpen = true;
                }}
              />
            {/if}
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
    <Field label={t("set.share.name")} for="l-name">
      <Input id="l-name" bind:value={name} maxlength={80} />
    </Field>
    <Field label={t("set.share.pw")} for="l-pw" optional hint={t("set.share.pwHint")} error={pwBad ? t("set.share.pwShort") : undefined}>
      <Input id="l-pw" type="password" bind:value={password} autocomplete="new-password" invalid={pwBad} />
    </Field>
    <Field label={t("set.share.expiry")}>
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
        <Input type="date" bind:value={custom} min={minDate} aria-label={t("set.share.exp.custom")} invalid={customBad && !!custom} />
      {/if}
    </Field>
    <Field label={t("set.share.segment")} optional hint={t("set.share.segmentHint")}>
      <Select
        label={t("set.share.segment")}
        block
        width={300}
        bind:value={segment}
        options={[{ value: "", label: t("common.none") }, ...SEGMENTS.map((s) => ({ value: s, label: s }))]}
      />
    </Field>
  </form>
  {#snippet footer()}
    <Button onclick={() => (open = false)}>{t("common.cancel")}</Button>
    <Button variant="primary" type="submit" form="share-form" disabled={!canCreate}>{t("set.share.create")}</Button>
  {/snippet}
</Dialog>

<Dialog bind:open={revokeOpen} size="sm" title={t("set.share.rv.title", { n: revokingLink?.name ?? "" })}>
  <p>{t("set.share.rv.body")}</p>
  {#snippet footer()}
    <Button onclick={() => (revokeOpen = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" onclick={confirmRevoke}>{t("set.share.revoke")}</Button>
  {/snippet}
</Dialog>

<style>
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
