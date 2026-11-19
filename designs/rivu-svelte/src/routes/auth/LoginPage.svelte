<!--
  登录页。按评审面板的 app.review.login 演示四种结果：
  ok 跳到站点列表；invalid 统一错误（不区分账号是否存在）；rateLimited 倒计时并禁用提交；
  totp 密码正确后同一张卡片换成验证码（开了两步验证的账号）。验证码一步里 000000 演示错误。
  配置了邮件（app.review.mail）时密码下面有「忘记密码？」。
-->
<script lang="ts">
  import { Eye, EyeOff } from "@lucide/svelte";
  import AuthShell from "./AuthShell.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Callout from "../../lib/ui/Callout.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import { t } from "../../lib/i18n";
  import { app } from "../../lib/state.svelte";
  import { href, navigate } from "../../lib/router.svelte";
  import "./strings";

  let email = $state("");
  let password = $state("");
  let show = $state(false);
  let busy = $state(false);
  let error = $state<"" | "invalid" | "required">("");
  let retry = $state(0);

  // 第二步：验证码或恢复码
  let step = $state<"password" | "code">("password");
  let recovery = $state(false);
  let code = $state("");
  let codeError = $state(false);
  let codeInput: HTMLInputElement | undefined = $state();

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
    if (step === "code") {
      void recovery;
      codeInput?.focus();
    }
  });

  function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy || retry > 0) return;
    if (!email.trim() || !password) {
      error = "required";
      return;
    }
    error = "";
    busy = true;
    setTimeout(() => {
      busy = false;
      const r = app.review.login;
      if (r === "ok") navigate("/sites");
      else if (r === "invalid") error = "invalid";
      else if (r === "totp") {
        step = "code";
        code = "";
        codeError = false;
      } else retry = 30;
    }, 650);
  }

  const codeReady = $derived(recovery ? code.replace(/[\s-]/g, "").length === 8 : /^\d{6}$/.test(code.replace(/\s/g, "")));

  function submitCode(e: SubmitEvent) {
    e.preventDefault();
    if (busy || !codeReady) return;
    busy = true;
    setTimeout(() => {
      busy = false;
      if (code.replace(/\s/g, "") === "000000") {
        codeError = true;
        code = "";
        codeInput?.focus();
      } else navigate("/sites");
    }, 500);
  }

  function back() {
    step = "password";
    password = "";
    recovery = false;
  }
</script>

<AuthShell>
  {#if step === "password"}
    <form onsubmit={submit} novalidate>
      <h1>{t("login.title")}</h1>

      {#if retry > 0}
        <Callout tone="warn">{t("login.rate", { n: retry })}</Callout>
      {:else if error === "invalid"}
        <Callout tone="bad">{t("login.invalid")}</Callout>
      {:else if error === "required"}
        <Callout tone="bad">{t("login.required")}</Callout>
      {/if}

      <Field label={t("login.email")} for="login-email">
        <Input id="login-email" type="email" size="lg" autocomplete="username" bind:value={email} invalid={error !== ""} />
      </Field>
      <Field label={t("login.password")} for="login-password">
        {#snippet labelExtra()}
          {#if app.review.mail}<a class="forgot" href={href("/forgot-password")}>{t("login.forgot")}</a>{/if}
        {/snippet}
          <Input
            id="login-password"
            type={show ? "text" : "password"}
            size="lg"
            autocomplete="current-password"
            bind:value={password}
            invalid={error !== ""}
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

      {#if codeError}<Callout tone="bad">{t("login.code.invalid")}</Callout>{/if}

      {#key recovery}
        <Field label={recovery ? t("login.code.recoveryLabel") : t("login.code.label")} for="login-code">
          <Input
            id="login-code"
            size="lg"
            mono
            bind:el={codeInput}
            bind:value={code}
            invalid={codeError}
            autocomplete={recovery ? "off" : "one-time-code"}
            inputmode={recovery ? "text" : "numeric"}
            maxlength={recovery ? 9 : 7}
            placeholder={recovery ? "xxxx-xxxx" : "000000"}
          />
        </Field>
      {/key}

      <Button type="submit" variant="primary" size="lg" block loading={busy} disabled={!codeReady}>{t("login.code.submit")}</Button>

      <div class="alt">
        <button
          type="button"
          class="link"
          onclick={() => {
            recovery = !recovery;
            code = "";
            codeError = false;
          }}>{recovery ? t("login.code.useApp") : t("login.code.useRecovery")}</button
        >
        <button type="button" class="link muted" onclick={back}>{t("login.code.back")}</button>
      </div>
    </form>
  {/if}

  {#snippet below()}{#if step === "password"}{app.review.mail ? t("login.helpMail") : t("login.help")}{/if}{/snippet}
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
