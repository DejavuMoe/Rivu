<!--
  添加站点：域名（实时规范化）、名称（默认跟随域名）、时区。只有当前组织的 owner / admin 能进，其他角色是 404。
  创建成功后跳到安装页。
-->
<script lang="ts">
  import { goto } from "$app/navigation";
  import NotFound from "#lib/app/NotFound.svelte";
  import PageHeader from "#lib/app/PageHeader.svelte";
  import TimezoneSelect from "#lib/app/TimezoneSelect.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Card from "#lib/ui/Card.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import { isValidDomain, normalizeDomain } from "#lib/admin/domain.ts";
  import { api, ApiError } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import { canAddSite, refreshSites, session } from "#lib/session.svelte.ts";
  import { toast } from "#lib/state.svelte.ts";
  import "../strings";

  function browserZone() {
    try {
      return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
    } catch {
      return "UTC";
    }
  }

  let raw = $state("");
  let name = $state("");
  let nameTouched = $state(false);
  let tz = $state(browserZone());
  let tried = $state(false);
  let busy = $state(false);
  /** 服务端按字段返回的错误 */
  let serverErr = $state<{ domain?: string; name?: string; timezone?: string }>({});

  const host = $derived(normalizeDomain(raw));
  const hostOk = $derived(isValidDomain(host));
  const domainError = $derived(
    serverErr.domain ?? (!raw.trim() ? t("as.domain.required") : !hostOk ? t("as.domain.invalid") : ""),
  );
  // 用户没有手动改过名称时，名称跟随规范化后的域名
  const shownName = $derived(nameTouched ? name : host);
  const showError = $derived(!!serverErr.domain || tried || (raw.trim().length > 0 && !hostOk && /\.[a-z0-9-]{2,}/i.test(raw)));

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    serverErr = {};
    if (!hostOk || busy || session.orgId === null) return;
    busy = true;
    try {
      const label = nameTouched ? shownName.trim() : "";
      const { site } = await api.createSite(session.orgId, { domain: raw.trim(), name: label || undefined, timezone: tz });
      await refreshSites();
      toast(t("as.created", { name: site.name }), "good");
      await goto(`/s/${site.id}/install`);
    } catch (err) {
      if (err instanceof ApiError && err.status === 400) {
        const m = err.message;
        if (m.startsWith("the domain")) serverErr.domain = m;
        else if (m.startsWith("the name")) serverErr.name = m;
        else if (m.startsWith("the timezone")) serverErr.timezone = m;
        else toast(m, "bad");
      } else toast(t("as.failed"), "bad");
    } finally {
      busy = false;
    }
  }
</script>

{#if !canAddSite()}
  <NotFound />
{:else}
  <div class="page">
    <PageHeader title={t("as.title")} />

    <form onsubmit={submit} novalidate>
      <Card>
        <Field layout="row" label={t("as.domain")} for="as-domain" error={showError ? domainError : ""}>
          <Input
            id="as-domain"
            mono
            autocomplete="off"
            autocapitalize="off"
            spellcheck="false"
            placeholder="example.com"
            bind:value={raw}
            oninput={() => (serverErr.domain = undefined)}
            invalid={showError && !!domainError}
          />
          {#if hostOk && host !== raw.trim()}
            <p class="norm" aria-live="polite">{t("as.normalized", { host })}</p>
          {/if}
        </Field>
        <Field layout="row" label={t("as.name")} for="as-name" error={serverErr.name}>
          <Input
            id="as-name"
            value={shownName}
            invalid={!!serverErr.name}
            oninput={(e) => {
              nameTouched = true;
              name = e.currentTarget.value;
              serverErr.name = undefined;
            }}
          />
        </Field>
        <Field layout="row" label={t("as.timezone")} for="as-tz" error={serverErr.timezone}>
          <TimezoneSelect id="as-tz" bind:value={tz} onchange={() => (serverErr.timezone = undefined)} />
        </Field>
        {#snippet footer()}
          <span></span>
          <div class="btns">
            <Button href="/sites" variant="ghost">{t("common.cancel")}</Button>
            <Button type="submit" variant="primary" loading={busy}>{t("as.submit")}</Button>
          </div>
        {/snippet}
      </Card>
    </form>
  </div>
{/if}

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
  .norm {
    color: var(--text-muted);
    font-size: var(--fs-sm);
  }
  .btns {
    display: flex;
    gap: var(--sp-2);
  }
  @media (max-width: 720px) {
    .page {
      padding: var(--sp-4) var(--sp-4) var(--sp-10);
    }
  }
</style>
