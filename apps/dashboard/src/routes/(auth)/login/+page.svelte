<!--
  登录页：错误统一提示（不区分账号是否存在）；被限流时倒计时并禁用提交。
  开了两步验证的账号：密码正确后同一张卡片换成验证码（可改用恢复码），凭 challenge 换会话。
  配置了邮件（GET /api/internal/auth-options）时密码标签右侧有「忘记密码？」。
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
  import { toast } from "#lib/state.svelte.ts";
  import "../strings";

  let email = $state("");
  let password = $state("");
  let show = $state(false);
  let busy = $state(false);
  let error = $state<"" | "invalid" | "required" | "network" | "failed">("");
  let retry = $state(0);
  let canReset = $state(false);

  $effect(() => {
    api.authOptions().then(
      (o) => (canReset = o.password_reset),
      () => {
        /* 拿不到就当没有配置邮件 */
      },
    );
  });

  // 第二步：验证码或恢复码
  let challenge = $state("");
  let recovery = $state(false);
  let code = $state("");
  let codeError = $state<"" | "invalid" | "expired" | "unavailable" | "network" | "failed">("");
  let codeInput: HTMLInputElement | undefined = $state();
  /** 第二步超时（challenge 过期或试了太多次），回到第一步时提示 */
  let expired = $state(false);

  // 倒计时：每秒减一，到 0 自动解除
  $effect(() => {
    if (retry <= 0) return;
    const id = setInterval(() => {
      retry -= 1;
      if (retry <= 0) clearInterval(id);
    }, 1000);
    return () => clearInterval(id);
  });

  // 进入第二步或换输入方式时把焦点放到输入框
  $effect(() => {
    if (challenge) {
      void recovery;
      codeInput?.focus();
    }
  });

  const next = () => goto(safeNext(page.url.searchParams.get("next")), { replace: true });

  function rateLimited(err: unknown): boolean {
    if (err instanceof ApiError && err.status === 429) {
      retry = err.retryAfter ?? 60;
      return true;
    }
    return false;
  }

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy || retry > 0) return;
    if (!email.trim() || !password) {
      error = "required";
      return;
    }
    error = "";
    expired = false;
    busy = true;
    try {
      const r = await api.login({ email: email.trim(), password });
      if (r.totp_required && r.challenge) {
        busy = false;
        challenge = r.challenge;
        code = "";
        codeError = "";
        recovery = false;
        return;
      }
      await next();
    } catch (err) {
      busy = false;
      if (rateLimited(err)) return;
      if (err instanceof ApiError && err.status === 401) error = "invalid";
      else if (err instanceof ApiError && err.status === 0) error = "network";
      else error = "failed";
    }
  }

  const codeReady = $derived(recovery ? code.replace(/[\s-]/g, "").length === 8 : /^\d{6}$/.test(code.replace(/\s/g, "")));

  async function submitCode(e: SubmitEvent) {
    e.preventDefault();
    if (busy || !codeReady || retry > 0) return;
    busy = true;
    try {
      const r = await api.loginTotp({ challenge, code });
      if (r.recovery_remaining !== undefined) toast(t("login.code.recoveryLeft", { n: r.recovery_remaining }));
      await next();
    } catch (err) {
      busy = false;
      code = "";
      if (rateLimited(err)) return;
      const c = err instanceof ApiError ? err.code : "";
      if (c === "challenge_expired") {
        back();
        error = "";
        codeError = "";
        expired = true;
        return;
      }
      codeError =
        c === "invalid_code" ? "invalid" : c === "totp_unavailable" ? "unavailable" : err instanceof ApiError && err.status === 0 ? "network" : "failed";
      codeInput?.focus();
    }
  }

  function back() {
    challenge = "";
    password = "";
    recovery = false;
  }
</script>

<AuthShell>
  {#if !challenge}
    <form onsubmit={submit} novalidate>
      <h1>{t("login.title")}</h1>

      {#if retry > 0}
        <Callout tone="warn">{t("login.rate", { n: retry })}</Callout>
      {:else if error !== ""}
        <Callout tone="bad">{t(`login.${error}`)}</Callout>
      {:else if expired}
        <Callout tone="warn">{t("login.code.expired")}</Callout>
      {/if}

      <Field label={t("login.email")} for="login-email">
        <Input id="login-email" type="email" size="lg" autocomplete="username" bind:value={email} invalid={error === "invalid" || error === "required"} />
      </Field>
      <Field label={t("login.password")} for="login-password">
        {#snippet labelExtra()}
          {#if canReset}<a class="forgot" href="/forgot-password">{t("login.forgot")}</a>{/if}
        {/snippet}
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
  {:else}
    <form class="step" onsubmit={submitCode} novalidate>
      <div class="head">
        <h1>{t("login.code.title")}</h1>
        <p class="muted">{recovery ? t("login.code.recoveryBody") : t("login.code.body", { email: email.trim() })}</p>
      </div>

      {#if retry > 0}
        <Callout tone="warn">{t("login.rate", { n: retry })}</Callout>
      {:else if codeError === "invalid"}
        <Callout tone="bad">{t("login.code.invalid")}</Callout>
      {:else if codeError === "unavailable"}
        <Callout tone="bad">{t("login.code.unavailable")}</Callout>
      {:else if codeError !== ""}
        <Callout tone="bad">{t(`login.${codeError}`)}</Callout>
      {/if}

      {#key recovery}
        <Field label={recovery ? t("login.code.recoveryLabel") : t("login.code.label")} for="login-code">
          <Input
            id="login-code"
            size="lg"
            mono
            bind:el={codeInput}
            bind:value={code}
            invalid={codeError === "invalid"}
            autocomplete={recovery ? "off" : "one-time-code"}
            inputmode={recovery ? "text" : "numeric"}
            maxlength={recovery ? 9 : 7}
            placeholder={recovery ? "xxxx-xxxx" : "000000"}
          />
        </Field>
      {/key}

      <Button type="submit" variant="primary" size="lg" block loading={busy} disabled={!codeReady || retry > 0}>{t("login.code.submit")}</Button>

      <div class="alt">
        <button
          type="button"
          class="link"
          onclick={() => {
            recovery = !recovery;
            code = "";
            codeError = "";
          }}>{recovery ? t("login.code.useApp") : t("login.code.useRecovery")}</button
        >
        <button type="button" class="link muted" onclick={back}>{t("login.code.back")}</button>
      </div>
    </form>
  {/if}

  {#snippet below()}{#if !challenge}{canReset ? t("login.helpMail") : t("login.help")}{/if}{/snippet}
</AuthShell>

<style>
  form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  /* 换成验证码一步时轻轻淡入：卡片高度会变，所以只做透明度，不做位移 */
  .step {
    animation: fade var(--dur-pop) var(--ease);
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  h1 {
    font-size: var(--fs-lg);
  }
  .head {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .forgot {
    color: var(--accent-text);
    font-size: var(--fs-sm);
    line-height: 20px;
  }
  .forgot:hover,
  .link:hover {
    text-decoration: underline;
    text-underline-offset: 3px;
  }
  .alt {
    display: flex;
    justify-content: space-between;
    gap: var(--sp-3);
    font-size: var(--fs-sm);
  }
  .link {
    padding: 0;
    border: 0;
    background: none;
    color: var(--accent-text);
    font: inherit;
    cursor: pointer;
  }
  .link.muted {
    color: var(--text-muted);
  }
</style>
