<!--
  找回密码：填邮箱 → 「查看你的邮箱」。不论账号是否存在都显示同样的结果（服务端也一样回 202）。
  实例没有配置邮件（app.review.mail 关）时只说明去找管理员。
-->
<script lang="ts">
  import { MailCheck } from "@lucide/svelte";
  import AuthShell from "./AuthShell.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import { t } from "../../lib/i18n";
  import { app } from "../../lib/state.svelte";
  import { href } from "../../lib/router.svelte";
  import "./strings";

  let email = $state("");
  let tried = $state(false);
  let busy = $state(false);
  let sent = $state(false);
  const valid = $derived(/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim()));

  function submit(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    if (!valid || busy) return;
    busy = true;
    setTimeout(() => {
      busy = false;
      sent = true;
    }, 600);
  }
</script>

<AuthShell>
  {#if !app.review.mail}
    <div class="col">
      <h1>{t("forgot.title")}</h1>
      <p class="muted">{t("forgot.unavailable")}</p>
      <Button size="lg" block href={href("/login")}>{t("forgot.back")}</Button>
    </div>
  {:else if sent}
    <div class="col done">
      <span class="icon" aria-hidden="true"><MailCheck size={20} strokeWidth={1.75} /></span>
      <h1>{t("forgot.sentTitle")}</h1>
      <p class="muted">{t("forgot.sentBody", { email: email.trim() })}</p>
      <Button size="lg" block href={href("/login")}>{t("forgot.back")}</Button>
      <button type="button" class="link" onclick={() => ((sent = false), (tried = false))}>{t("forgot.again")}</button>
    </div>
  {:else}
    <form class="col" onsubmit={submit} novalidate>
      <div class="head">
        <h1>{t("forgot.title")}</h1>
        <p class="muted">{t("forgot.body")}</p>
      </div>
      <Field label={t("login.email")} for="forgot-email" error={tried && !valid ? t("setup.email.invalid") : undefined}>
        <Input id="forgot-email" type="email" size="lg" autocomplete="username" bind:value={email} invalid={tried && !valid} />
      </Field>
      <Button type="submit" variant="primary" size="lg" block loading={busy}>{t("forgot.submit")}</Button>
    </form>
  {/if}

  {#snippet below()}{#if app.review.mail && !sent}<a href={href("/login")}>{t("forgot.back")}</a>{/if}{/snippet}
</AuthShell>

<style>
  .col {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .done {
    animation: fade var(--dur-pop) var(--ease);
  }
  @keyframes fade {
    from {
      opacity: 0;
    }
  }
  .head {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  h1 {
    font-size: var(--fs-lg);
  }
  .icon {
    display: grid;
    place-items: center;
    width: 40px;
    height: 40px;
    border-radius: 50%;
    background: var(--good-bg);
    color: var(--good-text);
  }
  .link {
    align-self: center;
    padding: 0;
    border: 0;
    background: none;
    color: var(--text-muted);
    font: inherit;
    font-size: var(--fs-sm);
    cursor: pointer;
  }
  .link:hover {
    color: var(--text);
  }
</style>
