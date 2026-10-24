<!--
  危险操作：删除站点（软删除，立即生效；目前没有恢复与定时清除）。
  卡片只写后果和一个按钮，名称确认在对话框里（服务端要求 confirm 与站点名称完全一致）。
-->
<script lang="ts">
  import { goto } from "$app/navigation";
  import Button from "#lib/ui/Button.svelte";
  import Card from "#lib/ui/Card.svelte";
  import Dialog from "#lib/ui/Dialog.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import { api, type Site } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import { refreshSites } from "#lib/session.svelte.ts";
  import { toast } from "#lib/state.svelte.ts";
  import { errText } from "./settings.svelte.ts";
  import "./strings";

  let { site }: { site: Site } = $props();

  let open = $state(false);
  let typed = $state("");
  let busy = $state(false);
  const nameOk = $derived(typed === site.name);

  async function remove() {
    if (!nameOk || busy) return;
    busy = true;
    const name = site.name;
    try {
      await api.deleteSite(site.id, name);
      open = false;
      await refreshSites();
      toast(t("set.danger.site.done", { n: name }));
      await goto("/sites");
    } catch (e) {
      toast(errText(e), "bad");
    } finally {
      busy = false;
    }
  }
</script>

<Card title={t("set.danger.site.title")}>
  <div class="row">
    <p>{t("set.danger.site.body")}</p>
    <Button
      onclick={() => {
        typed = "";
        open = true;
      }}>{t("set.danger.site.button")}</Button
    >
  </div>
</Card>

<Dialog bind:open size="sm" title={t("set.danger.site.dlg.title")}>
  <Field label={t("set.danger.typeName", { n: site.name })} for="d-name">
    <Input id="d-name" bind:value={typed} autocomplete="off" placeholder={site.name} />
  </Field>
  {#snippet footer()}
    <Button onclick={() => (open = false)}>{t("common.cancel")}</Button>
    <Button variant="danger" loading={busy} disabled={!nameOk} onclick={remove}>{t("set.danger.site.confirm")}</Button>
  {/snippet}
</Dialog>

<style>
  .row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-4);
  }
  @media (max-width: 560px) {
    .row {
      flex-direction: column;
      align-items: flex-start;
    }
  }
</style>
