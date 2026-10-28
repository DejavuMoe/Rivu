<!--
  重设密码（邮件里的链接）：先检查令牌，无效（过期、用过、不存在）时说明并给「重新申请」；
  有效时设新密码 → 完成后去登录（服务端撤销了全部会话，不自动登录）。
-->
<script lang="ts">
  import { untrack } from "svelte";
  import { CircleCheck, Link2Off } from "@lucide/svelte";
  import { page } from "$app/state";
  import AuthShell from "#lib/app/AuthShell.svelte";
  import Button from "#lib/ui/Button.svelte";
  import Callout from "#lib/ui/Callout.svelte";
  import Field from "#lib/ui/Field.svelte";
  import Input from "#lib/ui/Input.svelte";
  import Skeleton from "#lib/ui/Skeleton.svelte";
  import { api, ApiError } from "#lib/api.ts";
  import { t } from "#lib/i18n.ts";
  import "../../strings";

  const token = $derived(page.params.token ?? "");
  let status = $state<"loading" | "ready" | "invalid" | "done">("loading");

  $effect(() => {
    const tk = token;
    untrack(() => {
      status = "loading";
      api.checkPasswordReset(tk).then(
        () => (status = "ready"),
        // 404 与其他错误都按失效处理：用户能做的只有重新申请
        () => (status = "invalid"),
      );
    });
  });

  let next = $state("");
  let again = $state("");
  let tried = $state(false);
  let busy = $state(false);
  let failed = $state<"" | "network" | "failed">("");
  const len = $derived([...next].length);
  const err = $derived({
    next: len < 12 ? t("setup.password.short") : len > 128 ? t("setup.password.long") : "",
    again: again === next ? "" : t("reset.mismatch"),
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    if (err.next || err.again || busy) return;
    busy = true;
    failed = "";
    try {
      await api.resetPassword(token, next);
      status = "done";
    } catch (e) {
      if (e instanceof ApiError && e.status === 404) status = "invalid";
      else failed = e instanceof ApiError && e.status === 0 ? "network" : "failed";
    } finally {
      busy = false;
    }
  }
</script>

<AuthShell>
  {#if status === "loading"}
    <div class="col" aria-busy="true"><Skeleton width="50%" height={20} /><Skeleton height={40} radius={6} /></div>
  {:else if status === "invalid"}
    <div class="col">
      <span class="icon bad" aria-hidden="true"><Link2Off size={20} strokeWidth={1.75} /></span>
      <h1>{t("reset.invalidTitle")}</h1>
      <p class="muted">{t("reset.invalidBody")}</p>
      <Button variant="primary" size="lg" block href="/forgot-password">{t("reset.again")}</Button>
    </div>
  {:else if status === "done"}
    <div class="col done">
      <span class="icon good" aria-hidden="true"><CircleCheck size={20} strokeWidth={1.75} /></span>
      <h1>{t("reset.doneTitle")}</h1>
      <p class="muted">{t("reset.doneBody")}</p>
      <Button variant="primary" size="lg" block href="/login">{t("reset.toLogin")}</Button>
    </div>
  {:else}
    <form class="col" onsubmit={submit} novalidate>
      <h1>{t("reset.title")}</h1>
      {#if failed}<Callout tone="bad">{t(`login.${failed}`)}</Callout>{/if}
      <Field label={t("reset.new")} for="reset-new" hint={t("setup.password.hint")} error={tried || len > 128 ? err.next : undefined}>
        <Input id="reset-new" type="password" size="lg" autocomplete="new-password" bind:value={next} invalid={(tried || len > 128) && !!err.next} />
      </Field>
      <Field label={t("reset.confirm")} for="reset-again" error={tried ? err.again : undefined}>
        <Input id="reset-again" type="password" size="lg" autocomplete="new-password" bind:value={again} invalid={tried && !!err.again} />
      </Field>
      <Button type="submit" variant="primary" size="lg" block loading={busy}>{t("reset.submit")}</Button>
    </form>
  {/if}
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
  h1 {
    font-size: var(--fs-lg);
  }
  .icon {
    display: grid;
    place-items: center;
    width: 40px;
    height: 40px;
    border-radius: 50%;
  }
  .icon.good {
    background: var(--good-bg);
    color: var(--good-text);
  }
  .icon.bad {
    background: var(--bg-element);
    color: var(--text-muted);
  }
</style>
