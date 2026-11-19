<!--
  添加站点：域名（实时规范化）、名称（默认跟随域名）、时区。只有所有者和管理员能进，其他角色是 404 风格。
  提交后跳到示例的未接入站点（站点 4）的安装页。
-->
<script lang="ts">
  import PageHeader from "../../lib/app/PageHeader.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Card from "../../lib/ui/Card.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import NotFound from "../NotFound.svelte";
  import TimezoneSelect from "./TimezoneSelect.svelte";
  import { t } from "../../lib/i18n";
  import { canManage, toast } from "../../lib/state.svelte";
  import { href, navigate } from "../../lib/router.svelte";
  import { isValidDomain, normalizeDomain } from "./domain";
  import "./strings";

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

  const host = $derived(normalizeDomain(raw));
  const hostOk = $derived(isValidDomain(host));
  const domainError = $derived(!raw.trim() ? t("as.domain.required") : !hostOk ? t("as.domain.invalid") : "");
  // 用户没有手动改过名称时，名称跟随规范化后的域名
  const shownName = $derived(nameTouched ? name : host);
  const showError = $derived(tried || (raw.trim().length > 0 && !hostOk && /\.[a-z0-9-]{2,}/i.test(raw)));

  function submit(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    if (!hostOk || busy) return;
    busy = true;
    const label = shownName.trim() || host;
    setTimeout(() => {
      busy = false;
      toast(t("as.created", { name: label }), "good");
      navigate("/s/4/install");
    }, 650);
  }
</script>

{#if !canManage()}
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
            invalid={showError && !!domainError}
          />
          {#if hostOk && host !== raw.trim()}
            <p class="norm" aria-live="polite">{t("as.normalized", { host })}</p>
          {/if}
        </Field>
        <Field layout="row" label={t("as.name")} for="as-name">
          <Input
            id="as-name"
            value={shownName}
            oninput={(e) => {
              nameTouched = true;
              name = e.currentTarget.value;
            }}
          />
        </Field>
        <Field layout="row" label={t("as.timezone")} for="as-tz">
          <TimezoneSelect id="as-tz" bind:value={tz} />
        </Field>
        {#snippet footer()}
          <span></span>
          <div class="btns">
            <Button href={href("/sites")} variant="ghost">{t("common.cancel")}</Button>
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
    max-width: var(--content-max);
    margin: 0 auto;
    padding: var(--sp-6) var(--sp-8) var(--sp-12);
  }
  /* 与其他页面同一左缘，整页（含页头）限宽：动作按钮挨着正文 */
  .page > :global(*) {
    max-width: 880px;
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
