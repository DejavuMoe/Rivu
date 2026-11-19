<!--
  首次初始化：创建第一个用户，同时成为实例管理员和第一个组织的所有者。
  令牌字段始终显示且可留空：只有服务端配置了 RIVU_SETUP_TOKEN 才会校验，界面事先不知道。
  评审面板的「初始化」切换：有令牌（输入 rivu 才对，空着或填错是字段错误）、无令牌、已经初始化过（提交后换成说明）；
  「登录结果」演示提交时被限流与连不上服务器。
-->
<script lang="ts">
  import AuthShell from "./AuthShell.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Callout from "../../lib/ui/Callout.svelte";
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
  let already = $state(false);
  /** 服务端返回的错误：令牌错误落在令牌字段，其余进表单级提示 */
  let server = $state({ token: "", form: "" });

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

  function submit(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    if (!valid || busy) return;
    server = { token: "", form: "" };
    busy = true;
    setTimeout(() => {
      busy = false;
      const r = app.review.login;
      if (app.review.setup === "done") already = true;
      else if (r === "rateLimited") server.form = t("login.rate", { n: 30 });
      else if (r === "network") server.form = t("login.network");
      else if (app.review.setup === "token" && token.trim() !== "rivu") server.token = t("setup.token.invalid");
      else {
        toast(t("setup.done"), "good");
        navigate("/sites");
      }
    }, 800);
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
      <Field label={t("setup.name")} for="setup-name" optional error={tried ? errors.name : ""}>
        <Input id="setup-name" autocomplete="name" bind:value={name} invalid={tried && !!errors.name} />
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
  {/if}

  {#snippet below()}{t("setup.have")}<a class="text-link" href={href("/login")}>{t("setup.signIn")}</a>{/snippet}
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
