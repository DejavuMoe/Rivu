<!--
  首次初始化：创建第一个用户，同时成为实例管理员和第一个组织的所有者。
  初始化令牌字段只在 app.review.setupToken 为 true 时出现。
-->
<script lang="ts">
  import AuthShell from "./AuthShell.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import { t } from "../../lib/i18n";
  import { app, toast } from "../../lib/state.svelte";
  import { href, navigate } from "../../lib/router.svelte";
  import "./strings";

  let token = $state("");
  let name = $state("");
  let email = $state("");
  let password = $state("");
  let org = $state("");
  let tried = $state(false);
  let busy = $state(false);

  const emailOk = $derived(/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim()) && email.trim().length <= 320);
  const pwLen = $derived([...password].length);
  const orgLen = $derived([...org.trim()].length);

  const errors = $derived({
    token: app.review.setupToken && !token.trim() ? t("setup.token.required") : "",
    email: emailOk ? "" : t("setup.email.invalid"),
    password: pwLen < 12 ? t("setup.password.short") : pwLen > 128 ? t("setup.password.long") : "",
    org: orgLen < 1 || orgLen > 100 ? t("setup.orgName.required") : "",
  });
  const valid = $derived(!errors.token && !errors.email && !errors.password && !errors.org);

  function submit(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    if (!valid || busy) return;
    busy = true;
    setTimeout(() => {
      busy = false;
      toast(t("setup.done"), "good");
      navigate("/sites/new");
    }, 800);
  }
</script>

<AuthShell>
  <form onsubmit={submit} novalidate>
    <h1>{t("setup.title")}</h1>

    {#if app.review.setupToken}
      <Field label={t("setup.token")} for="setup-token" error={tried ? errors.token : ""}>
        <Input id="setup-token" mono autocomplete="off" spellcheck="false" bind:value={token} invalid={tried && !!errors.token} />
      </Field>
    {/if}
    <Field label={t("setup.name")} for="setup-name" optional>
      <Input id="setup-name" autocomplete="name" bind:value={name} />
    </Field>
    <Field label={t("setup.email")} for="setup-email" error={tried ? errors.email : ""}>
      <Input id="setup-email" type="email" autocomplete="username" bind:value={email} invalid={tried && !!errors.email} />
    </Field>
    <Field
      label={t("setup.password")}
      for="setup-password"
      hint={t("setup.password.hint")}
      error={tried || pwLen > 128 ? errors.password : ""}
    >
      <Input id="setup-password" type="password" autocomplete="new-password" bind:value={password} invalid={(tried || pwLen > 128) && !!errors.password} />
    </Field>
    <Field label={t("setup.orgName")} for="setup-org" error={tried ? errors.org : ""}>
      <Input id="setup-org" maxlength={120} bind:value={org} invalid={tried && !!errors.org} />
    </Field>

    <Button type="submit" variant="primary" size="lg" block loading={busy}>{t("setup.submit")}</Button>
  </form>

  {#snippet below()}{t("setup.have")}<a class="text-link" href={href("/login")}>{t("setup.signIn")}</a>{/snippet}
</AuthShell>

<style>
  form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  h1 {
    font-size: var(--fs-lg);
  }
</style>
