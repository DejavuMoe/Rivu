<!--
  登录页。按评审面板的 app.review.login 演示各种结果：
  ok 回到 ?next= 指向的站内页面，没有则去站点列表；invalid 统一错误（不区分账号是否存在）；rateLimited 倒计时并禁用提交；
  network、failed 是连不上服务器与其他失败；totp 密码正确后同一张卡片换成验证码（开了两步验证的账号，可改用恢复码）。
  验证码一步的结果由 app.review.loginCode 演示（超时回到第一步并提示、服务器没配 RIVU_SECRET_KEY、被限流、其他失败），
  正常时验证码 000000 演示「不正确」。配置了邮件（app.review.mail）时密码标签右侧有「忘记密码？」。
-->
<script lang="ts">
  import { Eye, EyeOff } from "@lucide/svelte";
  import AuthShell from "./AuthShell.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Callout from "../../lib/ui/Callout.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import { t } from "../../lib/i18n";
  import { app, setReview, toast } from "../../lib/state.svelte";
  import { href, navigate, route, safeNext } from "../../lib/router.svelte";
  import "./strings";

  let email = $state("");
  let password = $state("");
  let show = $state(false);
  let busy = $state(false);
  let error = $state<"" | "invalid" | "required" | "network" | "failed">("");
  let retry = $state(0);

  // 第二步：验证码或恢复码
  let step = $state<"password" | "code">("password");
  let recovery = $state(false);
  let code = $state("");
  let codeError = $state<"" | "invalid" | "unavailable" | "failed">("");
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
    if (step === "code") {
      void recovery;
      codeInput?.focus();
    }
  });

  function next() {
    const to = safeNext(route.current.query.get("next"));
    // 从邀请页来登录的，回去就是被邀请人本人：接受邀请那一步不再是「去登录」
    if (to.startsWith("/invite") && app.review.invite === "signedOut") setReview({ invite: "existing" });
    navigate(to);
  }

  function submit(e: SubmitEvent) {
    e.preventDefault();
    if (busy || retry > 0) return;
    if (!email.trim() || !password) {
      error = "required";
      return;
    }
    error = "";
    expired = false;
    busy = true;
    setTimeout(() => {
      busy = false;
      const r = app.review.login;
      if (r === "ok") next();
      else if (r === "invalid") error = "invalid";
      else if (r === "network" || r === "failed") error = r;
      else if (r === "totp") {
        step = "code";
        code = "";
        codeError = "";
        recovery = false;
      } else retry = 30;
    }, 650);
  }

  const codeReady = $derived(recovery ? code.replace(/[\s-]/g, "").length === 8 : /^\d{6}$/.test(code.replace(/\s/g, "")));

  function submitCode(e: SubmitEvent) {
    e.preventDefault();
    if (busy || !codeReady || retry > 0) return;
    const entered = code.replace(/\s/g, "");
    busy = true;
    setTimeout(() => {
      busy = false;
      const r = app.review.loginCode;
      if (r === "ok" && entered !== "000000") {
        // 用掉一个恢复码时提示还剩几个
        if (recovery) toast(t("login.code.recoveryLeft", { n: 7 }));
        next();
        return;
      }
      code = "";
      if (r === "rateLimited") retry = 30;
      else if (r === "expired") {
        back();
        error = "";
        codeError = "";
        expired = true;
      } else {
        codeError = r === "ok" ? "invalid" : r;
        codeInput?.focus();
      }
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
          {#if app.review.mail}<a class="forgot" href={href("/forgot-password")}>{t("login.forgot")}</a>{/if}
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
