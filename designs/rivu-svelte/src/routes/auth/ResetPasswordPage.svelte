<!--
  重设密码（邮件里的链接）：先检查令牌，无效（过期、用过、不存在）时说明并给「重新申请」；
  有效时设新密码 → 完成后去登录（服务端撤销了全部会话，不自动登录）。
  原型里令牌为 expired 演示失效；评审面板的「数据状态」为加载时是检查令牌的骨架，「登录结果」演示提交时连不上服务器与其他失败。
-->
<script lang="ts">
  import { CircleCheck, Link2Off } from "@lucide/svelte";
  import AuthShell from "./AuthShell.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Callout from "../../lib/ui/Callout.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import { t } from "../../lib/i18n";
  import { app } from "../../lib/state.svelte";
  import { href } from "../../lib/router.svelte";
  import "./strings";

  let { token }: { token: string } = $props();
  const invalid = $derived(token === "expired" || !token);

  let next = $state("");
  let again = $state("");
  let tried = $state(false);
  let busy = $state(false);
  let done = $state(false);
  let failed = $state<"" | "network" | "failed">("");
  const len = $derived([...next].length);
  const err = $derived({
    next: len < 12 ? t("setup.password.short") : len > 128 ? t("setup.password.long") : "",
    again: again === next ? "" : t("reset.mismatch"),
  });

  function submit(e: SubmitEvent) {
    e.preventDefault();
    tried = true;
    if (err.next || err.again || busy) return;
    busy = true;
    failed = "";
    setTimeout(() => {
      busy = false;
      const r = app.review.login;
      if (r === "network" || r === "failed") failed = r;
      else done = true;
    }, 700);
  }
</script>

<AuthShell>
  {#if app.review.dataState === "loading"}
    <div class="col" aria-busy="true"><Skeleton width="50%" height={20} /><Skeleton height={40} radius={6} /></div>
  {:else if invalid}
    <div class="col">
      <span class="icon bad" aria-hidden="true"><Link2Off size={20} strokeWidth={1.75} /></span>
      <h1>{t("reset.invalidTitle")}</h1>
      <p class="muted">{t("reset.invalidBody")}</p>
      <Button variant="primary" size="lg" block href={href("/forgot-password")}>{t("reset.again")}</Button>
    </div>
  {:else if done}
    <div class="col done">
      <span class="icon good" aria-hidden="true"><CircleCheck size={20} strokeWidth={1.75} /></span>
      <h1>{t("reset.doneTitle")}</h1>
      <p class="muted">{t("reset.doneBody")}</p>
      <Button variant="primary" size="lg" block href={href("/login")}>{t("reset.toLogin")}</Button>
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
