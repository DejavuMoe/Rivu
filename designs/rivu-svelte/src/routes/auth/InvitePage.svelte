<!--
  接受邀请（公开页）。状态由评审面板的 app.review.invite 切换：
  loading 骨架；invalid 无效或过期（失效、已用、已撤销都只显示这一种）；
  new 没有账号，填名字（可选）和密码；existing 已用被邀请的邮箱登录；
  other 登录的是别人，提示并可退出；signedOut 有账号但没登录，去登录。
  app.review.inviteScope 切换整个组织的邀请与单个站点的访客邀请。
-->
<script lang="ts">
  import AuthShell from "./AuthShell.svelte";
  import Button from "../../lib/ui/Button.svelte";
  import Field from "../../lib/ui/Field.svelte";
  import Input from "../../lib/ui/Input.svelte";
  import Skeleton from "../../lib/ui/Skeleton.svelte";
  import { t } from "../../lib/i18n";
  import { app, setReview, toast } from "../../lib/state.svelte";
  import { href, navigate } from "../../lib/router.svelte";
  import "./strings";

  const org = "Northwind Labs";
  const site = "Northwind Docs";
  const inviter = "林夏";
  const email = "sam.lee@example.com";
  const other = "alex@example.com";

  let name = $state("");
  let password = $state("");
  let tried = $state(false);
  let busy = $state(false);

  const mode = $derived(app.review.invite);
  const isSite = $derived(app.review.inviteScope === "site");
  const roleLabel = $derived(t(isSite ? "role.viewer" : "role.editor"));

  const pwLen = $derived([...password].length);
  const nameLen = $derived([...name.trim()].length);
  const errors = $derived({
    name: nameLen > 100 ? t("setup.name.long") : "",
    password: pwLen < 12 ? t("setup.password.short") : pwLen > 128 ? t("setup.password.long") : "",
  });

  function accept(e?: SubmitEvent) {
    e?.preventDefault();
    if (busy) return;
    if (mode === "new") {
      tried = true;
      if (errors.name || errors.password) return;
    }
    busy = true;
    setTimeout(() => {
      busy = false;
      toast(t("inv.acc.joined", { org }), "good");
      navigate(isSite ? "/s/2" : "/sites");
    }, 650);
  }

  function signOut() {
    if (busy) return;
    busy = true;
    setTimeout(() => {
      busy = false;
      setReview({ invite: "signedOut" });
    }, 400);
  }
</script>

<AuthShell>
  {#if mode === "loading"}
    <div class="stack" aria-busy="true">
      <Skeleton width="60%" height={20} />
      <Skeleton width="80%" height={14} />
      <Skeleton height={40} radius={6} />
    </div>
  {:else if mode === "invalid"}
    <div class="stack">
      <h1>{t("inv.acc.invalid")}</h1>
      <Button href={href("/login")} block>{t("inv.acc.toLogin")}</Button>
    </div>
  {:else}
    <form class="stack" onsubmit={accept} novalidate>
      <h1>{isSite ? t("inv.acc.title.site", { org, site }) : t("inv.acc.title.org", { org })}</h1>
      <p class="muted">{t("inv.acc.as", { email, role: roleLabel })}</p>
      <p class="muted">{t("inv.acc.by", { name: inviter })}</p>

      {#if mode === "existing"}
        <Button type="submit" variant="primary" size="lg" block loading={busy}>{t("inv.acc.accept")}</Button>
      {:else if mode === "other"}
        <p>{t("inv.acc.other", { current: other, email })}</p>
        <Button size="lg" block loading={busy} onclick={signOut}>{t("inv.acc.signOut")}</Button>
      {:else if mode === "signedOut"}
        <Button variant="primary" size="lg" block href={href("/login")}>{t("inv.acc.signIn")}</Button>
      {:else}
        <Field label={t("setup.name")} for="inv-name" optional error={tried ? errors.name : ""}>
          <Input id="inv-name" autocomplete="name" bind:value={name} invalid={tried && !!errors.name} />
        </Field>
        <Field
          label={t("setup.password")}
          for="inv-password"
          hint={t("setup.password.hint")}
          error={tried || pwLen > 128 ? errors.password : ""}
        >
          <Input
            id="inv-password"
            type="password"
            autocomplete="new-password"
            bind:value={password}
            invalid={(tried || pwLen > 128) && !!errors.password}
          />
        </Field>
        <Button type="submit" variant="primary" size="lg" block loading={busy}>{t("inv.acc.create")}</Button>
      {/if}
    </form>
  {/if}
</AuthShell>

<style>
  .stack {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  h1 {
    font-size: var(--fs-lg);
  }
  p {
    line-height: var(--lh);
  }
</style>
