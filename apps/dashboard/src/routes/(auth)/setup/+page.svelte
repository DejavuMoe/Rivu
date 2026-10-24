<!--
  首次初始化：创建第一个用户，同时成为实例管理员和第一个组织的所有者。
  令牌字段始终显示且可留空：只有服务端配置了 RIVU_SETUP_TOKEN 才会校验，界面事先不知道。
  服务端返回 404 表示已经初始化过。
-->
<script lang="ts">
  import { goto } from "$app/navigation";
  import AuthShell from "#lib/app/AuthShell.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Callout from "#lib/ui/Callout.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import { api, ApiError } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import { toast } from "#lib/state.svelte.ts";
  import "../strings";

  let token = $state("");
  let name = $state("");
  let email = $state("");
  let password = $state("");
  let org = $state("");
  let tried = $state(false);
  let busy = $state(false);
  let already = $state(false);
  /** 服务端返回的错误：映射到字段的放这里，其余进表单级提示 */
  let server = $state({ token: "", name: "", email: "", password: "", org: "", form: "" });

  const emailOk = $derived(/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim()) && email.trim().length <= 320);
  const pwLen = $derived([...password].length);
  const orgLen = $derived([...org.trim()].length);
  const nameLen = $derived([...name.trim()].length);

  const errors = $derived({
    name: nameLen > 100 ? t("setup.name.long") : "",
    email: emailOk ? "" : t("setup.email.invalid"),
    password: pwLen < 12 ? t("setup.password.short") : pwLen > 128 ? t("setup.password.long") : "",
    org: orgLen < 1 || orgLen > 100 ? t("setup.orgName.required") : "",
  });
  const valid = $derived(!errors.name && !errors.email && !errors.password && !errors.org);

  /** 400 invalid_request 的 message 以被校验的字段开头（the email … / the password … / the organization name … / the name …） */
  function mapInvalid(msg: string) {
    const m = msg.toLowerCase();
    if (m.includes("password")) server.password = msg;
    else if (m.includes("email")) server.email = msg;
    else if (m.includes("organization")) server.org = msg;
    else if (m.startsWith("the name")) server.name = msg;
    else server.form = msg;
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    if (!valid || busy) return;
    server = { token: "", name: "", email: "", password: "", org: "", form: "" };
    busy = true;
    try {
      await api.setup({
        token: token.trim() || undefined,
        email: email.trim(),
        password,
        org_name: org.trim(),
        name: name.trim() || undefined,
      });
      toast(t("setup.done"), "good");
      await goto("/sites", { replace: true });
    } catch (err) {
      busy = false;
      if (!(err instanceof ApiError)) throw err;
      if (err.status === 404) already = true;
      else if (err.status === 403) server.token = t("setup.token.invalid");
      else if (err.status === 400) mapInvalid(err.message);
      else if (err.status === 429) server.form = t("login.rate", { n: err.retryAfter ?? 60 });
      else server.form = err.status === 0 ? t("login.network") : err.message;
    }
  }
</script>

<AuthShell>
  {#if already}
    <div class="stack">
      <h1>{t("setup.title")}</h1>
      <Callout tone="info">{t("setup.already")}</Callout>
    </div>
  {:else}
    <form onsubmit={submit} novalidate>
      <h1>{t("setup.title")}</h1>

      {#if server.form}<Callout tone="bad">{server.form}</Callout>{/if}

      <Field label={t("setup.token")} for="setup-token" optional error={server.token}>
        <Input id="setup-token" mono autocomplete="off" spellcheck="false" bind:value={token} invalid={!!server.token} />
      </Field>
      <Field label={t("setup.name")} for="setup-name" optional error={(tried && errors.name) || server.name}>
        <Input id="setup-name" autocomplete="name" bind:value={name} invalid={(tried && !!errors.name) || !!server.name} />
      </Field>
      <Field label={t("setup.email")} for="setup-email" error={(tried && errors.email) || server.email}>
        <Input id="setup-email" type="email" autocomplete="username" bind:value={email} invalid={(tried && !!errors.email) || !!server.email} />
      </Field>
      <Field
        label={t("setup.password")}
        for="setup-password"
        hint={t("setup.password.hint")}
        error={((tried || pwLen > 128) && errors.password) || server.password}
      >
        <Input
          id="setup-password"
          type="password"
          autocomplete="new-password"
          bind:value={password}
          invalid={((tried || pwLen > 128) && !!errors.password) || !!server.password}
        />
      </Field>
      <Field label={t("setup.orgName")} for="setup-org" error={(tried && errors.org) || server.org}>
        <Input id="setup-org" maxlength={120} bind:value={org} invalid={(tried && !!errors.org) || !!server.org} />
      </Field>

      <Button type="submit" variant="primary" size="lg" block loading={busy}>{t("setup.submit")}</Button>
    </form>
  {/if}

  {#snippet below()}{t("setup.have")}<a class="text-link" href="/login">{t("setup.signIn")}</a>{/snippet}
</AuthShell>

<style>
  form,
  .stack {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  h1 {
    font-size: var(--fs-lg);
  }
</style>
