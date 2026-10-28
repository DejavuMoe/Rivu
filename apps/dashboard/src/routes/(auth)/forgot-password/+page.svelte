<!--
  找回密码：填邮箱 → 「查看你的邮箱」。不论账号是否存在都显示同样的结果（服务端一律 202）。
  实例没有配置邮件时（auth-options 的 password_reset 为假）只说明去找管理员。
-->
<script lang="ts">
  import { MailCheck } from "@lucide/svelte";
  import AuthShell from "#lib/app/AuthShell.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Callout from "#lib/ui/Callout.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import { api, ApiError } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import "../strings";

  let available = $state<boolean | null>(null);
  $effect(() => {
    api.authOptions().then(
      (o) => (available = o.password_reset),
      () => (available = false),
    );
  });

  let email = $state("");
  let tried = $state(false);
  let busy = $state(false);
  let sent = $state(false);
  let error = $state<"" | "rate" | "network" | "failed">("");
  let retry = $state(0);
  const valid = $derived(/^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim()));

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    if (!valid || busy) return;
    busy = true;
    error = "";
    try {
      await api.requestPasswordReset(email.trim());
      sent = true;
    } catch (err) {
      if (err instanceof ApiError && err.status === 429) {
        error = "rate";
        retry = err.retryAfter ?? 60;
      } else if (err instanceof ApiError && err.status === 404) available = false;
      else error = err instanceof ApiError && err.status === 0 ? "network" : "failed";
    } finally {
      busy = false;
    }
  }
</script>

<AuthShell>
  {#if available === null}
    <div class="col" aria-busy="true"><Skeleton width="50%" height={20} /><Skeleton height={40} radius={6} /></div>
  {:else if !available}
    <div class="col">
      <h1>{t("forgot.title")}</h1>
      <p class="muted">{t("forgot.unavailable")}</p>
      <Button size="lg" block href="/login">{t("forgot.back")}</Button>
    </div>
  {:else if sent}
    <div class="col done">
      <span class="icon" aria-hidden="true"><MailCheck size={20} strokeWidth={1.75} /></span>
      <h1>{t("forgot.sentTitle")}</h1>
      <p class="muted">{t("forgot.sentBody", { email: email.trim() })}</p>
      <Button size="lg" block href="/login">{t("forgot.back")}</Button>
      <button type="button" class="link" onclick={() => ((sent = false), (tried = false))}>{t("forgot.again")}</button>
    </div>
  {:else}
    <form class="col" onsubmit={submit} novalidate>
      <div class="head">
        <h1>{t("forgot.title")}</h1>
        <p class="muted">{t("forgot.body")}</p>
      </div>
      {#if error === "rate"}
        <Callout tone="warn">{t("login.rate", { n: retry })}</Callout>
      {:else if error !== ""}
        <Callout tone="bad">{t(`login.${error}`)}</Callout>
      {/if}
      <Field label={t("login.email")} for="forgot-email" error={tried && !valid ? t("setup.email.invalid") : undefined}>
        <Input id="forgot-email" type="email" size="lg" autocomplete="username" bind:value={email} invalid={tried && !valid} />
      </Field>
      <Button type="submit" variant="primary" size="lg" block loading={busy}>{t("forgot.submit")}</Button>
    </form>
  {/if}

  {#snippet below()}{#if available && !sent}<a href="/login">{t("forgot.back")}</a>{/if}{/snippet}
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
