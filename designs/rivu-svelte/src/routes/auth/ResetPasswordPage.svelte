<!--
  重设密码（邮件里的链接）：先检查令牌，无效时说明并给「重新申请」；有效时设新密码 → 完成后去登录（不自动登录）。
  原型里令牌为 expired 演示失效。
-->
<script lang="ts">
  import { CircleCheck, Link2Off } from "@lucide/svelte";
  import AuthShell from "./AuthShell.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import { t } from "../../lib/i18n";
  import { href } from "../../lib/router.svelte";
  import "./strings";

  let { token }: { token: string } = $props();
  const invalid = $derived(token === "expired" || !token);

  let next = $state("");
  let again = $state("");
  let tried = $state(false);
  let busy = $state(false);
  let done = $state(false);
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
    setTimeout(() => {
      busy = false;
      done = true;
    }, 700);
  }
</script>

<AuthShell>
  {#if invalid}
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
