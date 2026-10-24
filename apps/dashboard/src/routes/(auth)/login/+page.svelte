<!--
  登录页：错误统一提示（不区分账号是否存在）；被限流时倒计时并禁用提交。
  登录后回到 ?next= 指向的站内页面，没有则去站点列表。
-->
<script lang="ts">
  import { Eye, EyeOff } from "@lucide/svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import AuthShell from "#lib/app/AuthShell.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Callout from "#lib/ui/Callout.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import { api, ApiError } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import { safeNext } from "#lib/nav.ts";
  import "../strings";

  let email = $state("");
  let password = $state("");
  let show = $state(false);
  let busy = $state(false);
  let error = $state<"" | "invalid" | "required" | "network" | "failed">("");
  let retry = $state(0);

  // 倒计时：每秒减一，到 0 自动解除
  $effect(() => {
    if (retry <= 0) return;
    const id = setInterval(() => {
      retry -= 1;
      if (retry <= 0) clearInterval(id);
    }, 1000);
    return () => clearInterval(id);
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy || retry > 0) return;
    if (!email.trim() || !password) {
      error = "required";
      return;
    }
    error = "";
    busy = true;
    try {
      await api.login({ email: email.trim(), password });
      await goto(safeNext(page.url.searchParams.get("next")), { replace: true });
    } catch (err) {
      busy = false;
      if (err instanceof ApiError && err.status === 429) retry = err.retryAfter ?? 60;
      else if (err instanceof ApiError && err.status === 401) error = "invalid";
      else if (err instanceof ApiError && err.status === 0) error = "network";
      else error = "failed";
    }
  }
</script>

<AuthShell>
  <form onsubmit={submit} novalidate>
    <h1>{t("login.title")}</h1>

    {#if retry > 0}
      <Callout tone="warn">{t("login.rate", { n: retry })}</Callout>
    {:else if error !== ""}
      <Callout tone="bad">{t(`login.${error}`)}</Callout>
    {/if}

    <Field label={t("login.email")} for="login-email">
      <Input id="login-email" type="email" size="lg" autocomplete="username" bind:value={email} invalid={error === "invalid" || error === "required"} />
    </Field>
    <Field label={t("login.password")} for="login-password">
      <Input
        id="login-password"
        type={show ? "text" : "password"}
        size="lg"
        autocomplete="current-password"
        bind:value={password}
        invalid={error === "invalid" || error === "required"}
      >
        {#snippet suffix()}
          <Button
            variant="ghost"
            size="sm"
            icon={show ? EyeOff : Eye}
            aria-label={show ? t("login.hidePassword") : t("login.showPassword")}
            aria-pressed={show}
            onclick={() => (show = !show)}
          />
        {/snippet}
      </Input>
    </Field>

    <Button type="submit" variant="primary" size="lg" block loading={busy} disabled={retry > 0}>
      {retry > 0 ? t("login.rate.wait", { n: retry }) : t("login.submit")}
    </Button>
  </form>

  {#snippet below()}{t("login.help")}{/snippet}
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
